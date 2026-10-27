//! Batch publication faults model visible IO, not power-loss durability.
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io;
use std::rc::Rc;
use hoke_core::directory::Directory;
use hoke_core::storage::Storage;
use hoke_core::{
    BatchOperation, Database, MAX_BATCH_BYTES, MAX_BATCH_OPERATIONS, ScanOptions, validate_batch,
};

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

    fn names(&self, suffix: &str) -> Vec<String> {
        self.files
            .borrow()
            .keys()
            .filter(|name| name.ends_with(suffix))
            .cloned()
            .collect()
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
            .map(|_| ())
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "missing file"))
    }

    fn sync_directory(&self) -> io::Result<()> {
        self.operation("sync_directory".into())
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

fn seed(directory: &SharedDir) -> Database<SharedDir> {
    let mut db = Database::open(directory.clone()).unwrap();
    db.put(b"stable", b"retained").unwrap();
    db.put(b"overwrite", b"table value").unwrap();
    db.put(b"delete", b"table value").unwrap();
    db.flush().unwrap();
    db.put(b"confirmed", b"WAL value").unwrap();
    db.put(b"overwrite", b"WAL override").unwrap();
    db.delete(b"delete").unwrap();
    db
}

fn operations() -> Vec<BatchOperation> {
    vec![
        put(b"overwrite", b"batch override"),
        delete(b"confirmed"),
        put(b"created", b"first"),
        delete(b"created"),
        put(b"created", b"last"),
        put(b"", b""),
        put(&[255, 0, 128], &[0, 255]),
        put(b"delete", b"temporary"),
        delete(b"delete"),
        delete(b"missing"),
    ]
}

fn before() -> BTreeMap<Vec<u8>, Vec<u8>> {
    BTreeMap::from([
        (b"stable".to_vec(), b"retained".to_vec()),
        (b"confirmed".to_vec(), b"WAL value".to_vec()),
        (b"overwrite".to_vec(), b"WAL override".to_vec()),
    ])
}

fn apply(model: &mut BTreeMap<Vec<u8>, Vec<u8>>, operations: &[BatchOperation]) {
    for operation in operations {
        match operation {
            BatchOperation::Put { key, value } => {
                model.insert(key.clone(), value.clone());
            }
            BatchOperation::Delete { key } => {
                model.remove(key);
            }
        }
    }
}

fn state(db: &mut Database<SharedDir>) -> BTreeMap<Vec<u8>, Vec<u8>> {
    db.scan(ScanOptions {
        limit: usize::MAX,
        ..ScanOptions::default()
    })
    .unwrap()
    .into_iter()
    .collect()
}

fn assert_poisoned(db: &mut Database<SharedDir>) {
    assert!(
        db.write_batch(&[])
            .unwrap_err()
            .to_string()
            .contains("poison")
    );
    assert!(
        db.write_batch(&operations())
            .unwrap_err()
            .to_string()
            .contains("poison")
    );
    assert!(db.put(b"forbidden", b"value").is_err());
    assert!(db.delete(b"stable").is_err());
    assert!(db.flush().is_err());
    assert!(db.compact().is_err());
    assert!(db.collect_garbage().is_err());
}

#[test]
fn batch_preserves_confirmed_wal_and_applies_ordered_binary_operations() {
    let directory = SharedDir::default();
    let mut db = seed(&directory);
    let mut expected = before();
    apply(&mut expected, &operations());
    db.write_batch(&operations()).unwrap();
    assert_eq!(state(&mut db), expected);
    assert_eq!(db.stats().memtable_entries, 0);
    assert_eq!(db.stats().wal_bytes, 16);
    assert_eq!(db.stats().table_count, 2);
    db.close().unwrap();
    let mut db = Database::open(directory.clone()).unwrap();
    assert_eq!(state(&mut db), expected);
    assert_eq!(db.get(b"delete").unwrap(), None);
    db.put(b"created", b"later WAL").unwrap();
    expected.insert(b"created".to_vec(), b"later WAL".to_vec());
    db.delete(b"overwrite").unwrap();
    expected.remove(b"overwrite".as_slice());
    db.flush().unwrap();
    db.compact().unwrap();
    db.collect_garbage().unwrap();
    db.close().unwrap();
    let mut db = Database::open(directory).unwrap();
    assert_eq!(state(&mut db), expected);
}

#[test]
fn empty_batch_has_no_io_and_preserves_dirty_wal() {
    let directory = SharedDir::default();
    let mut db = seed(&directory);
    let stats = db.stats();
    let names = directory.names("");
    directory.arm(None);
    for _ in 0..3 {
        db.write_batch(&[]).unwrap();
    }
    assert!(directory.fault.borrow().operations.is_empty());
    assert_eq!(db.stats(), stats);
    assert_eq!(directory.names(""), names);
    assert_eq!(state(&mut db), before());
}

fn fault_operations() -> Vec<String> {
    let directory = SharedDir::default();
    let mut db = seed(&directory);
    directory.arm(None);
    db.write_batch(&operations()).unwrap();
    let operations = directory.fault.borrow().operations.clone();
    assert!(operations.iter().any(|name| name.starts_with("rename ")));
    assert!(
        operations
            .iter()
            .filter(|name| name.as_str() == "sync_directory")
            .count()
            >= 2
    );
    operations
}

fn check_fault(index: usize, operation: &str, write_after: bool, rename_after: bool) {
    let directory = SharedDir::default();
    let mut db = seed(&directory);
    directory.arm(Some(index + 1));
    directory.fault.borrow_mut().write_then_fail = write_after;
    directory.fault.borrow_mut().rename_then_fail = rename_after;
    // rename_then_fail independently models a successful rename returning Err.
    if rename_after {
        directory.fault.borrow_mut().fail_at = None;
    }
    assert!(
        db.write_batch(&operations()).is_err(),
        "fault {index}: {operation}"
    );
    directory.arm(None);
    assert_eq!(state(&mut db), before(), "live state after {operation}");
    assert_poisoned(&mut db);
    drop(db);
    let mut db =
        Database::open(directory).unwrap_or_else(|error| panic!("after {operation}: {error}"));
    let actual = state(&mut db);
    let mut committed = before();
    apply(&mut committed, &operations());
    assert!(
        actual == before() || actual == committed,
        "partial batch after {operation}: {actual:?}"
    );
    assert_eq!(db.get(b"stable").unwrap(), Some(b"retained".to_vec()));
    assert_eq!(db.get(b"delete").unwrap(), None);
    assert_eq!(db.get(b"forbidden").unwrap(), None);
    db.put(b"reopened", b"writable").unwrap();
    db.flush().unwrap();
}

#[test]
fn every_batch_io_failure_keeps_live_state_and_reopens_all_or_none() {
    for (index, operation) in fault_operations().iter().enumerate() {
        check_fault(index, operation, false, false);
    }
}

#[test]
fn completed_writes_returning_errors_do_not_publish_partial_batch() {
    let mut count = 0;
    for (index, operation) in fault_operations().iter().enumerate() {
        if operation.starts_with("write ") {
            check_fault(index, operation, true, false);
            count += 1;
        }
    }
    assert!(count >= 3, "cover SST, WAL header and manifest writes");
}

#[test]
fn successful_manifest_rename_returning_error_requires_reopen() {
    let operations = fault_operations();
    let (index, operation) = operations
        .iter()
        .enumerate()
        .find(|(_, op)| op.starts_with("rename "))
        .unwrap();
    check_fault(index, operation, false, true);
}

#[test]
fn invalid_batches_do_no_io_and_leave_instance_writable() {
    let invalid = [
        vec![delete(b"x"); MAX_BATCH_OPERATIONS + 1],
        vec![put(b"valid", b"first"), delete(&vec![0; 64 * 1024 + 1])],
        vec![
            put(b"valid", b"first"),
            put(b"oversize", &vec![0; 16 * 1024 * 1024 + 1]),
        ],
        // Repeated keys still count in the raw input-byte budget.
        vec![put(b"x", &vec![0; MAX_BATCH_BYTES / 2]); 2],
    ];
    for operations in invalid {
        assert!(validate_batch(&operations).is_err());
        let directory = SharedDir::default();
        let mut db = seed(&directory);
        let stats = db.stats();
        let names = directory.names("");
        directory.arm(None);
        assert!(db.write_batch(&operations).is_err());
        assert!(directory.fault.borrow().operations.is_empty());
        assert_eq!(db.stats(), stats);
        assert_eq!(directory.names(""), names);
        assert_eq!(state(&mut db), before());
        db.put(b"still", b"writable").unwrap();
        db.write_batch(&[put(b"accepted", b"next")]).unwrap();
    }
}

#[test]
fn exact_batch_and_record_limits_are_accepted() {
    assert!(validate_batch(&vec![delete(b"x"); MAX_BATCH_OPERATIONS]).is_ok());
    assert!(validate_batch(&[delete(&vec![0; 64 * 1024])]).is_ok());
    let value = vec![0; 16 * 1024 * 1024];
    assert_eq!(MAX_BATCH_BYTES, value.len() * 2);
    assert!(validate_batch(&[put(b"", &value), put(b"", &value)]).is_ok());
    let directory = SharedDir::default();
    let mut db = Database::open(directory).unwrap();
    db.write_batch(&vec![delete(b"x"); MAX_BATCH_OPERATIONS])
        .unwrap();
    assert_eq!(db.get(b"x").unwrap(), None);
}

#[test]
fn deterministic_model_interleaves_batches_flush_compaction_gc_and_reopen() {
    let directory = SharedDir::default();
    let mut db = Database::open(directory.clone()).unwrap();
    let mut expected = BTreeMap::new();
    let mut random = 0x9197_1234_86ab_cdef_u64;
    for step in 0..160_u32 {
        let mut batch = Vec::new();
        for item in 0..7_u8 {
            random = random
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            let key = vec![(random >> 32) as u8 % 31, 0, 255];
            batch.push(if (random >> 48).is_multiple_of(3) {
                delete(&key)
            } else {
                put(&key, &[step as u8, item, 255, 0])
            });
        }
        db.write_batch(&batch).unwrap();
        apply(&mut expected, &batch);
        if step.is_multiple_of(3) {
            let key = vec![200, step as u8];
            db.put(&key, b"ordinary WAL").unwrap();
            expected.insert(key, b"ordinary WAL".to_vec());
        }
        if step.is_multiple_of(7) {
            db.flush().unwrap();
        }
        if step.is_multiple_of(11) {
            db.compact().unwrap();
        }
        if step.is_multiple_of(13) {
            db.collect_garbage().unwrap();
        }
        if step.is_multiple_of(5) {
            db.close().unwrap();
            db = Database::open(directory.clone()).unwrap();
        }
        assert_eq!(state(&mut db), expected, "step {step}");
        let reverse = db
            .scan(ScanOptions {
                reverse: true,
                limit: usize::MAX,
                ..ScanOptions::default()
            })
            .unwrap();
        assert_eq!(
            reverse,
            expected
                .iter()
                .rev()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect::<Vec<_>>()
        );
    }
    db.close().unwrap();
    let mut db = Database::open(directory).unwrap();
    assert_eq!(state(&mut db), expected);
}

#[test]
fn combined_sst_index_capacity_is_checked_before_io_without_poison() {
    let directory = SharedDir::default();
    let mut db = Database::open(directory.clone()).unwrap();
    fn wide_key(index: u16) -> Vec<u8> {
        let mut key = vec![0; 64 * 1024];
        key[..2].copy_from_slice(&index.to_be_bytes());
        key
    }
    // All individual records fit, and the new batch fits its 32 MiB budget.
    // The combined WAL delta + batch would exceed the SST's 64 MiB index.
    for index in 0..513 {
        db.put(&wide_key(index), b"").unwrap();
    }
    let operations = (513..1024)
        .map(|index| delete(&wide_key(index)))
        .collect::<Vec<_>>();
    validate_batch(&operations).unwrap();
    let names = directory.names("");
    let stats = db.stats();
    directory.arm(None);
    assert!(db.write_batch(&operations).is_err());
    assert!(directory.fault.borrow().operations.is_empty());
    assert_eq!(directory.names(""), names);
    assert_eq!(db.stats(), stats);
    assert_eq!(db.get(&wide_key(0)).unwrap(), Some(Vec::new()));
    db.put(b"still", b"writable").unwrap();
}

#[test]
fn table_count_limit_is_checked_before_io_without_poison() {
    let directory = SharedDir::default();
    seed(&directory).close().unwrap();
    let table = directory
        .files
        .borrow()
        .get(&directory.names(".sst")[0])
        .unwrap()
        .clone();
    let wal = directory
        .files
        .borrow()
        .get(&directory.names(".wal").last().unwrap().clone())
        .unwrap()
        .clone();
    // Reuse immutable valid table bytes to construct the supported table-count
    // boundary cheaply, without performing 4096 independent publications.
    let mut manifest = b"STOWMAN\0".to_vec();
    manifest.extend_from_slice(&1_u32.to_le_bytes());
    manifest.extend_from_slice(&4096_u64.to_le_bytes());
    manifest.extend_from_slice(&4096_u32.to_le_bytes());
    for generation in 1..=4096_u64 {
        directory
            .files
            .borrow_mut()
            .insert(format!("g{generation:020}.sst"), table.clone());
        manifest.extend_from_slice(&generation.to_le_bytes());
    }
    let crc = crc32fast::hash(&manifest);
    manifest.extend_from_slice(&crc.to_le_bytes());
    directory
        .files
        .borrow_mut()
        .insert("MANIFEST".into(), Rc::new(RefCell::new(manifest)));
    directory
        .files
        .borrow_mut()
        .insert("g00000000000000004096.wal".into(), wal);
    let mut db = Database::open(directory.clone()).unwrap();
    assert_eq!(db.stats().table_count, 4096);
    let names = directory.names("");
    let stats = db.stats();
    directory.arm(None);
    assert!(db.write_batch(&[put(b"batch", b"rejected")]).is_err());
    assert!(directory.fault.borrow().operations.is_empty());
    assert_eq!(directory.names(""), names);
    assert_eq!(db.stats(), stats);
    assert_eq!(db.get(b"batch").unwrap(), None);
    db.put(b"still", b"writable").unwrap();
    // An empty batch needs neither another table nor a publication.
    db.write_batch(&[]).unwrap();
}

#[test]
fn exhausted_generation_is_a_retryable_limit_without_poison() {
    let directory = SharedDir::default();
    let mut db = seed(&directory);
    let orphan = "g18446744073709551615.wal";
    directory
        .files
        .borrow_mut()
        .insert(orphan.into(), Rc::new(RefCell::new(Vec::new())));
    let before_files = directory
        .files
        .borrow()
        .iter()
        .map(|(name, bytes)| (name.clone(), bytes.borrow().clone()))
        .collect::<BTreeMap<_, _>>();
    let stats = db.stats();
    directory.arm(None);
    assert!(matches!(
        db.write_batch(&operations()),
        Err(hoke_core::DbError::Limit(_))
    ));
    assert_eq!(directory.fault.borrow().operations, ["list"]);
    assert_eq!(
        directory
            .files
            .borrow()
            .iter()
            .map(|(name, bytes)| (name.clone(), bytes.borrow().clone()))
            .collect::<BTreeMap<_, _>>(),
        before_files
    );
    assert_eq!(db.stats(), stats);
    assert_eq!(state(&mut db), before());
    db.put(b"after limit", b"still writable").unwrap();
    db.collect_garbage().unwrap();
    assert!(!directory.files.borrow().contains_key(orphan));
    db.write_batch(&operations()).unwrap();
    let mut expected = before();
    expected.insert(b"after limit".to_vec(), b"still writable".to_vec());
    apply(&mut expected, &operations());
    assert_eq!(state(&mut db), expected);
    db.close().unwrap();
    let mut db = Database::open(directory).unwrap();
    assert_eq!(state(&mut db), expected);
}
