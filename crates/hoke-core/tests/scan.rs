//! Ordered reads across immutable runs and the mutable WAL, using real files.
#![cfg(unix)]
use std::cell::Cell;
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::ops::Bound::{Excluded, Included, Unbounded};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use hoke_core::directory::Directory;
use hoke_core::storage::{FileStorage, Storage};
use hoke_core::{Database, DbError, KeyValue, NativeDirectory, ScanOptions};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        loop {
            let path = std::env::temp_dir().join(format!(
                "hoke-scan-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("create temporary directory: {e}"),
            }
        }
    }
    fn open(&self) -> Database<NativeDirectory> {
        Database::open(NativeDirectory::open(&self.0).unwrap()).unwrap()
    }
    fn tables(&self) -> Vec<PathBuf> {
        let mut files: Vec<_> = fs::read_dir(&self.0)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "sst"))
            .collect();
        files.sort();
        files
    }
    fn corrupt_value(&self, table: usize, value: &[u8]) {
        let path = &self.tables()[table];
        let mut bytes = fs::read(path).unwrap();
        let matches: Vec<_> = bytes
            .windows(value.len())
            .enumerate()
            .filter(|(_, window)| *window == value)
            .map(|(offset, _)| offset)
            .collect();
        assert_eq!(matches.len(), 1, "unique corruption target");
        bytes[matches[0]] ^= 0x80;
        fs::write(path, bytes).unwrap();
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
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
fn expected(model: &BTreeMap<Vec<u8>, Vec<u8>>, options: &ScanOptions) -> Vec<KeyValue> {
    let mut rows: Vec<_> = model
        .iter()
        .filter(|(key, _)| {
            let start = match &options.start {
                Included(v) => *key >= v,
                Excluded(v) => *key > v,
                Unbounded => true,
            };
            let end = match &options.end {
                Included(v) => *key <= v,
                Excluded(v) => *key < v,
                Unbounded => true,
            };
            start
                && end
                && options
                    .prefix
                    .as_ref()
                    .is_none_or(|prefix| key.starts_with(prefix))
        })
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    if options.reverse {
        rows.reverse();
    }
    rows.truncate(options.limit);
    rows
}

#[test]
fn newest_values_tombstones_and_reinsertions_merge_across_runs() {
    let temp = Temp::new();
    let mut db = temp.open();
    for key in [b"a", b"b", b"c", b"d", b"e"] {
        db.put(key, b"old").unwrap();
    }
    db.flush().unwrap();
    db.put(b"b", b"table two").unwrap();
    db.delete(b"a").unwrap();
    db.delete(b"c").unwrap();
    db.flush().unwrap();
    db.put(b"a", b"reinserted").unwrap();
    db.delete(b"b").unwrap();
    db.put(b"d", b"memtable").unwrap();
    db.delete(b"absent").unwrap();
    let rows = vec![
        pair(b"a", b"reinserted"),
        pair(b"d", b"memtable"),
        pair(b"e", b"old"),
    ];
    assert_eq!(db.scan(all()).unwrap(), rows);
    for limit in 0..=4 {
        for reverse in [false, true] {
            let mut want = rows.clone();
            if reverse {
                want.reverse();
            }
            want.truncate(limit);
            assert_eq!(
                db.range(ScanOptions {
                    limit,
                    reverse,
                    ..all()
                })
                .unwrap(),
                want
            );
        }
    }
    db.close().unwrap();
    let mut db = temp.open();
    assert_eq!(db.scan(all()).unwrap(), rows);
    db.flush().unwrap();
    assert_eq!(db.scan(all()).unwrap(), rows);
}

#[test]
fn all_bound_combinations_equal_bounds_and_invalid_ranges() {
    let temp = Temp::new();
    let mut db = temp.open();
    let model = BTreeMap::from([
        pair(b"a", b"1"),
        pair(b"b", b"2"),
        pair(b"c", b"3"),
        pair(b"d", b"4"),
    ]);
    for (k, v) in &model {
        db.put(k, v).unwrap();
    }
    db.flush().unwrap();
    for start in [Unbounded, Included(b"b".to_vec()), Excluded(b"b".to_vec())] {
        for end in [Unbounded, Included(b"c".to_vec()), Excluded(b"c".to_vec())] {
            for reverse in [false, true] {
                let options = ScanOptions {
                    start: start.clone(),
                    end: end.clone(),
                    reverse,
                    ..all()
                };
                assert_eq!(
                    db.range(options.clone()).unwrap(),
                    expected(&model, &options),
                    "{options:?}"
                );
            }
        }
    }
    for start in [Included(b"b".to_vec()), Excluded(b"b".to_vec())] {
        for end in [Included(b"b".to_vec()), Excluded(b"b".to_vec())] {
            let options = ScanOptions {
                start: start.clone(),
                end,
                ..all()
            };
            assert_eq!(
                db.scan(options.clone()).unwrap(),
                expected(&model, &options)
            );
        }
    }
    for limit in [0, 100] {
        for start in [Included(b"z".to_vec()), Excluded(b"z".to_vec())] {
            for end in [Included(b"a".to_vec()), Excluded(b"a".to_vec())] {
                assert!(matches!(
                    db.scan(ScanOptions {
                        start: start.clone(),
                        end,
                        limit,
                        ..all()
                    }),
                    Err(DbError::InvalidRange)
                ));
            }
        }
    }
}

#[test]
fn binary_empty_and_all_ff_prefixes_intersect_bounds() {
    let temp = Temp::new();
    let mut db = temp.open();
    let model: BTreeMap<_, _> = [
        vec![],
        vec![0],
        vec![0, 0],
        vec![0, 255],
        vec![1],
        vec![254, 255],
        vec![255],
        vec![255, 0],
        vec![255, 255],
        vec![255, 255, 0],
        vec![255, 255, 255],
    ]
    .into_iter()
    .map(|k| (k.clone(), k))
    .collect();
    for (i, (k, v)) in model.iter().enumerate() {
        db.put(k, v).unwrap();
        if i.is_multiple_of(3) {
            db.flush().unwrap();
        }
    }
    for prefix in [
        vec![],
        vec![0],
        vec![0, 255],
        vec![255],
        vec![255, 255],
        vec![255, 255, 255],
        vec![2],
    ] {
        for (start, end) in [
            (Unbounded, Unbounded),
            (Included(vec![255]), Excluded(vec![255, 255])),
            (Excluded(vec![255, 0]), Included(vec![255, 255, 0])),
            (Included(vec![0]), Excluded(vec![1])),
        ] {
            for reverse in [false, true] {
                let options = ScanOptions {
                    prefix: Some(prefix.clone()),
                    start: start.clone(),
                    end: end.clone(),
                    reverse,
                    ..all()
                };
                assert_eq!(
                    db.scan(options.clone()).unwrap(),
                    expected(&model, &options),
                    "{options:?}"
                );
            }
        }
    }
}

#[test]
fn empty_database_and_default_limit() {
    let temp = Temp::new();
    let mut db = temp.open();
    assert!(db.scan(ScanOptions::default()).unwrap().is_empty());
    for i in 0u16..130 {
        db.put(&i.to_be_bytes(), b"value").unwrap();
    }
    db.flush().unwrap();
    let rows = db.scan(ScanOptions::default()).unwrap();
    assert_eq!(rows.len(), 100);
    assert_eq!(rows.first().unwrap().0, 0u16.to_be_bytes());
    assert_eq!(rows.last().unwrap().0, 99u16.to_be_bytes());
}

#[test]
fn deterministic_model_with_flush_reopen_and_varied_options() {
    let temp = Temp::new();
    let mut db = temp.open();
    let mut model = BTreeMap::new();
    let mut random = 0x38f4_b72a_9135_600du64;
    for step in 0u32..160 {
        random = random
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        let key = vec![((random >> 32) % 5) as u8, ((random >> 40) % 7) as u8];
        if (random >> 48).is_multiple_of(3) {
            db.delete(&key).unwrap();
            model.remove(&key);
        } else {
            let value = step.to_le_bytes().to_vec();
            db.put(&key, &value).unwrap();
            model.insert(key, value);
        }
        if step.is_multiple_of(9) {
            db.flush().unwrap();
        }
        if step.is_multiple_of(17) {
            db.close().unwrap();
            db = temp.open();
        }
        if step.is_multiple_of(5) {
            for reverse in [false, true] {
                for limit in [0, 1, 3, 100] {
                    for (start, end, prefix) in [
                        (Unbounded, Unbounded, None),
                        (Included(vec![1, 2]), Excluded(vec![4, 3]), None),
                        (Excluded(vec![2]), Included(vec![4]), Some(vec![3])),
                        (Unbounded, Unbounded, Some(vec![2])),
                        (Included(vec![4]), Included(vec![4, 6]), Some(vec![0])),
                    ] {
                        let options = ScanOptions {
                            start,
                            end,
                            prefix,
                            reverse,
                            limit,
                        };
                        assert_eq!(
                            db.scan(options.clone()).unwrap(),
                            expected(&model, &options),
                            "step {step}, {options:?}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn corrupt_winning_payload_returns_error_without_falling_back() {
    let temp = Temp::new();
    let mut db = temp.open();
    db.put(b"key", b"old fallback sentinel").unwrap();
    db.flush().unwrap();
    db.put(b"key", b"winner corruption sentinel").unwrap();
    db.flush().unwrap();
    db.close().unwrap();
    temp.corrupt_value(1, b"winner corruption sentinel");
    let mut db = temp.open();
    assert!(db.scan(all()).is_err());
    assert!(
        db.scan(ScanOptions {
            reverse: true,
            ..all()
        })
        .is_err()
    );
}

#[test]
fn shadowed_corrupt_payload_is_never_read_for_new_value_or_tombstone() {
    for tombstone in [false, true] {
        for flush_new in [false, true] {
            let temp = Temp::new();
            let mut db = temp.open();
            db.put(b"key", b"shadowed corruption sentinel").unwrap();
            db.flush().unwrap();
            if tombstone {
                db.delete(b"key").unwrap();
            } else {
                db.put(b"key", b"new").unwrap();
            }
            if flush_new {
                db.flush().unwrap();
            }
            db.close().unwrap();
            temp.corrupt_value(0, b"shadowed corruption sentinel");
            let mut db = temp.open();
            let want = if tombstone {
                vec![]
            } else {
                vec![pair(b"key", b"new")]
            };
            for reverse in [false, true] {
                assert_eq!(db.scan(ScanOptions { reverse, ..all() }).unwrap(), want);
            }
        }
    }
}

#[derive(Default)]
struct Reads {
    count: Cell<usize>,
    fail: Cell<bool>,
}
struct ObservedDirectory {
    inner: NativeDirectory,
    reads: Rc<Reads>,
}
struct ObservedFile {
    inner: FileStorage,
    reads: Rc<Reads>,
}
impl Storage for ObservedFile {
    fn len(&self) -> io::Result<u64> {
        self.inner.len()
    }
    fn read_at(&mut self, offset: u64, buffer: &mut [u8]) -> io::Result<usize> {
        self.reads.count.set(self.reads.count.get() + 1);
        if self.reads.fail.get() {
            return Err(io::Error::other("injected scan read failure"));
        }
        self.inner.read_at(offset, buffer)
    }
    fn write_all_at(&mut self, offset: u64, data: &[u8]) -> io::Result<()> {
        self.inner.write_all_at(offset, data)
    }
    fn set_len(&mut self, len: u64) -> io::Result<()> {
        self.inner.set_len(len)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
    fn sync_all(&mut self) -> io::Result<()> {
        self.inner.sync_all()
    }
}
impl Directory for ObservedDirectory {
    type File = ObservedFile;
    fn open_existing(&self, name: &str) -> io::Result<Self::File> {
        Ok(ObservedFile {
            inner: self.inner.open_existing(name)?,
            reads: self.reads.clone(),
        })
    }
    fn create_new(&self, name: &str) -> io::Result<Self::File> {
        Ok(ObservedFile {
            inner: self.inner.create_new(name)?,
            reads: self.reads.clone(),
        })
    }
    fn list(&self) -> io::Result<Vec<String>> {
        self.inner.list()
    }
    fn rename(&self, from: &str, to: &str) -> io::Result<()> {
        self.inner.rename(from, to)
    }
    fn sync_directory(&self) -> io::Result<()> {
        self.inner.sync_directory()
    }
}

#[test]
fn zero_limit_and_invalid_bounds_do_no_reads_and_winner_io_error_propagates() {
    let temp = Temp::new();
    let mut db = temp.open();
    db.put(b"a", b"first").unwrap();
    db.flush().unwrap();
    db.close().unwrap();
    let reads = Rc::new(Reads::default());
    let mut db = Database::open(ObservedDirectory {
        inner: NativeDirectory::open(&temp.0).unwrap(),
        reads: reads.clone(),
    })
    .unwrap();
    reads.count.set(0);
    reads.fail.set(true);
    assert!(
        db.scan(ScanOptions { limit: 0, ..all() })
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        db.scan(ScanOptions {
            start: Included(b"z".to_vec()),
            end: Excluded(b"a".to_vec()),
            ..all()
        }),
        Err(DbError::InvalidRange)
    ));
    assert_eq!(reads.count.get(), 0);
    assert!(db.scan(all()).is_err());
    assert!(reads.count.get() > 0);
    reads.fail.set(false);
    assert_eq!(db.scan(all()).unwrap(), vec![pair(b"a", b"first")]);
}

#[test]
fn bounded_and_limited_scans_skip_corrupt_unselected_values() {
    let temp = Temp::new();
    let mut db = temp.open();
    db.put(b"a", b"first").unwrap();
    db.put(b"b", b"unselected corruption sentinel").unwrap();
    db.put(b"c", b"last").unwrap();
    db.flush().unwrap();
    db.close().unwrap();
    temp.corrupt_value(0, b"unselected corruption sentinel");
    let mut db = temp.open();
    assert_eq!(
        db.scan(ScanOptions { limit: 1, ..all() }).unwrap(),
        vec![pair(b"a", b"first")]
    );
    assert_eq!(
        db.scan(ScanOptions {
            limit: 1,
            reverse: true,
            ..all()
        })
        .unwrap(),
        vec![pair(b"c", b"last")]
    );
    assert_eq!(
        db.scan(ScanOptions {
            end: Excluded(b"b".to_vec()),
            ..all()
        })
        .unwrap(),
        vec![pair(b"a", b"first")]
    );
    assert_eq!(
        db.scan(ScanOptions {
            start: Excluded(b"b".to_vec()),
            ..all()
        })
        .unwrap(),
        vec![pair(b"c", b"last")]
    );
    assert!(db.scan(all()).is_err());
}

#[test]
fn winning_tombstone_is_validated_but_shadowed_tombstone_is_not_read() {
    let temp = Temp::new();
    let mut db = temp.open();
    db.put(b"key", b"old").unwrap();
    db.flush().unwrap();
    db.delete(b"key").unwrap();
    db.flush().unwrap();
    db.close().unwrap();
    let path = &temp.tables()[1];
    let mut bytes = fs::read(path).unwrap();
    // SST v1 has a 32-byte header; the first frame's 28-byte header is
    // followed by the key. Keep both headers and the index valid at open.
    bytes[32 + 28] ^= 0x80;
    fs::write(path, bytes).unwrap();
    let mut db = temp.open();
    assert!(db.scan(all()).is_err());
    db.put(b"key", b"new memtable value").unwrap();
    assert_eq!(
        db.scan(all()).unwrap(),
        vec![pair(b"key", b"new memtable value")]
    );
}
