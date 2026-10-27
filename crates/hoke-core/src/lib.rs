//! Experimental WAL and directory-backed sorted-run key/value engines.
//!
//! Records are synchronized before publication and replayed on open. Full record
//! corruption is an error; incomplete EOF appends are discarded during recovery.
//! Database adds immutable indexed SSTables and atomic manifest publication.
//! Manual compaction reclaims obsolete files. Atomic directory batches use manifest publication; no production durability claim.
//! Memory storage is explicit and volatile; file storage has no automatic fallback.

pub mod storage;

mod engine;
mod wal;

pub use engine::{Engine, Error, Recovery};

mod batch;
mod database;
pub mod directory;
mod manifest;
mod native_directory;
mod options;
mod scan;
mod sstable;

pub use batch::{BatchOperation, MAX_BATCH_BYTES, MAX_BATCH_OPERATIONS, validate_batch};
pub use database::{
    CleanupError, CleanupReport, CompactionReport, Database, DatabaseStats, DbError,
};
pub use native_directory::NativeDirectory;
pub use options::DatabaseOptions;
pub use scan::{KeyValue, ScanOptions};
pub use sstable::SstError;
