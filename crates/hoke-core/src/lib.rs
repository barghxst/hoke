//! Experimental local key/value storage, independent of browser and UI APIs.
//!
//! Three coordinators share WAL/SST codecs and ordered read algorithms:
//! - [`Engine`]: one WAL and its recovered in-memory state;
//! - [`Database`]: directory-backed WAL/SST files published through a manifest;
//! - [`ContainerDatabase`]: immutable segments in one append-only commit journal.
//!
//! Platform adapters implement [`storage::Storage`]. Writes synchronize before
//! publication; complete corruption is an error, while physically incomplete EOF
//! appends may be discarded during recovery. Only directory compaction reclaims
//! obsolete files; container compaction does not shrink the file.
//!
//! Memory storage is explicitly volatile. No automatic fallback or production
//! durability claim is provided. See docs/ARCHITECTURE.md for the component map.

pub mod container;
pub mod storage;

mod engine;
mod wal;

pub use engine::{Engine, Error, Recovery};

mod batch;
mod database;
pub mod directory;
mod manifest;
#[cfg(not(target_family = "wasm"))]
mod native_directory;
mod options;
pub mod read;
mod scan;
pub mod sstable;

pub use batch::{BatchOperation, MAX_BATCH_BYTES, MAX_BATCH_OPERATIONS, validate_batch};
pub use database::{
    CleanupError, CleanupReport, CompactionReport, Database, DatabaseStats, DbError,
};
#[cfg(not(target_family = "wasm"))]
pub use native_directory::NativeDirectory;
pub use options::DatabaseOptions;
pub use scan::{KeyValue, ScanOptions};
pub use sstable::SstError;

pub use container::{ContainerDatabase, ContainerDbError, ContainerStats};
