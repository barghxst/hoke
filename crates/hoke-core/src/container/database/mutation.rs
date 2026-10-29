use super::{ContainerDatabase, ContainerDbError, MAX_DIRTY_BYTES, MAX_SOURCES};
use crate::container::SegmentKind;
use crate::{
    BatchOperation, DbError, sstable,
    storage::Storage,
    wal::{self, Operation},
};
use std::collections::BTreeMap;

fn operation(op: &BatchOperation) -> Operation {
    match op {
        BatchOperation::Put { key, value } => Operation::Put {
            key: key.clone(),
            value: value.clone(),
        },
        BatchOperation::Delete { key } => Operation::Delete { key: key.clone() },
    }
}
impl<S: Storage> ContainerDatabase<S> {
    pub fn put(&mut self, key: &[u8], value: &[u8]) -> Result<(), ContainerDbError> {
        self.write_batch(&[BatchOperation::Put {
            key: key.to_vec(),
            value: value.to_vec(),
        }])
    }
    pub fn delete(&mut self, key: &[u8]) -> Result<(), ContainerDbError> {
        self.write_batch(&[BatchOperation::Delete { key: key.to_vec() }])
    }
    pub fn write_batch(&mut self, operations: &[BatchOperation]) -> Result<(), ContainerDbError> {
        self.usable()?;
        crate::validate_batch(operations)?;
        if operations.is_empty() {
            return Ok(());
        }

        let mut payload = wal::encode_file_header().to_vec();
        let mut final_changes = BTreeMap::new();
        for (index, op) in operations.iter().enumerate() {
            let encoded = wal::encode_frame(index as u64 + 1, &operation(op))
                .map_err(|e| ContainerDbError::Corruption(e.to_string()))?;
            payload
                .try_reserve(encoded.len())
                .map_err(|_| ContainerDbError::Data(DbError::AllocationFailed))?;
            payload.extend_from_slice(&encoded);
            match op {
                BatchOperation::Put { key, value } => {
                    final_changes.insert(key.as_slice(), Some(value.as_slice()));
                }
                BatchOperation::Delete { key } => {
                    final_changes.insert(key.as_slice(), None);
                }
            }
        }
        let mut projected = self.memory.change_bytes();
        let mut projected_entries = self.memory.change_count();
        for (key, value) in final_changes {
            if self.memory.lookup_change(key).is_none() {
                projected_entries += 1;
            }
            let old = self
                .memory
                .lookup_change(key)
                .map_or(0, |v| key.len() + v.map_or(0, <[u8]>::len));
            projected = projected - old + key.len() + value.map_or(0, <[u8]>::len);
        }
        if projected > MAX_DIRTY_BYTES
            || projected_entries > sstable::MAX_ENTRIES
            || self.stats().wal_segments == MAX_SOURCES
        {
            self.flush()?;
        }
        let appended = self.journal.append_segment(SegmentKind::Wal, &payload);
        let reference = self.persistent(appended)?;
        let root = self.journal.root().expect("initialized root").clone();
        let mut wal = root.wal;
        wal.push(reference);
        let committed = self.journal.commit(&root.tables, &wal);
        if committed.is_err() {
            self.poisoned = true;
        }
        self.persistent(committed)?;
        for op in operations {
            self.memory.apply_committed(operation(op));
        }
        Ok(())
    }
}
