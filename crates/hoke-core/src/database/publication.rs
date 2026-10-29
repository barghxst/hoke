use std::collections::BTreeMap;

use crate::{
    Engine,
    directory::Directory,
    manifest::{self, Manifest},
    sstable::{self, SSTable},
    storage::Storage,
};

use super::{Database, DbError, error::io_error};

pub(super) fn publish<D: Directory>(directory: &D, manifest: &Manifest) -> Result<(), DbError> {
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
    pub(super) fn flush_inner(
        &mut self,
        entries: &BTreeMap<Vec<u8>, Option<Vec<u8>>>,
    ) -> Result<(), DbError> {
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
    pub(super) fn next_generation(&self) -> Result<u64, DbError> {
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
}
