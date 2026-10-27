//! Kill a real process before/after atomic batch publication and after ACK.
//! These checks exercise process crashes, not loss of power or filesystem caches.
#![cfg(unix)]

use std::cell::Cell;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use hoke_core::directory::Directory;
use hoke_core::storage::FileStorage;
use hoke_core::{BatchOperation, Database, NativeDirectory};

const MODE: &str = "HOKE_BATCH_PROCESS_TEST_MODE";
const ROOT: &str = "HOKE_BATCH_PROCESS_TEST_ROOT";
static NEXT: AtomicU64 = AtomicU64::new(0);

struct TempDirectory(PathBuf);

impl TempDirectory {
    fn new() -> Self {
        loop {
            let path = std::env::temp_dir().join(format!(
                "hoke-batch-process-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create process test directory: {error}"),
            }
        }
    }

    fn database(&self) -> PathBuf {
        self.0.join("database")
    }
}

impl Drop for TempDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct RunningChild(Child);

impl RunningChild {
    fn spawn(root: &TempDirectory, mode: &str) -> Self {
        Self(
            Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "batch_child_driver", "--nocapture"])
                .env(MODE, mode)
                .env(ROOT, &root.0)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        )
    }

    fn await_ready(&mut self, root: &TempDirectory) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                panic!("child exited before reaching batch boundary: {status}");
            }
            if root.0.join("ready").is_file() {
                return;
            }
            assert!(Instant::now() < deadline, "child readiness timed out");
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn terminate(&mut self) {
        self.0.kill().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                assert!(
                    !status.success(),
                    "child must be killed at the requested batch boundary"
                );
                return;
            }
            assert!(Instant::now() < deadline, "child termination timed out");
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for RunningChild {
    fn drop(&mut self) {
        // Clean up even on assertion failure, before deleting the test files.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct PausedPublication {
    directory: NativeDirectory,
    armed: Rc<Cell<bool>>,
    mode: String,
    ready: PathBuf,
}

impl PausedPublication {
    fn pause(&self, boundary: &str) -> ! {
        // The marker is outside the database and is emitted only after every
        // acknowledged mutation has returned successfully in the child driver.
        let pending = self.ready.with_extension("pending");
        fs::write(&pending, boundary).unwrap();
        fs::rename(pending, &self.ready).unwrap();
        loop {
            thread::park();
        }
    }
}

impl Directory for PausedPublication {
    type File = FileStorage;

    fn open_existing(&self, name: &str) -> io::Result<Self::File> {
        self.directory.open_existing(name)
    }

    fn create_new(&self, name: &str) -> io::Result<Self::File> {
        self.directory.create_new(name)
    }

    fn list(&self) -> io::Result<Vec<String>> {
        self.directory.list()
    }

    fn rename(&self, from: &str, to: &str) -> io::Result<()> {
        if self.armed.get() && self.mode == "before" {
            assert_eq!(to, "MANIFEST");
            self.pause("before manifest rename");
        }
        self.directory.rename(from, to)?;
        if self.armed.get() && self.mode == "after" {
            // Pause before returning to Database, hence before directory fsync
            // and switching the in-memory WAL/table handles.
            self.pause("after manifest rename");
        }
        Ok(())
    }

    fn sync_directory(&self) -> io::Result<()> {
        self.directory.sync_directory()
    }
}

#[test]
fn batch_child_driver() {
    let Some(mode) = std::env::var_os(MODE) else {
        return;
    };
    let root = PathBuf::from(std::env::var_os(ROOT).unwrap());
    let mode = mode.into_string().unwrap();
    assert!(["before", "after", "ack"].contains(&mode.as_str()));
    let armed = Rc::new(Cell::new(false));
    let directory = PausedPublication {
        directory: NativeDirectory::open(root.join("database")).unwrap(),
        armed: Rc::clone(&armed),
        mode,
        ready: root.join("ready"),
    };
    let mut database = Database::open(directory).unwrap();
    database.put(b"updated", b"acknowledged update").unwrap();
    database.delete(b"deleted").unwrap();
    database
        .put(b"inserted", b"acknowledged insertion")
        .unwrap();
    armed.set(true);
    database
        .write_batch(&[
            BatchOperation::Put {
                key: b"updated".to_vec(),
                value: b"batch value".to_vec(),
            },
            BatchOperation::Delete {
                key: b"untouched".to_vec(),
            },
            BatchOperation::Put {
                key: b"new".to_vec(),
                value: b"superseded".to_vec(),
            },
            BatchOperation::Delete {
                key: b"new".to_vec(),
            },
            BatchOperation::Put {
                key: b"new".to_vec(),
                value: b"last wins".to_vec(),
            },
        ])
        .unwrap();
    assert_eq!(std::env::var(MODE).unwrap(), "ack");
    // Publication and write_batch's success acknowledgement precede this marker.
    // Kill while Database still owns its locks and open files.
    let pending = root.join("ready.pending");
    fs::write(&pending, "acknowledged batch").unwrap();
    fs::rename(pending, root.join("ready")).unwrap();
    loop {
        thread::park();
    }
}

fn assert_state(database: &mut Database<NativeDirectory>, has_batch: bool) {
    assert_eq!(
        database.get(b"updated").unwrap(),
        Some(if has_batch {
            b"batch value".to_vec()
        } else {
            b"acknowledged update".to_vec()
        })
    );
    assert_eq!(
        database.get(b"untouched").unwrap(),
        if has_batch {
            None
        } else {
            Some(b"original".to_vec())
        }
    );
    assert_eq!(
        database.get(b"new").unwrap(),
        if has_batch {
            Some(b"last wins".to_vec())
        } else {
            None
        }
    );
    // These confirmed changes are outside the batch and remain in both possible
    // manifest states, including an acknowledged deletion in the old WAL.
    assert_eq!(database.get(b"deleted").unwrap(), None);
    assert_eq!(
        database.get(b"inserted").unwrap(),
        Some(b"acknowledged insertion".to_vec())
    );
    assert_eq!(
        database.get(b"second-table").unwrap(),
        Some(b"retained".to_vec())
    );
    let rows = database.scan(Default::default()).unwrap();
    assert_eq!(
        rows.iter()
            .filter(|(key, _)| key.as_slice() != b"after")
            .count(),
        4
    );
}

fn kill_at_publication(mode: &str) {
    let root = TempDirectory::new();
    let mut seed = Database::open(NativeDirectory::open(root.database()).unwrap()).unwrap();
    seed.put(b"updated", b"old value").unwrap();
    seed.put(b"deleted", b"must not resurrect").unwrap();
    seed.put(b"untouched", b"original").unwrap();
    seed.flush().unwrap();
    seed.put(b"second-table", b"retained").unwrap();
    seed.flush().unwrap();
    seed.close().unwrap();
    let manifest = root.database().join("MANIFEST");
    let initial_manifest = fs::read(&manifest).unwrap();

    let mut child = RunningChild::spawn(&root, mode);
    child.await_ready(&root);
    assert!(NativeDirectory::open(root.database()).is_err());
    let visible_manifest = fs::read(&manifest).unwrap();
    if mode == "before" {
        assert_eq!(visible_manifest, initial_manifest);
    } else {
        assert_ne!(visible_manifest, initial_manifest);
    }
    child.terminate();

    let mut recovered = Database::open(NativeDirectory::open(root.database()).unwrap()).unwrap();
    assert_state(&mut recovered, mode != "before");
    // Recovery must also leave a writable database, including generation choice
    // above the orphan SST/WAL/temp files from a pre-publication termination.
    recovered.put(b"after", b"recovery works").unwrap();
    recovered.flush().unwrap();
    let compacted = recovered.compact().unwrap();
    assert!(compacted.cleanup.errors.is_empty());
    let cleanup = recovered.collect_garbage().unwrap();
    assert!(cleanup.errors.is_empty());
    recovered.close().unwrap();
    let mut reopened = Database::open(NativeDirectory::open(root.database()).unwrap()).unwrap();
    assert_state(&mut reopened, mode != "before");
    assert_eq!(
        reopened.get(b"after").unwrap(),
        Some(b"recovery works".to_vec())
    );
}

#[test]
fn no_batch_changes_visible_after_kill_before_manifest_rename() {
    kill_at_publication("before");
}

#[test]
fn whole_batch_visible_after_kill_after_manifest_rename_before_directory_sync() {
    kill_at_publication("after");
}

#[test]
fn batch_survives_kill_after_success_acknowledgement_without_destructors() {
    kill_at_publication("ack");
}
