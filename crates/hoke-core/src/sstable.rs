//! Immutable, indexed sorted runs shared by native and platform adapters.
//!
//! This module reads and writes the existing SST v1 format. It does not publish
//! files or make them durable: callers must finish writing, flush and synchronize
//! their storage, then publish the reference using their own commit protocol.
//! Keep storage backing an open table immutable for the table's entire lifetime.
use std::{collections::BTreeMap, error::Error, fmt, io, ops::Bound};

use crate::{
    storage::Storage,
    wal::{self, Operation},
};

const HEADER_LEN: usize = 32;
const MAGIC: &[u8; 8] = b"STOWSST\0";
const VERSION: u32 = 1;
/// Maximum number of records, including tombstones, in one table.
pub const MAX_ENTRIES: usize = 1_000_000;
/// Maximum encoded key-index size, excluding its trailing checksum.
pub const MAX_INDEX_BYTES: usize = 64 * 1024 * 1024;

/// Storage, validation, allocation, or record-codec failure.
#[derive(Debug)]
pub enum SstError {
    Io(io::Error),
    Invalid(&'static str),
    Allocation,
    Codec(String),
}
impl fmt::Display for SstError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "SST I/O: {error}"),
            Self::Invalid(reason) => write!(f, "invalid SST: {reason}"),
            Self::Allocation => f.write_str("could not allocate SST buffer"),
            Self::Codec(reason) => write!(f, "invalid SST record: {reason}"),
        }
    }
}
impl Error for SstError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}
impl From<io::Error> for SstError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}
impl From<wal::CodecError> for SstError {
    fn from(error: wal::CodecError) -> Self {
        Self::Codec(error.to_string())
    }
}

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

fn buffer(len: usize) -> Result<Vec<u8>, SstError> {
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(len)
        .map_err(|_| SstError::Allocation)?;
    bytes.resize(len, 0);
    Ok(bytes)
}
fn copy(bytes: &[u8]) -> Result<Vec<u8>, SstError> {
    let mut result = buffer(bytes.len())?;
    result.copy_from_slice(bytes);
    Ok(result)
}
fn read_exact<S: Storage>(
    storage: &mut S,
    mut offset: u64,
    mut bytes: &mut [u8],
) -> Result<(), SstError> {
    while !bytes.is_empty() {
        let count = storage.read_at(offset, bytes)?;
        if count == 0 {
            return Err(SstError::Invalid("truncated file"));
        }
        if count > bytes.len() {
            return Err(SstError::Invalid("storage returned invalid read length"));
        }
        offset = offset
            .checked_add(count as u64)
            .ok_or(SstError::Invalid("offset overflow"))?;
        bytes = &mut bytes[count..];
    }
    Ok(())
}
fn u32_at(bytes: &[u8], start: usize) -> u32 {
    u32::from_le_bytes(bytes[start..start + 4].try_into().unwrap())
}
fn u64_at(bytes: &[u8], start: usize) -> u64 {
    u64::from_le_bytes(bytes[start..start + 8].try_into().unwrap())
}
fn header(count: usize, index_offset: u64) -> [u8; HEADER_LEN] {
    let mut bytes = [0; HEADER_LEN];
    bytes[..8].copy_from_slice(MAGIC);
    bytes[8..12].copy_from_slice(&VERSION.to_le_bytes());
    bytes[12..20].copy_from_slice(&(count as u64).to_le_bytes());
    bytes[20..28].copy_from_slice(&index_offset.to_le_bytes());
    let crc = crc32fast::hash(&bytes[..28]);
    bytes[28..].copy_from_slice(&crc.to_le_bytes());
    bytes
}

/// Write sorted entries to an empty destination, with `None` encoding a tombstone.
///
/// Input sizes are checked before the first write. An I/O error can leave a partial
/// file, which must not be published. Success does not flush or synchronize storage;
/// the caller must do so before publishing its reference.
pub fn write_table<S: Storage>(
    storage: &mut S,
    entries: &BTreeMap<Vec<u8>, Option<Vec<u8>>>,
) -> Result<(), SstError> {
    if !storage.is_empty()? {
        return Err(SstError::Invalid("destination is not empty"));
    }
    validate_entries(entries)?;
    let mut writer = TableWriter::new(storage)?;
    for (key, value) in entries {
        writer.append(key, value.as_deref())?;
    }
    writer.finish()?;
    Ok(())
}

/// Check record and index size limits without performing I/O.
///
/// This allows a caller to reject capacity errors before creating a candidate file.
/// Ordering and uniqueness follow from the input's [`BTreeMap`] representation.
pub fn validate_entries(entries: &BTreeMap<Vec<u8>, Option<Vec<u8>>>) -> Result<(), SstError> {
    if entries.len() > MAX_ENTRIES {
        return Err(SstError::Invalid("too many entries"));
    }
    // Preflight every caller-controlled size before changing the destination.
    let mut index_len = 0usize;
    for (key, value) in entries {
        if key.len() > wal::MAX_KEY_BYTES {
            return Err(SstError::Invalid("key too large"));
        }
        if value
            .as_ref()
            .is_some_and(|v| v.len() > wal::MAX_VALUE_BYTES)
        {
            return Err(SstError::Invalid("value too large"));
        }
        index_len = index_len
            .checked_add(12 + key.len())
            .ok_or(SstError::Invalid("index size overflow"))?;
        if index_len > MAX_INDEX_BYTES {
            return Err(SstError::Invalid("index too large"));
        }
    }
    Ok(())
}

/// Stream an immutable run to a new file while retaining only its bounded key index.
/// Any append error invalidates the writer; the partial file must not be published.
/// The caller must flush/sync the completed file before publishing its reference.
pub struct TableWriter<'a, S: Storage> {
    storage: &'a mut S,
    index: Vec<u8>,
    count: usize,
    offset: u64,
    last_key_start: Option<usize>,
    failed: bool,
}

impl<'a, S: Storage> TableWriter<'a, S> {
    /// Begin a table in empty storage without flushing or synchronizing it.
    /// A failed initialization can leave a partial file; do not publish that file.
    pub fn new(storage: &'a mut S) -> Result<Self, SstError> {
        if !storage.is_empty()? {
            return Err(SstError::Invalid("destination is not empty"));
        }
        storage.write_all_at(0, &[0; HEADER_LEN])?;
        Ok(Self {
            storage,
            index: Vec::new(),
            count: 0,
            offset: HEADER_LEN as u64,
            last_key_start: None,
            failed: false,
        })
    }

    /// Append one key in strictly increasing byte order, including tombstones.
    /// Any error invalidates this writer; subsequent append/finish calls fail.
    pub fn append(&mut self, key: &[u8], value: Option<&[u8]>) -> Result<(), SstError> {
        if self.failed {
            return Err(SstError::Invalid("writer failed"));
        }
        let result = self.append_inner(key, value);
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn append_inner(&mut self, key: &[u8], value: Option<&[u8]>) -> Result<(), SstError> {
        if self.count >= MAX_ENTRIES {
            return Err(SstError::Invalid("too many entries"));
        }
        if key.len() > wal::MAX_KEY_BYTES {
            return Err(SstError::Invalid("key too large"));
        }
        if value.is_some_and(|value| value.len() > wal::MAX_VALUE_BYTES) {
            return Err(SstError::Invalid("value too large"));
        }
        if self
            .last_key_start
            .is_some_and(|start| &self.index[start..self.index.len() - 8] >= key)
        {
            return Err(SstError::Invalid("keys are not strictly sorted"));
        }
        let added = 12 + key.len();
        let index_len = self
            .index
            .len()
            .checked_add(added)
            .ok_or(SstError::Invalid("index size overflow"))?;
        if index_len > MAX_INDEX_BYTES {
            return Err(SstError::Invalid("index too large"));
        }
        if self.index.capacity() < index_len {
            // Geometric growth without requesting a capacity above the format limit.
            let capacity = self
                .index
                .capacity()
                .saturating_mul(2)
                .max(index_len)
                .min(MAX_INDEX_BYTES);
            self.index
                .try_reserve_exact(capacity - self.index.len())
                .map_err(|_| SstError::Allocation)?;
        }
        let operation = match value {
            Some(value) => Operation::Put {
                key: copy(key)?,
                value: copy(value)?,
            },
            None => Operation::Delete { key: copy(key)? },
        };
        let frame = wal::encode_frame(self.count as u64 + 1, &operation)?;
        let next_offset = self
            .offset
            .checked_add(frame.len() as u64)
            .ok_or(SstError::Invalid("offset overflow"))?;
        self.storage.write_all_at(self.offset, &frame)?;
        self.last_key_start = Some(self.index.len() + 4);
        self.index
            .extend_from_slice(&(key.len() as u32).to_le_bytes());
        self.index.extend_from_slice(key);
        self.index.extend_from_slice(&self.offset.to_le_bytes());
        self.offset = next_offset;
        self.count += 1;
        Ok(())
    }

    /// Finish the index/header and return the record count without flush or sync.
    /// On failure, discard the partial candidate rather than publishing it.
    pub fn finish(self) -> Result<usize, SstError> {
        if self.failed {
            return Err(SstError::Invalid("writer failed"));
        }
        let checksum_offset = self
            .offset
            .checked_add(self.index.len() as u64)
            .ok_or(SstError::Invalid("offset overflow"))?;
        self.storage.write_all_at(self.offset, &self.index)?;
        self.storage
            .write_all_at(checksum_offset, &crc32fast::hash(&self.index).to_le_bytes())?;
        self.storage
            .write_all_at(0, &header(self.count, self.offset))?;
        Ok(self.count)
    }
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
mod tests {
    use super::*;
    use crate::storage::MemoryStorage;

    fn sample() -> MemoryStorage {
        let entries = BTreeMap::from([
            (vec![], Some(vec![])),
            (b"a".to_vec(), Some(b"first".to_vec())),
            (b"b".to_vec(), None),
            (vec![255, 0], Some(vec![0, 255])),
        ]);
        let mut storage = MemoryStorage::new();
        write_table(&mut storage, &entries).unwrap();
        storage
    }
    fn bytes(mut storage: MemoryStorage) -> Vec<u8> {
        let mut result = vec![0; storage.len().unwrap() as usize];
        read_exact(&mut storage, 0, &mut result).unwrap();
        result
    }
    fn from_bytes(bytes: &[u8]) -> MemoryStorage {
        let mut storage = MemoryStorage::new();
        storage.write_all_at(0, bytes).unwrap();
        storage
    }
    fn repair_header(bytes: &mut [u8]) {
        let crc = crc32fast::hash(&bytes[..28]);
        bytes[28..32].copy_from_slice(&crc.to_le_bytes());
    }
    fn repair_index(bytes: &mut [u8]) {
        let start = u64_at(bytes, 20) as usize;
        let end = bytes.len() - 4;
        let crc = crc32fast::hash(&bytes[start..end]);
        bytes[end..].copy_from_slice(&crc.to_le_bytes());
    }
    #[test]
    fn index_ranges_match_byte_bounds_including_tombstone_keys() {
        let table = SSTable::open(sample()).unwrap();
        let keys: [&[u8]; 4] = [b"", b"a", b"b", &[255, 0]];
        for (position, key) in keys.iter().enumerate() {
            assert_eq!(table.key_at(position), Some(*key));
        }
        assert_eq!(table.key_at(keys.len()), None);
        assert_eq!(table.key_at(usize::MAX), None);

        let mut bounds = vec![Bound::Unbounded];
        for key in [
            vec![],
            vec![0],
            b"a".to_vec(),
            b"aa".to_vec(),
            b"b".to_vec(),
            vec![255],
            vec![255, 0],
            vec![255, 255],
        ] {
            bounds.push(Bound::Included(key.clone()));
            bounds.push(Bound::Excluded(key));
        }
        for start in &bounds {
            for end in &bounds {
                let positions = table.range_positions(start, end);
                assert!(positions.start <= positions.end);
                assert!(positions.end <= keys.len());
                let actual: Vec<_> = positions
                    .map(|position| table.key_at(position).unwrap())
                    .collect();
                let expected: Vec<_> = keys
                    .iter()
                    .copied()
                    .filter(|key| match start {
                        Bound::Included(start) => *key >= start.as_slice(),
                        Bound::Excluded(start) => *key > start.as_slice(),
                        Bound::Unbounded => true,
                    })
                    .filter(|key| match end {
                        Bound::Included(end) => *key <= end.as_slice(),
                        Bound::Excluded(end) => *key < end.as_slice(),
                        Bound::Unbounded => true,
                    })
                    .collect();
                assert_eq!(actual, expected, "start={start:?}, end={end:?}");
            }
        }
    }
    #[test]
    fn empty_table_index_ranges_are_empty() {
        let mut storage = MemoryStorage::new();
        write_table(&mut storage, &BTreeMap::new()).unwrap();
        let table = SSTable::open(storage).unwrap();
        assert_eq!(table.key_at(0), None);
        for bound in [
            Bound::Unbounded,
            Bound::Included(vec![]),
            Bound::Excluded(vec![255]),
        ] {
            assert_eq!(table.range_positions(&bound, &Bound::Unbounded), 0..0);
            assert_eq!(table.range_positions(&Bound::Unbounded, &bound), 0..0);
        }
    }
    #[test]
    fn index_metadata_does_not_read_corrupt_record_payloads() {
        let mut raw = bytes(sample());
        // Damage the tombstone's key while preserving the valid index/header.
        let tombstone_offset = HEADER_LEN
            + wal::FRAME_HEADER_LEN
            + wal::TRAILER_LEN
            + wal::FRAME_HEADER_LEN
            + 1
            + b"first".len()
            + wal::TRAILER_LEN;
        raw[tombstone_offset + wal::FRAME_HEADER_LEN] ^= 1;
        let mut table = SSTable::open(from_bytes(&raw)).unwrap();
        assert_eq!(table.key_at(2), Some(b"b".as_slice()));
        assert_eq!(
            table.range_positions(
                &Bound::Included(b"b".to_vec()),
                &Bound::Included(b"b".to_vec())
            ),
            2..3
        );
        assert!(table.get(b"b").is_err());
    }
    #[test]
    fn empty_and_binary_roundtrips_distinguish_missing_empty_and_deleted() {
        let mut storage = MemoryStorage::new();
        write_table(&mut storage, &BTreeMap::new()).unwrap();
        assert_eq!(storage.len().unwrap(), 36);
        assert_eq!(
            SSTable::open(storage).unwrap().get(b"absent").unwrap(),
            Lookup::Missing
        );
        let mut table = SSTable::open(sample()).unwrap();
        for (key, expected) in [
            (vec![], Lookup::Value(vec![])),
            (b"a".to_vec(), Lookup::Value(b"first".to_vec())),
            (b"b".to_vec(), Lookup::Deleted),
            (vec![255, 0], Lookup::Value(vec![0, 255])),
            (b"missing".to_vec(), Lookup::Missing),
        ] {
            assert_eq!(table.get(&key).unwrap(), expected);
        }
    }
    #[test]
    fn rejects_every_truncation_and_trailing_bytes() {
        let valid = bytes(sample());
        for length in 0..valid.len() {
            assert!(
                SSTable::open(from_bytes(&valid[..length])).is_err(),
                "length {length}"
            );
        }
        let mut extra = valid;
        extra.push(0);
        assert!(SSTable::open(from_bytes(&extra)).is_err());
    }
    #[test]
    fn rejects_corrupt_headers_and_index_but_payload_is_lazy() {
        let valid = bytes(sample());
        let index_offset = u64_at(&valid, 20) as usize;
        for i in (0..HEADER_LEN).chain(index_offset..valid.len()) {
            let mut bad = valid.clone();
            bad[i] ^= 1;
            assert!(SSTable::open(from_bytes(&bad)).is_err(), "byte {i}");
        }
        // The second frame contains key a and value first.
        let second = HEADER_LEN + wal::FRAME_HEADER_LEN + wal::TRAILER_LEN;
        let mut bad = valid;
        bad[second + wal::FRAME_HEADER_LEN + 1] ^= 1;
        let mut table = SSTable::open(from_bytes(&bad)).unwrap();
        assert_eq!(table.get(b"b").unwrap(), Lookup::Deleted);
        assert!(table.get(b"a").is_err());
    }
    #[test]
    fn rejects_repaired_header_limits_and_hostile_offsets() {
        let valid = bytes(sample());
        for (start, replacement) in [
            (12, (MAX_ENTRIES as u64 + 1)),
            (12, u64::MAX),
            (20, 0),
            (20, 31),
            (20, u64::MAX),
        ] {
            let mut bad = valid.clone();
            bad[start..start + 8].copy_from_slice(&replacement.to_le_bytes());
            repair_header(&mut bad);
            assert!(SSTable::open(from_bytes(&bad)).is_err());
        }
        let mut bad = valid;
        bad[8..12].copy_from_slice(&2u32.to_le_bytes());
        repair_header(&mut bad);
        assert!(SSTable::open(from_bytes(&bad)).is_err());
    }
    #[test]
    fn repaired_index_checksum_does_not_bypass_semantics() {
        let valid = bytes(sample());
        let start = u64_at(&valid, 20) as usize;
        for replacement in [0, 31, 33, u64::MAX] {
            let mut bad = valid.clone();
            bad[start + 4..start + 12].copy_from_slice(&replacement.to_le_bytes());
            repair_index(&mut bad);
            assert!(SSTable::open(from_bytes(&bad)).is_err());
        }
        let mut bad = valid.clone();
        bad[start..start + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        repair_index(&mut bad);
        assert!(SSTable::open(from_bytes(&bad)).is_err());
        // b -> a makes duplicate sorted keys despite a valid CRC.
        let mut bad = valid.clone();
        bad[start + 12 + 13 + 4] = b'a';
        repair_index(&mut bad);
        assert!(SSTable::open(from_bytes(&bad)).is_err());
        // a -> c is no longer sorted ahead of b.
        let mut bad = valid;
        bad[start + 12 + 4] = b'c';
        repair_index(&mut bad);
        assert!(SSTable::open(from_bytes(&bad)).is_err());
    }
    #[test]
    fn lazy_decode_checks_key_against_index_even_with_valid_frame_crc() {
        let mut valid = bytes(sample());
        let offset = HEADER_LEN + wal::FRAME_HEADER_LEN + wal::TRAILER_LEN;
        let header: &[u8; wal::FRAME_HEADER_LEN] = valid[offset..offset + wal::FRAME_HEADER_LEN]
            .try_into()
            .unwrap();
        let len = wal::decode_frame_header(header).unwrap().frame_len();
        valid[offset + wal::FRAME_HEADER_LEN] = b'z';
        let crc = crc32fast::hash(&valid[offset..offset + len - 4]);
        valid[offset + len - 4..offset + len].copy_from_slice(&crc.to_le_bytes());
        let mut table = SSTable::open(from_bytes(&valid)).unwrap();
        assert!(table.get(b"a").is_err());
    }
    #[test]
    fn validates_fixed_record_headers_at_open() {
        let valid = bytes(sample());
        for index in HEADER_LEN..HEADER_LEN + wal::FRAME_HEADER_LEN {
            let mut bad = valid.clone();
            bad[index] ^= 1;
            assert!(SSTable::open(from_bytes(&bad)).is_err());
        }
        let mut bad = valid;
        bad[HEADER_LEN + 4..HEADER_LEN + 12].copy_from_slice(&2u64.to_le_bytes());
        let crc = crc32fast::hash(&bad[HEADER_LEN..HEADER_LEN + 24]);
        bad[HEADER_LEN + 24..HEADER_LEN + 28].copy_from_slice(&crc.to_le_bytes());
        assert!(SSTable::open(from_bytes(&bad)).is_err());
    }
    #[test]
    fn refuses_nonempty_and_preflights_limits_before_writing() {
        let mut storage = sample();
        let original = bytes(sample());
        assert!(write_table(&mut storage, &BTreeMap::new()).is_err());
        assert_eq!(bytes(storage), original);
        for entries in [
            BTreeMap::from([(vec![0; wal::MAX_KEY_BYTES + 1], None)]),
            BTreeMap::from([(vec![], Some(vec![0; wal::MAX_VALUE_BYTES + 1]))]),
        ] {
            let mut storage = MemoryStorage::new();
            assert!(write_table(&mut storage, &entries).is_err());
            assert!(storage.is_empty().unwrap());
        }
    }
    #[test]
    fn index_limit_checked_before_destination_write() {
        let entries = (0..1024u32)
            .map(|i| {
                let mut key = vec![0; wal::MAX_KEY_BYTES];
                key[..4].copy_from_slice(&i.to_be_bytes());
                (key, None)
            })
            .collect();
        let mut storage = MemoryStorage::new();
        assert!(write_table(&mut storage, &entries).is_err());
        assert!(storage.is_empty().unwrap());
    }
    #[test]
    fn tombstone_metadata_distinguishes_delete_from_empty_live_values() {
        assert!(SSTable::open(sample()).unwrap().has_tombstones());
        for entries in [
            BTreeMap::new(),
            BTreeMap::from([
                (vec![], Some(vec![])),
                (b"a".to_vec(), Some(b"value".to_vec())),
            ]),
        ] {
            let mut storage = MemoryStorage::new();
            write_table(&mut storage, &entries).unwrap();
            assert!(!SSTable::open(storage).unwrap().has_tombstones());
        }
        let mut storage = MemoryStorage::new();
        write_table(&mut storage, &BTreeMap::from([(vec![], None)])).unwrap();
        assert!(SSTable::open(storage).unwrap().has_tombstones());
    }

    #[test]
    fn streaming_writer_roundtrips_reused_buffers_and_matches_v1_bytes() {
        let entries = BTreeMap::from([
            (vec![], Some(vec![])),
            (b"a".to_vec(), Some(b"first".to_vec())),
            (b"b".to_vec(), None),
            (vec![255, 0], Some(vec![0, 255])),
        ]);
        let mut storage = MemoryStorage::new();
        let mut writer = TableWriter::new(&mut storage).unwrap();
        let mut scratch = Vec::new();
        for (key, value) in &entries {
            scratch.clear();
            if let Some(value) = value {
                scratch.extend_from_slice(value);
            }
            writer
                .append(key, value.as_ref().map(|_| scratch.as_slice()))
                .unwrap();
            scratch.fill(42);
        }
        assert_eq!(writer.finish().unwrap(), 4);
        assert_eq!(bytes(storage), bytes(sample()));
        let mut empty = MemoryStorage::new();
        assert_eq!(TableWriter::new(&mut empty).unwrap().finish().unwrap(), 0);
        assert_eq!(
            SSTable::open(empty).unwrap().get(b"anything").unwrap(),
            Lookup::Missing
        );
    }

    #[test]
    fn streaming_writer_refuses_nonempty_and_unsorted_or_duplicate_keys() {
        let mut nonempty = sample();
        assert!(TableWriter::new(&mut nonempty).is_err());
        assert_eq!(bytes(nonempty), bytes(sample()));
        for second in [b"b".as_slice(), b"a"] {
            let mut storage = MemoryStorage::new();
            let mut writer = TableWriter::new(&mut storage).unwrap();
            writer.append(b"b", Some(b"value")).unwrap();
            assert!(writer.append(second, None).is_err());
            assert!(writer.append(b"z", None).is_err());
            assert!(writer.finish().is_err());
            assert!(SSTable::open(storage).is_err());
        }
    }

    #[test]
    fn streaming_writer_enforces_each_format_bound_without_appending() {
        let mut storage = MemoryStorage::new();
        let mut writer = TableWriter::new(&mut storage).unwrap();
        assert!(matches!(
            writer.append(&vec![0; wal::MAX_KEY_BYTES + 1], None),
            Err(SstError::Invalid("key too large"))
        ));
        drop(writer);
        assert_eq!(storage.len().unwrap(), HEADER_LEN as u64);

        let mut storage = MemoryStorage::new();
        let mut writer = TableWriter::new(&mut storage).unwrap();
        assert!(matches!(
            writer.append(b"", Some(&vec![0; wal::MAX_VALUE_BYTES + 1])),
            Err(SstError::Invalid("value too large"))
        ));
        drop(writer);
        assert_eq!(storage.len().unwrap(), HEADER_LEN as u64);

        let mut storage = MemoryStorage::new();
        let mut writer = TableWriter::new(&mut storage).unwrap();
        // Exercise boundary counters without writing a million records.
        writer.count = MAX_ENTRIES;
        assert!(matches!(
            writer.append(b"", None),
            Err(SstError::Invalid("too many entries"))
        ));
        drop(writer);
        assert_eq!(storage.len().unwrap(), HEADER_LEN as u64);

        let mut storage = MemoryStorage::new();
        let mut writer = TableWriter::new(&mut storage).unwrap();
        writer.index.resize(MAX_INDEX_BYTES - 11, 0);
        assert!(matches!(
            writer.append(b"", None),
            Err(SstError::Invalid("index too large"))
        ));
        assert!(writer.finish().is_err());
        assert_eq!(storage.len().unwrap(), HEADER_LEN as u64);
    }

    #[test]
    fn streaming_write_errors_are_propagated_and_prevent_finish_after_append_error() {
        struct FailWrites {
            storage: MemoryStorage,
            writes: usize,
            fail_at: usize,
        }
        impl Storage for FailWrites {
            fn len(&self) -> io::Result<u64> {
                self.storage.len()
            }
            fn read_at(&mut self, offset: u64, bytes: &mut [u8]) -> io::Result<usize> {
                self.storage.read_at(offset, bytes)
            }
            fn write_all_at(&mut self, offset: u64, bytes: &[u8]) -> io::Result<()> {
                self.writes += 1;
                if self.writes == self.fail_at {
                    return Err(io::Error::other("injected write"));
                }
                self.storage.write_all_at(offset, bytes)
            }
            fn set_len(&mut self, len: u64) -> io::Result<()> {
                self.storage.set_len(len)
            }
            fn flush(&mut self) -> io::Result<()> {
                self.storage.flush()
            }
            fn sync_all(&mut self) -> io::Result<()> {
                self.storage.sync_all()
            }
        }
        // Placeholder, two records, index, index CRC and final header.
        for fail_at in 1..=6 {
            let mut storage = FailWrites {
                storage: MemoryStorage::new(),
                writes: 0,
                fail_at,
            };
            let result = (|| {
                let mut writer = TableWriter::new(&mut storage)?;
                for key in [b"a", b"b"] {
                    if let Err(error) = writer.append(key, Some(b"value")) {
                        assert!(writer.finish().is_err());
                        return Err(error);
                    }
                }
                writer.finish()
            })();
            assert!(matches!(result, Err(SstError::Io(_))), "write {fail_at}");
        }
    }
    #[test]
    fn bounded_short_reads_and_io_source() {
        struct Short(MemoryStorage);
        impl Storage for Short {
            fn len(&self) -> io::Result<u64> {
                self.0.len()
            }
            fn read_at(&mut self, offset: u64, bytes: &mut [u8]) -> io::Result<usize> {
                let len = bytes.len().min(3);
                self.0.read_at(offset, &mut bytes[..len])
            }
            fn write_all_at(&mut self, _: u64, _: &[u8]) -> io::Result<()> {
                unreachable!()
            }
            fn set_len(&mut self, _: u64) -> io::Result<()> {
                unreachable!()
            }
            fn flush(&mut self) -> io::Result<()> {
                unreachable!()
            }
            fn sync_all(&mut self) -> io::Result<()> {
                unreachable!()
            }
        }
        assert_eq!(
            SSTable::open(Short(sample())).unwrap().get(b"a").unwrap(),
            Lookup::Value(b"first".to_vec())
        );
        let error = SstError::from(io::Error::other("injected"));
        assert!(error.source().is_some());
        assert!(error.to_string().contains("injected"));
    }
}
