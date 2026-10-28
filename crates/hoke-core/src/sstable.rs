//! Immutable, indexed sorted runs shared by native and platform adapters.
//!
//! This module reads and writes the existing SST v1 format. It does not publish
//! files or make them durable: callers must finish writing, flush and synchronize
//! their storage, then publish the reference using their own commit protocol.
//! Keep storage backing an open table immutable for the table's entire lifetime.
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

/// Maximum number of records, including tombstones, in one table.
pub const MAX_ENTRIES: usize = 1_000_000;
/// Maximum encoded key-index size, excluding its trailing checksum.
pub const MAX_INDEX_BYTES: usize = 64 * 1024 * 1024;
/// Maximum number of immutable runs accepted by one streaming merge.
pub const MAX_MERGE_SOURCES: usize = 64;

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
