use std::{cell::Cell, collections::BTreeMap, io, ops::Bound, rc::Rc};

use hoke_core::{
    DbError, Engine, KeyValue, ScanOptions,
    read::{get_from_sources, scan_sources_with_budget},
    sstable::{SSTable, write_table},
    storage::{MemoryStorage, Storage},
};

#[derive(Default)]
struct Access {
    reads: Cell<usize>,
    writes: Cell<usize>,
    fail_reads: Cell<bool>,
}
struct Observed {
    inner: MemoryStorage,
    access: Rc<Access>,
}
impl Observed {
    fn new(inner: MemoryStorage) -> Self {
        Self {
            inner,
            access: Rc::default(),
        }
    }
}
impl Storage for Observed {
    fn len(&self) -> io::Result<u64> {
        self.access.reads.set(self.access.reads.get() + 1);
        if self.access.fail_reads.get() {
            return Err(io::Error::other("selected read failed"));
        }
        self.inner.len()
    }
    fn read_at(&mut self, offset: u64, bytes: &mut [u8]) -> io::Result<usize> {
        self.access.reads.set(self.access.reads.get() + 1);
        if self.access.fail_reads.get() {
            return Err(io::Error::other("selected read failed"));
        }
        let count = bytes.len().min(5);
        self.inner.read_at(offset, &mut bytes[..count])
    }
    fn write_all_at(&mut self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        self.access.writes.set(self.access.writes.get() + 1);
        self.inner.write_all_at(offset, bytes)
    }
    fn set_len(&mut self, len: u64) -> io::Result<()> {
        self.access.writes.set(self.access.writes.get() + 1);
        self.inner.set_len(len)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.access.writes.set(self.access.writes.get() + 1);
        self.inner.flush()
    }
    fn sync_all(&mut self) -> io::Result<()> {
        self.access.writes.set(self.access.writes.get() + 1);
        self.inner.sync_all()
    }
}
fn wal() -> (Engine<Observed>, Rc<Access>) {
    let storage = Observed::new(MemoryStorage::new());
    let access = storage.access.clone();
    let engine = Engine::open(storage).unwrap();
    (engine, access)
}
fn table(rows: &[(&[u8], Option<&[u8]>)]) -> (SSTable<Observed>, Rc<Access>) {
    let entries: BTreeMap<_, _> = rows
        .iter()
        .map(|(key, value)| (key.to_vec(), value.map(<[u8]>::to_vec)))
        .collect();
    let mut memory = MemoryStorage::new();
    write_table(&mut memory, &entries).unwrap();
    let storage = Observed::new(memory);
    let access = storage.access.clone();
    let table = SSTable::open(storage).unwrap();
    access.reads.set(0);
    (table, access)
}
fn pair(key: &[u8], value: &[u8]) -> KeyValue {
    (key.to_vec(), value.to_vec())
}
fn all() -> ScanOptions {
    ScanOptions {
        limit: usize::MAX,
        ..ScanOptions::default()
    }
}

#[test]
fn wal_values_and_tombstones_hide_unreadable_older_tables_without_writes() {
    let (mut wal, wal_access) = wal();
    let (old, old_access) = table(&[(b"a", Some(b"old")), (b"b", Some(b"old"))]);
    wal.put(b"a", b"new").unwrap();
    wal.delete(b"b").unwrap();
    wal_access.reads.set(0);
    wal_access.writes.set(0);
    wal_access.fail_reads.set(true);
    old_access.fail_reads.set(true);
    let mut tables = [old];
    assert_eq!(
        get_from_sources(&wal, &mut tables, b"a").unwrap(),
        Some(b"new".to_vec())
    );
    assert_eq!(get_from_sources(&wal, &mut tables, b"b").unwrap(), None);
    assert_eq!(
        scan_sources_with_budget(&wal, &mut tables, all(), 4).unwrap(),
        vec![pair(b"a", b"new")]
    );
    for access in [wal_access, old_access] {
        assert_eq!(access.reads.get(), 0);
        assert_eq!(access.writes.get(), 0);
    }
}

#[test]
fn newest_table_tombstones_values_and_missing_keys_resolve_in_order() {
    let (wal, _) = wal();
    let (old, old_access) = table(&[(b"a", Some(b"old")), (b"b", Some(b"old"))]);
    let (new, _) = table(&[(b"a", Some(b"new")), (b"b", None)]);
    old_access.fail_reads.set(true);
    let mut tables = [old, new];
    assert_eq!(
        get_from_sources(&wal, &mut tables, b"a").unwrap(),
        Some(b"new".to_vec())
    );
    assert_eq!(get_from_sources(&wal, &mut tables, b"b").unwrap(), None);
    assert_eq!(
        get_from_sources(&wal, &mut tables, b"absent").unwrap(),
        None
    );
    assert_eq!(old_access.reads.get(), 0);
    assert_eq!(
        scan_sources_with_budget(&wal, &mut tables, all(), 4).unwrap(),
        vec![pair(b"a", b"new")]
    );
}

#[test]
fn reverse_prefix_bounds_and_live_limit_share_version_resolution() {
    let (mut wal, _) = wal();
    let (old, _) = table(&[
        (b"p:a", Some(b"old")),
        (b"p:b", Some(b"old")),
        (b"p:c", Some(b"old")),
        (b"q", Some(b"other")),
    ]);
    let (new, _) = table(&[(b"p:a", Some(b"new")), (b"p:b", None)]);
    wal.delete(b"p:c").unwrap();
    wal.put(b"p:d", b"latest").unwrap();
    let mut tables = [old, new];
    let options = ScanOptions {
        prefix: Some(b"p:".to_vec()),
        reverse: true,
        limit: 2,
        ..all()
    };
    assert_eq!(
        scan_sources_with_budget(&wal, &mut tables, options.clone(), usize::MAX).unwrap(),
        vec![pair(b"p:d", b"latest"), pair(b"p:a", b"new")]
    );
    let options = ScanOptions {
        start: Bound::Excluded(b"p:a".to_vec()),
        end: Bound::Included(b"p:d".to_vec()),
        ..options
    };
    assert_eq!(
        scan_sources_with_budget(&wal, &mut tables, options, usize::MAX).unwrap(),
        vec![pair(b"p:d", b"latest")]
    );
}

#[test]
fn invalid_and_empty_ranges_do_not_read_or_write_storage() {
    let (wal, wal_access) = wal();
    let (sst, access) = table(&[(b"a", Some(b"value"))]);
    wal_access.reads.set(0);
    wal_access.writes.set(0);
    wal_access.fail_reads.set(true);
    access.fail_reads.set(true);
    let mut tables = [sst];
    let invalid = ScanOptions {
        start: Bound::Included(b"z".to_vec()),
        end: Bound::Included(b"a".to_vec()),
        limit: 0,
        ..all()
    };
    assert!(matches!(
        scan_sources_with_budget(&wal, &mut tables, invalid, 0),
        Err(DbError::InvalidRange)
    ));
    for options in [
        ScanOptions { limit: 0, ..all() },
        ScanOptions {
            start: Bound::Included(b"a".to_vec()),
            end: Bound::Excluded(b"a".to_vec()),
            ..all()
        },
        ScanOptions {
            prefix: Some(b"z".to_vec()),
            end: Bound::Included(b"a".to_vec()),
            ..all()
        },
    ] {
        assert!(
            scan_sources_with_budget(&wal, &mut tables, options, 0)
                .unwrap()
                .is_empty()
        );
    }
    for access in [wal_access, access] {
        assert_eq!(access.reads.get(), 0);
        assert_eq!(access.writes.get(), 0);
    }
}

#[test]
fn payload_budget_is_exact_counts_live_rows_and_never_returns_partial_results() {
    let (mut wal, _) = wal();
    let (sst, access) = table(&[(b"a", Some(b"12")), (b"b", None), (b"c", Some(b"345"))]);
    wal.delete(b"b").unwrap();
    let mut tables = [sst];
    assert_eq!(
        scan_sources_with_budget(&wal, &mut tables, all(), 7).unwrap(),
        vec![pair(b"a", b"12"), pair(b"c", b"345")]
    );
    assert!(matches!(
        scan_sources_with_budget(&wal, &mut tables, all(), 6),
        Err(DbError::Limit(_))
    ));
    assert_eq!(
        scan_sources_with_budget(&wal, &mut tables, ScanOptions { limit: 1, ..all() }, 3).unwrap(),
        vec![pair(b"a", b"12")]
    );
    assert!(matches!(
        scan_sources_with_budget(&wal, &mut tables, all(), 0),
        Err(DbError::Limit(_))
    ));
    assert_eq!(access.writes.get(), 0);
}

#[test]
fn selected_read_errors_never_fall_back_to_older_versions_or_partial_rows() {
    let (mut wal, _) = wal();

    wal.put(b"a", b"prefix").unwrap();
    let (old, old_access) = table(&[(b"b", Some(b"old"))]);
    let (new, access) = table(&[(b"b", Some(b"winner"))]);
    access.fail_reads.set(true);
    let mut tables = [old, new];
    assert!(matches!(
        get_from_sources(&wal, &mut tables, b"b"),
        Err(DbError::Sst(_))
    ));
    assert!(matches!(
        scan_sources_with_budget(&wal, &mut tables, all(), usize::MAX),
        Err(DbError::Sst(_))
    ));
    assert_eq!(old_access.reads.get(), 0);
    assert_eq!(old_access.writes.get(), 0);
    assert_eq!(access.writes.get(), 0);
}

#[test]
fn binary_ff_prefix_intersects_bounds_and_reverses_unique_keys() {
    let (mut wal, _) = wal();
    let (sst, _) = table(&[
        (b"\xfe", Some(b"outside")),
        (b"\xff", Some(b"root")),
        (b"\xff\x00", Some(b"old")),
        (b"\xff\xff", Some(b"tail")),
    ]);
    wal.put(b"\xff\x00", b"new").unwrap();
    let mut tables = [sst];
    let options = ScanOptions {
        prefix: Some(vec![255]),
        start: Bound::Excluded(vec![255]),
        reverse: true,
        ..all()
    };
    assert_eq!(
        scan_sources_with_budget(&wal, &mut tables, options, usize::MAX).unwrap(),
        vec![pair(b"\xff\xff", b"tail"), pair(b"\xff\x00", b"new")]
    );
}
