//! Preflight and streaming construction of unpublished immutable tables.
use std::collections::BTreeMap;

use crate::{
    storage::Storage,
    wal::{self, Operation},
};

use super::{
    MAX_ENTRIES, MAX_INDEX_BYTES, SstError,
    format::{HEADER_LEN, copy, header},
};

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

#[cfg(test)]
#[path = "writer_tests.rs"]
mod tests;
