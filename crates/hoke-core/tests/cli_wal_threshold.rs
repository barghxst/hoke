use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct TempDirectory(PathBuf);

impl TempDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "hoke-wal-threshold-cli-{}-{}",
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
fn repeated_overwrites_flush_before_exceeding_recovered_wal_limit() {
    let directory = TempDirectory::new();
    let path = directory.database();
    success(&path, &["--wal-limit", "90", "put", "a", "1234"]);
    assert_eq!(
        success(&path, &["info"]),
        "memtable_entries=1, tables=0, wal_bytes=53, memtable_bytes=5\n"
    );
    success(&path, &["--wal-limit", "90", "put", "a", "5678"]);
    assert_eq!(
        success(&path, &["info"]),
        "memtable_entries=1, tables=0, wal_bytes=90, memtable_bytes=5\n"
    );
    let before = snapshot(&path);
    for args in [
        vec!["--wal-limit", "16", "info"],
        vec!["--wal-limit", "16", "get", "a"],
        vec!["--wal-limit", "16", "scan"],
    ] {
        success(&path, &args);
    }
    assert_eq!(snapshot(&path), before);
    success(&path, &["--wal-limit", "90", "put", "a", "9999"]);
    assert_eq!(
        success(&path, &["info"]),
        "memtable_entries=1, tables=1, wal_bytes=53, memtable_bytes=5\n"
    );
    assert_eq!(success(&path, &["get", "a"]), "value=\"9999\"\n");
}

#[test]
fn both_flags_accept_either_order_and_default_does_not_persist_policy() {
    let directory = TempDirectory::new();
    for (index, flags) in [
        ["--wal-limit", "90", "--memtable-limit", "100"],
        ["--memtable-limit", "100", "--wal-limit", "90"],
    ]
    .iter()
    .enumerate()
    {
        let path = directory.0.join(index.to_string());
        let mut args = flags.to_vec();
        args.extend(["put", "a", "1234"]);
        success(&path, &args);
        success(&path, &["put", "a", "5678"]);
        success(&path, &["put", "a", "9999"]);
        assert_eq!(
            success(&path, &["info"]),
            "memtable_entries=1, tables=0, wal_bytes=127, memtable_bytes=5\n"
        );
        let mut args = flags.to_vec();
        args.extend(["put", "a", "done"]);
        success(&path, &args);
        assert_eq!(
            success(&path, &["info"]),
            "memtable_entries=1, tables=1, wal_bytes=53, memtable_bytes=5\n"
        );
    }
    let path = directory.database();
    success(
        &path,
        &["--wal-limit", "18446744073709551615", "put", "", ""],
    );
    success(&path, &["--wal-limit", "16", "delete", ""]);
    assert!(success(&path, &["info"]).contains("tables=1, wal_bytes=48"));
}

#[test]
fn invalid_wal_flags_leave_absent_and_existing_paths_untouched() {
    let directory = TempDirectory::new();
    let absent = directory.database();
    let existing = directory.0.join("existing");
    success(&existing, &["put", "keep", "confirmed"]);
    let before = snapshot(&existing);
    let mut cases = vec![
        vec!["--wal-limit"],
        vec!["--wal-limit", "90"],
        vec!["--wal-limit", "90", "--wal-limit", "90", "info"],
        vec![
            "--wal-limit",
            "90",
            "--memtable-limit",
            "2",
            "--wal-limit",
            "90",
            "info",
        ],
        vec![
            "--wal-limit",
            "90",
            "--memtable-limit",
            "2",
            "--memtable-limit",
            "2",
            "info",
        ],
        vec!["--wal-limit=90", "info"],
        vec!["--unknown", "90", "info"],
        vec!["info", "--wal-limit", "90"],
        vec!["--wal-limit", "90", "put", "a"],
    ];
    for bad in [
        "0",
        "15",
        "-1",
        "+90",
        "90.0",
        "",
        "９０",
        "18446744073709551616",
    ] {
        cases.push(vec!["--wal-limit", bad, "info"]);
    }
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
    for exists in [false, true] {
        if exists {
            std::fs::write(&legacy, b"unchanged").unwrap();
        }
        let output = Command::new(env!("CARGO_BIN_EXE_hoke"))
            .arg(&legacy)
            .args(["--wal-limit", "90", "info"])
            .output()
            .unwrap();
        assert!(!output.status.success());
        if exists {
            assert_eq!(std::fs::read(&legacy).unwrap(), b"unchanged");
        } else {
            assert!(!legacy.exists());
        }
    }
}

#[test]
fn command_and_batch_key_values_preserve_wal_flag_literals() {
    let directory = TempDirectory::new();
    let path = directory.database();
    success(
        &path,
        &[
            "--wal-limit",
            "90",
            "put",
            "--wal-limit",
            "--memtable-limit",
        ],
    );
    assert_eq!(
        success(&path, &["get", "--wal-limit"]),
        "value=\"--memtable-limit\"\n"
    );
    success(
        &path,
        &[
            "--wal-limit",
            "16",
            "batch",
            "put",
            "--wal-limit",
            "--wal-limit",
            "put",
            "--memtable-limit",
            "--wal-limit",
        ],
    );
    assert_eq!(
        success(&path, &["get", "--wal-limit"]),
        "value=\"--wal-limit\"\n"
    );
    assert_eq!(
        success(&path, &["get", "--memtable-limit"]),
        "value=\"--wal-limit\"\n"
    );
    assert!(success(&path, &["info"]).contains("memtable_entries=0, tables=1, wal_bytes=16"));
    success(&path, &["--wal-limit", "16", "delete", "--wal-limit"]);
    assert_eq!(success(&path, &["get", "--wal-limit"]), "NOT_FOUND\n");
}
