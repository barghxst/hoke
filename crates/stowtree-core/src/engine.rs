use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io;
use std::ops::Bound;

use crate::storage::Storage;
use crate::wal::{self, CodecError, Operation};

#[derive(Debug)]
pub enum Error {
    Io {
        operation: &'static str,
        source: io::Error,
    },
    Corruption {
        offset: u64,
        reason: String,
    },
    UnsupportedVersion(u32),
    InvalidInput(String),
    AllocationFailed,
    Poisoned,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { operation, source } => write!(f, "{operation}: {source}"),
            Self::Corruption { offset, reason } => {
                write!(f, "corrupt WAL at byte {offset}: {reason}")
            }
            Self::UnsupportedVersion(version) => write!(f, "unsupported WAL version: {version}"),
            Self::InvalidInput(reason) => write!(f, "invalid input: {reason}"),
            Self::AllocationFailed => write!(f, "could not allocate a bounded WAL buffer"),
            Self::Poisoned => write!(
                f,
                "WAL write outcome is unknown; close and reopen before writing"
            ),
        }
    }
}
impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Recovery {
    pub records_replayed: u64,
    pub discarded_tail_bytes: u64,
}

/// Experimental WAL-only key/value engine; no SSTables, compaction or transactions.
///
/// Keys and values are arbitrary bytes. Each successful mutation writes and syncs
/// its WAL frame before becoming visible. An I/O error leaves the write outcome
/// unknown and poisons further writes; previously published values remain readable.
/// MemoryStorage implements the same protocol but is explicitly not durable.
pub struct Engine<S: Storage> {
    storage: S,
    values: BTreeMap<Vec<u8>, Vec<u8>>,
    deleted: BTreeSet<Vec<u8>>,
    change_bytes: usize,
    sequence: u64,
    wal_len: u64,
    poisoned: bool,
    recovery: Recovery,
}

fn io_error(operation: &'static str) -> impl FnOnce(io::Error) -> Error {
    move |source| Error::Io { operation, source }
}
fn corrupt(offset: u64, error: impl fmt::Display) -> Error {
    Error::Corruption {
        offset,
        reason: error.to_string(),
    }
}
fn read_exact<S: Storage>(storage: &mut S, offset: u64, buffer: &mut [u8]) -> Result<(), Error> {
    let mut read = 0;
    while read < buffer.len() {
        let position = offset
            .checked_add(read as u64)
            .ok_or_else(|| corrupt(offset, "read offset overflow"))?;
        let count = storage
            .read_at(position, &mut buffer[read..])
            .map_err(io_error("read WAL"))?;
        if count == 0 {
            return Err(Error::Io {
                operation: "read WAL",
                source: io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "WAL shorter than its reported length",
                ),
            });
        }
        if count > buffer.len() - read {
            return Err(corrupt(position, "storage violated read contract"));
        }
        read += count;
    }
    Ok(())
}
fn sync<S: Storage>(storage: &mut S) -> Result<(), Error> {
    storage.flush().map_err(io_error("flush WAL"))?;
    storage.sync_all().map_err(io_error("sync WAL"))
}
fn apply(
    values: &mut BTreeMap<Vec<u8>, Vec<u8>>,
    deleted: &mut BTreeSet<Vec<u8>>,
    change_bytes: &mut usize,
    operation: Operation,
) {
    let (key, value) = match &operation {
        Operation::Put { key, value } => (key.as_slice(), Some(value.as_slice())),
        Operation::Delete { key } => (key.as_slice(), None),
    };
    let previous = change_size(values, deleted, key);
    let next_bytes = *change_bytes - previous + key.len() + value.map_or(0, <[u8]>::len);
    match operation {
        Operation::Put { key, value } => {
            deleted.remove(&key);
            values.insert(key, value);
        }
        Operation::Delete { key } => {
            values.remove(&key);
            deleted.insert(key);
        }
    }
    *change_bytes = next_bytes;
}

fn change_size(
    values: &BTreeMap<Vec<u8>, Vec<u8>>,
    deleted: &BTreeSet<Vec<u8>>,
    key: &[u8],
) -> usize {
    if let Some(value) = values.get(key) {
        key.len() + value.len()
    } else if deleted.contains(key) {
        key.len()
    } else {
        0
    }
}

impl<S: Storage> Engine<S> {
    /// Open an exclusively owned storage and replay checked records.
    ///
    /// Only a physically incomplete final frame is discarded. A full bad header,
    /// checksum mismatch or sequence gap fails without rewriting the file.
    /// Creating a new file does not yet synchronize its parent directory entry.
    pub fn open(mut storage: S) -> Result<Self, Error> {
        let mut length = storage.len().map_err(io_error("read WAL length"))?;
        if length == 0 {
            storage
                .write_all_at(0, &wal::encode_file_header())
                .map_err(io_error("initialize WAL"))?;
            sync(&mut storage)?;
            length = wal::FILE_HEADER_LEN as u64;
        }
        if length < wal::FILE_HEADER_LEN as u64 {
            return Err(corrupt(
                0,
                "incomplete file header; automatic repair is unsafe",
            ));
        }
        let mut file_header = [0; wal::FILE_HEADER_LEN];
        read_exact(&mut storage, 0, &mut file_header)?;
        if let Err(error) = wal::decode_file_header(&file_header) {
            return Err(match error {
                CodecError::UnsupportedVersion(version) => Error::UnsupportedVersion(version),
                other => corrupt(0, other),
            });
        }
        let mut values = BTreeMap::new();
        let mut deleted = BTreeSet::new();
        let mut change_bytes = 0;
        let mut sequence = 0_u64;
        let mut offset = wal::FILE_HEADER_LEN as u64;
        let mut recovery = Recovery::default();
        while offset < length {
            let remaining = length - offset;
            if remaining < wal::FRAME_HEADER_LEN as u64 {
                break;
            }
            let mut header = [0; wal::FRAME_HEADER_LEN];
            read_exact(&mut storage, offset, &mut header)?;
            let decoded =
                wal::decode_frame_header(&header).map_err(|error| corrupt(offset, error))?;
            let expected = sequence
                .checked_add(1)
                .ok_or_else(|| corrupt(offset, "sequence overflow"))?;
            if decoded.sequence != expected {
                return Err(corrupt(
                    offset,
                    format!("expected sequence {expected}, got {}", decoded.sequence),
                ));
            }
            let frame_len = decoded.frame_len() as u64;
            if remaining < frame_len {
                break;
            }
            let buffer_len = decoded.payload_len() + wal::TRAILER_LEN;
            let mut payload = Vec::new();
            payload
                .try_reserve_exact(buffer_len)
                .map_err(|_| Error::AllocationFailed)?;
            payload.resize(buffer_len, 0);
            read_exact(
                &mut storage,
                offset + wal::FRAME_HEADER_LEN as u64,
                &mut payload,
            )?;
            let (next_sequence, operation) =
                wal::decode_frame(&header, &payload).map_err(|error| corrupt(offset, error))?;
            apply(&mut values, &mut deleted, &mut change_bytes, operation);
            sequence = next_sequence;
            offset = offset
                .checked_add(frame_len)
                .ok_or_else(|| corrupt(offset, "frame offset overflow"))?;
            recovery.records_replayed += 1;
        }
        if offset < length {
            // Parsing the complete prefix succeeded; only then repair a short EOF tail.
            storage
                .set_len(offset)
                .map_err(io_error("truncate incomplete WAL tail"))?;
            sync(&mut storage)?;
            recovery.discarded_tail_bytes = length - offset;
        }
        Ok(Self {
            storage,
            values,
            deleted,
            change_bytes,
            sequence,
            wal_len: offset,
            poisoned: false,
            recovery,
        })
    }

    pub fn get(&self, key: &[u8]) -> Option<&[u8]> {
        self.values.get(key).map(Vec::as_slice)
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
    pub fn recovery(&self) -> Recovery {
        self.recovery
    }

    pub fn put(&mut self, key: &[u8], value: &[u8]) -> Result<(), Error> {
        self.check_writable()?;
        if key.len() > wal::MAX_KEY_BYTES || value.len() > wal::MAX_VALUE_BYTES {
            return Err(Error::InvalidInput(
                "key or value exceeds WAL size limit".into(),
            ));
        }
        self.commit(Operation::Put {
            key: key.to_vec(),
            value: value.to_vec(),
        })
    }

    pub fn delete(&mut self, key: &[u8]) -> Result<(), Error> {
        self.check_writable()?;
        if key.len() > wal::MAX_KEY_BYTES {
            return Err(Error::InvalidInput("key exceeds WAL size limit".into()));
        }
        self.commit(Operation::Delete { key: key.to_vec() })
    }

    fn check_writable(&self) -> Result<(), Error> {
        if self.poisoned {
            Err(Error::Poisoned)
        } else {
            Ok(())
        }
    }

    fn commit(&mut self, operation: Operation) -> Result<(), Error> {
        self.check_writable()?;
        let sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| Error::InvalidInput("sequence overflow".into()))?;
        let frame = wal::encode_frame(sequence, &operation)
            .map_err(|error| Error::InvalidInput(error.to_string()))?;
        let new_len = self
            .wal_len
            .checked_add(frame.len() as u64)
            .ok_or_else(|| Error::InvalidInput("WAL offset overflow".into()))?;
        let result = self
            .storage
            .write_all_at(self.wal_len, &frame)
            .map_err(io_error("append WAL"))
            .and_then(|()| sync(&mut self.storage));
        if let Err(error) = result {
            self.poisoned = true;
            return Err(error);
        }
        apply(
            &mut self.values,
            &mut self.deleted,
            &mut self.change_bytes,
            operation,
        );
        self.sequence = sequence;
        self.wal_len = new_len;
        Ok(())
    }

    /// Explicitly check final synchronization and release the storage/lock.
    /// A poisoned engine must instead be dropped and reopened for recovery.
    pub fn close(mut self) -> Result<(), Error> {
        self.check_writable()?;
        sync(&mut self.storage)
    }

    pub(crate) fn lookup_change(&self, key: &[u8]) -> Option<Option<&[u8]>> {
        if let Some(value) = self.values.get(key) {
            return Some(Some(value));
        }
        if self.deleted.contains(key) {
            return Some(None);
        }
        None
    }
    pub(crate) fn value_keys<'a>(
        &'a self,
        start: &Bound<Vec<u8>>,
        end: &Bound<Vec<u8>>,
        reverse: bool,
    ) -> Box<dyn Iterator<Item = &'a [u8]> + 'a> {
        let keys = self
            .values
            .range::<Vec<u8>, _>((start.as_ref(), end.as_ref()))
            .map(|(key, _)| key.as_slice());
        if reverse {
            Box::new(keys.rev())
        } else {
            Box::new(keys)
        }
    }
    pub(crate) fn deleted_keys<'a>(
        &'a self,
        start: &Bound<Vec<u8>>,
        end: &Bound<Vec<u8>>,
        reverse: bool,
    ) -> Box<dyn Iterator<Item = &'a [u8]> + 'a> {
        let keys = self
            .deleted
            .range::<Vec<u8>, _>((start.as_ref(), end.as_ref()))
            .map(|key| key.as_slice());
        if reverse {
            Box::new(keys.rev())
        } else {
            Box::new(keys)
        }
    }
    pub(crate) fn change_count(&self) -> usize {
        self.values.len() + self.deleted.len()
    }
    /// Logical key/value payload in this WAL delta, excluding allocation overhead.
    pub(crate) fn change_bytes(&self) -> usize {
        self.change_bytes
    }
    pub(crate) fn projected_change_bytes(&self, key: &[u8], value: Option<&[u8]>) -> usize {
        self.change_bytes - change_size(&self.values, &self.deleted, key)
            + key.len()
            + value.map_or(0, <[u8]>::len)
    }
    pub(crate) fn byte_len(&self) -> u64 {
        self.wal_len
    }
    pub(crate) fn snapshot_changes(&self) -> Result<BTreeMap<Vec<u8>, Option<Vec<u8>>>, Error> {
        self.check_writable()?;
        let mut changes: BTreeMap<_, _> = self
            .values
            .iter()
            .map(|(key, value)| (key.clone(), Some(value.clone())))
            .collect();
        for key in &self.deleted {
            changes.insert(key.clone(), None);
        }
        Ok(changes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryStorage;

    #[test]
    fn payload_bytes_track_binary_empty_overwrites_and_tombstones() {
        let mut engine = Engine::open(MemoryStorage::new()).unwrap();
        assert_eq!(engine.change_bytes(), 0);
        assert_eq!(engine.projected_change_bytes(b"", Some(b"")), 0);
        engine.put(b"", b"").unwrap();
        assert_eq!(engine.change_count(), 1);
        assert_eq!(engine.change_bytes(), 0);
        engine.put(b"", b"abc").unwrap();
        assert_eq!(engine.change_bytes(), 3);
        assert_eq!(
            engine.projected_change_bytes(&[0, 255], Some(&[255, 0, 1])),
            8
        );
        engine.put(&[0, 255], &[255, 0, 1]).unwrap();
        assert_eq!(engine.change_bytes(), 8);
        assert_eq!(engine.projected_change_bytes(&[0, 255], Some(b"x")), 6);
        assert_eq!(engine.change_bytes(), 8);
        engine.put(&[0, 255], b"x").unwrap();
        assert_eq!(engine.change_bytes(), 6);
        assert_eq!(engine.projected_change_bytes(&[0, 255], None), 5);
        engine.delete(&[0, 255]).unwrap();
        assert_eq!(engine.change_bytes(), 5);
        assert_eq!(engine.projected_change_bytes(&[0, 255], None), 5);
        engine.delete(&[0, 255]).unwrap();
        assert_eq!(engine.change_bytes(), 5);
        assert_eq!(engine.projected_change_bytes(&[0, 255], Some(b"1234")), 9);
        engine.put(&[0, 255], b"1234").unwrap();
        assert_eq!(engine.change_bytes(), 9);
        assert_eq!(engine.projected_change_bytes(b"absent", None), 15);
        engine.delete(b"absent").unwrap();
        assert_eq!(engine.change_bytes(), 15);
        assert_eq!(engine.projected_change_bytes(b"", None), 12);
        engine.delete(b"").unwrap();
        assert_eq!(engine.change_bytes(), 12);
        engine.delete(b"").unwrap();
        assert_eq!(engine.change_bytes(), 12);
        assert_eq!(engine.change_count(), 3);
        let reopened = Engine::open(engine.storage).unwrap();
        assert_eq!(reopened.change_bytes(), 12);
        assert_eq!(reopened.change_count(), 3);
        assert_eq!(reopened.recovery().records_replayed, 10);
    }

    #[derive(Clone, Copy)]
    enum Failure {
        Append,
        Flush,
        Sync,
    }

    #[derive(Default)]
    struct FaultStorage {
        inner: MemoryStorage,
        failure: Option<Failure>,
    }

    impl FaultStorage {
        fn fail(&self, stage: Failure) -> io::Result<()> {
            if matches!(
                (self.failure, stage),
                (Some(Failure::Append), Failure::Append)
                    | (Some(Failure::Flush), Failure::Flush)
                    | (Some(Failure::Sync), Failure::Sync)
            ) {
                Err(io::Error::other("injected failure"))
            } else {
                Ok(())
            }
        }
    }

    impl Storage for FaultStorage {
        fn len(&self) -> io::Result<u64> {
            self.inner.len()
        }
        fn read_at(&mut self, offset: u64, buffer: &mut [u8]) -> io::Result<usize> {
            self.inner.read_at(offset, buffer)
        }
        fn write_all_at(&mut self, offset: u64, bytes: &[u8]) -> io::Result<()> {
            self.inner.write_all_at(offset, bytes)?;
            self.fail(Failure::Append)
        }
        fn set_len(&mut self, len: u64) -> io::Result<()> {
            self.inner.set_len(len)
        }
        fn flush(&mut self) -> io::Result<()> {
            self.fail(Failure::Flush)
        }
        fn sync_all(&mut self) -> io::Result<()> {
            self.fail(Failure::Sync)
        }
    }

    #[test]
    fn failed_append_flush_or_sync_does_not_publish_payload_bytes() {
        for failure in [Failure::Append, Failure::Flush, Failure::Sync] {
            for delete in [false, true] {
                let mut engine = Engine::open(FaultStorage::default()).unwrap();
                engine.put(b"key", b"confirmed").unwrap();
                assert_eq!(engine.change_bytes(), 12);
                engine.storage.failure = Some(failure);
                let result = if delete {
                    engine.delete(b"key")
                } else {
                    engine.put(b"key", b"changed")
                };
                assert!(matches!(result, Err(Error::Io { .. })));
                assert_eq!(engine.change_bytes(), 12);
                assert_eq!(engine.get(b"key"), Some(b"confirmed".as_slice()));
                assert!(matches!(engine.put(b"later", b"x"), Err(Error::Poisoned)));
                assert_eq!(engine.change_bytes(), 12);
                let mut storage = engine.storage;
                storage.failure = None;
                // The fully written, unacknowledged operation may recover; its
                // contribution is then derived from the actual checked WAL.
                let reopened = Engine::open(storage).unwrap();
                assert_eq!(reopened.change_bytes(), if delete { 3 } else { 10 });
            }
        }
    }

    #[test]
    fn replay_repairs_incomplete_tail_without_counting_its_payload() {
        let mut engine = Engine::open(MemoryStorage::new()).unwrap();
        engine.put(b"stable", b"value").unwrap();
        engine.delete(b"gone").unwrap();
        let length = engine.byte_len();
        let frame = wal::encode_frame(
            3,
            &Operation::Put {
                key: b"partial".to_vec(),
                value: b"not complete".to_vec(),
            },
        )
        .unwrap();
        engine
            .storage
            .write_all_at(length, &frame[..frame.len() - 1])
            .unwrap();
        let reopened = Engine::open(engine.storage).unwrap();
        assert_eq!(reopened.change_bytes(), 15);
        assert_eq!(reopened.change_count(), 2);
        assert_eq!(reopened.recovery().records_replayed, 2);
        assert_eq!(reopened.get(b"partial"), None);
        assert_eq!(reopened.byte_len(), length);
    }
}
