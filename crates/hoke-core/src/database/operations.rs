use crate::{Error, directory::Directory, manifest, sstable};

use super::{Database, DbError};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DatabaseStats {
    pub memtable_entries: usize,
    pub table_count: usize,
    pub wal_bytes: u64,

    pub memtable_bytes: usize,
}

impl<D: Directory> Database<D> {
    pub fn get(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>, DbError> {
        crate::read::get_from_sources(&self.wal, &mut self.tables, key)
    }

    pub fn scan(&mut self, options: crate::ScanOptions) -> Result<Vec<crate::KeyValue>, DbError> {
        crate::scan::scan_sources(&self.wal, &mut self.tables, options)
    }

    pub fn range(&mut self, options: crate::ScanOptions) -> Result<Vec<crate::KeyValue>, DbError> {
        self.scan(options)
    }
    fn mutation(&mut self, result: Result<(), Error>) -> Result<(), DbError> {
        if let Err(error) = result {
            if matches!(&error, Error::Io { .. } | Error::Poisoned) {
                self.poisoned = true;
            }
            return Err(DbError::Wal(error));
        }
        Ok(())
    }
    pub fn put(&mut self, key: &[u8], value: &[u8]) -> Result<(), DbError> {
        self.prepare_write(key, Some(value))?;
        let result = self.wal.put(key, value);
        self.mutation(result)
    }
    pub fn delete(&mut self, key: &[u8]) -> Result<(), DbError> {
        self.prepare_write(key, None)?;
        let result = self.wal.delete(key);
        self.mutation(result)
    }

    fn prepare_write(&mut self, key: &[u8], value: Option<&[u8]>) -> Result<(), DbError> {
        self.writable()?;
        if key.len() > crate::wal::MAX_KEY_BYTES
            || value.is_some_and(|v| v.len() > crate::wal::MAX_VALUE_BYTES)
        {
            return Err(DbError::Wal(Error::InvalidInput(
                if value.is_some() {
                    "key or value exceeds WAL size limit"
                } else {
                    "key exceeds WAL size limit"
                }
                .into(),
            )));
        }
        let payload_exceeded = self
            .options
            .max_memtable_bytes
            .is_some_and(|limit| self.wal.projected_change_bytes(key, value) > limit);
        let frame_bytes = (crate::wal::FRAME_HEADER_LEN + crate::wal::TRAILER_LEN) as u64
            + key.len() as u64
            + value.map_or(0, |v| v.len() as u64);
        let wal_exceeded = self.options.max_wal_bytes.is_some_and(|limit| {
            self.wal
                .byte_len()
                .checked_add(frame_bytes)
                .is_none_or(|projected| projected > limit)
        });
        if self.wal.change_count() > 0 && (payload_exceeded || wal_exceeded) {
            self.flush()?;
        }
        Ok(())
    }

    pub fn write_batch(&mut self, operations: &[crate::BatchOperation]) -> Result<(), DbError> {
        self.writable()?;
        crate::validate_batch(operations)?;
        if operations.is_empty() {
            return Ok(());
        }
        if self.manifest.tables.len() >= manifest::MAX_TABLES {
            return Err(DbError::Limit(
                "table limit reached; compaction is required",
            ));
        }
        let mut entries = self.wal.snapshot_changes().map_err(DbError::Wal)?;
        for operation in operations {
            match operation {
                crate::BatchOperation::Put { key, value } => {
                    entries.insert(key.clone(), Some(value.clone()));
                }
                crate::BatchOperation::Delete { key } => {
                    entries.insert(key.clone(), None);
                }
            }
        }
        sstable::validate_entries(&entries).map_err(DbError::Sst)?;
        if let Err(error) = self.flush_inner(&entries) {
            if !matches!(&error, DbError::Limit(_)) {
                self.poisoned = true;
            }
            return Err(error);
        }
        Ok(())
    }

    pub fn stats(&self) -> DatabaseStats {
        DatabaseStats {
            memtable_entries: self.wal.change_count(),
            table_count: self.tables.len(),
            wal_bytes: self.wal.byte_len(),
            memtable_bytes: self.wal.change_bytes(),
        }
    }
}
