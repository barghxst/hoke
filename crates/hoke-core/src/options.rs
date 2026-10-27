use crate::DbError;

/// Per-open maintenance policy; not persisted in the database files.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DatabaseOptions {
    /// Soft threshold for dirty key/value payload bytes, excluding SST indexes and
    /// allocator/node overhead. None disables this condition. A single larger record
    /// is allowed; the next mutation may flush before it writes its own WAL frame.
    pub max_memtable_bytes: Option<usize>,
    /// Soft threshold for the active WAL file including its 16-byte header and
    /// record framing. None disables this condition. One larger record is allowed.
    pub max_wal_bytes: Option<u64>,
}
impl DatabaseOptions {
    /// Validate before opening a native directory if no creation is desired on error.
    pub fn validate(&self) -> Result<(), DbError> {
        if self.max_memtable_bytes == Some(0) {
            return Err(DbError::Limit("memtable byte threshold must be positive"));
        }
        if self
            .max_wal_bytes
            .is_some_and(|limit| limit < crate::wal::FILE_HEADER_LEN as u64)
        {
            return Err(DbError::Limit("WAL byte threshold must be at least 16"));
        }
        Ok(())
    }
}
