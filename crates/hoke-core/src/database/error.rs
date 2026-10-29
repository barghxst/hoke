use std::{fmt, io};

use crate::{Error, sstable::SstError};

#[derive(Debug)]
pub enum DbError {
    Io {
        operation: &'static str,
        source: io::Error,
    },
    Wal(Error),
    Sst(SstError),
    Corruption(String),
    Limit(&'static str),
    InvalidRange,
    AllocationFailed,
    Poisoned,
}
impl fmt::Display for DbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { operation, source } => write!(f, "{operation}: {source}"),
            Self::Wal(error) => write!(f, "WAL: {error}"),
            Self::Sst(error) => write!(f, "SST: {error}"),
            Self::Corruption(reason) => write!(f, "corrupt database: {reason}"),
            Self::Limit(reason) => write!(f, "database limit: {reason}"),
            Self::InvalidRange => write!(f, "invalid range: start is greater than end"),
            Self::AllocationFailed => write!(f, "could not allocate scan results"),
            Self::Poisoned => write!(f, "database is poisoned; close and reopen before mutations"),
        }
    }
}
impl std::error::Error for DbError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Wal(e) => Some(e),
            Self::Sst(e) => Some(e),
            _ => None,
        }
    }
}
pub(super) fn io_error(operation: &'static str) -> impl FnOnce(io::Error) -> DbError {
    move |source| DbError::Io { operation, source }
}
