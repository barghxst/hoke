use super::{ContainerDatabase, ContainerDbError, MAX_DIRTY_BYTES, MAX_SOURCES};
use crate::container::{
    Journal,
    storage::{ReadSegment, SharedStorage},
};
use crate::{
    DbError, Engine,
    sstable::{self, SSTable},
    storage::{MemoryStorage, Storage},
    wal::{self, Operation},
};
use std::fmt;

pub(super) fn empty_memory() -> Result<Engine<MemoryStorage>, ContainerDbError> {
    Engine::open(MemoryStorage::new()).map_err(|e| ContainerDbError::Data(DbError::Wal(e)))
}
fn decode_wal(bytes: &[u8]) -> Result<Vec<Operation>, ContainerDbError> {
    let corrupt = |e: &dyn fmt::Display| ContainerDbError::Corruption(e.to_string());
    let header = bytes
        .get(..wal::FILE_HEADER_LEN)
        .ok_or_else(|| corrupt(&"short immutable WAL header"))?;
    wal::decode_file_header(header.try_into().unwrap()).map_err(|e| corrupt(&e))?;
    let mut position = wal::FILE_HEADER_LEN;
    let mut operations = Vec::new();
    let mut payload_bytes = 0usize;
    while position < bytes.len() {
        if operations.len() == crate::MAX_BATCH_OPERATIONS {
            return Err(corrupt(&"immutable WAL operation count"));
        }
        let header_end = position + wal::FRAME_HEADER_LEN;
        let header = bytes
            .get(position..header_end)
            .ok_or_else(|| corrupt(&"short immutable WAL frame"))?;
        let decoded =
            wal::decode_frame_header(header.try_into().unwrap()).map_err(|e| corrupt(&e))?;
        if decoded.sequence != operations.len() as u64 + 1 {
            return Err(corrupt(&"immutable WAL sequence"));
        }
        payload_bytes += decoded.payload_len();
        if payload_bytes > crate::MAX_BATCH_BYTES {
            return Err(corrupt(&"immutable WAL payload budget"));
        }
        let end = position + decoded.frame_len();
        let payload = bytes
            .get(header_end..end)
            .ok_or_else(|| corrupt(&"short immutable WAL payload"))?;
        let (_, op) =
            wal::decode_frame(header.try_into().unwrap(), payload).map_err(|e| corrupt(&e))?;
        operations.push(op);
        position = end;
    }
    if operations.is_empty() {
        return Err(corrupt(&"empty immutable WAL segment"));
    }
    Ok(operations)
}
impl<S: Storage> ContainerDatabase<S> {
    pub fn open(storage: S) -> Result<Self, ContainerDbError> {
        let shared = SharedStorage::new(storage);
        let mut journal = Journal::open_deferred(shared.clone())?;
        let mut memory = empty_memory()?;
        let mut tables = Vec::new();
        if let Some(root) = journal.root().cloned() {
            if root.tables.len() > MAX_SOURCES || root.wal.len() > MAX_SOURCES {
                return Err(ContainerDbError::Limit("active source count"));
            }
            for reference in root.tables {
                let view = ReadSegment::new(shared.clone(), reference)
                    .map_err(|e| ContainerDbError::Corruption(e.to_string()))?;
                tables.push(
                    SSTable::open(view).map_err(|e| ContainerDbError::Data(DbError::Sst(e)))?,
                );
            }
            for reference in root.wal {
                if reference.payload_len
                    > (wal::FILE_HEADER_LEN
                        + crate::MAX_BATCH_BYTES
                        + crate::MAX_BATCH_OPERATIONS * (wal::FRAME_HEADER_LEN + wal::TRAILER_LEN))
                        as u64
                {
                    return Err(ContainerDbError::Corruption(
                        "immutable WAL segment too large".into(),
                    ));
                }

                for op in decode_wal(&journal.read_segment(&reference)?)? {
                    memory.apply_committed(op);
                }
                if memory.change_count() > sstable::MAX_ENTRIES {
                    return Err(ContainerDbError::Corruption("dirty entry budget".into()));
                }
                if memory.change_bytes() > MAX_DIRTY_BYTES {
                    return Err(ContainerDbError::Corruption("dirty payload budget".into()));
                }
            }
        }
        journal.repair_tail()?;
        if journal.root().is_none() {
            journal.commit(&[], &[])?;
        }
        Ok(Self {
            journal,
            shared,
            memory,
            tables,
            poisoned: false,
        })
    }
}
