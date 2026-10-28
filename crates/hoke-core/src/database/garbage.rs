//! Reclaim only obsolete managed generations after synchronizing the visible manifest.
use std::io;

use crate::{directory::Directory, manifest};

use super::{Database, DbError};

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
impl<D: Directory> Database<D> {
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
}
