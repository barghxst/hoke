//! Flush/recovery fault tests. The byte backend models visible file operations,
//! not loss of power or a filesystem's durability guarantees.
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io;
use std::rc::Rc;
use stowtree_core::directory::Directory;
use stowtree_core::storage::Storage;
use stowtree_core::{BatchOperation, Database, DatabaseOptions, ScanOptions};

type Bytes = Rc<RefCell<Vec<u8>>>;

#[derive(Default)]
struct Fault {
    operations: Vec<String>,
    fail_at: Option<usize>,
    rename_then_fail: bool,
    write_then_fail: bool,
}

#[derive(Clone, Default)]
struct SharedDir {
    files: Rc<RefCell<BTreeMap<String, Bytes>>>,
    fault: Rc<RefCell<Fault>>,
}

impl SharedDir {
    fn operation(&self, name: String) -> io::Result<()> {
        let mut fault = self.fault.borrow_mut();
        fault.operations.push(name);
        if fault.fail_at == Some(fault.operations.len()) {
            return Err(io::Error::other("injected file operation failure"));
        }
        Ok(())
    }

    fn arm(&self, fail_at: Option<usize>) {
        *self.fault.borrow_mut() = Fault {
            fail_at,
            ..Fault::default()
        };
    }
}

struct SharedFile {
    name: String,
    bytes: Bytes,
    directory: SharedDir,
}

impl SharedFile {
    fn operation(&self, kind: &str) -> io::Result<()> {
        self.directory.operation(format!("{kind} {}", self.name))
    }
}

impl Storage for SharedFile {
    fn len(&self) -> io::Result<u64> {
        self.operation("len")?;
        Ok(self.bytes.borrow().len() as u64)
    }

    fn read_at(&mut self, offset: u64, buffer: &mut [u8]) -> io::Result<usize> {
        self.operation("read")?;
        let bytes = self.bytes.borrow();
        if offset >= bytes.len() as u64 {
            return Ok(0);
        }
        let start = offset as usize;
        let count = buffer.len().min(bytes.len() - start);
        buffer[..count].copy_from_slice(&bytes[start..start + count]);
        Ok(count)
    }

    fn write_all_at(&mut self, offset: u64, data: &[u8]) -> io::Result<()> {
        let result = self.operation("write");
        let fail_after = result.is_err() && self.directory.fault.borrow().write_then_fail;
        if !fail_after {
            result?;
        }
        if data.is_empty() {
            return Ok(());
        }
        let end = offset
            .checked_add(data.len() as u64)
            .and_then(|value| usize::try_from(value).ok())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "offset overflow"))?;
        let mut bytes = self.bytes.borrow_mut();
        if bytes.len() < end {
            bytes.resize(end, 0);
        }
        bytes[offset as usize..end].copy_from_slice(data);
        if fail_after {
            return Err(io::Error::other(
                "bytes written but write result is unknown",
            ));
        }
        Ok(())
    }

    fn set_len(&mut self, len: u64) -> io::Result<()> {
        self.operation("truncate")?;
        let len = usize::try_from(len)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "length overflow"))?;
        self.bytes.borrow_mut().resize(len, 0);
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.operation("flush")
    }

    fn sync_all(&mut self) -> io::Result<()> {
        self.operation("sync")
    }
}

impl Directory for SharedDir {
    type File = SharedFile;

    fn open_existing(&self, name: &str) -> io::Result<Self::File> {
        self.operation(format!("open {name}"))?;
        let bytes = self
            .files
            .borrow()
            .get(name)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "missing file"))?;
        Ok(SharedFile {
            name: name.into(),
            bytes,
            directory: self.clone(),
        })
    }

    fn create_new(&self, name: &str) -> io::Result<Self::File> {
        self.operation(format!("create {name}"))?;
        let mut files = self.files.borrow_mut();
        if files.contains_key(name) {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, "file exists"));
        }
        let bytes = Rc::new(RefCell::new(Vec::new()));
        files.insert(name.into(), bytes.clone());
        Ok(SharedFile {
            name: name.into(),
            bytes,
            directory: self.clone(),
        })
    }

    fn list(&self) -> io::Result<Vec<String>> {
        self.operation("list".into())?;
        Ok(self.files.borrow().keys().cloned().collect())
    }

    fn rename(&self, from: &str, to: &str) -> io::Result<()> {
        self.operation(format!("rename {from} -> {to}"))?;
        let mut files = self.files.borrow_mut();
        let bytes = files
            .remove(from)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "missing rename source"))?;
        files.insert(to.into(), bytes);
        if self.fault.borrow().rename_then_fail {
            return Err(io::Error::other("rename succeeded but result is unknown"));
        }
        Ok(())
    }

    fn remove_file(&self, name: &str) -> io::Result<()> {
        self.operation(format!("remove {name}"))?;
        self.files
            .borrow_mut()
            .remove(name)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "missing file"))?;
        Ok(())
    }

    fn sync_directory(&self) -> io::Result<()> {
        self.operation("sync_directory".into())
    }
}

fn options(wal: u64) -> DatabaseOptions {
    DatabaseOptions {
        max_memtable_bytes: None,
        max_wal_bytes: Some(wal),
    }
}

fn open(directory: &SharedDir, wal: u64) -> Database<SharedDir> {
    Database::open_with_options(directory.clone(), options(wal)).unwrap()
}

#[test]
fn equality_then_repeated_overwrite_flushes_constant_payload() {
    let directory = SharedDir::default();
    let mut db = open(&directory, 90);
    db.put(b"a", b"1111").unwrap();
    db.put(b"a", b"2222").unwrap();
    assert_eq!(db.stats().wal_bytes, 90);
    assert_eq!(db.stats().memtable_bytes, 5);
    assert_eq!(db.stats().table_count, 0);
    for i in 0..8 {
        db.put(b"a", &[i; 4]).unwrap();
        assert_eq!(db.stats().wal_bytes, if i % 2 == 0 { 53 } else { 90 });
        assert_eq!(db.stats().table_count, 1 + i as usize / 2);
        assert_eq!(db.stats().memtable_bytes, 5);
        assert_eq!(db.get(b"a").unwrap(), Some(vec![i; 4]));
    }
}

#[test]
fn minimum_threshold_counts_empty_put_and_repeated_tombstones() {
    let directory = SharedDir::default();
    let mut db = open(&directory, 16);
    db.put(b"", b"").unwrap();
    assert_eq!(db.stats().wal_bytes, 48);
    assert_eq!(db.stats().table_count, 0);
    assert_eq!(db.stats().memtable_bytes, 0);
    db.delete(b"").unwrap();
    assert_eq!(db.stats().table_count, 1);
    assert_eq!(db.stats().wal_bytes, 48);
    assert_eq!(db.stats().memtable_bytes, 0);
    db.delete(b"").unwrap();
    assert_eq!(db.stats().table_count, 2);
    assert_eq!(db.get(b"").unwrap(), None);
    db.put(b"", b"large").unwrap();
    assert_eq!(db.stats().table_count, 3);
    assert_eq!(db.stats().wal_bytes, 53);
    db.flush().unwrap();
    assert_eq!(db.stats().wal_bytes, 16);
}

#[test]
fn delete_equality_and_next_delete_are_measured_as_records() {
    let directory = SharedDir::default();
    let mut db = open(&directory, 82);
    db.delete(b"a").unwrap();
    db.delete(b"a").unwrap();
    assert_eq!(db.stats().wal_bytes, 82);
    assert_eq!(db.stats().table_count, 0);
    db.delete(b"a").unwrap();
    assert_eq!(db.stats().wal_bytes, 49);
    assert_eq!(db.stats().table_count, 1);
    assert_eq!(db.stats().memtable_bytes, 1);
}

#[test]
fn both_thresholds_trigger_only_one_flush() {
    let directory = SharedDir::default();
    let mut db = Database::open_with_options(
        directory.clone(),
        DatabaseOptions {
            max_memtable_bytes: Some(5),
            max_wal_bytes: Some(53),
        },
    )
    .unwrap();
    db.put(b"a", b"1111").unwrap();
    directory.arm(None);
    db.put(b"b", b"2222").unwrap();
    assert_eq!(db.stats().table_count, 1);
    assert_eq!(db.stats().wal_bytes, 53);
    assert_eq!(
        directory
            .fault
            .borrow()
            .operations
            .iter()
            .filter(|op| op.starts_with("rename "))
            .count(),
        1
    );
    assert_eq!(db.get(b"a").unwrap(), Some(b"1111".to_vec()));
    assert_eq!(db.get(b"b").unwrap(), Some(b"2222".to_vec()));
}

#[test]
fn recovered_length_is_used_but_open_read_close_and_empty_batch_do_not_flush() {
    let directory = SharedDir::default();
    let mut db = Database::open(directory.clone()).unwrap();
    for _ in 0..6 {
        db.put(b"a", b"1111").unwrap();
    }
    db.close().unwrap();
    let mut db = open(&directory, 16);
    assert_eq!(db.stats().wal_bytes, 238);
    assert_eq!(db.stats().table_count, 0);
    db.get(b"a").unwrap();
    db.scan(ScanOptions::default()).unwrap();
    directory.arm(None);
    db.write_batch(&[]).unwrap();
    assert!(directory.fault.borrow().operations.is_empty());
    db.close().unwrap();
    let mut db = open(&directory, 16);
    assert_eq!(db.stats().table_count, 0);
    db.put(b"a", b"2222").unwrap();
    assert_eq!(db.stats().table_count, 1);
    assert_eq!(db.stats().wal_bytes, 53);
}

#[test]
fn default_and_maximum_threshold_leave_overwrite_wal_unflushed() {
    assert_eq!(DatabaseOptions::default().max_wal_bytes, None);
    for option in [DatabaseOptions::default(), options(u64::MAX)] {
        let directory = SharedDir::default();
        let mut db = Database::open_with_options(directory, option).unwrap();
        for _ in 0..9 {
            db.put(b"a", b"1111").unwrap();
        }
        assert_eq!(db.stats().table_count, 0);
        assert_eq!(db.stats().wal_bytes, 349);
        assert_eq!(db.stats().memtable_bytes, 5);
    }
}

#[test]
fn invalid_options_and_inputs_do_not_touch_storage_or_poison_instance() {
    for limit in [0, 1, 15] {
        let directory = SharedDir::default();
        assert!(options(limit).validate().is_err());
        assert!(Database::open_with_options(directory.clone(), options(limit)).is_err());
        assert!(directory.fault.borrow().operations.is_empty());
        assert!(directory.files.borrow().is_empty());
    }
    let directory = SharedDir::default();
    let mut db = open(&directory, 16);
    db.put(b"a", b"ack").unwrap();
    directory.arm(None);
    assert!(db.put(&vec![0; 65537], b"x").is_err());
    assert!(db.delete(&vec![0; 65537]).is_err());
    assert!(db.put(b"x", &vec![0; 16 * 1024 * 1024 + 1]).is_err());
    assert!(directory.fault.borrow().operations.is_empty());
    db.put(b"a", b"next").unwrap();
    assert_eq!(db.get(b"a").unwrap(), Some(b"next".to_vec()));
}

#[test]
fn batch_is_unsplit_and_starts_empty_wal() {
    let directory = SharedDir::default();
    let mut db = open(&directory, 16);
    db.put(b"old", b"ack").unwrap();
    db.write_batch(&[
        BatchOperation::Put {
            key: b"a".to_vec(),
            value: vec![1; 200],
        },
        BatchOperation::Put {
            key: b"b".to_vec(),
            value: vec![2; 200],
        },
        BatchOperation::Delete {
            key: b"old".to_vec(),
        },
    ])
    .unwrap();
    assert_eq!(db.stats().table_count, 1);
    assert_eq!(db.stats().wal_bytes, 16);
    assert_eq!(db.stats().memtable_entries, 0);
    assert_eq!(db.get(b"a").unwrap(), Some(vec![1; 200]));
    assert_eq!(db.get(b"b").unwrap(), Some(vec![2; 200]));
    assert_eq!(db.get(b"old").unwrap(), None);
    db.close().unwrap();
    let mut db = open(&directory, 16);
    assert_eq!(db.get(b"a").unwrap(), Some(vec![1; 200]));
}

fn seed(directory: &SharedDir) -> Database<SharedDir> {
    let mut db = open(directory, 90);
    db.put(b"a", b"1111").unwrap();
    db.put(b"a", b"2222").unwrap();
    db
}

#[test]
fn every_preflush_fault_preserves_latest_ack_and_excludes_requested_overwrite() {
    let probe = SharedDir::default();
    let mut db = seed(&probe);
    probe.arm(None);
    db.put(b"a", b"3333").unwrap();
    let operations = probe.fault.borrow().operations.clone();
    let final_sync = operations
        .iter()
        .rposition(|op| op == "sync_directory")
        .unwrap()
        + 1;
    for failure in 1..=final_sync {
        let directory = SharedDir::default();
        let mut db = seed(&directory);
        directory.arm(Some(failure));
        assert!(
            db.put(b"a", b"3333").is_err(),
            "operation {failure}: {}",
            operations[failure - 1]
        );
        directory.arm(None);
        assert_eq!(db.get(b"a").unwrap(), Some(b"2222".to_vec()));
        assert!(db.put(b"a", b"forbidden").is_err());
        drop(db);
        let mut recovered = open(&directory, 90);
        assert_eq!(recovered.get(b"a").unwrap(), Some(b"2222".to_vec()));
        recovered.put(b"a", b"4444").unwrap();
    }
}

#[test]
fn unknown_manifest_rename_does_not_append_new_overwrite() {
    let directory = SharedDir::default();
    let mut db = seed(&directory);
    directory.arm(None);
    directory.fault.borrow_mut().rename_then_fail = true;
    assert!(db.put(b"a", b"3333").is_err());
    directory.arm(None);
    assert_eq!(db.get(b"a").unwrap(), Some(b"2222".to_vec()));
    assert!(db.delete(b"a").is_err());
    drop(db);
    let mut recovered = open(&directory, 90);
    assert_eq!(recovered.get(b"a").unwrap(), Some(b"2222".to_vec()));
}

#[test]
fn append_failure_after_preflush_retains_unknown_outcome_contract() {
    let probe = SharedDir::default();
    let mut db = seed(&probe);
    probe.arm(None);
    db.put(b"a", b"3333").unwrap();
    let operations = probe.fault.borrow().operations.clone();
    let append_write = operations
        .iter()
        .rposition(|op| op.starts_with("write "))
        .unwrap()
        + 1;
    for write_then_fail in [false, true] {
        let directory = SharedDir::default();
        let mut db = seed(&directory);
        directory.arm(Some(append_write));
        directory.fault.borrow_mut().write_then_fail = write_then_fail;
        assert!(db.put(b"a", b"3333").is_err());
        directory.arm(None);
        assert_eq!(db.stats().table_count, 1);
        assert_eq!(db.get(b"a").unwrap(), Some(b"2222".to_vec()));
        assert!(db.put(b"a", b"forbidden").is_err());
        drop(db);
        let mut recovered = open(&directory, 90);
        assert_eq!(
            recovered.get(b"a").unwrap(),
            Some(if write_then_fail {
                b"3333".to_vec()
            } else {
                b"2222".to_vec()
            })
        );
    }
}

#[test]
fn ordered_model_survives_both_thresholds_batch_and_maintenance() {
    let directory = SharedDir::default();
    let make_options = |i: u16| DatabaseOptions {
        max_memtable_bytes: Some(if i.is_multiple_of(2) { 19 } else { 45 }),
        max_wal_bytes: Some(if i.is_multiple_of(3) { 90 } else { 160 }),
    };
    let mut db = Database::open_with_options(directory.clone(), make_options(0)).unwrap();
    let mut model = BTreeMap::new();
    for i in 0_u16..140 {
        let key = vec![(i.wrapping_mul(17) % 23) as u8, 0, 255];
        if i % 4 == 0 {
            db.delete(&key).unwrap();
            model.remove(&key);
        } else {
            let value = vec![(i % 255) as u8; usize::from(i % 13)];
            db.put(&key, &value).unwrap();
            model.insert(key, value);
        }
        if i % 13 == 0 {
            let key = vec![254, (i % 7) as u8];
            let value = i.to_le_bytes().to_vec();
            db.write_batch(&[BatchOperation::Put {
                key: key.clone(),
                value: value.clone(),
            }])
            .unwrap();
            model.insert(key, value);
            assert_eq!(db.stats().wal_bytes, 16);
        }
        if i % 17 == 0 {
            db.flush().unwrap();
        }
        if i % 31 == 0 {
            db.compact().unwrap();
        }
        if i % 23 == 0 {
            assert!(db.collect_garbage().unwrap().errors.is_empty());
        }
        if i % 19 == 0 {
            db.close().unwrap();
            db = Database::open_with_options(directory.clone(), make_options(i)).unwrap();
        }
        let expected: Vec<_> = model
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        assert_eq!(
            db.scan(ScanOptions {
                limit: usize::MAX,
                ..ScanOptions::default()
            })
            .unwrap(),
            expected
        );
        for (key, value) in &model {
            assert_eq!(db.get(key).unwrap().as_ref(), Some(value));
        }
    }
}
