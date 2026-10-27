//! Flush/recovery fault tests. The byte backend models visible file operations,
//! not loss of power or a filesystem's durability guarantees.
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io;
use std::rc::Rc;
use hoke_core::Database;
use hoke_core::directory::Directory;
use hoke_core::storage::Storage;

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

    fn corrupt_last_byte(&self, name: &str) {
        let file = self.files.borrow().get(name).unwrap().clone();
        let mut bytes = file.borrow_mut();
        *bytes.last_mut().unwrap() ^= 0x80;
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

    fn sync_directory(&self) -> io::Result<()> {
        self.operation("sync_directory".into())
    }
}

fn seed(directory: &SharedDir) -> Database<SharedDir> {
    let mut database = Database::open(directory.clone()).unwrap();
    database.put(b"old", b"before").unwrap();
    database.put(b"deleted", b"must not resurrect").unwrap();
    database.put(b"stable", b"untouched").unwrap();
    database.flush().unwrap();
    database.put(b"old", b"after").unwrap();
    database.delete(b"deleted").unwrap();
    database.put(b"new", b"new value").unwrap();
    database.put(b"", b"empty key").unwrap();
    database.put(&[0, 255], &[255, 0]).unwrap();
    database
}

fn assert_state(database: &mut Database<SharedDir>) {
    for (key, expected) in [
        (b"old".as_slice(), Some(b"after".as_slice())),
        (b"deleted".as_slice(), None),
        (b"stable".as_slice(), Some(b"untouched".as_slice())),
        (b"new".as_slice(), Some(b"new value".as_slice())),
        (b"".as_slice(), Some(b"empty key".as_slice())),
        (&[0, 255], Some([255, 0].as_slice())),
    ] {
        assert_eq!(
            database.get(key).unwrap().as_deref(),
            expected,
            "key {key:?}"
        );
    }
}

fn assert_poisoned(database: &mut Database<SharedDir>) {
    assert!(
        database
            .put(b"forbidden", b"write")
            .unwrap_err()
            .to_string()
            .contains("poison")
    );
    assert!(
        database
            .delete(b"stable")
            .unwrap_err()
            .to_string()
            .contains("poison")
    );
    assert!(database.flush().unwrap_err().to_string().contains("poison"));
}

#[test]
fn repeated_flushes_release_memtable_and_preserve_tombstones() {
    let directory = SharedDir::default();
    let mut database = seed(&directory);
    assert_eq!(database.stats().table_count, 1);
    assert!(database.stats().memtable_entries > 0);
    let previous_wal = database.stats().wal_bytes;
    database.flush().unwrap();
    assert_eq!(database.stats().table_count, 2);
    assert_eq!(database.stats().memtable_entries, 0);
    assert!(database.stats().wal_bytes < previous_wal);
    assert_state(&mut database);
    let names = directory.names("");
    let stats = database.stats();
    directory.arm(None);
    for _ in 0..5 {
        database.flush().unwrap();
    }
    assert_eq!(directory.names(""), names);
    assert_eq!(database.stats(), stats);
    assert!(directory.fault.borrow().operations.is_empty());
    database.close().unwrap();
    let mut database = Database::open(directory.clone()).unwrap();
    assert_state(&mut database);
    database.put(b"new", b"third table").unwrap();
    database.delete(b"stable").unwrap();
    database.flush().unwrap();
    database.close().unwrap();
    let mut database = Database::open(directory).unwrap();
    assert_eq!(database.stats().table_count, 3);
    assert_eq!(
        database.get(b"new").unwrap().as_deref(),
        Some(b"third table".as_slice())
    );
    assert_eq!(database.get(b"stable").unwrap(), None);
    assert_eq!(database.get(b"deleted").unwrap(), None);
}

#[test]
fn every_flush_operation_failure_reopens_all_confirmed_changes() {
    let probe_directory = SharedDir::default();
    let mut probe = seed(&probe_directory);
    probe_directory.arm(None);
    probe.flush().unwrap();
    let operations = probe_directory.fault.borrow().operations.clone();
    assert!(operations.iter().any(|name| name.starts_with("rename ")));
    assert!(
        operations
            .iter()
            .filter(|name| *name == "sync_directory")
            .count()
            >= 2
    );
    for (index, operation) in operations.iter().enumerate() {
        let directory = SharedDir::default();
        let mut database = seed(&directory);
        directory.arm(Some(index + 1));
        assert!(
            database.flush().is_err(),
            "operation {}: {operation}",
            index + 1
        );
        directory.arm(None);
        assert_poisoned(&mut database);
        drop(database);
        let mut reopened = Database::open(directory).unwrap_or_else(|error| {
            panic!(
                "reopen after operation {} ({operation}): {error}",
                index + 1
            )
        });
        assert_state(&mut reopened);
        assert_eq!(reopened.get(b"forbidden").unwrap(), None);
        reopened.put(b"reopened", b"writable").unwrap();
        reopened.flush().unwrap();
    }
}

#[test]
fn rename_with_unknown_outcome_poisoned_until_reopen() {
    let directory = SharedDir::default();
    let mut database = seed(&directory);
    directory.arm(None);
    directory.fault.borrow_mut().rename_then_fail = true;
    assert!(database.flush().is_err());
    directory.arm(None);
    assert_poisoned(&mut database);
    drop(database);
    let mut reopened = Database::open(directory).unwrap();
    assert_state(&mut reopened);
    assert_eq!(reopened.stats().memtable_entries, 0);
    assert_eq!(reopened.stats().table_count, 2);
}

#[test]
fn missing_referenced_files_are_never_recreated() {
    for suffix in [".sst", ".wal"] {
        let directory = SharedDir::default();
        let mut database = seed(&directory);
        database.flush().unwrap();
        database.close().unwrap();
        let name = directory.names(suffix).last().unwrap().clone();
        directory.files.borrow_mut().remove(&name);
        assert!(Database::open(directory.clone()).is_err(), "missing {name}");
        assert!(!directory.files.borrow().contains_key(&name));
    }
}

#[test]
fn corrupt_manifest_and_table_index_are_errors_without_fallback() {
    for suffix in ["MANIFEST", ".sst"] {
        let directory = SharedDir::default();
        let mut database = seed(&directory);
        database.flush().unwrap();
        database.close().unwrap();
        let name = directory.names(suffix).last().unwrap().clone();
        directory.corrupt_last_byte(&name);
        let before = directory
            .files
            .borrow()
            .get(&name)
            .unwrap()
            .borrow()
            .clone();
        assert!(Database::open(directory.clone()).is_err(), "corrupt {name}");
        assert_eq!(
            *directory.files.borrow().get(&name).unwrap().borrow(),
            before
        );
    }
}

#[test]
fn orphan_generations_are_skipped_without_overwrite() {
    let directory = SharedDir::default();
    let database = seed(&directory);
    database.close().unwrap();
    let orphan = "g00000000000000001000.sst";
    directory.files.borrow_mut().insert(
        orphan.into(),
        Rc::new(RefCell::new(b"orphan sentinel".to_vec())),
    );
    let mut database = Database::open(directory.clone()).unwrap();
    assert_state(&mut database);
    database.flush().unwrap();
    assert_eq!(
        *directory.files.borrow().get(orphan).unwrap().borrow(),
        b"orphan sentinel"
    );
    assert!(
        directory
            .names(".sst")
            .iter()
            .any(|name| name.as_str() > orphan)
    );
    drop(database);
    let mut database = Database::open(directory).unwrap();
    assert_state(&mut database);
}

#[test]
fn deterministic_model_survives_many_flush_and_reopen_cycles() {
    let directory = SharedDir::default();
    let mut database = Database::open(directory.clone()).unwrap();
    let mut expected = BTreeMap::<Vec<u8>, Vec<u8>>::new();
    let mut random = 0x6e62_4eb7_2f32_815du64;
    for step in 0u32..200 {
        random = random
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        let key = vec![((random >> 32) % 16) as u8, 0, 255];
        if (random >> 48).is_multiple_of(3) {
            database.delete(&key).unwrap();
            expected.remove(&key);
        } else {
            let value = step.to_le_bytes().to_vec();
            database.put(&key, &value).unwrap();
            expected.insert(key, value);
        }
        if step.is_multiple_of(7) {
            database.flush().unwrap();
            assert_eq!(database.stats().memtable_entries, 0);
            database.close().unwrap();
            database = Database::open(directory.clone()).unwrap();
        }
        for byte in 0..16 {
            let key = vec![byte, 0, 255];
            assert_eq!(
                database.get(&key).unwrap(),
                expected.get(&key).cloned(),
                "step {step}, key {key:?}"
            );
        }
    }
}

#[test]
fn nonempty_directory_without_manifest_is_not_initialized() {
    let directory = SharedDir::default();
    directory.files.borrow_mut().insert(
        "unrelated.txt".into(),
        Rc::new(RefCell::new(b"retain me".to_vec())),
    );
    let before = directory.names("");
    assert!(Database::open(directory.clone()).is_err());
    assert_eq!(directory.names(""), before);
    assert_eq!(
        *directory
            .files
            .borrow()
            .get("unrelated.txt")
            .unwrap()
            .borrow(),
        b"retain me"
    );
}

#[test]
fn write_errors_after_all_bytes_are_visible_recover_confirmed_changes() {
    let probe_directory = SharedDir::default();
    let mut probe = seed(&probe_directory);
    probe_directory.arm(None);
    probe.flush().unwrap();
    let operations = probe_directory.fault.borrow().operations.clone();
    let writes = operations
        .iter()
        .enumerate()
        .filter(|(_, name)| name.starts_with("write "));
    let mut checked = 0;
    for (index, operation) in writes {
        let directory = SharedDir::default();
        let mut database = seed(&directory);
        directory.arm(Some(index + 1));
        directory.fault.borrow_mut().write_then_fail = true;
        assert!(database.flush().is_err(), "operation {operation}");
        directory.arm(None);
        assert_poisoned(&mut database);
        drop(database);
        let mut reopened = Database::open(directory)
            .unwrap_or_else(|error| panic!("reopen after {operation}: {error}"));
        assert_state(&mut reopened);
        checked += 1;
    }
    assert!(checked >= 3, "must cover SST, WAL, and manifest writes");
}

#[test]
fn truncated_referenced_wal_is_rejected_without_repair_or_initialization() {
    // WAL v1 has a 16-byte header; none of its incomplete prefixes is a new WAL.
    for length in 0..16 {
        let directory = SharedDir::default();
        let mut database = seed(&directory);
        database.flush().unwrap();
        database.close().unwrap();
        let name = directory.names(".wal").last().unwrap().clone();
        directory
            .files
            .borrow()
            .get(&name)
            .unwrap()
            .borrow_mut()
            .truncate(length);
        assert!(
            Database::open(directory.clone()).is_err(),
            "WAL length {length}"
        );
        assert_eq!(
            directory.files.borrow().get(&name).unwrap().borrow().len(),
            length
        );
    }
}

#[test]
fn unsupported_manifest_version_with_valid_crc_preserves_original_bytes() {
    let directory = SharedDir::default();
    let database = seed(&directory);
    database.close().unwrap();
    let file = directory.files.borrow().get("MANIFEST").unwrap().clone();
    {
        let mut bytes = file.borrow_mut();
        bytes[8..12].copy_from_slice(&2u32.to_le_bytes());
        let crc_offset = bytes.len() - 4;
        let checksum = crc32fast::hash(&bytes[..crc_offset]);
        bytes[crc_offset..].copy_from_slice(&checksum.to_le_bytes());
    }
    let before = file.borrow().clone();
    let names = directory.names("");
    assert!(Database::open(directory.clone()).is_err());
    assert_eq!(*file.borrow(), before);
    assert_eq!(directory.names(""), names);
}
