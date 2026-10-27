use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct TempDirectory(PathBuf);

impl TempDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "hoke-scan-cli-{}-{}",
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

#[test]
fn scans_merge_tables_and_memtable_with_bounds_prefix_and_limits() {
    let directory = TempDirectory::new();
    let path = directory.database();
    for (key, value) in [("aa", "old"), ("ab", "removed"), ("b", "bee")] {
        success(&path, &["put", key, value]);
    }
    success(&path, &["flush"]);
    success(&path, &["put", "aa", "fresh"]);
    success(&path, &["delete", "ab"]);
    success(&path, &["put", "ac", "see"]);
    success(&path, &["flush"]);
    success(&path, &["put", "aa", "newest"]);
    success(&path, &["put", "ad", "dee"]);
    assert_eq!(
        success(&path, &["scan"]),
        "key=\"aa\", value=\"newest\"\nkey=\"ac\", value=\"see\"\nkey=\"ad\", value=\"dee\"\nkey=\"b\", value=\"bee\"\n"
    );
    assert_eq!(
        success(&path, &["prefix", "a", "--limit", "2"]),
        "key=\"aa\", value=\"newest\"\nkey=\"ac\", value=\"see\"\n"
    );
    assert_eq!(
        success(&path, &["range", "ab", "b", "--reverse", "--limit", "1"]),
        "key=\"ad\", value=\"dee\"\n"
    );
    assert_eq!(
        success(&path, &["scan", "--limit", "2", "--reverse"]),
        "key=\"b\", value=\"bee\"\nkey=\"ad\", value=\"dee\"\n"
    );
    for arguments in [
        vec!["scan", "--limit", "0"],
        vec!["prefix", "missing"],
        vec!["range", "aa", "aa"],
    ] {
        assert_eq!(success(&path, &arguments), "EMPTY\n");
    }
}

#[test]
fn invalid_scan_invocations_do_not_create_database() {
    let directory = TempDirectory::new();
    let path = directory.database();
    let cases = [
        vec!["scan", "extra"],
        vec!["scan", "--unknown"],
        vec!["scan", "--limit"],
        vec!["scan", "--limit", "-1"],
        vec!["scan", "--limit", "+1"],
        vec!["scan", "--limit", "1.5"],
        vec!["scan", "--limit", "99999999999999999999999999999999999999"],
        vec!["scan", "--reverse", "--reverse"],
        vec!["scan", "--limit", "1", "--limit", "2"],
        vec!["prefix"],
        vec!["prefix", "--reverse"],
        vec!["prefix", "a", "extra"],
        vec!["range", "a"],
        vec!["range", "z", "a"],
        vec!["range", "a", "z", "extra"],
        vec!["range", "a", "z", "--limit", "-1"],
    ];
    for arguments in cases {
        let output = invoke(&path, &arguments);
        assert!(!output.status.success(), "{arguments:?}");
        assert!(!path.exists(), "created database for {arguments:?}");
    }
}

#[test]
fn scans_are_rejected_in_legacy_wal_mode_without_creating_file() {
    let directory = TempDirectory::new();
    let path = directory.0.join("legacy.wal");
    for arguments in [vec!["scan"], vec!["prefix", "a"], vec!["range", "a", "z"]] {
        let output = Command::new(env!("CARGO_BIN_EXE_hoke"))
            .arg(&path)
            .args(&arguments)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{arguments:?}");
        assert!(!path.exists());
    }
}

#[test]
fn scan_output_escapes_keys_and_values_and_accepts_empty_prefix() {
    let directory = TempDirectory::new();
    let path = directory.database();
    success(&path, &["put", "a\n\"", "value\t\""]);
    assert_eq!(
        success(&path, &["prefix", ""]),
        "key=\"a\\n\\\"\", value=\"value\\t\\\"\"\n"
    );
}

#[test]
fn existing_commands_keep_flag_shaped_keys_and_values_literal() {
    let directory = TempDirectory::new();
    let path = directory.database();
    success(&path, &["put", "--limit", "--reverse"]);
    assert_eq!(success(&path, &["get", "--limit"]), "value=\"--reverse\"\n");
    assert_eq!(
        success(&path, &["scan"]),
        "key=\"--limit\", value=\"--reverse\"\n"
    );
    success(&path, &["delete", "--limit"]);
    assert_eq!(success(&path, &["scan"]), "EMPTY\n");
}
