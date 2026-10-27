//! Full compaction and obsolete-file reclamation against real, exclusively locked files.
#![cfg(unix)]
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use stowtree_core::directory::Directory;
use stowtree_core::storage::{FileStorage, Storage};
use stowtree_core::{Database, DbError, KeyValue, NativeDirectory, ScanOptions};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        loop {
            let path = std::env::temp_dir().join(format!(
                "stowtree-compact-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("temporary directory: {error}"),
            }
        }
    }
    fn open(&self) -> Database<NativeDirectory> {
        Database::open(NativeDirectory::open(&self.0).unwrap()).unwrap()
    }
    fn files(&self) -> BTreeMap<String, Vec<u8>> {
        fs::read_dir(&self.0)
            .unwrap()
            .map(|entry| {
                let path = entry.unwrap().path();
                (
                    path.file_name().unwrap().to_str().unwrap().to_owned(),
                    fs::read(path).unwrap(),
                )
            })
            .collect()
    }
    fn bytes(&self) -> usize {
        self.files().values().map(Vec::len).sum()
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn all() -> ScanOptions {
    ScanOptions {
        limit: usize::MAX,
        ..ScanOptions::default()
    }
}
fn rows(model: &BTreeMap<Vec<u8>, Vec<u8>>) -> Vec<KeyValue> {
    model
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

#[test]
fn compaction_preserves_more_than_default_scan_limit_and_reclaims_old_values() {
    let temp = Temp::new();
    let mut db = temp.open();
    let mut model = BTreeMap::new();
    for i in 0_u16..256 {
        let key = i.to_be_bytes().to_vec();
        db.put(&key, &[42; 2048]).unwrap();
        model.insert(key, vec![42; 2048]);
    }
    db.flush().unwrap();
    for i in 0_u16..256 {
        let key = i.to_be_bytes().to_vec();
        if i % 4 == 0 {
            db.delete(&key).unwrap();
            model.remove(&key);
        } else {
            db.put(&key, &[i as u8]).unwrap();
            model.insert(key, vec![i as u8]);
        }
    }
    db.flush().unwrap();
    // The compacted state must include writes that have never reached an SST.
    for key in [vec![], vec![0], vec![255], vec![255, 255]] {
        db.put(&key, b"newest").unwrap();
        model.insert(key, b"newest".to_vec());
    }
    db.put(&0_u16.to_be_bytes(), b"reinserted").unwrap();
    model.insert(0_u16.to_be_bytes().to_vec(), b"reinserted".to_vec());
    db.delete(&1_u16.to_be_bytes()).unwrap();
    model.remove(1_u16.to_be_bytes().as_slice());
    let bytes_before = temp.bytes();
    let report = db.compact().unwrap();
    assert_eq!((report.tables_before, report.tables_after), (2, 1));
    assert!(report.cleanup.errors.is_empty());
    assert!(report.cleanup.removed_files >= 4);
    assert!(
        temp.bytes() < bytes_before / 2,
        "obsolete large values reclaimed"
    );
    assert_eq!(db.stats().memtable_entries, 0);
    assert_eq!(db.stats().wal_bytes, 16);
    assert_eq!(db.scan(all()).unwrap(), rows(&model));
    for (key, value) in &model {
        assert_eq!(db.get(key).unwrap().as_ref(), Some(value));
    }
    let mut reversed = rows(&model);
    reversed.reverse();
    assert_eq!(
        db.scan(ScanOptions {
            reverse: true,
            ..all()
        })
        .unwrap(),
        reversed
    );
    let files = temp.files();
    let again = db.compact().unwrap();
    assert_eq!((again.tables_before, again.tables_after), (1, 1));
    assert_eq!(again.cleanup.removed_files, 0);
    assert!(again.cleanup.errors.is_empty());
    assert_eq!(
        temp.files(),
        files,
        "idempotent compaction does not allocate generations"
    );
    db.close().unwrap();
    let mut db = temp.open();
    assert_eq!(db.scan(all()).unwrap(), rows(&model));
    assert_eq!(db.get(&1_u16.to_be_bytes()).unwrap(), None);
    db.put(b"after", b"compaction").unwrap();
    db.close().unwrap();
    assert_eq!(
        temp.open().get(b"after").unwrap(),
        Some(b"compaction".to_vec())
    );
}

#[test]
fn deleting_every_key_compacts_to_no_tables_without_resurrection() {
    let temp = Temp::new();
    let mut db = temp.open();
    for key in [b"a", b"b", b"c"] {
        db.put(key, b"old").unwrap();
    }
    db.flush().unwrap();
    db.delete(b"a").unwrap();
    db.delete(b"b").unwrap();
    db.flush().unwrap();
    db.delete(b"c").unwrap();
    db.delete(b"never existed").unwrap();
    let report = db.compact().unwrap();
    assert_eq!((report.tables_before, report.tables_after), (2, 0));
    assert!(report.cleanup.errors.is_empty());
    assert_eq!(db.stats().table_count, 0);
    assert_eq!(
        temp.files()
            .keys()
            .filter(|name| name.ends_with(".sst"))
            .count(),
        0
    );
    db.close().unwrap();
    let mut db = temp.open();
    assert!(db.scan(all()).unwrap().is_empty());
    for key in [b"a", b"b", b"c"] {
        assert_eq!(db.get(key).unwrap(), None);
    }
    db.put(b"a", b"again").unwrap();
    db.compact().unwrap();
    db.close().unwrap();
    assert_eq!(temp.open().get(b"a").unwrap(), Some(b"again".to_vec()));
}

#[test]
fn empty_compaction_and_gc_are_idempotent() {
    let temp = Temp::new();
    let mut db = temp.open();
    let files = temp.files();
    let report = db.compact().unwrap();
    assert_eq!((report.tables_before, report.tables_after), (0, 0));
    assert_eq!(report.cleanup.removed_files, 0);
    assert!(report.cleanup.errors.is_empty());
    assert_eq!(temp.files(), files);
    assert_eq!(db.collect_garbage().unwrap().removed_files, 0);
}

#[test]
fn gc_removes_only_strict_obsolete_managed_names_and_preserves_active_state() {
    let temp = Temp::new();
    let mut db = temp.open();
    db.put(b"table", b"value").unwrap();
    db.flush().unwrap();
    db.put(b"wal", b"latest").unwrap();
    let active_before = temp.files();
    let unmanaged = [
        "notes.txt",
        "g2.sst",
        "g00000000000000000003.sst.bak",
        "g0000000000000000000x.wal",
        "MANIFEST.bak",
    ];
    for name in unmanaged {
        fs::write(temp.0.join(name), b"user data").unwrap();
    }
    let orphaned = [
        "g00000000000000000700.sst",
        "g00000000000000000701.wal",
        "g00000000000000000702.manifest.tmp",
    ];
    for name in orphaned {
        fs::write(temp.0.join(name), b"incomplete orphan").unwrap();
    }
    let report = db.collect_garbage().unwrap();
    // One WAL from the prior flush plus three explicitly created orphans.
    assert_eq!(report.removed_files, 4);
    assert!(report.errors.is_empty());
    for name in orphaned {
        assert!(!temp.0.join(name).exists());
    }
    for name in unmanaged {
        assert_eq!(fs::read(temp.0.join(name)).unwrap(), b"user data");
    }
    for (name, bytes) in active_before {
        if name != "g00000000000000000001.wal" {
            assert_eq!(fs::read(temp.0.join(name)).unwrap(), bytes);
        }
    }
    assert_eq!(db.get(b"table").unwrap(), Some(b"value".to_vec()));
    assert_eq!(db.get(b"wal").unwrap(), Some(b"latest".to_vec()));
    db.close().unwrap();
    let mut db = temp.open();
    assert_eq!(db.collect_garbage().unwrap().removed_files, 0);
    assert_eq!(db.get(b"wal").unwrap(), Some(b"latest".to_vec()));
}

#[derive(Default)]
struct Fault {
    count: Cell<usize>,
    fail_at: Cell<Option<usize>>,
    rename_then_error: Cell<bool>,
    remove_then_error: Cell<bool>,
    events: RefCell<Vec<&'static str>>,
}
impl Fault {
    fn hit(&self, operation: &'static str) -> io::Result<()> {
        self.events.borrow_mut().push(operation);
        let count = self.count.get() + 1;
        self.count.set(count);
        if self.fail_at.get() == Some(count) {
            Err(io::Error::other("injected compaction failure"))
        } else {
            Ok(())
        }
    }
    fn arm(&self, target: Option<usize>) {
        self.count.set(0);
        self.fail_at.set(target);
        self.events.borrow_mut().clear();
    }
}
struct FaultFile {
    inner: FileStorage,
    fault: Rc<Fault>,
}
impl Storage for FaultFile {
    fn len(&self) -> io::Result<u64> {
        self.fault.hit("len")?;
        self.inner.len()
    }
    fn read_at(&mut self, offset: u64, bytes: &mut [u8]) -> io::Result<usize> {
        self.fault.hit("read")?;
        self.inner.read_at(offset, bytes)
    }
    fn write_all_at(&mut self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        self.fault.hit("write")?;
        self.inner.write_all_at(offset, bytes)
    }
    fn set_len(&mut self, len: u64) -> io::Result<()> {
        self.fault.hit("truncate")?;
        self.inner.set_len(len)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.fault.hit("flush")?;
        self.inner.flush()
    }
    fn sync_all(&mut self) -> io::Result<()> {
        self.fault.hit("sync_file")?;
        self.inner.sync_all()
    }
}
struct FaultDir {
    inner: NativeDirectory,
    fault: Rc<Fault>,
}
impl Directory for FaultDir {
    type File = FaultFile;
    fn open_existing(&self, name: &str) -> io::Result<Self::File> {
        self.fault.hit("open")?;
        Ok(FaultFile {
            inner: self.inner.open_existing(name)?,
            fault: self.fault.clone(),
        })
    }
    fn create_new(&self, name: &str) -> io::Result<Self::File> {
        self.fault.hit("create")?;
        Ok(FaultFile {
            inner: self.inner.create_new(name)?,
            fault: self.fault.clone(),
        })
    }
    fn list(&self) -> io::Result<Vec<String>> {
        self.fault.hit("list")?;
        self.inner.list()
    }
    fn rename(&self, from: &str, to: &str) -> io::Result<()> {
        self.fault.hit("rename")?;
        self.inner.rename(from, to)?;
        if self.fault.rename_then_error.replace(false) {
            Err(io::Error::other("rename committed, response failed"))
        } else {
            Ok(())
        }
    }
    fn sync_directory(&self) -> io::Result<()> {
        self.fault.hit("sync_dir")?;
        self.inner.sync_directory()
    }
    fn remove_file(&self, name: &str) -> io::Result<()> {
        self.fault.hit("remove")?;
        self.inner.remove_file(name)?;
        if self.fault.remove_then_error.replace(false) {
            Err(io::Error::other("remove committed, response failed"))
        } else {
            Ok(())
        }
    }
}
fn fixture(temp: &Temp, fault: Rc<Fault>) -> Database<FaultDir> {
    let mut db = Database::open(FaultDir {
        inner: NativeDirectory::open(&temp.0).unwrap(),
        fault,
    })
    .unwrap();
    db.put(b"deleted", b"old").unwrap();
    db.put(b"overwritten", b"old").unwrap();
    db.flush().unwrap();
    db.delete(b"deleted").unwrap();
    db.put(b"overwritten", b"new").unwrap();
    db.flush().unwrap();
    db.put(b"wal", b"unflushed").unwrap();
    db
}
fn verify<D: Directory>(db: &mut Database<D>) {
    assert_eq!(
        db.scan(all()).unwrap(),
        vec![
            (b"overwritten".to_vec(), b"new".to_vec()),
            (b"wal".to_vec(), b"unflushed".to_vec())
        ]
    );
    assert_eq!(db.get(b"deleted").unwrap(), None);
}

#[test]
fn every_compaction_io_failure_preserves_confirmed_state_and_cleanup_is_after_publication() {
    let baseline = Temp::new();
    let fault = Rc::new(Fault::default());
    let mut db = fixture(&baseline, fault.clone());
    fault.arm(None);
    let report = db.compact().unwrap();
    assert!(report.cleanup.errors.is_empty());
    let operations = fault.count.get();
    let events = fault.events.borrow().clone();
    let rename = events
        .iter()
        .position(|operation| *operation == "rename")
        .unwrap();
    let first_remove = events
        .iter()
        .position(|operation| *operation == "remove")
        .unwrap();
    assert!(events[rename + 1..first_remove].contains(&"sync_dir"));
    fault.fail_at.set(None);
    drop(db);
    for target in 1..=operations {
        let temp = Temp::new();
        let fault = Rc::new(Fault::default());
        let mut db = fixture(&temp, fault.clone());
        let before = temp.files();
        fault.arm(Some(target));
        let result = db.compact();
        fault.fail_at.set(None);
        match result {
            Err(_) => {
                assert!(
                    !fault.events.borrow().contains(&"remove"),
                    "no deletion before publication, operation {target}"
                );
                for (name, bytes) in &before {
                    if name == "MANIFEST" {
                        continue;
                    }
                    assert_eq!(
                        fs::read(temp.0.join(name)).unwrap(),
                        *bytes,
                        "old state retained, operation {target}: {name}"
                    );
                }
                assert!(
                    matches!(db.put(b"forbidden", b"write"), Err(DbError::Poisoned)),
                    "failed compaction must poison, operation {target}"
                );
            }
            Ok(report) => {
                assert!(
                    !report.cleanup.errors.is_empty(),
                    "cleanup error is reported, operation {target}"
                );
                verify(&mut db);
                db.put(b"usable", b"yes").unwrap();
                assert!(db.collect_garbage().unwrap().errors.is_empty());
                db.delete(b"usable").unwrap();
            }
        }
        drop(db);
        let mut reopened = temp.open();
        verify(&mut reopened);
    }
}

#[test]
fn uncertain_manifest_rename_poisons_without_unlinking_the_previous_state() {
    let temp = Temp::new();
    let fault = Rc::new(Fault::default());
    let mut db = fixture(&temp, fault.clone());
    let before = temp.files();
    fault.arm(None);
    fault.rename_then_error.set(true);
    assert!(db.compact().is_err());
    assert!(!fault.events.borrow().contains(&"remove"));
    for (name, bytes) in before {
        if name != "MANIFEST" {
            assert_eq!(fs::read(temp.0.join(name)).unwrap(), bytes);
        }
    }
    assert!(matches!(db.delete(b"wal"), Err(DbError::Poisoned)));
    drop(db);
    let mut reopened = temp.open();
    verify(&mut reopened);
    assert!(reopened.collect_garbage().unwrap().errors.is_empty());
    verify(&mut reopened);
}

#[test]
fn gc_syncs_current_manifest_before_deleting_orphans_after_reopen() {
    let temp = Temp::new();
    let fault = Rc::new(Fault::default());
    drop(fixture(&temp, fault.clone()));
    let mut db = Database::open(FaultDir {
        inner: NativeDirectory::open(&temp.0).unwrap(),
        fault: fault.clone(),
    })
    .unwrap();
    let before = temp.files();
    fault.arm(Some(1));
    let result = db.collect_garbage();
    fault.fail_at.set(None);
    assert!(fault.events.borrow().contains(&"sync_dir"));
    assert!(!fault.events.borrow().contains(&"remove"));
    assert_eq!(temp.files(), before);
    assert!(!result.unwrap().errors.is_empty());
    verify(&mut db);
    assert!(db.collect_garbage().unwrap().errors.is_empty());
}

fn corrupt_unique_payload(temp: &Temp, value: &[u8]) {
    let mut changed = 0;
    for (name, mut bytes) in temp.files() {
        if !name.ends_with(".sst") {
            continue;
        }
        let offsets: Vec<_> = bytes
            .windows(value.len())
            .enumerate()
            .filter_map(|(offset, candidate)| (candidate == value).then_some(offset))
            .collect();
        for offset in offsets {
            bytes[offset] ^= 0x80;
            changed += 1;
        }
        fs::write(temp.0.join(name), bytes).unwrap();
    }
    assert_eq!(changed, 1, "the fixture has one corruption target");
}

#[test]
fn corrupt_winning_payload_aborts_compaction_without_publishing_or_cleaning_up() {
    let temp = Temp::new();
    let mut db = temp.open();
    db.put(b"winner", b"unique payload to corrupt").unwrap();
    db.flush().unwrap();
    db.put(b"other", b"other value").unwrap();
    db.flush().unwrap();
    db.close().unwrap();
    corrupt_unique_payload(&temp, b"unique payload to corrupt");
    let before = temp.files();
    let mut db = temp.open();
    assert!(db.compact().is_err());
    assert!(matches!(
        db.put(b"forbidden", b"write"),
        Err(DbError::Poisoned)
    ));
    for (name, bytes) in before {
        assert_eq!(
            fs::read(temp.0.join(&name)).unwrap(),
            bytes,
            "source file remains: {name}"
        );
    }
    drop(db);
    let mut db = temp.open();
    assert!(db.get(b"winner").is_err());
    assert_eq!(db.get(b"other").unwrap(), Some(b"other value".to_vec()));
}

#[test]
fn compaction_discards_shadowed_corruption_instead_of_reading_obsolete_values() {
    let temp = Temp::new();
    let mut db = temp.open();
    db.put(b"overwritten", b"shadowed corrupt payload").unwrap();
    db.flush().unwrap();
    db.put(b"overwritten", b"new value").unwrap();
    db.flush().unwrap();
    db.close().unwrap();
    corrupt_unique_payload(&temp, b"shadowed corrupt payload");
    let mut db = temp.open();
    let report = db.compact().unwrap();
    assert!(report.cleanup.errors.is_empty());
    assert_eq!(db.get(b"overwritten").unwrap(), Some(b"new value".to_vec()));
    db.close().unwrap();
    assert_eq!(
        temp.open().get(b"overwritten").unwrap(),
        Some(b"new value".to_vec())
    );
}

#[test]
fn repeated_compact_gc_flush_and_reopen_match_ordered_model() {
    let temp = Temp::new();
    let mut db = temp.open();
    let mut model = BTreeMap::new();
    let mut random = 0x9137_7abc_40ef_d321_u64;
    for step in 0_u16..240 {
        random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
        let key = vec![(random >> 32) as u8 % 41, (random >> 40) as u8 % 3];
        if random & 7 < 3 {
            db.delete(&key).unwrap();
            model.remove(&key);
        } else {
            let value = step.to_be_bytes().to_vec();
            db.put(&key, &value).unwrap();
            model.insert(key, value);
        }
        if step % 13 == 0 {
            db.flush().unwrap();
        }
        if step % 19 == 0 {
            assert!(db.compact().unwrap().cleanup.errors.is_empty());
        }
        if step % 11 == 0 {
            assert!(db.collect_garbage().unwrap().errors.is_empty());
        }
        if step % 23 == 0 {
            db.close().unwrap();
            db = temp.open();
        }
        assert_eq!(db.scan(all()).unwrap(), rows(&model), "step {step}");
        for (key, value) in &model {
            assert_eq!(db.get(key).unwrap().as_ref(), Some(value), "step {step}");
        }
    }
    assert!(db.compact().unwrap().cleanup.errors.is_empty());
    db.close().unwrap();
    assert_eq!(temp.open().scan(all()).unwrap(), rows(&model));
}

#[test]
fn cleanup_remove_then_error_reports_partial_outcome_and_retry_is_safe() {
    let temp = Temp::new();
    let fault = Rc::new(Fault::default());
    let mut db = fixture(&temp, fault.clone());
    fault.arm(None);
    fault.remove_then_error.set(true);
    let report = db.compact().unwrap();
    assert_eq!(report.cleanup.errors.len(), 1);
    assert!(report.cleanup.errors[0].file.is_some());
    assert!(
        report.cleanup.removed_files > 0,
        "cleanup continues after one failure"
    );
    verify(&mut db);
    let retry = db.collect_garbage().unwrap();
    assert!(retry.errors.is_empty());
    assert_eq!(retry.removed_files, 0, "failed remove already took effect");
    db.put(b"usable", b"after partial cleanup").unwrap();
    db.close().unwrap();
    let mut db = temp.open();
    assert_eq!(
        db.get(b"usable").unwrap(),
        Some(b"after partial cleanup".to_vec())
    );
    db.delete(b"usable").unwrap();
    verify(&mut db);
}

#[test]
fn sole_sst_with_only_tombstones_compacts_to_zero_tables() {
    let temp = Temp::new();
    let mut db = temp.open();
    db.delete(b"never present").unwrap();
    db.delete(b"also absent").unwrap();
    db.flush().unwrap();
    assert_eq!(db.stats().table_count, 1);
    assert_eq!(db.stats().memtable_entries, 0);
    let report = db.compact().unwrap();
    assert_eq!((report.tables_before, report.tables_after), (1, 0));
    assert!(report.cleanup.errors.is_empty());
    assert_eq!(
        temp.files()
            .keys()
            .filter(|name| name.ends_with(".sst"))
            .count(),
        0
    );
    let files = temp.files();
    let again = db.compact().unwrap();
    assert_eq!((again.tables_before, again.tables_after), (0, 0));
    assert_eq!(again.cleanup.removed_files, 0);
    assert_eq!(temp.files(), files);
    db.close().unwrap();
    let mut db = temp.open();
    assert!(db.scan(all()).unwrap().is_empty());
    db.put(b"never present", b"inserted later").unwrap();
    db.close().unwrap();
    assert_eq!(
        temp.open().get(b"never present").unwrap(),
        Some(b"inserted later".to_vec())
    );
}

#[test]
fn sole_sst_with_live_values_and_tombstones_rewrites_once_without_tombstones() {
    let temp = Temp::new();
    let mut db = temp.open();
    db.put(b"live", b"value").unwrap();
    db.delete(b"absent").unwrap();
    db.put(b"removed", b"old value").unwrap();
    db.delete(b"removed").unwrap();
    db.flush().unwrap();
    let report = db.compact().unwrap();
    assert_eq!((report.tables_before, report.tables_after), (1, 1));
    assert!(report.cleanup.errors.is_empty());
    let files = temp.files();
    let tables: Vec<_> = files
        .iter()
        .filter(|(name, _)| name.ends_with(".sst"))
        .collect();
    assert_eq!(tables.len(), 1);
    // SST v1's record count must contain the one live key, with no tombstones.
    assert_eq!(
        u64::from_le_bytes(tables[0].1[12..20].try_into().unwrap()),
        1
    );
    assert_eq!(
        db.scan(all()).unwrap(),
        vec![(b"live".to_vec(), b"value".to_vec())]
    );
    let again = db.compact().unwrap();
    assert_eq!((again.tables_before, again.tables_after), (1, 1));
    assert_eq!(again.cleanup.removed_files, 0);
    assert!(again.cleanup.errors.is_empty());
    assert_eq!(temp.files(), files, "a sole live-only SST is not rewritten");
    db.close().unwrap();
    let mut db = temp.open();
    assert_eq!(db.get(b"live").unwrap(), Some(b"value".to_vec()));
    assert_eq!(db.get(b"removed").unwrap(), None);
    assert_eq!(db.get(b"absent").unwrap(), None);
}
