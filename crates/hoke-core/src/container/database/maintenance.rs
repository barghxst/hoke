//! Flush/compaction candidates and atomic root publication.
use super::{
    ContainerDatabase, ContainerDbError, MAX_SOURCES, recovery::empty_memory,
    staging::CappedStorage,
};
use crate::container::{SegmentKind, storage::ReadSegment};
use crate::{
    DbError, Engine, ScanOptions,
    sstable::{self, SSTable, TableWriter},
    storage::{MemoryStorage, Storage},
};

impl<S: Storage> ContainerDatabase<S> {
    pub fn flush(&mut self) -> Result<(), ContainerDbError> {
        self.usable()?;
        if self.memory.change_count() == 0 {
            return Ok(());
        }
        if self.tables.len() == MAX_SOURCES {
            return Err(ContainerDbError::Limit("64 SSTs; compact before flushing"));
        }
        let reset = empty_memory()?;
        let changes = self
            .memory
            .snapshot_changes()
            .map_err(|e| ContainerDbError::Data(DbError::Wal(e)))?;
        let mut stage = CappedStorage::default();
        let result = sstable::write_table(&mut stage, &changes);
        if stage.exceeded {
            return Err(ContainerDbError::Limit("SST staging exceeds 256 MiB"));
        }
        result.map_err(|e| ContainerDbError::Data(DbError::Sst(e)))?;
        let bytes = stage.bytes()?;
        self.publish_table(Some(&bytes), reset, false)
    }
    pub fn compact(&mut self) -> Result<(), ContainerDbError> {
        self.usable()?;
        if self.tables.is_empty() && self.memory.change_count() == 0 {
            return Ok(());
        }
        let reset = empty_memory()?;
        let mut stage = CappedStorage::default();
        let mut writer =
            TableWriter::new(&mut stage).map_err(|e| ContainerDbError::Data(DbError::Sst(e)))?;
        let options = ScanOptions {
            limit: usize::MAX,
            ..Default::default()
        };
        let mut output_error = false;
        let result =
            crate::scan::visit_sources(&self.memory, &mut self.tables, options, |key, value| {
                let result = writer.append(&key, Some(&value)).map_err(DbError::Sst);
                output_error = result.is_err();
                result
            });
        if let Err(error) = result {
            drop(writer);
            if stage.exceeded {
                return Err(ContainerDbError::Limit("SST staging exceeds 256 MiB"));
            }
            if output_error {
                return Err(ContainerDbError::Data(error));
            }
            return self.source_result(Err(error));
        }
        let finished = writer.finish();
        if stage.exceeded {
            return Err(ContainerDbError::Limit("SST staging exceeds 256 MiB"));
        }
        let count = finished.map_err(|e| ContainerDbError::Data(DbError::Sst(e)))?;
        let bytes = if count == 0 {
            None
        } else {
            Some(stage.bytes()?)
        };
        self.publish_table(bytes.as_deref(), reset, true)
    }
    fn publish_table(
        &mut self,
        bytes: Option<&[u8]>,
        reset: Engine<MemoryStorage>,
        replace: bool,
    ) -> Result<(), ContainerDbError> {
        let mut refs = if replace {
            Vec::new()
        } else {
            self.journal.root().unwrap().tables.clone()
        };
        // Prepare all list capacity before appending persistent output.
        if bytes.is_some() {
            refs.try_reserve(1)
                .map_err(|_| ContainerDbError::Data(DbError::AllocationFailed))?;
        }
        let mut readers = Vec::new();
        readers
            .try_reserve_exact(refs.len() + usize::from(bytes.is_some()))
            .map_err(|_| ContainerDbError::Data(DbError::AllocationFailed))?;
        let mut candidate = None;
        if let Some(bytes) = bytes {
            let appended = self.journal.append_segment(SegmentKind::Sst, bytes);
            let reference = self.persistent(appended)?;
            let reader = ReadSegment::new(self.shared.clone(), reference)
                .map_err(|e| ContainerDbError::Corruption(e.to_string()))
                .and_then(|view| {
                    SSTable::open(view).map_err(|e| ContainerDbError::Data(DbError::Sst(e)))
                });
            match reader {
                Ok(reader) => candidate = Some(reader),
                Err(e) => {
                    self.poisoned = true;
                    return Err(e);
                }
            }
            refs.push(reference);
        }
        let committed = self.journal.commit(&refs, &[]);
        // Output has already been appended: any publication failure requires reopen.
        if committed.is_err() {
            self.poisoned = true;
        }
        self.persistent(committed)?;
        if !replace {
            readers.append(&mut self.tables);
        }
        if let Some(candidate) = candidate {
            readers.push(candidate);
        }
        self.tables = readers;
        self.memory = reset;
        Ok(())
    }
}
