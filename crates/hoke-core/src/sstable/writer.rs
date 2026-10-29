use std::collections::BTreeMap;

use crate::{
    storage::Storage,
    wal::{self, Operation},
};

use super::{
    MAX_ENTRIES, MAX_INDEX_BYTES, SstError,
    format::{HEADER_LEN, copy, header},
};

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

pub fn validate_entries(entries: &BTreeMap<Vec<u8>, Option<Vec<u8>>>) -> Result<(), SstError> {
    if entries.len() > MAX_ENTRIES {
        return Err(SstError::Invalid("too many entries"));
    }

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

pub struct TableWriter<'a, S: Storage> {
    storage: &'a mut S,
    index: Vec<u8>,
    count: usize,
    offset: u64,
    last_key_start: Option<usize>,
    failed: bool,
}

impl<'a, S: Storage> TableWriter<'a, S> {
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
