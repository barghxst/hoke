//! Flush/recovery fault tests. The byte backend models visible file operations,
//! not loss of power or a filesystem's durability guarantees.
use hoke_core::directory::Directory;
use hoke_core::storage::Storage;
use hoke_core::{BatchOperation, Database, DatabaseOptions, ScanOptions};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io;
use std::rc::Rc;

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

fn options(limit: usize) -> DatabaseOptions {
    DatabaseOptions {
        max_memtable_bytes: Some(limit),
        ..Default::default()
    }
}

fn open(directory: &SharedDir, limit: usize) -> Database<SharedDir> {
    Database::open_with_options(directory.clone(), options(limit)).unwrap()
}

#[test]
fn equality_stays_in_wal_and_next_write_preflushes() {
    let directory = SharedDir::default();
    let mut db = open(&directory, 6);
    db.put(b"a", b"12").unwrap();
    db.put(b"b", b"34").unwrap();
    assert_eq!(db.stats().memtable_bytes, 6);
    assert_eq!(db.stats().table_count, 0);
    db.put(b"c", b"5").unwrap();
    assert_eq!(db.stats().table_count, 1);
    assert_eq!(db.stats().memtable_bytes, 2);
    for (key, value) in [(b"a", b"12".as_slice()), (b"b", b"34"), (b"c", b"5")] {
        assert_eq!(db.get(key).unwrap().as_deref(), Some(value));
    }
}

#[test]
fn accounting_tracks_overwrites_deletes_and_binary_empty_keys() {
    let directory = SharedDir::default();
    let mut db = open(&directory, 20);
    db.put(b"x", &[1; 30]).unwrap();
    assert_eq!(db.stats().memtable_bytes, 31);
    assert_eq!(db.stats().table_count, 0);
    db.put(b"x", b"12").unwrap();
    assert_eq!(db.stats().memtable_bytes, 3);
    assert_eq!(db.stats().table_count, 0);
    db.delete(b"x").unwrap();
    assert_eq!(db.stats().memtable_bytes, 1);
    db.delete(b"x").unwrap();
    assert_eq!(db.stats().memtable_bytes, 1);
    db.put(b"", b"123").unwrap();
    db.put(&[0, 255], &[0]).unwrap();
    assert_eq!(db.stats().memtable_bytes, 7);
    db.delete(b"").unwrap();
    assert_eq!(db.stats().memtable_bytes, 4);
    db.flush().unwrap();
    assert_eq!(db.stats().memtable_bytes, 0);
    db.delete(b"never-present").unwrap();
    assert_eq!(db.stats().memtable_bytes, 13);
}

#[test]
fn nonempty_zero_byte_tombstone_still_preflushes() {
    let directory = SharedDir::default();
    let mut db = open(&directory, 1);
    db.delete(b"").unwrap();
    assert_eq!(db.stats().memtable_bytes, 0);
    db.put(b"a", b"b").unwrap();
    assert_eq!(db.stats().table_count, 1);
    assert_eq!(db.stats().memtable_bytes, 2);
}

#[test]
fn reopen_counts_recovered_delta_without_flushing_until_next_mutation() {
    let directory = SharedDir::default();
    let mut db = Database::open(directory.clone()).unwrap();
    db.put(b"long", b"value").unwrap();
    db.put(b"x", b"old").unwrap();
    db.delete(b"x").unwrap();
    db.delete(b"").unwrap();
    db.close().unwrap();
    let mut db = open(&directory, 2);
    assert_eq!(db.stats().memtable_bytes, 10);
    assert_eq!(db.stats().table_count, 0);
    assert_eq!(db.get(b"long").unwrap(), Some(b"value".to_vec()));
    assert_eq!(db.scan(ScanOptions::default()).unwrap().len(), 1);
    assert_eq!(db.stats().table_count, 0);
    db.put(b"n", b"v").unwrap();
    assert_eq!(db.stats().table_count, 1);
    assert_eq!(db.stats().memtable_bytes, 2);
}

#[test]
fn default_is_manual_and_invalid_options_touch_no_files() {
    let directory = SharedDir::default();
    assert!(options(0).validate().is_err());
    assert!(Database::open_with_options(directory.clone(), options(0)).is_err());
    assert!(directory.fault.borrow().operations.is_empty());
    assert!(directory.files.borrow().is_empty());
    assert_eq!(DatabaseOptions::default().max_memtable_bytes, None);
    let mut db = Database::open(directory).unwrap();
    for _ in 0..5 {
        db.put(b"key", &[1; 100]).unwrap();
    }
    assert_eq!(db.stats().table_count, 0);
    assert_eq!(db.stats().memtable_bytes, 103);
}

#[test]
fn invalid_input_is_rejected_before_preflush_without_poison() {
    let directory = SharedDir::default();
    let mut db = open(&directory, 1);
    db.put(b"old", b"ack").unwrap();
    directory.arm(None);
    assert!(db.put(&vec![0; 65537], b"v").is_err());
    assert!(db.delete(&vec![0; 65537]).is_err());
    assert!(db.put(b"x", &vec![0; 16 * 1024 * 1024 + 1]).is_err());
    assert!(directory.fault.borrow().operations.is_empty());
    assert_eq!(db.stats().memtable_bytes, 6);
    db.put(b"new", b"ok").unwrap();
    assert_eq!(db.get(b"old").unwrap(), Some(b"ack".to_vec()));
}

#[test]
fn empty_batch_does_not_flush_and_large_batch_is_not_split() {
    let directory = SharedDir::default();
    let mut db = open(&directory, 1);
    db.put(b"old", b"ack").unwrap();
    directory.arm(None);
    db.write_batch(&[]).unwrap();
    assert!(directory.fault.borrow().operations.is_empty());
    assert_eq!(db.stats().memtable_bytes, 6);
    db.write_batch(&[
        BatchOperation::Put {
            key: b"a".to_vec(),
            value: vec![1; 20],
        },
        BatchOperation::Put {
            key: b"b".to_vec(),
            value: vec![2; 20],
        },
        BatchOperation::Delete {
            key: b"old".to_vec(),
        },
    ])
    .unwrap();
    assert_eq!(db.stats().table_count, 1);
    assert_eq!(db.stats().memtable_bytes, 0);
    assert_eq!(db.get(b"a").unwrap(), Some(vec![1; 20]));
    assert_eq!(db.get(b"b").unwrap(), Some(vec![2; 20]));
    assert_eq!(db.get(b"old").unwrap(), None);
}

fn preflush_seed(directory: &SharedDir) -> Database<SharedDir> {
    let mut db = open(directory, 8);
    db.put(b"old", b"ack").unwrap();
    db.delete(b"x").unwrap();
    db
}

#[test]
fn every_preflush_fault_preserves_acks_and_excludes_requested_operation() {
    let probe = SharedDir::default();
    let mut db = preflush_seed(&probe);
    probe.arm(None);
    db.put(b"new", b"value").unwrap();
    let operations = probe.fault.borrow().operations.clone();
    let publication_sync = operations
        .iter()
        .rposition(|op| op == "sync_directory")
        .unwrap();
    for failure in 1..=publication_sync + 1 {
        let directory = SharedDir::default();
        let mut db = preflush_seed(&directory);
        directory.arm(Some(failure));
        assert!(
            db.put(b"new", b"value").is_err(),
            "operation {failure}: {}",
            operations[failure - 1]
        );
        directory.arm(None);
        assert_eq!(db.get(b"old").unwrap(), Some(b"ack".to_vec()));
        assert_eq!(db.get(b"new").unwrap(), None);
        assert!(db.put(b"forbidden", b"v").is_err());
        drop(db);
        let mut recovered = open(&directory, 8);
        assert_eq!(recovered.get(b"old").unwrap(), Some(b"ack".to_vec()));
        assert_eq!(recovered.get(b"x").unwrap(), None);
        assert_eq!(recovered.get(b"new").unwrap(), None);
        recovered.put(b"after", b"recovery").unwrap();
    }
}

#[test]
fn rename_unknown_outcome_still_never_appends_requested_operation() {
    let directory = SharedDir::default();
    let mut db = preflush_seed(&directory);
    directory.arm(None);
    directory.fault.borrow_mut().rename_then_fail = true;
    assert!(db.delete(b"old").is_ok()); // Shrinks below the threshold, so no flush.
    db.put(b"old", b"ack").unwrap();
    assert!(db.put(b"new", b"value").is_err());
    directory.arm(None);
    drop(db);
    let mut recovered = open(&directory, 8);
    assert_eq!(recovered.get(b"old").unwrap(), Some(b"ack".to_vec()));
    assert_eq!(recovered.get(b"new").unwrap(), None);
}

#[test]
fn ordered_model_survives_threshold_changes_and_maintenance() {
    let directory = SharedDir::default();
    let mut db = open(&directory, 19);
    let mut model = BTreeMap::new();
    for i in 0_u16..180 {
        let key = vec![(i.wrapping_mul(17) % 29) as u8, 0, 255];
        if i % 4 == 0 {
            db.delete(&key).unwrap();
            model.remove(&key);
        } else {
            let value = vec![(i % 255) as u8; usize::from(i % 13)];
            db.put(&key, &value).unwrap();
            model.insert(key, value);
        }
        if i % 17 == 0 {
            db.flush().unwrap();
        }
        if i % 31 == 0 {
            db.compact().unwrap();
        }
        if i % 23 == 0 {
            db.collect_garbage().unwrap();
        }
        if i % 19 == 0 {
            db.close().unwrap();
            db = open(&directory, if i % 2 == 0 { 7 } else { 45 });
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

#[test]
fn append_failure_after_preflush_preserves_published_data_and_unknown_outcome() {
    let probe = SharedDir::default();
    let mut db = preflush_seed(&probe);
    probe.arm(None);
    db.put(b"new", b"value").unwrap();
    let operations = probe.fault.borrow().operations.clone();
    let append_write = operations
        .iter()
        .rposition(|op| op.starts_with("write "))
        .unwrap()
        + 1;
    assert!(
        append_write
            > operations
                .iter()
                .rposition(|op| op == "sync_directory")
                .unwrap()
                + 1
    );
    for after_write in [false, true] {
        let directory = SharedDir::default();
        let mut db = preflush_seed(&directory);
        directory.arm(Some(append_write));
        directory.fault.borrow_mut().write_then_fail = after_write;
        assert!(db.put(b"new", b"value").is_err());
        directory.arm(None);
        assert_eq!(db.stats().table_count, 1);
        assert_eq!(db.stats().memtable_bytes, 0);
        assert_eq!(db.get(b"old").unwrap(), Some(b"ack".to_vec()));
        assert_eq!(db.get(b"new").unwrap(), None);
        assert!(db.delete(b"old").is_err());
        drop(db);
        let mut recovered = open(&directory, 8);
        assert_eq!(recovered.get(b"old").unwrap(), Some(b"ack".to_vec()));
        assert_eq!(
            recovered.get(b"new").unwrap(),
            if after_write {
                Some(b"value".to_vec())
            } else {
                None
            }
        );
    }
}
