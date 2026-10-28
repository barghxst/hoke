//! KV coordination over immutable container segments (ADR 0019).
use std::{collections::BTreeMap, fmt, io};

use super::storage::{ReadSegment, SharedStorage};
use super::{Journal, JournalError, MAX_SEGMENT_BYTES, SegmentKind};
use crate::{
    BatchOperation, DbError, Engine, KeyValue, ScanOptions,
    sstable::{self, SSTable, TableWriter},
    storage::{MemoryStorage, Storage},
    wal::{self, Operation},
};

const MAX_SOURCES: usize = 64;
const MAX_DIRTY_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug)]
pub enum ContainerDbError {
    Journal(JournalError),
    Data(DbError),
    Corruption(String),
    Limit(&'static str),
    Poisoned,
}
impl fmt::Display for ContainerDbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Journal(e) => e.fmt(f),
            Self::Data(e) => e.fmt(f),
            Self::Corruption(e) => write!(f, "corrupt container database: {e}"),
            Self::Limit(e) => write!(f, "container database limit: {e}"),
            Self::Poisoned => {
                f.write_str("container outcome unknown; reopen before further operations")
            }
        }
    }
}
impl std::error::Error for ContainerDbError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Journal(e) => Some(e),
            Self::Data(e) => Some(e),
            _ => None,
        }
    }
}
impl From<JournalError> for ContainerDbError {
    fn from(e: JournalError) -> Self {
        Self::Journal(e)
    }
}
impl From<DbError> for ContainerDbError {
    fn from(e: DbError) -> Self {
        Self::Data(e)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContainerStats {
    pub generation: u64,
    pub table_count: usize,
    pub wal_segments: usize,
    pub memtable_entries: usize,
    pub memtable_bytes: usize,
}

/// Exclusive append-only container KV. Publication never implies physical reclamation.
pub struct ContainerDatabase<S: Storage> {
    journal: Journal<SharedStorage<S>>,
    shared: SharedStorage<S>,
    memory: Engine<MemoryStorage>,
    tables: Vec<SSTable<ReadSegment<S>>>,
    poisoned: bool,
}
fn empty_memory() -> Result<Engine<MemoryStorage>, ContainerDbError> {
    Engine::open(MemoryStorage::new()).map_err(|e| ContainerDbError::Data(DbError::Wal(e)))
}
fn operation(op: &BatchOperation) -> Operation {
    match op {
        BatchOperation::Put { key, value } => Operation::Put {
            key: key.clone(),
            value: value.clone(),
        },
        BatchOperation::Delete { key } => Operation::Delete { key: key.clone() },
    }
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
                // Immutable inner WAL tails are errors; never use repairing Engine::open here.
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
        // Encode and calculate the final-key delta before any persistent I/O.
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

#[derive(Default)]
struct CappedStorage {
    inner: MemoryStorage,
    exceeded: bool,
}
impl CappedStorage {
    fn check(&mut self, end: u64) -> io::Result<()> {
        if end > MAX_SEGMENT_BYTES {
            self.exceeded = true;
            return Err(io::Error::other("SST staging cap"));
        }
        Ok(())
    }
    fn bytes(self) -> Result<Vec<u8>, ContainerDbError> {
        Ok(self.inner.into_bytes())
    }
}
impl Storage for CappedStorage {
    fn len(&self) -> io::Result<u64> {
        self.inner.len()
    }
    fn read_at(&mut self, offset: u64, buffer: &mut [u8]) -> io::Result<usize> {
        self.inner.read_at(offset, buffer)
    }
    fn write_all_at(&mut self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        self.check(
            offset
                .checked_add(bytes.len() as u64)
                .ok_or_else(|| io::Error::other("staging offset"))?,
        )?;
        self.inner.write_all_at(offset, bytes)
    }
    fn set_len(&mut self, len: u64) -> io::Result<()> {
        self.check(len)?;
        self.inner.set_len(len)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
    fn sync_all(&mut self) -> io::Result<()> {
        self.inner.sync_all()
    }
}
