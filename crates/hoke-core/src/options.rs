use crate::DbError;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DatabaseOptions {
    pub max_memtable_bytes: Option<usize>,

    pub max_wal_bytes: Option<u64>,
}
impl DatabaseOptions {
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
