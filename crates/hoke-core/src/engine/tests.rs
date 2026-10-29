use super::*;
use crate::storage::MemoryStorage;
use crate::wal::{self, Operation};
use std::io;

#[test]
fn derived_committed_changes_never_grow_volatile_wal() {
    let mut engine = Engine::open(MemoryStorage::new()).unwrap();
    for i in 0..100 {
        engine.apply_committed(Operation::Put {
            key: b"key".to_vec(),
            value: vec![i; 100],
        });
    }
    engine.apply_committed(Operation::Delete {
        key: b"key".to_vec(),
    });
    assert_eq!(engine.lookup_change(b"key"), Some(None));
    assert_eq!(engine.change_bytes(), 3);
    assert_eq!(engine.change_count(), 1);
    assert_eq!(engine.byte_len(), wal::FILE_HEADER_LEN as u64);
    assert_eq!(engine.storage.len().unwrap(), wal::FILE_HEADER_LEN as u64);
    assert_eq!(engine.recovery().records_replayed, 0);
}

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
