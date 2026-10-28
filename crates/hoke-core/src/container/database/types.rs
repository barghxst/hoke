//! Public errors and diagnostic counters for the container database.
use crate::DbError;
use crate::container::JournalError;
use std::fmt;

#[derive(Debug)]
pub enum ContainerDbError {
    Journal(JournalError),
    Data(DbError),
    Corruption(String),
    Limit(&'static str),
    Poisoned,
}
impl fmt::Display for ContainerDbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Journal(e) => e.fmt(f),
            Self::Data(e) => e.fmt(f),
            Self::Corruption(e) => write!(f, "corrupt container database: {e}"),
            Self::Limit(e) => write!(f, "container database limit: {e}"),
            Self::Poisoned => {
                f.write_str("container outcome unknown; reopen before further operations")
            }
        }
    }
}
impl std::error::Error for ContainerDbError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Journal(e) => Some(e),
            Self::Data(e) => Some(e),
            _ => None,
        }
    }
}
impl From<JournalError> for ContainerDbError {
    fn from(e: JournalError) -> Self {
        Self::Journal(e)
    }
}
impl From<DbError> for ContainerDbError {
    fn from(e: DbError) -> Self {
        Self::Data(e)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContainerStats {
    pub generation: u64,
    pub table_count: usize,
    pub wal_segments: usize,
    pub memtable_entries: usize,
    pub memtable_bytes: usize,
}
