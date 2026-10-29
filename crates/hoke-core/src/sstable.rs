mod format;
mod merge;
mod reader;
mod writer;

#[cfg(test)]
mod test_support;

pub use merge::merge_tables;
pub use reader::{Lookup, SSTable};
pub use writer::{TableWriter, validate_entries, write_table};

use std::{error::Error, fmt, io};

use crate::wal;

pub const MAX_ENTRIES: usize = 1_000_000;

pub const MAX_INDEX_BYTES: usize = 64 * 1024 * 1024;

pub const MAX_MERGE_SOURCES: usize = 64;

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
