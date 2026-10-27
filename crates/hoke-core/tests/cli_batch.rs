use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use hoke_core::MAX_BATCH_OPERATIONS;

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct TempDirectory(PathBuf);

impl TempDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "hoke-batch-cli-{}-{}",
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
fn batch_orders_duplicates_deletes_and_reinsertions_and_persists_on_reopen() {
    let directory = TempDirectory::new();
    let path = directory.database();
    success(&path, &["put", "removed", "old"]);
    success(&path, &["flush"]);
    success(&path, &["put", "pending", "wal"]);
    let output = success(
        &path,
        &[
            "batch", "put", "first", "old", "delete", "removed", "put", "first", "new", "delete",
            "first", "put", "first", "last", "put", "second", "two",
        ],
    );
    assert_eq!(
        output,
        "OK batch (published)\noperations=6\nmemtable_entries=0, tables=2, wal_bytes=16, memtable_bytes=0\n"
    );
    assert_eq!(success(&path, &["get", "first"]), "value=\"last\"\n");
    assert_eq!(success(&path, &["get", "removed"]), "NOT_FOUND\n");
    assert_eq!(
        success(&path, &["scan"]),
        "key=\"first\", value=\"last\"\nkey=\"pending\", value=\"wal\"\nkey=\"second\", value=\"two\"\n"
    );
    assert_eq!(
        success(&path, &["info"]),
        "memtable_entries=0, tables=2, wal_bytes=16, memtable_bytes=0\n"
    );
    success(&path, &["put", "after", "normal"]);
    success(&path, &["flush"]);
    success(&path, &["compact"]);
    assert_eq!(success(&path, &["get", "first"]), "value=\"last\"\n");
    assert_eq!(success(&path, &["get", "removed"]), "NOT_FOUND\n");
    assert_eq!(success(&path, &["get", "after"]), "value=\"normal\"\n");
}

#[test]
fn invalid_batch_invocations_do_not_create_or_modify_database() {
    let directory = TempDirectory::new();
    let absent = directory.database();
    let existing = directory.0.join("existing");
    success(&existing, &["put", "keep", "confirmed"]);
    let before = snapshot(&existing);
    let mut cases = vec![
        vec!["batch", "unknown"],
        vec!["batch", "put"],
        vec!["batch", "put", "key"],
        vec!["batch", "delete"],
        vec!["batch", "put", "key", "value", "unknown"],
        vec!["batch", "put", "key", "value", "delete"],
        vec!["batch", "--reverse"],
    ];
    let mut too_many = vec!["batch"];
    for _ in 0..=MAX_BATCH_OPERATIONS {
        too_many.extend(["delete", "key"]);
    }
    cases.push(too_many);
    let oversized_key = "k".repeat(64 * 1024 + 1);
    cases.push(vec!["batch", "delete", &oversized_key]);
    for arguments in cases {
        for path in [&absent, &existing] {
            let output = invoke(path, &arguments);
            assert!(!output.status.success(), "unexpected success");
            assert!(output.stdout.is_empty());
        }
        assert!(!absent.exists());
        assert_eq!(snapshot(&existing), before);
    }
    assert_eq!(
        success(&existing, &["get", "keep"]),
        "value=\"confirmed\"\n"
    );
}

#[test]
fn batch_treats_operation_names_and_flags_in_keys_and_values_as_literals() {
    let directory = TempDirectory::new();
    let path = directory.database();
    success(
        &path,
        &[
            "batch",
            "put",
            "put",
            "delete",
            "put",
            "delete",
            "put",
            "delete",
            "delete",
            "put",
            "--limit",
            "--reverse",
            "put",
            "",
            "",
        ],
    );
    assert_eq!(success(&path, &["get", "put"]), "value=\"delete\"\n");
    assert_eq!(success(&path, &["get", "delete"]), "NOT_FOUND\n");
    assert_eq!(success(&path, &["get", "--limit"]), "value=\"--reverse\"\n");
    assert_eq!(success(&path, &["get", ""]), "value=\"\"\n");
}

#[test]
fn empty_batch_is_allowed_and_does_not_flush_or_rewrite_existing_database() {
    let directory = TempDirectory::new();
    let path = directory.database();
    assert_eq!(
        success(&path, &["batch"]),
        "OK batch (published)\noperations=0\nmemtable_entries=0, tables=0, wal_bytes=16, memtable_bytes=0\n"
    );
    success(&path, &["put", "pending", "value"]);
    let before = snapshot(&path);
    let output = success(&path, &["batch"]);
    assert!(
        output.starts_with("OK batch (published)\noperations=0\nmemtable_entries=1, tables=0,")
    );
    assert_eq!(snapshot(&path), before);
    assert_eq!(success(&path, &["get", "pending"]), "value=\"value\"\n");
}

#[test]
fn legacy_wal_mode_rejects_batch_without_creating_file() {
    let directory = TempDirectory::new();
    let path = directory.0.join("legacy.wal");
    for arguments in [vec!["batch"], vec!["batch", "put", "key", "value"]] {
        let output = Command::new(env!("CARGO_BIN_EXE_hoke"))
            .arg(&path)
            .args(arguments)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(!path.exists());
    }
}
