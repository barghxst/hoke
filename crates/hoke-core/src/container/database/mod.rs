mod maintenance;
mod mutation;
mod recovery;
mod staging;
mod types;

use super::{
    Journal, JournalError,
    storage::{ReadSegment, SharedStorage},
};
use crate::{
    DbError, Engine, KeyValue, ScanOptions,
    sstable::SSTable,
    storage::{MemoryStorage, Storage},
    wal,
};
pub use types::{ContainerDbError, ContainerStats};

const MAX_SOURCES: usize = 64;
const MAX_DIRTY_BYTES: usize = 32 * 1024 * 1024;

pub struct ContainerDatabase<S: Storage> {
    journal: Journal<SharedStorage<S>>,
    shared: SharedStorage<S>,
    memory: Engine<MemoryStorage>,
    tables: Vec<SSTable<ReadSegment<S>>>,
    poisoned: bool,
}
impl<S: Storage> ContainerDatabase<S> {
    fn usable(&self) -> Result<(), ContainerDbError> {
        if self.is_poisoned() {
            Err(ContainerDbError::Poisoned)
        } else {
            Ok(())
        }
    }
    fn source_result<T>(&mut self, result: Result<T, DbError>) -> Result<T, ContainerDbError> {
        if matches!(
            &result,
            Err(DbError::Io { .. }
                | DbError::Sst(_)
                | DbError::Corruption(_)
                | DbError::Wal(_)
                | DbError::Poisoned)
        ) {
            self.poisoned = true;
        }
        result.map_err(ContainerDbError::Data)
    }
    fn persistent<T>(&mut self, result: Result<T, JournalError>) -> Result<T, ContainerDbError> {
        if result.is_err() && !matches!(&result, Err(JournalError::Limit(_))) {
            self.poisoned = true;
        }
        result.map_err(ContainerDbError::Journal)
    }
    pub fn get(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>, ContainerDbError> {
        self.usable()?;
        if key.len() > wal::MAX_KEY_BYTES {
            return Err(ContainerDbError::Limit("key exceeds 64 KiB"));
        }
        let result = crate::read::get_from_mixed_sources(&self.memory, &mut self.tables, key);
        self.source_result(result)
    }
    pub fn scan_with_budget(
        &mut self,
        options: ScanOptions,
        max_payload_bytes: usize,
    ) -> Result<Vec<KeyValue>, ContainerDbError> {
        self.usable()?;
        let result = crate::read::scan_mixed_sources_with_budget(
            &self.memory,
            &mut self.tables,
            options,
            max_payload_bytes,
        );
        self.source_result(result)
    }
    pub fn stats(&self) -> ContainerStats {
        let root = self.journal.root();
        ContainerStats {
            generation: root.map_or(0, |r| r.generation),
            table_count: self.tables.len(),
            wal_segments: root.map_or(0, |r| r.wal.len()),
            memtable_entries: self.memory.change_count(),
            memtable_bytes: self.memory.change_bytes(),
        }
    }
    pub fn is_poisoned(&self) -> bool {
        self.poisoned || self.journal.is_poisoned()
    }
    pub fn into_storage(self) -> S {
        let Self {
            journal,
            shared,
            memory,
            tables,
            ..
        } = self;
        drop(tables);
        drop(memory);
        drop(journal.into_storage());
        match shared.try_into_inner() {
            Ok(storage) => storage,
            Err(_) => panic!("private container storage unexpectedly shared"),
        }
    }
}
