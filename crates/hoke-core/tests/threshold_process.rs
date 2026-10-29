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
use hoke_core::{Database, DatabaseOptions, NativeDirectory};

const MODE: &str = "HOKE_THRESHOLD_PROCESS_TEST_MODE";
const ROOT: &str = "HOKE_THRESHOLD_PROCESS_TEST_ROOT";
static NEXT: AtomicU64 = AtomicU64::new(0);

struct TempDirectory(PathBuf);

impl TempDirectory {
    fn new() -> Self {
        loop {
            let path = std::env::temp_dir().join(format!(
                "hoke-threshold-process-test-{}-{}",
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
                .args(["--exact", "threshold_child_driver", "--nocapture"])
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
                panic!("child exited before reaching manifest boundary: {status}");
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
                    "child must be killed before flush returns"
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
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct PausedPublication {
    directory: NativeDirectory,
    armed: Rc<Cell<bool>>,
    pause_after_rename: bool,
    ready: PathBuf,
}

impl PausedPublication {
    fn pause(&self) -> ! {
        fs::write(&self.ready, b"ready").unwrap();
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
        if self.armed.get() {
            assert_eq!(to, "MANIFEST");
            if !self.pause_after_rename {
                self.pause();
            }
        }
        self.directory.rename(from, to)?;
        if self.armed.get() {
            self.pause();
        }
        Ok(())
    }

    fn sync_directory(&self) -> io::Result<()> {
        self.directory.sync_directory()
    }
}

#[test]
fn threshold_child_driver() {
    let Some(mode) = std::env::var_os(MODE) else {
        return;
    };
    let root = PathBuf::from(std::env::var_os(ROOT).unwrap());
    let pause_after_rename = match mode.to_str().unwrap() {
        "before" => false,
        "after" => true,
        other => panic!("unknown process mode: {other}"),
    };
    let armed = Rc::new(Cell::new(false));
    let directory = PausedPublication {
        directory: NativeDirectory::open(root.join("database")).unwrap(),
        armed: Rc::clone(&armed),
        pause_after_rename,
        ready: root.join("ready"),
    };
    let mut database = Database::open_with_options(
        directory,
        DatabaseOptions {
            max_memtable_bytes: Some(100),
            ..Default::default()
        },
    )
    .unwrap();
    database.put(b"updated", b"acknowledged update").unwrap();
    database.delete(b"deleted").unwrap();
    database
        .put(b"inserted", b"acknowledged insertion")
        .unwrap();
    armed.set(true);
    database.put(b"unrequested", &[1; 100]).unwrap();
    panic!("threshold preflush unexpectedly returned instead of pausing");
}

fn assert_committed(database: &mut Database<NativeDirectory>) {
    assert_eq!(database.get(b"unrequested").unwrap(), None);
    assert_eq!(
        database.get(b"updated").unwrap(),
        Some(b"acknowledged update".to_vec())
    );
    assert_eq!(database.get(b"deleted").unwrap(), None);
    assert_eq!(
        database.get(b"inserted").unwrap(),
        Some(b"acknowledged insertion".to_vec())
    );
    assert_eq!(
        database.get(b"untouched").unwrap(),
        Some(b"original".to_vec())
    );
}

fn kill_at_publication(mode: &str) {
    let root = TempDirectory::new();
    let mut seed = Database::open(NativeDirectory::open(root.database()).unwrap()).unwrap();
    seed.put(b"updated", b"old value").unwrap();
    seed.put(b"deleted", b"must not resurrect").unwrap();
    seed.put(b"untouched", b"original").unwrap();
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
    assert_committed(&mut recovered);

    recovered.put(b"after", b"recovery works").unwrap();
    recovered.flush().unwrap();
    recovered.close().unwrap();
    let mut reopened = Database::open(NativeDirectory::open(root.database()).unwrap()).unwrap();
    assert_committed(&mut reopened);
    assert_eq!(
        reopened.get(b"after").unwrap(),
        Some(b"recovery works".to_vec())
    );
}

#[test]
fn preflush_excludes_new_operation_and_preserves_acks_on_kill_before_manifest_rename() {
    kill_at_publication("before");
}

#[test]
fn preflush_excludes_new_operation_and_preserves_acks_on_kill_after_manifest_rename_before_directory_sync()
 {
    kill_at_publication("after");
}
