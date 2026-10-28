use hoke_core::{
    BatchOperation, ContainerDatabase, ContainerDbError, ScanOptions,
    container::{Journal, SegmentKind},
    storage::{MemoryStorage, Storage},
};
use std::{cell::RefCell, io, ops::Bound, rc::Rc};

#[derive(Default)]
struct State {
    bytes: MemoryStorage,
    syncs: usize,
    fail_sync: Option<usize>,
    fail_read: bool,
    writes: usize,
}
#[derive(Clone, Default)]
struct Fault(Rc<RefCell<State>>);
impl Storage for Fault {
    fn len(&self) -> io::Result<u64> {
        self.0.borrow().bytes.len()
    }
    fn read_at(&mut self, offset: u64, bytes: &mut [u8]) -> io::Result<usize> {
        let mut s = self.0.borrow_mut();
        if s.fail_read {
            return Err(io::Error::other("read fault"));
        }
        s.bytes.read_at(offset, bytes)
    }
    fn write_all_at(&mut self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        let mut s = self.0.borrow_mut();
        s.writes += 1;
        s.bytes.write_all_at(offset, bytes)
    }
    fn set_len(&mut self, len: u64) -> io::Result<()> {
        let mut s = self.0.borrow_mut();
        s.writes += 1;
        s.bytes.set_len(len)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
    fn sync_all(&mut self) -> io::Result<()> {
        let mut s = self.0.borrow_mut();
        s.syncs += 1;
        if s.fail_sync == Some(s.syncs) {
            Err(io::Error::other("sync fault"))
        } else {
            Ok(())
        }
    }
}
fn put(key: &[u8], value: &[u8]) -> BatchOperation {
    BatchOperation::Put {
        key: key.to_vec(),
        value: value.to_vec(),
    }
}
fn delete(key: &[u8]) -> BatchOperation {
    BatchOperation::Delete { key: key.to_vec() }
}
fn all() -> ScanOptions {
    ScanOptions {
        limit: usize::MAX,
        ..Default::default()
    }
}

#[test]
fn mutations_batches_flush_compact_and_reopen_resolve_latest_state() {
    let mut db = ContainerDatabase::open(MemoryStorage::new()).unwrap();
    assert_eq!(db.stats().generation, 1);
    db.put(b"a", b"old").unwrap();
    db.put(b"b", b"stay").unwrap();
    db.flush().unwrap();
    db.write_batch(&[
        put(b"a", b"new"),
        delete(b"b"),
        put(b"z", b"last"),
        put(b"z", b"winner"),
    ])
    .unwrap();
    assert_eq!(db.stats().wal_segments, 1);
    assert_eq!(db.get(b"a").unwrap(), Some(b"new".to_vec()));
    assert_eq!(db.get(b"b").unwrap(), None);
    let mut db = ContainerDatabase::open(db.into_storage()).unwrap();
    assert_eq!(
        db.scan_with_budget(all(), 100).unwrap(),
        vec![
            (b"a".to_vec(), b"new".to_vec()),
            (b"z".to_vec(), b"winner".to_vec())
        ]
    );
    db.flush().unwrap();
    assert_eq!(db.stats().table_count, 2);
    assert_eq!(db.stats().memtable_entries, 0);
    db.compact().unwrap();
    assert_eq!(db.stats().table_count, 1);
    assert_eq!(db.stats().wal_segments, 0);
    let mut db = ContainerDatabase::open(db.into_storage()).unwrap();
    assert_eq!(db.get(b"b").unwrap(), None);
    assert_eq!(db.get(b"z").unwrap(), Some(b"winner".to_vec()));
}
#[test]
fn empty_binary_keys_and_tombstones_survive_flush_and_empty_compaction() {
    let mut db = ContainerDatabase::open(MemoryStorage::new()).unwrap();
    db.put(b"", b"").unwrap();
    db.put(&[0, 255], &[255, 0]).unwrap();
    db.flush().unwrap();
    db.delete(b"").unwrap();
    db.flush().unwrap();
    assert_eq!(db.get(b"").unwrap(), None);
    assert_eq!(db.get(&[0, 255]).unwrap(), Some(vec![255, 0]));
    db.delete(&[0, 255]).unwrap();
    db.compact().unwrap();
    assert_eq!(db.stats().table_count, 0);
    let mut db = ContainerDatabase::open(db.into_storage()).unwrap();
    assert!(db.scan_with_budget(all(), 0).unwrap().is_empty());
}
#[test]
fn invalid_inputs_and_noops_do_not_write_or_poison() {
    let storage = Fault::default();
    let mut db = ContainerDatabase::open(storage.clone()).unwrap();
    let writes = storage.0.borrow().writes;
    let generation = db.stats().generation;
    db.write_batch(&[]).unwrap();
    db.flush().unwrap();
    db.compact().unwrap();
    assert!(db.write_batch(&[put(&vec![0; 65537], b"x")]).is_err());
    assert!(db.write_batch(&vec![delete(b"x"); 1025]).is_err());
    assert_eq!(storage.0.borrow().writes, writes);
    assert_eq!(db.stats().generation, generation);
    assert!(!db.is_poisoned());
    db.put(b"ok", b"yes").unwrap();
}
#[test]
fn scan_range_reverse_prefix_and_budget_failure_keep_database_usable() {
    let mut db = ContainerDatabase::open(MemoryStorage::new()).unwrap();
    db.write_batch(&[put(b"aa", b"1"), put(b"ab", b"2"), put(b"ba", b"3")])
        .unwrap();
    db.flush().unwrap();
    db.put(b"ac", b"4").unwrap();
    db.delete(b"ab").unwrap();
    let options = ScanOptions {
        prefix: Some(b"a".to_vec()),
        reverse: true,
        ..all()
    };
    assert_eq!(
        db.scan_with_budget(options, 6).unwrap(),
        vec![
            (b"ac".to_vec(), b"4".to_vec()),
            (b"aa".to_vec(), b"1".to_vec())
        ]
    );
    assert!(db.scan_with_budget(all(), 1).is_err());
    assert!(!db.is_poisoned());
    assert!(
        db.scan_with_budget(
            ScanOptions {
                start: Bound::Included(b"z".to_vec()),
                end: Bound::Included(b"a".to_vec()),
                ..all()
            },
            100
        )
        .is_err()
    );
    db.put(b"c", b"ok").unwrap();
}
#[test]
fn wal_fanin_preflush_and_sst_capacity_are_explicit_and_recoverable_by_compaction() {
    let mut db = ContainerDatabase::open(MemoryStorage::new()).unwrap();
    for _ in 0..64 {
        db.put(b"x", b"v").unwrap();
    }
    assert_eq!(db.stats().wal_segments, 64);
    db.put(b"x", b"65").unwrap();
    assert_eq!(db.stats().wal_segments, 1);
    assert_eq!(db.stats().table_count, 1);
    db.flush().unwrap();
    for _ in 2..64 {
        db.put(b"x", b"v").unwrap();
        db.flush().unwrap();
    }
    db.put(b"x", b"latest").unwrap();
    let generation = db.stats().generation;
    assert!(matches!(db.flush(), Err(ContainerDbError::Limit(_))));
    assert_eq!(db.stats().generation, generation);
    assert!(!db.is_poisoned());
    db.compact().unwrap();
    assert_eq!(db.stats().table_count, 1);
    assert_eq!(db.get(b"x").unwrap(), Some(b"latest".to_vec()));
}
#[test]
fn commit_sync_failure_hides_local_state_and_requires_reopen() {
    for second in [false, true] {
        let storage = Fault::default();
        let mut db = ContainerDatabase::open(storage.clone()).unwrap();
        db.put(b"key", b"acknowledged").unwrap();
        let stats = db.stats();
        let fail = storage.0.borrow().syncs + if second { 2 } else { 1 };
        storage.0.borrow_mut().fail_sync = Some(fail);
        assert!(db.put(b"key", b"unknown").is_err());
        assert!(db.is_poisoned());
        assert_eq!(db.stats(), stats);
        assert!(matches!(db.get(b"key"), Err(ContainerDbError::Poisoned)));
        assert!(db.compact().is_err());
        storage.0.borrow_mut().fail_sync = None;
        let mut reopened = ContainerDatabase::open(db.into_storage()).unwrap();
        assert_eq!(
            reopened.get(b"key").unwrap(),
            Some(if second {
                b"unknown".to_vec()
            } else {
                b"acknowledged".to_vec()
            })
        );
    }
}
#[test]
fn flush_failure_preserves_old_root_locally_and_recovery_selects_actual_commit() {
    let storage = Fault::default();
    let mut db = ContainerDatabase::open(storage.clone()).unwrap();
    db.put(b"x", b"v").unwrap();
    let stats = db.stats();
    let fail = storage.0.borrow().syncs + 2;
    storage.0.borrow_mut().fail_sync = Some(fail);
    assert!(db.flush().is_err());
    assert_eq!(db.stats(), stats);
    assert!(db.is_poisoned());
    storage.0.borrow_mut().fail_sync = None;
    let mut db = ContainerDatabase::open(db.into_storage()).unwrap();
    assert_eq!(db.stats().table_count, 1);
    assert_eq!(db.get(b"x").unwrap(), Some(b"v".to_vec()));
}
#[test]
fn source_io_error_never_falls_back_and_poison_blocks_every_read() {
    let storage = Fault::default();
    let mut db = ContainerDatabase::open(storage.clone()).unwrap();
    db.put(b"x", b"old").unwrap();
    db.flush().unwrap();
    db.put(b"x", b"new").unwrap();
    db.flush().unwrap();
    storage.0.borrow_mut().fail_read = true;
    assert!(db.get(b"x").is_err());
    assert!(db.is_poisoned());
    storage.0.borrow_mut().fail_read = false;
    assert!(db.get(b"x").is_err());
    assert!(db.scan_with_budget(all(), 100).is_err());
    let mut db = ContainerDatabase::open(db.into_storage()).unwrap();
    assert_eq!(db.get(b"x").unwrap(), Some(b"new".to_vec()));
}
#[test]
fn orphan_payload_is_not_user_state_and_empty_root_is_explicit() {
    let mut journal = Journal::open(MemoryStorage::new()).unwrap();
    journal
        .append_segment(SegmentKind::Wal, b"not even a WAL")
        .unwrap();
    let mut db = ContainerDatabase::open(journal.into_storage()).unwrap();
    assert_eq!(db.stats().generation, 1);
    assert_eq!(db.stats().wal_segments, 0);
    assert!(db.scan_with_budget(all(), 0).unwrap().is_empty());
}
#[test]
fn invalid_active_inner_payload_does_not_repair_incomplete_outer_tail() {
    for kind in [SegmentKind::Wal, SegmentKind::Sst] {
        let storage = Fault::default();
        let mut journal = Journal::open(storage.clone()).unwrap();
        let reference = journal.append_segment(kind, b"bad inner payload").unwrap();
        match kind {
            SegmentKind::Wal => journal.commit(&[], &[reference]).unwrap(),
            SegmentKind::Sst => journal.commit(&[reference], &[]).unwrap(),
        };
        let mut storage = journal.into_storage();
        let end = storage.len().unwrap();
        storage.write_all_at(end, b"partial").unwrap();
        let writes = storage.0.borrow().writes;
        assert!(ContainerDatabase::open(storage.clone()).is_err());
        assert_eq!(storage.len().unwrap(), end + 7);
        assert_eq!(storage.0.borrow().writes, writes);
    }
}
#[test]
fn valid_active_state_repairs_only_incomplete_outer_tail() {
    let mut db = ContainerDatabase::open(MemoryStorage::new()).unwrap();
    db.put(b"x", b"v").unwrap();
    let mut storage = db.into_storage();
    let len = storage.len().unwrap();
    storage.write_all_at(len, b"partial").unwrap();
    let mut db = ContainerDatabase::open(storage).unwrap();
    assert_eq!(db.get(b"x").unwrap(), Some(b"v".to_vec()));
    assert_eq!(db.into_storage().len().unwrap(), len);
}

fn wal_payload(sequence: u64) -> Vec<u8> {
    let mut bytes = b"STOWWAL\0".to_vec();
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(&crc32fast::hash(&bytes).to_le_bytes());
    let mut frame = [0; 28];
    frame[..4].copy_from_slice(&2u32.to_le_bytes());
    frame[4..12].copy_from_slice(&sequence.to_le_bytes());
    frame[12] = 1;
    frame[16..20].copy_from_slice(&1u32.to_le_bytes());
    frame[20..24].copy_from_slice(&1u32.to_le_bytes());
    let crc = crc32fast::hash(&frame[..24]);
    frame[24..].copy_from_slice(&crc.to_le_bytes());
    let mut record = frame.to_vec();
    record.extend_from_slice(b"kv");
    let crc = crc32fast::hash(&record);
    record.extend_from_slice(&crc.to_le_bytes());
    bytes.extend_from_slice(&record);
    bytes
}
#[test]
fn complete_outer_records_never_make_partial_bad_crc_or_sequence_inner_wal_repairable() {
    let good = wal_payload(1);
    let mut bad_crc = good.clone();
    *bad_crc.last_mut().unwrap() ^= 1;
    let mut trailing = good.clone();
    trailing.push(1);
    for payload in [
        good[..good.len() - 1].to_vec(),
        bad_crc,
        trailing,
        wal_payload(2),
    ] {
        let storage = Fault::default();
        let mut journal = Journal::open(storage.clone()).unwrap();
        let reference = journal.append_segment(SegmentKind::Wal, &payload).unwrap();
        journal.commit(&[], &[reference]).unwrap();
        drop(journal);
        let writes = storage.0.borrow().writes;
        assert!(ContainerDatabase::open(storage.clone()).is_err());
        assert_eq!(storage.0.borrow().writes, writes);
    }
}
#[test]
fn dirty_budget_uses_last_operation_and_flushes_previous_state_before_growth() {
    let mut db = ContainerDatabase::open(MemoryStorage::new()).unwrap();
    let large = vec![7; 16 * 1024 * 1024 - 1];
    db.put(b"a", &large).unwrap();
    db.put(b"b", &large).unwrap();
    assert_eq!(db.stats().memtable_bytes, 32 * 1024 * 1024);
    db.write_batch(&[put(b"a", b"small"), put(b"a", &large)])
        .unwrap();
    assert_eq!(db.stats().table_count, 0);
    assert_eq!(db.stats().memtable_bytes, 32 * 1024 * 1024);
    db.put(b"c", b"v").unwrap();
    assert_eq!(db.stats().table_count, 1);
    assert_eq!(db.stats().memtable_bytes, 2);
    let mut db = ContainerDatabase::open(db.into_storage()).unwrap();
    assert_eq!(db.get(b"a").unwrap().unwrap().len(), large.len());
    assert_eq!(db.get(b"c").unwrap(), Some(b"v".to_vec()));
}
