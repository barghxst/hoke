use std::cell::RefCell;
use std::io;
use std::rc::Rc;

use stowtree_core::storage::Storage;
use stowtree_core::{Engine, Error};

const FILE_HEADER: usize = 16;
const FRAME_HEADER: usize = 28;

#[derive(Default)]
struct State {
    bytes: Vec<u8>,
    max_read: Option<usize>,
    fail_read_at: Option<u64>,
    fail_len: bool,
    fail_write_prefix: Option<usize>,
    fail_truncate: bool,
    fail_flush: bool,
    fail_sync: bool,
    writes: usize,
    truncations: usize,
    reads: usize,
}

#[derive(Clone, Default)]
struct SharedStorage(Rc<RefCell<State>>);

fn injected() -> io::Error {
    io::Error::other("injected storage failure")
}

impl SharedStorage {
    fn from_bytes(bytes: Vec<u8>) -> Self {
        Self(Rc::new(RefCell::new(State {
            bytes,
            ..State::default()
        })))
    }

    fn bytes(&self) -> Vec<u8> {
        self.0.borrow().bytes.clone()
    }
}

impl Storage for SharedStorage {
    fn len(&self) -> io::Result<u64> {
        let state = self.0.borrow();
        if state.fail_len {
            return Err(injected());
        }
        Ok(state.bytes.len() as u64)
    }

    fn read_at(&mut self, offset: u64, buffer: &mut [u8]) -> io::Result<usize> {
        let mut state = self.0.borrow_mut();
        state.reads += 1;
        if state.fail_read_at.is_some_and(|start| offset >= start) {
            return Err(injected());
        }
        if offset >= state.bytes.len() as u64 {
            return Ok(0);
        }
        let offset = offset as usize;
        let count = buffer
            .len()
            .min(state.bytes.len() - offset)
            .min(state.max_read.unwrap_or(usize::MAX));
        buffer[..count].copy_from_slice(&state.bytes[offset..offset + count]);
        Ok(count)
    }

    fn write_all_at(&mut self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        let end = offset
            .checked_add(bytes.len() as u64)
            .and_then(|end| usize::try_from(end).ok())
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        let mut state = self.0.borrow_mut();
        state.writes += 1;
        let count = state
            .fail_write_prefix
            .unwrap_or(bytes.len())
            .min(bytes.len());
        if count > 0 {
            let start = end - bytes.len();
            if start + count > state.bytes.len() {
                state.bytes.resize(start + count, 0);
            }
            state.bytes[start..start + count].copy_from_slice(&bytes[..count]);
        }
        if state.fail_write_prefix.is_some() {
            return Err(injected());
        }
        Ok(())
    }

    fn set_len(&mut self, len: u64) -> io::Result<()> {
        let mut state = self.0.borrow_mut();
        if state.fail_truncate {
            return Err(injected());
        }
        state.truncations += 1;
        state.bytes.resize(len as usize, 0);
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.0.borrow().fail_flush {
            Err(injected())
        } else {
            Ok(())
        }
    }

    fn sync_all(&mut self) -> io::Result<()> {
        if self.0.borrow().fail_sync {
            Err(injected())
        } else {
            Ok(())
        }
    }
}

fn fixture() -> (Vec<u8>, usize, usize) {
    let storage = SharedStorage::default();
    let mut engine = Engine::open(storage.clone()).unwrap();
    engine.put(b"prior", b"confirmed").unwrap();
    let second = storage.bytes().len();
    engine.put(b"middle", b"payload").unwrap();
    let third = storage.bytes().len();
    engine.put(b"final", b"payload").unwrap();
    engine.close().unwrap();
    (storage.bytes(), second, third)
}

fn assert_io(error: Error, expected_operation: &str) {
    match error {
        Error::Io { operation, source } => {
            assert_eq!(operation, expected_operation);
            assert_eq!(source.kind(), io::ErrorKind::Other);
            assert_eq!(source.to_string(), "injected storage failure");
        }
        other => panic!("expected I/O error, got {other:?}"),
    }
}

#[test]
fn roundtrip_overwrites_deletes_and_binary_empty_bytes() {
    let storage = SharedStorage::default();
    let mut engine = Engine::open(storage.clone()).unwrap();
    assert!(engine.is_empty());
    engine.put(b"", b"").unwrap();
    engine.put(&[0, 255, 128], &[255, 0, 128]).unwrap();
    engine.put(b"replace", b"old").unwrap();
    engine.put(b"replace", b"new").unwrap();
    engine.put(b"deleted", b"gone").unwrap();
    engine.delete(b"deleted").unwrap();
    engine.delete(b"absent").unwrap();
    assert_eq!(engine.len(), 3);
    engine.close().unwrap();

    let mut engine = Engine::open(storage.clone()).unwrap();
    assert_eq!(engine.recovery().records_replayed, 7);
    assert_eq!(engine.get(b""), Some(&b""[..]));
    assert_eq!(engine.get(&[0, 255, 128]), Some(&[255, 0, 128][..]));
    assert_eq!(engine.get(b"replace"), Some(&b"new"[..]));
    assert_eq!(engine.get(b"deleted"), None);
    engine.delete(b"").unwrap();
    engine.close().unwrap();
    let engine = Engine::open(storage).unwrap();
    assert_eq!(engine.get(b""), None);
    assert_eq!(engine.len(), 2);
}

#[test]
fn every_incomplete_final_frame_cut_keeps_prefix_and_allows_new_append() {
    let (bytes, _, final_start) = fixture();
    for cut in final_start..bytes.len() {
        let storage = SharedStorage::from_bytes(bytes[..cut].to_vec());
        let mut engine = Engine::open(storage.clone()).unwrap();
        assert_eq!(engine.get(b"prior"), Some(&b"confirmed"[..]), "cut {cut}");
        assert_eq!(engine.get(b"middle"), Some(&b"payload"[..]));
        assert_eq!(engine.get(b"final"), None);
        assert_eq!(engine.recovery().records_replayed, 2);
        assert_eq!(
            engine.recovery().discarded_tail_bytes,
            (cut - final_start) as u64
        );
        assert_eq!(storage.bytes(), bytes[..final_start]);
        engine.put(b"after", b"repaired").unwrap();
        engine.close().unwrap();
        let reopened = Engine::open(storage).unwrap();
        assert_eq!(reopened.get(b"prior"), Some(&b"confirmed"[..]));
        assert_eq!(reopened.get(b"after"), Some(&b"repaired"[..]));
        assert_eq!(reopened.get(b"final"), None);
        assert_eq!(reopened.recovery().records_replayed, 3);
    }
}

#[test]
fn complete_corrupt_headers_and_payloads_at_middle_or_end_are_never_truncated() {
    let (bytes, middle, final_start) = fixture();
    for (start, end) in [(middle, final_start), (final_start, bytes.len())] {
        // Every byte in the complete frame, including header/checksums, matters.
        for index in start..end {
            let mut corrupt = bytes.clone();
            corrupt[index] ^= 1;
            let storage = SharedStorage::from_bytes(corrupt.clone());
            let error = Engine::open(storage.clone()).err().unwrap();
            assert!(matches!(error, Error::Corruption { offset, .. } if offset == start as u64));
            assert_eq!(storage.bytes(), corrupt, "changed corrupt byte {index}");
            assert_eq!(storage.0.borrow().truncations, 0);
            assert_eq!(storage.0.borrow().writes, 0);
        }
    }
}

#[test]
fn sequence_gap_with_valid_header_checksum_is_corruption() {
    let (mut bytes, middle, _) = fixture();
    bytes[middle + 4..middle + 12].copy_from_slice(&99u64.to_le_bytes());
    let crc = crc32fast::hash(&bytes[middle..middle + 24]);
    bytes[middle + 24..middle + 28].copy_from_slice(&crc.to_le_bytes());
    let storage = SharedStorage::from_bytes(bytes.clone());
    assert!(
        matches!(Engine::open(storage.clone()), Err(Error::Corruption { offset, .. }) if offset == middle as u64)
    );
    assert_eq!(storage.bytes(), bytes);
    assert_eq!(storage.0.borrow().truncations, 0);
}

#[test]
fn invalid_magic_version_checksum_and_short_initial_headers_are_untouched() {
    let (bytes, _, _) = fixture();
    let mut invalid_magic = bytes.clone();
    invalid_magic[0] ^= 1;
    let mut invalid_crc = bytes.clone();
    invalid_crc[12] ^= 1;
    let mut future_version = bytes.clone();
    future_version[8..12].copy_from_slice(&99u32.to_le_bytes());
    let crc = crc32fast::hash(&future_version[..12]);
    future_version[12..16].copy_from_slice(&crc.to_le_bytes());
    for raw in [invalid_magic, invalid_crc, future_version] {
        let storage = SharedStorage::from_bytes(raw.clone());
        let error = Engine::open(storage.clone()).err().unwrap();
        if raw[8] == 99 {
            assert!(matches!(error, Error::UnsupportedVersion(99)));
        } else {
            assert!(matches!(error, Error::Corruption { offset: 0, .. }));
        }
        assert_eq!(storage.bytes(), raw);
        assert_eq!(storage.0.borrow().writes, 0);
        assert_eq!(storage.0.borrow().truncations, 0);
    }
    // Zero bytes mean a new WAL, so incomplete existing headers start at one.
    for cut in 1..FILE_HEADER {
        let storage = SharedStorage::from_bytes(bytes[..cut].to_vec());
        assert!(matches!(
            Engine::open(storage.clone()),
            Err(Error::Corruption { offset: 0, .. })
        ));
        assert_eq!(storage.bytes(), bytes[..cut]);
        assert_eq!(storage.0.borrow().writes, 0);
        assert_eq!(storage.0.borrow().truncations, 0);
    }
}

#[test]
fn replay_loops_over_one_byte_short_reads() {
    let (bytes, _, _) = fixture();
    let storage = SharedStorage::from_bytes(bytes.clone());
    storage.0.borrow_mut().max_read = Some(1);
    let engine = Engine::open(storage.clone()).unwrap();
    assert_eq!(engine.get(b"final"), Some(&b"payload"[..]));
    assert_eq!(engine.recovery().records_replayed, 3);
    assert_eq!(storage.0.borrow().reads, bytes.len());
    assert_eq!(storage.bytes(), bytes);
}

#[test]
fn each_partial_append_failure_poisoned_without_publishing_and_reopens_prefix() {
    let (bytes, _, final_start) = fixture();
    let frame_len = bytes.len() - final_start;
    for prefix in 0..=frame_len {
        let storage = SharedStorage::from_bytes(bytes[..final_start].to_vec());
        let mut engine = Engine::open(storage.clone()).unwrap();
        storage.0.borrow_mut().fail_write_prefix = Some(prefix);
        assert_io(engine.put(b"final", b"payload").unwrap_err(), "append WAL");
        assert_eq!(engine.get(b"final"), None);
        assert_eq!(engine.get(b"prior"), Some(&b"confirmed"[..]));
        let after_failure = storage.bytes();
        assert!(matches!(engine.put(b"later", b"no"), Err(Error::Poisoned)));
        assert!(matches!(engine.delete(b"prior"), Err(Error::Poisoned)));
        assert_eq!(storage.bytes(), after_failure);
        drop(engine);
        storage.0.borrow_mut().fail_write_prefix = None;
        let mut reopened = Engine::open(storage.clone()).unwrap();
        assert_eq!(reopened.get(b"prior"), Some(&b"confirmed"[..]));
        assert_eq!(reopened.get(b"middle"), Some(&b"payload"[..]));
        if prefix == frame_len {
            // A fully appended but unacknowledged frame may survive an error.
            assert_eq!(reopened.get(b"final"), Some(&b"payload"[..]));
        } else {
            assert_eq!(reopened.get(b"final"), None);
        }
        reopened.put(b"later", b"yes").unwrap();
        reopened.close().unwrap();
        assert_eq!(
            Engine::open(storage).unwrap().get(b"later"),
            Some(&b"yes"[..])
        );
    }
}

#[test]
fn flush_and_sync_failures_do_not_publish_overwrite_or_delete() {
    for sync_failure in [false, true] {
        for delete in [false, true] {
            let storage = SharedStorage::default();
            let mut engine = Engine::open(storage.clone()).unwrap();
            engine.put(b"key", b"confirmed").unwrap();
            {
                let mut state = storage.0.borrow_mut();
                state.fail_flush = !sync_failure;
                state.fail_sync = sync_failure;
            }
            let error = if delete {
                engine.delete(b"key").unwrap_err()
            } else {
                engine.put(b"key", b"unacknowledged").unwrap_err()
            };
            assert_io(
                error,
                if sync_failure {
                    "sync WAL"
                } else {
                    "flush WAL"
                },
            );
            assert_eq!(engine.get(b"key"), Some(&b"confirmed"[..]));
            assert_eq!(engine.len(), 1);
            assert!(matches!(engine.delete(b"key"), Err(Error::Poisoned)));
            assert!(matches!(engine.put(b"key", b"retry"), Err(Error::Poisoned)));
            assert!(matches!(engine.close(), Err(Error::Poisoned)));
            {
                let mut state = storage.0.borrow_mut();
                state.fail_flush = false;
                state.fail_sync = false;
            }
            let reopened = Engine::open(storage).unwrap();
            assert_eq!(reopened.recovery().records_replayed, 2);
            assert_eq!(
                reopened.get(b"key"),
                if delete {
                    None
                } else {
                    Some(&b"unacknowledged"[..])
                }
            );
        }
    }
}

#[test]
fn read_and_length_errors_propagate_without_writing_or_truncating() {
    let (bytes, middle, _) = fixture();
    for failure_offset in [
        None,
        Some(0),
        Some(FILE_HEADER as u64),
        Some((middle + FRAME_HEADER) as u64),
    ] {
        let storage = SharedStorage::from_bytes(bytes.clone());
        {
            let mut state = storage.0.borrow_mut();
            state.fail_len = failure_offset.is_none();
            state.fail_read_at = failure_offset;
        }
        let error = Engine::open(storage.clone()).err().unwrap();
        assert_io(
            error,
            if failure_offset.is_none() {
                "read WAL length"
            } else {
                "read WAL"
            },
        );
        assert_eq!(storage.bytes(), bytes);
        assert_eq!(storage.0.borrow().writes, 0);
        assert_eq!(storage.0.borrow().truncations, 0);
    }
}

#[test]
fn zero_short_read_is_unexpected_eof_and_does_not_repair() {
    let (bytes, _, _) = fixture();
    let storage = SharedStorage::from_bytes(bytes.clone());
    storage.0.borrow_mut().max_read = Some(0);
    let error = Engine::open(storage.clone()).err().unwrap();
    assert!(
        matches!(error, Error::Io { source, .. } if source.kind() == io::ErrorKind::UnexpectedEof)
    );
    assert_eq!(storage.bytes(), bytes);
    assert_eq!(storage.0.borrow().truncations, 0);
}

#[test]
fn failed_truncation_and_repair_flush_or_sync_fail_open() {
    let (bytes, _, final_start) = fixture();
    let damaged = bytes[..final_start + 1].to_vec();
    for operation in ["truncate incomplete WAL tail", "flush WAL", "sync WAL"] {
        let storage = SharedStorage::from_bytes(damaged.clone());
        {
            let mut state = storage.0.borrow_mut();
            state.fail_truncate = operation == "truncate incomplete WAL tail";
            state.fail_flush = operation == "flush WAL";
            state.fail_sync = operation == "sync WAL";
        }
        assert_io(Engine::open(storage.clone()).err().unwrap(), operation);
        assert_eq!(
            storage.bytes(),
            if operation == "truncate incomplete WAL tail" {
                damaged.clone()
            } else {
                bytes[..final_start].to_vec()
            }
        );
        {
            let mut state = storage.0.borrow_mut();
            state.fail_truncate = false;
            state.fail_flush = false;
            state.fail_sync = false;
        }
        let engine = Engine::open(storage).unwrap();
        assert_eq!(engine.get(b"prior"), Some(&b"confirmed"[..]));
        assert_eq!(engine.get(b"middle"), Some(&b"payload"[..]));
    }
}

#[test]
fn initialization_and_close_report_write_flush_and_sync_failures() {
    for operation in ["initialize WAL", "flush WAL", "sync WAL"] {
        let storage = SharedStorage::default();
        {
            let mut state = storage.0.borrow_mut();
            state.fail_write_prefix = (operation == "initialize WAL").then_some(0);
            state.fail_flush = operation == "flush WAL";
            state.fail_sync = operation == "sync WAL";
        }
        assert_io(Engine::open(storage).err().unwrap(), operation);
    }
    for operation in ["flush WAL", "sync WAL"] {
        let storage = SharedStorage::default();
        let engine = Engine::open(storage.clone()).unwrap();
        {
            let mut state = storage.0.borrow_mut();
            state.fail_flush = operation == "flush WAL";
            state.fail_sync = operation == "sync WAL";
        }
        assert_io(engine.close().unwrap_err(), operation);
    }
}
