use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct TempDirectory(PathBuf);

impl TempDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "hoke-threshold-cli-{}-{}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn database(&self) -> PathBuf {
        self.0.join("database")
    }
}

impl Drop for TempDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn invoke(path: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_hoke"))
        .arg("--db")
        .arg(path)
        .args(arguments)
        .output()
        .unwrap()
}

fn success(path: &Path, arguments: &[&str]) -> String {
    let output = invoke(path, arguments);
    assert!(
        output.status.success(),
        "{arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn snapshot(path: &Path) -> Vec<(String, Vec<u8>)> {
    let mut files: Vec<_> = std::fs::read_dir(path)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (
                entry.file_name().into_string().unwrap(),
                std::fs::read(entry.path()).unwrap(),
            )
        })
        .collect();
    files.sort();
    files
}

#[test]
fn successive_processes_recover_payload_and_flush_only_before_exceeding_threshold() {
    let directory = TempDirectory::new();
    let path = directory.database();
    success(&path, &["--memtable-limit", "4", "put", "a", "1"]);
    success(&path, &["--memtable-limit", "4", "put", "b", "2"]);
    let at_limit = success(&path, &["info"]);
    assert!(at_limit.starts_with("memtable_entries=2, tables=0,"));
    assert!(at_limit.ends_with("memtable_bytes=4\n"));
    let before = snapshot(&path);

    success(&path, &["--memtable-limit", "1", "info"]);
    success(&path, &["--memtable-limit", "1", "get", "a"]);
    success(&path, &["--memtable-limit", "1", "scan"]);
    assert_eq!(snapshot(&path), before);

    success(&path, &["--memtable-limit", "4", "put", "c", "3"]);
    let after = success(&path, &["info"]);
    assert!(after.starts_with("memtable_entries=1, tables=1,"));
    assert!(after.ends_with("memtable_bytes=2\n"));
    success(&path, &["--memtable-limit", "4", "put", "d", "44"]);
    assert!(success(&path, &["info"]).starts_with("memtable_entries=1, tables=2,"));
    assert_eq!(success(&path, &["get", "a"]), "value=\"1\"\n");
    assert_eq!(
        success(&path, &["scan"]),
        "key=\"a\", value=\"1\"\nkey=\"b\", value=\"2\"\nkey=\"c\", value=\"3\"\nkey=\"d\", value=\"44\"\n"
    );
}

#[test]
fn threshold_is_not_persisted_and_manual_mode_remains_default() {
    let directory = TempDirectory::new();
    let path = directory.database();

    success(&path, &["--memtable-limit", "1", "put", "first", "large"]);
    success(&path, &["put", "second", "larger"]);
    let info = success(&path, &["info"]);
    assert!(info.starts_with("memtable_entries=2, tables=0,"));
    assert!(info.ends_with("memtable_bytes=22\n"));
    success(&path, &["--memtable-limit", "1", "delete", "first"]);
    assert!(success(&path, &["info"]).starts_with("memtable_entries=1, tables=1,"));
    assert_eq!(success(&path, &["get", "first"]), "NOT_FOUND\n");
    assert_eq!(success(&path, &["get", "second"]), "value=\"larger\"\n");
}

#[test]
fn invalid_flags_fail_before_creating_or_modifying_files() {
    let directory = TempDirectory::new();
    let absent = directory.database();
    let existing = directory.0.join("existing");
    success(&existing, &["put", "keep", "confirmed"]);
    let before = snapshot(&existing);
    let cases = [
        vec!["--memtable-limit"],
        vec!["--memtable-limit", "4"],
        vec!["--memtable-limit", "0", "info"],
        vec!["--memtable-limit", "-1", "info"],
        vec!["--memtable-limit", "+1", "info"],
        vec!["--memtable-limit", "1.0", "info"],
        vec!["--memtable-limit", "", "info"],
        vec!["--memtable-limit", "１２", "info"],
        vec!["--memtable-limit", "9999999999999999999999999999", "info"],
        vec!["--memtable-limit", "4", "--memtable-limit", "4", "info"],
        vec!["--memtable-limit=4", "info"],
        vec!["info", "--memtable-limit", "4"],
        vec!["--memtable-limit", "4", "put", "key"],
    ];
    for arguments in cases {
        for path in [&absent, &existing] {
            let output = invoke(path, &arguments);
            assert!(
                !output.status.success(),
                "unexpected success: {arguments:?}"
            );
            assert!(output.stdout.is_empty());
        }
        assert!(!absent.exists());
        assert_eq!(snapshot(&existing), before);
    }
    let legacy = directory.0.join("legacy.wal");
    let output = Command::new(env!("CARGO_BIN_EXE_hoke"))
        .arg(&legacy)
        .args(["--memtable-limit", "4", "info"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!legacy.exists());
}

#[test]
fn flags_in_key_and_value_positions_remain_literals() {
    let directory = TempDirectory::new();
    let path = directory.database();
    success(
        &path,
        &[
            "--memtable-limit",
            "40",
            "put",
            "--memtable-limit",
            "--memtable-limit",
        ],
    );
    assert_eq!(
        success(&path, &["get", "--memtable-limit"]),
        "value=\"--memtable-limit\"\n"
    );
    success(
        &path,
        &[
            "--memtable-limit",
            "1",
            "batch",
            "put",
            "--memtable-limit",
            "batch",
            "put",
            "other",
            "value",
        ],
    );
    assert_eq!(
        success(&path, &["get", "--memtable-limit"]),
        "value=\"batch\"\n"
    );
    assert_eq!(success(&path, &["get", "other"]), "value=\"value\"\n");
    assert!(success(&path, &["info"]).ends_with("memtable_bytes=0\n"));
}
