//! Immutable table index validation and lazy, checksum-validated point reads.
use std::ops::Bound;

use crate::{
    storage::Storage,
    wal::{self, Operation},
};

use super::{
    MAX_ENTRIES, MAX_INDEX_BYTES, SstError,
    format::{HEADER_LEN, MAGIC, VERSION, buffer, copy, read_exact, u32_at, u64_at},
};

/// Result of a lookup within a single immutable table.
#[derive(Debug, PartialEq, Eq)]
pub enum Lookup {
    /// The key does not occur in this table.
    Missing,
    /// The table contains a tombstone; older tables must not supply a value.
    Deleted,
    /// An independently owned value read and validated from the table.
    Value(Vec<u8>),
}
struct Entry {
    key: Vec<u8>,
    offset: u64,
    frame_len: usize,
}
/// Immutable table reader owning its storage handle and bounded in-memory index.
///
/// The caller must prevent writes through other handles while this reader exists.
/// Opening validates the header, index, and record headers; payload checksums are
/// validated lazily by [`SSTable::get`], not by a full-table scrub at open time.
pub struct SSTable<S: Storage> {
    storage: S,
    index: Vec<Entry>,
    has_tombstones: bool,
}

impl<S: Storage> SSTable<S> {
    /// Open and validate an existing immutable table, taking ownership of storage.
    /// This does not create, repair, synchronize, or publish a table.
    pub fn open(mut storage: S) -> Result<Self, SstError> {
        let file_len = storage.len()?;
        if file_len < (HEADER_LEN + 4) as u64 {
            return Err(SstError::Invalid("truncated header/index"));
        }
        let mut bytes = [0; HEADER_LEN];
        read_exact(&mut storage, 0, &mut bytes)?;
        if &bytes[..8] != MAGIC {
            return Err(SstError::Invalid("magic"));
        }
        if crc32fast::hash(&bytes[..28]) != u32_at(&bytes, 28) {
            return Err(SstError::Invalid("header checksum"));
        }
        if u32_at(&bytes, 8) != VERSION {
            return Err(SstError::Invalid("unsupported version"));
        }
        let count = u64_at(&bytes, 12);
        if count > MAX_ENTRIES as u64 {
            return Err(SstError::Invalid("too many entries"));
        }
        let index_offset = u64_at(&bytes, 20);
        if index_offset < HEADER_LEN as u64 || index_offset > file_len - 4 {
            return Err(SstError::Invalid("index offset"));
        }
        let index_len = file_len - 4 - index_offset;
        if index_len > MAX_INDEX_BYTES as u64
            || index_len < count * 12
            || index_len > count * (12 + wal::MAX_KEY_BYTES) as u64
        {
            return Err(SstError::Invalid("index size"));
        }
        let mut raw = buffer(index_len as usize)?;
        read_exact(&mut storage, index_offset, &mut raw)?;
        let mut checksum = [0; 4];
        read_exact(&mut storage, file_len - 4, &mut checksum)?;
        if crc32fast::hash(&raw) != u32::from_le_bytes(checksum) {
            return Err(SstError::Invalid("index checksum"));
        }
        let mut index: Vec<Entry> = Vec::new();
        index
            .try_reserve_exact(count as usize)
            .map_err(|_| SstError::Allocation)?;
        let mut cursor = 0;
        let mut expected_offset = HEADER_LEN as u64;
        let mut has_tombstones = false;
        for ordinal in 1..=count {
            if raw.len() - cursor < 12 {
                return Err(SstError::Invalid("truncated index entry"));
            }
            let key_len = u32_at(&raw, cursor) as usize;
            if key_len > wal::MAX_KEY_BYTES || key_len > raw.len() - cursor - 12 {
                return Err(SstError::Invalid("index key length"));
            }
            cursor += 4;
            let key = &raw[cursor..cursor + key_len];
            if index
                .last()
                .is_some_and(|previous| previous.key.as_slice() >= key)
            {
                return Err(SstError::Invalid("keys are not strictly sorted"));
            }
            cursor += key_len;
            let offset = u64_at(&raw, cursor);
            cursor += 8;
            if offset != expected_offset
                || index_offset - expected_offset < wal::FRAME_HEADER_LEN as u64
            {
                return Err(SstError::Invalid("record offset"));
            }
            let mut frame_header = [0; wal::FRAME_HEADER_LEN];
            read_exact(&mut storage, offset, &mut frame_header)?;
            let decoded = wal::decode_frame_header(&frame_header)?;
            has_tombstones |= decoded.is_delete();
            if decoded.sequence != ordinal || decoded.key_len != key_len {
                return Err(SstError::Invalid("record/index mismatch"));
            }
            expected_offset = offset
                .checked_add(decoded.frame_len() as u64)
                .ok_or(SstError::Invalid("offset overflow"))?;
            if expected_offset > index_offset {
                return Err(SstError::Invalid("record crosses index"));
            }
            index.push(Entry {
                key: copy(key)?,
                offset,
                frame_len: decoded.frame_len(),
            });
        }
        if cursor != raw.len() || expected_offset != index_offset {
            return Err(SstError::Invalid("trailing record/index bytes"));
        }
        Ok(Self {
            storage,
            index,
            has_tombstones,
        })
    }

    /// Number of indexed records, including tombstones.
    pub fn len(&self) -> usize {
        self.index.len()
    }

    /// Whether the table contains no records, including tombstones.
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    /// Whether validated record headers include any delete operations.
    pub fn has_tombstones(&self) -> bool {
        self.has_tombstones
    }

    /// Borrow a key from the loaded index, including keys of tombstones.
    pub fn key_at(&self, position: usize) -> Option<&[u8]> {
        self.index.get(position).map(|entry| entry.key.as_slice())
    }

    /// Locate a range using index metadata only, without reading record frames.
    pub fn range_positions(
        &self,
        start: &Bound<Vec<u8>>,
        end: &Bound<Vec<u8>>,
    ) -> std::ops::Range<usize> {
        let start = match start {
            Bound::Included(key) => self.index.partition_point(|entry| entry.key < *key),
            Bound::Excluded(key) => self.index.partition_point(|entry| entry.key <= *key),
            Bound::Unbounded => 0,
        };
        let end = match end {
            Bound::Included(key) => self.index.partition_point(|entry| entry.key <= *key),
            Bound::Excluded(key) => self.index.partition_point(|entry| entry.key < *key),
            Bound::Unbounded => self.index.len(),
        };
        start..end.max(start)
    }

    /// Read a key and validate its complete record checksum against the index.
    /// A missing key requires no storage read; a returned value is an owned copy.
    pub fn get(&mut self, key: &[u8]) -> Result<Lookup, SstError> {
        let position = match self
            .index
            .binary_search_by(|entry| entry.key.as_slice().cmp(key))
        {
            Ok(position) => position,
            Err(_) => return Ok(Lookup::Missing),
        };
        let entry = &self.index[position];
        let mut frame = buffer(entry.frame_len)?;
        read_exact(&mut self.storage, entry.offset, &mut frame)?;
        let (sequence, operation) = wal::decode_frame(
            frame[..wal::FRAME_HEADER_LEN].try_into().unwrap(),
            &frame[wal::FRAME_HEADER_LEN..],
        )?;
        if sequence != position as u64 + 1 {
            return Err(SstError::Invalid("record sequence changed"));
        }
        let (actual_key, value) = match operation {
            Operation::Put { key, value } => (key, Lookup::Value(value)),
            Operation::Delete { key } => (key, Lookup::Deleted),
        };
        if actual_key != entry.key {
            return Err(SstError::Invalid("record key differs from index"));
        }
        Ok(value)
    }
}

#[cfg(test)]
#[path = "reader_tests.rs"]
mod tests;
