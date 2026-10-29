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

pub struct Database<D: Directory> {
    wal: Engine<D::File>,
    tables: Vec<SSTable<D::File>>,
    manifest: Manifest,
    poisoned: bool,
    options: crate::DatabaseOptions,

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
