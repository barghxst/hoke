use crate::directory::Directory;
use crate::manifest::{self, Manifest};
use crate::sstable::{self, SSTable, SstError};
use crate::storage::Storage;
use crate::{Engine, Error};
use std::collections::BTreeMap;
use std::fmt;
use std::io;

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
fn io_error(operation: &'static str) -> impl FnOnce(io::Error) -> DbError {
    move |source| DbError::Io { operation, source }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DatabaseStats {
    pub memtable_entries: usize,
    pub table_count: usize,
    pub wal_bytes: u64,
    /// Dirty key/value payload bytes, excluding container/allocator overhead.
    pub memtable_bytes: usize,
}

/// An error during cleanup; published data remains usable.
#[derive(Debug)]
pub struct CleanupError {
    pub file: Option<String>,
    pub operation: &'static str,
    pub source: io::Error,
}
#[derive(Debug, Default)]
pub struct CleanupReport {
    pub removed_files: usize,
    pub errors: Vec<CleanupError>,
}
#[derive(Debug)]
pub struct CompactionReport {
    pub tables_before: usize,
    pub tables_after: usize,
    pub cleanup: CleanupReport,
}

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
fn publish<D: Directory>(directory: &D, manifest: &Manifest) -> Result<(), DbError> {
    let bytes = manifest.encode().map_err(io_error("encode manifest"))?;
    let name = manifest::temp_name(manifest.wal);
    let mut file = directory
        .create_new(&name)
        .map_err(io_error("create manifest temp"))?;
    file.write_all_at(0, &bytes)
        .map_err(io_error("write manifest"))?;
    file.flush().map_err(io_error("flush manifest"))?;
    file.sync_all().map_err(io_error("sync manifest"))?;
    drop(file);
    directory
        .rename(&name, "MANIFEST")
        .map_err(io_error("publish manifest"))?;
    directory
        .sync_directory()
        .map_err(io_error("sync published manifest"))
}
impl<D: Directory> Database<D> {
    pub fn open(directory: D) -> Result<Self, DbError> {
        Self::open_with_options(directory, crate::DatabaseOptions::default())
    }

    /// Open with per-instance pre-write flush thresholds; opening never auto-flushes.
    pub fn open_with_options(
        directory: D,
        options: crate::DatabaseOptions,
    ) -> Result<Self, DbError> {
        options.validate()?;
        let manifest = match directory.open_existing("MANIFEST") {
            Ok(mut file) => Manifest::read(&mut file).map_err(io_error("read manifest"))?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if !directory
                    .list()
                    .map_err(io_error("list new database"))?
                    .is_empty()
                {
                    return Err(DbError::Corruption(
                        "manifest missing in nonempty directory".into(),
                    ));
                }
                let manifest = Manifest {
                    wal: 1,
                    tables: vec![],
                };
                let file = directory
                    .create_new(&manifest::wal_name(1))
                    .map_err(io_error("create initial WAL"))?;
                let wal = Engine::open(file).map_err(DbError::Wal)?;
                directory
                    .sync_directory()
                    .map_err(io_error("sync initial WAL name"))?;
                publish(&directory, &manifest)?;
                return Ok(Self {
                    wal,
                    tables: vec![],
                    manifest,
                    poisoned: false,
                    options,
                    directory,
                });
            }
            Err(error) => return Err(io_error("open manifest")(error)),
        };
        let mut tables = Vec::with_capacity(manifest.tables.len());
        for generation in &manifest.tables {
            let file = directory
                .open_existing(&manifest::table_name(*generation))
                .map_err(io_error("open referenced SST"))?;
            tables.push(SSTable::open(file).map_err(DbError::Sst)?);
        }
        let file = directory
            .open_existing(&manifest::wal_name(manifest.wal))
            .map_err(io_error("open referenced WAL"))?;
        if file.len().map_err(io_error("read referenced WAL length"))?
            < crate::wal::FILE_HEADER_LEN as u64
        {
            return Err(DbError::Corruption(
                "referenced WAL has no complete header".into(),
            ));
        }
        let wal = Engine::open(file).map_err(DbError::Wal)?;
        Ok(Self {
            wal,
            tables,
            manifest,
            poisoned: false,
            options,
            directory,
        })
    }
    fn writable(&self) -> Result<(), DbError> {
        if self.poisoned {
            Err(DbError::Poisoned)
        } else {
            Ok(())
        }
    }
    pub fn get(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>, DbError> {
        crate::read::get_from_sources(&self.wal, &mut self.tables, key)
    }

    /// Merge bounded keys across memtable and SSTs; read only winning values.
    /// The default result limit is 100. Errors never return a partial result.
    pub fn scan(&mut self, options: crate::ScanOptions) -> Result<Vec<crate::KeyValue>, DbError> {
        crate::scan::scan_sources(&self.wal, &mut self.tables, options)
    }

    /// Alias for scan, with the same Included/Excluded boundary semantics.
    pub fn range(&mut self, options: crate::ScanOptions) -> Result<Vec<crate::KeyValue>, DbError> {
        self.scan(options)
    }
    fn mutation(&mut self, result: Result<(), Error>) -> Result<(), DbError> {
        if let Err(error) = result {
            if matches!(&error, Error::Io { .. } | Error::Poisoned) {
                self.poisoned = true;
            }
            return Err(DbError::Wal(error));
        }
        Ok(())
    }
    pub fn put(&mut self, key: &[u8], value: &[u8]) -> Result<(), DbError> {
        self.prepare_write(key, Some(value))?;
        let result = self.wal.put(key, value);
        self.mutation(result)
    }
    pub fn delete(&mut self, key: &[u8]) -> Result<(), DbError> {
        self.prepare_write(key, None)?;
        let result = self.wal.delete(key);
        self.mutation(result)
    }
    // Flush before the requested mutation is appended. A preflush error cannot
    // hide a successful commit of that mutation behind a maintenance error.
    fn prepare_write(&mut self, key: &[u8], value: Option<&[u8]>) -> Result<(), DbError> {
        self.writable()?;
        if key.len() > crate::wal::MAX_KEY_BYTES
            || value.is_some_and(|v| v.len() > crate::wal::MAX_VALUE_BYTES)
        {
            return Err(DbError::Wal(Error::InvalidInput(
                if value.is_some() {
                    "key or value exceeds WAL size limit"
                } else {
                    "key exceeds WAL size limit"
                }
                .into(),
            )));
        }
        let payload_exceeded = self
            .options
            .max_memtable_bytes
            .is_some_and(|limit| self.wal.projected_change_bytes(key, value) > limit);
        let frame_bytes = (crate::wal::FRAME_HEADER_LEN + crate::wal::TRAILER_LEN) as u64
            + key.len() as u64
            + value.map_or(0, |v| v.len() as u64);
        let wal_exceeded = self.options.max_wal_bytes.is_some_and(|limit| {
            self.wal
                .byte_len()
                .checked_add(frame_bytes)
                .is_none_or(|projected| projected > limit)
        });
        if self.wal.change_count() > 0 && (payload_exceeded || wal_exceeded) {
            self.flush()?;
        }
        Ok(())
    }

    /// Atomically publish all operations in order, including the current WAL delta.
    /// Each nonempty batch creates an SST and an empty WAL through manifest commit.
    /// Input/capacity errors leave the database writable; publication errors poison
    /// it until reopen, where the complete batch may be present or absent.
    pub fn write_batch(&mut self, operations: &[crate::BatchOperation]) -> Result<(), DbError> {
        self.writable()?;
        crate::validate_batch(operations)?;
        if operations.is_empty() {
            return Ok(());
        }
        if self.manifest.tables.len() >= manifest::MAX_TABLES {
            return Err(DbError::Limit(
                "table limit reached; compaction is required",
            ));
        }
        let mut entries = self.wal.snapshot_changes().map_err(DbError::Wal)?;
        for operation in operations {
            match operation {
                crate::BatchOperation::Put { key, value } => {
                    entries.insert(key.clone(), Some(value.clone()));
                }
                crate::BatchOperation::Delete { key } => {
                    entries.insert(key.clone(), None);
                }
            }
        }
        sstable::validate_entries(&entries).map_err(DbError::Sst)?;
        if let Err(error) = self.flush_inner(&entries) {
            // Limit errors here can only precede file creation (generation/table
            // exhaustion); unlike an uncertain IO outcome they do not poison.
            if !matches!(&error, DbError::Limit(_)) {
                self.poisoned = true;
            }
            return Err(error);
        }
        Ok(())
    }

    pub fn stats(&self) -> DatabaseStats {
        DatabaseStats {
            memtable_entries: self.wal.change_count(),
            table_count: self.tables.len(),
            wal_bytes: self.wal.byte_len(),
            memtable_bytes: self.wal.change_bytes(),
        }
    }
    pub fn flush(&mut self) -> Result<(), DbError> {
        self.writable()?;
        let entries = self.wal.snapshot_changes().map_err(DbError::Wal)?;
        if entries.is_empty() {
            return Ok(());
        }
        // Capacity validation is pure and leaves the existing delta writable.
        sstable::validate_entries(&entries).map_err(DbError::Sst)?;
        if let Err(error) = self.flush_inner(&entries) {
            // Table/generation limits precede file creation. IO failures can
            // have an unknown publication outcome and must still poison.
            if !matches!(&error, DbError::Limit(_)) {
                self.poisoned = true;
            }
            return Err(error);
        }
        Ok(())
    }
    fn flush_inner(&mut self, entries: &BTreeMap<Vec<u8>, Option<Vec<u8>>>) -> Result<(), DbError> {
        if self.manifest.tables.len() >= manifest::MAX_TABLES {
            return Err(DbError::Limit(
                "table limit reached; compaction is required",
            ));
        }
        let generation = self.next_generation()?;
        let mut file = self
            .directory
            .create_new(&manifest::table_name(generation))
            .map_err(io_error("create SST"))?;
        sstable::write_table(&mut file, entries).map_err(DbError::Sst)?;
        file.flush().map_err(io_error("flush SST"))?;
        file.sync_all().map_err(io_error("sync SST"))?;
        let table = SSTable::open(file).map_err(DbError::Sst)?;
        let file = self
            .directory
            .create_new(&manifest::wal_name(generation))
            .map_err(io_error("create new WAL"))?;
        let new_wal = Engine::open(file).map_err(DbError::Wal)?;
        self.directory
            .sync_directory()
            .map_err(io_error("sync SST/WAL names"))?;
        let mut next = self.manifest.clone();
        next.wal = generation;
        next.tables.push(generation);
        publish(&self.directory, &next)?;
        self.tables.push(table);
        let previous = std::mem::replace(&mut self.wal, new_wal);
        drop(previous);
        self.manifest = next;
        Ok(())
    }
    fn next_generation(&self) -> Result<u64, DbError> {
        let mut generation = self.manifest.wal;
        for name in self
            .directory
            .list()
            .map_err(io_error("list generations"))?
        {
            if let Some(id) = manifest::generation(&name) {
                generation = generation.max(id);
            }
        }
        generation
            .checked_add(1)
            .ok_or(DbError::Limit("generation overflow"))
    }

    /// Merge every active SST and WAL change, dropping covered tombstones.
    /// Publication failures poison mutations until reopen. Cleanup failures are
    /// reported separately after publication and do not roll back the new state.
    pub fn compact(&mut self) -> Result<CompactionReport, DbError> {
        self.writable()?;
        let tables_before = self.tables.len();
        if (self.tables.len() > 1
            || self.wal.change_count() > 0
            || self.tables.iter().any(SSTable::has_tombstones))
            && let Err(error) = self.compact_inner()
        {
            // The only Limit here is generation exhaustion, before creation.
            if !matches!(&error, DbError::Limit(_)) {
                self.poisoned = true;
            }
            return Err(error);
        }
        let cleanup = self.collect_garbage()?;
        Ok(CompactionReport {
            tables_before,
            tables_after: self.tables.len(),
            cleanup,
        })
    }

    fn compact_inner(&mut self) -> Result<(), DbError> {
        let generation = self.next_generation()?;
        let mut file = self
            .directory
            .create_new(&manifest::table_name(generation))
            .map_err(io_error("create compacted SST"))?;
        let mut writer = sstable::TableWriter::new(&mut file).map_err(DbError::Sst)?;
        crate::scan::visit_sources(
            &self.wal,
            &mut self.tables,
            crate::ScanOptions {
                limit: usize::MAX,
                ..Default::default()
            },
            |key, value| writer.append(&key, Some(&value)).map_err(DbError::Sst),
        )?;
        let count = writer.finish().map_err(DbError::Sst)?;
        file.flush().map_err(io_error("flush compacted SST"))?;
        file.sync_all().map_err(io_error("sync compacted SST"))?;
        let table = if count > 0 {
            Some(SSTable::open(file).map_err(DbError::Sst)?)
        } else {
            drop(file);
            None
        };
        let file = self
            .directory
            .create_new(&manifest::wal_name(generation))
            .map_err(io_error("create compacted WAL"))?;
        let new_wal = Engine::open(file).map_err(DbError::Wal)?;
        self.directory
            .sync_directory()
            .map_err(io_error("sync compacted file names"))?;
        let next = Manifest {
            wal: generation,
            tables: if count > 0 { vec![generation] } else { vec![] },
        };
        publish(&self.directory, &next)?;
        // Close old handles before unlinking superseded files.
        self.tables = table.into_iter().collect();
        drop(std::mem::replace(&mut self.wal, new_wal));
        self.manifest = next;
        Ok(())
    }

    /// Remove only recognised generation files absent from the active manifest.
    /// Errors in this report concern cleanup, never rollback of published data.
    pub fn collect_garbage(&mut self) -> Result<CleanupReport, DbError> {
        self.writable()?;
        let mut report = CleanupReport::default();
        // Also required after reopen following an uncertain rename/dirsync:
        // persist the visible manifest before deleting files it does not name.
        if let Err(source) = self.directory.sync_directory() {
            report.errors.push(CleanupError {
                file: None,
                operation: "sync manifest before cleanup",
                source,
            });
            return Ok(report);
        }
        let names = match self.directory.list() {
            Ok(names) => names,
            Err(source) => {
                report.errors.push(CleanupError {
                    file: None,
                    operation: "list cleanup files",
                    source,
                });
                return Ok(report);
            }
        };
        let active_wal = manifest::wal_name(self.manifest.wal);
        for name in names {
            if manifest::generation(&name).is_none()
                || name == active_wal
                || self
                    .manifest
                    .tables
                    .iter()
                    .any(|id| name == manifest::table_name(*id))
            {
                continue;
            }
            match self.directory.remove_file(&name) {
                Ok(()) => report.removed_files += 1,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(source) => report.errors.push(CleanupError {
                    file: Some(name),
                    operation: "remove obsolete file",
                    source,
                }),
            }
        }
        // A removal may have taken effect even when its adapter returned Err.
        if let Err(source) = self.directory.sync_directory() {
            report.errors.push(CleanupError {
                file: None,
                operation: "sync cleanup removals",
                source,
            });
        }
        Ok(report)
    }

    pub fn close(self) -> Result<(), DbError> {
        self.writable()?;
        self.wal.close().map_err(DbError::Wal)
    }
}
