use crate::{
    Engine,
    directory::Directory,
    manifest::{self, Manifest},
    sstable::{self, SSTable},
    storage::Storage,
};

use super::{CleanupReport, Database, DbError, error::io_error, publication::publish};

#[derive(Debug)]
pub struct CompactionReport {
    pub tables_before: usize,
    pub tables_after: usize,
    pub cleanup: CleanupReport,
}

impl<D: Directory> Database<D> {
    pub fn flush(&mut self) -> Result<(), DbError> {
        self.writable()?;
        let entries = self.wal.snapshot_changes().map_err(DbError::Wal)?;
        if entries.is_empty() {
            return Ok(());
        }

        sstable::validate_entries(&entries).map_err(DbError::Sst)?;
        if let Err(error) = self.flush_inner(&entries) {
            if !matches!(&error, DbError::Limit(_)) {
                self.poisoned = true;
            }
            return Err(error);
        }
        Ok(())
    }

    pub fn compact(&mut self) -> Result<CompactionReport, DbError> {
        self.writable()?;
        let tables_before = self.tables.len();
        if (self.tables.len() > 1
            || self.wal.change_count() > 0
            || self.tables.iter().any(SSTable::has_tombstones))
            && let Err(error) = self.compact_inner()
        {
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

        self.tables = table.into_iter().collect();
        drop(std::mem::replace(&mut self.wal, new_wal));
        self.manifest = next;
        Ok(())
    }
}
