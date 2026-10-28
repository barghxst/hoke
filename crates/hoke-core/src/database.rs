//! Native directory database: coordinate WAL, immutable tables, and manifest commits.
//! Candidate publication and cleanup live beside the operations that own their policy.
mod error;
mod garbage;
mod lifecycle;
mod maintenance;
mod operations;
mod publication;

pub use error::DbError;
pub use garbage::{CleanupError, CleanupReport};
pub use maintenance::CompactionReport;
pub use operations::DatabaseStats;

use crate::{Engine, directory::Directory, manifest::Manifest, sstable::SSTable};

/// Experimental directory-backed sorted runs with manual or threshold-based flush.
/// Values leave the memtable after flush; complete key indexes remain in memory.
/// Manual full compaction and explicit garbage collection reclaim obsolete files.
pub struct Database<D: Directory> {
    wal: Engine<D::File>,
    tables: Vec<SSTable<D::File>>,
    manifest: Manifest,
    poisoned: bool,
    options: crate::DatabaseOptions,
    // The directory lock must outlive all opened data file handles.
    directory: D,
}
impl<D: Directory> Database<D> {
    fn writable(&self) -> Result<(), DbError> {
        if self.poisoned {
            Err(DbError::Poisoned)
        } else {
            Ok(())
        }
    }
}
