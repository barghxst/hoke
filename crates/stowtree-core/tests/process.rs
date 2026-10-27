//! Real process termination checks on Unix (SIGKILL, without Rust destructors).
//! These demonstrate process-crash recovery, not power-loss durability.
#![cfg(unix)]

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use stowtree_core::Engine;
use stowtree_core::storage::{FileStorage, Storage};

const MODE: &str = "STOWTREE_PROCESS_TEST_MODE";
const DIRECTORY: &str = "STOWTREE_PROCESS_TEST_DIRECTORY";
const INTERRUPTED_BYTES: usize = 29;
static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct TempDirectory(PathBuf);

impl TempDirectory {
    fn new() -> Self {
        loop {
            let path = std::env::temp_dir().join(format!(
                "stowtree-process-test-{}-{}",
                std::process::id(),
                NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create process test directory: {error}"),
            }
        }
    }

    fn wal(&self) -> PathBuf {
        self.0.join("wal")
    }
}

impl Drop for TempDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct RunningChild(Child);

impl RunningChild {
    fn spawn(directory: &TempDirectory, mode: &str) -> Self {
        Self(
            Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "child_driver", "--nocapture"])
                .env(MODE, mode)
                .env(DIRECTORY, &directory.0)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        )
    }

    fn await_ready(&mut self, directory: &TempDirectory) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            // A child that exits early (including an unmatched test filter) must
            // fail immediately instead of masquerading as a successful signal.
            if let Some(status) = self.0.try_wait().unwrap() {
                panic!("child exited before termination was requested: {status}");
            }
            if directory.0.join("ready").is_file() {
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
                assert!(!status.success(), "child should have been killed");
                return;
            }
            assert!(Instant::now() < deadline, "child termination timed out");
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for RunningChild {
    fn drop(&mut self) {
        // Also executes during assertion unwinding, before the directory cleanup.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn signal_and_wait(directory: &Path) -> ! {
    fs::write(directory.join("ready"), b"ready").unwrap();
    loop {
        thread::park();
    }
}

struct InterruptedAppend {
    file: FileStorage,
    directory: PathBuf,
}

impl Storage for InterruptedAppend {
    fn len(&self) -> io::Result<u64> {
        self.file.len()
    }

    fn read_at(&mut self, offset: u64, buffer: &mut [u8]) -> io::Result<usize> {
        self.file.read_at(offset, buffer)
    }

    fn write_all_at(&mut self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        assert!(bytes.len() > INTERRUPTED_BYTES);
        assert_eq!(offset, self.file.len()?);
        self.file
            .write_all_at(offset, &bytes[..INTERRUPTED_BYTES])?;
        self.file.flush()?;
        // Make the injected prefix observable before signaling. Engine never
        // receives an Ok for the full append and never publishes this mutation.
        self.file.sync_all()?;
        signal_and_wait(&self.directory);
    }

    fn set_len(&mut self, length: u64) -> io::Result<()> {
        self.file.set_len(length)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }

    fn sync_all(&mut self) -> io::Result<()> {
        self.file.sync_all()
    }
}

#[test]
fn child_driver() {
    let Some(mode) = std::env::var_os(MODE) else {
        return;
    };
    let directory = PathBuf::from(std::env::var_os(DIRECTORY).unwrap());
    let file = FileStorage::open(directory.join("wal")).unwrap();
    match mode.to_str().unwrap() {
        "lock" => {
            let _locked_file = file;
            signal_and_wait(&directory);
        }
        "committed" => {
            let mut engine = Engine::open(file).unwrap();
            engine.put(b"committed", b"acknowledged value").unwrap();
            signal_and_wait(&directory);
        }
        "partial" => {
            let mut engine = Engine::open(InterruptedAppend {
                file,
                directory: directory.clone(),
            })
            .unwrap();
            engine.put(b"uncommitted", b"incomplete value").unwrap();
            panic!("interrupted write unexpectedly returned");
        }
        other => panic!("unknown child mode: {other}"),
    }
}

#[test]
fn another_process_owns_the_lock_until_killed_and_reaped() {
    let directory = TempDirectory::new();
    let mut child = RunningChild::spawn(&directory, "lock");
    child.await_ready(&directory);
    assert!(FileStorage::open(directory.wal()).is_err());
    child.terminate();
    let _reopened = FileStorage::open(directory.wal()).unwrap();
}

#[test]
fn acknowledged_commit_survives_process_kill_without_drop() {
    let directory = TempDirectory::new();
    let mut child = RunningChild::spawn(&directory, "committed");
    child.await_ready(&directory);
    child.terminate();
    let engine = Engine::open(FileStorage::open(directory.wal()).unwrap()).unwrap();
    assert_eq!(engine.get(b"committed"), Some(&b"acknowledged value"[..]));
    assert_eq!(engine.recovery().records_replayed, 1);
    assert_eq!(engine.recovery().discarded_tail_bytes, 0);
}

#[test]
fn interrupted_append_recovers_prior_commit_and_allows_future_commits() {
    let directory = TempDirectory::new();
    let mut seed = Engine::open(FileStorage::open(directory.wal()).unwrap()).unwrap();
    seed.put(b"prior", b"confirmed").unwrap();
    seed.close().unwrap();
    let committed_length = fs::metadata(directory.wal()).unwrap().len();

    let mut child = RunningChild::spawn(&directory, "partial");
    child.await_ready(&directory);
    assert_eq!(
        fs::metadata(directory.wal()).unwrap().len(),
        committed_length + INTERRUPTED_BYTES as u64
    );
    child.terminate();

    let mut recovered = Engine::open(FileStorage::open(directory.wal()).unwrap()).unwrap();
    assert_eq!(recovered.get(b"prior"), Some(&b"confirmed"[..]));
    assert_eq!(recovered.get(b"uncommitted"), None);
    assert_eq!(recovered.recovery().records_replayed, 1);
    assert_eq!(
        recovered.recovery().discarded_tail_bytes,
        INTERRUPTED_BYTES as u64
    );
    assert_eq!(
        fs::metadata(directory.wal()).unwrap().len(),
        committed_length
    );
    recovered.put(b"after", b"recovered write").unwrap();
    recovered.close().unwrap();

    let reopened = Engine::open(FileStorage::open(directory.wal()).unwrap()).unwrap();
    assert_eq!(reopened.get(b"prior"), Some(&b"confirmed"[..]));
    assert_eq!(reopened.get(b"after"), Some(&b"recovered write"[..]));
    assert_eq!(reopened.get(b"uncommitted"), None);
    assert_eq!(reopened.recovery().records_replayed, 2);
    assert_eq!(reopened.recovery().discarded_tail_bytes, 0);
}
