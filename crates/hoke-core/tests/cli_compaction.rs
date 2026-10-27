use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct TempDirectory(PathBuf);
impl TempDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "hoke-compaction-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
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
fn names(path: &Path) -> Vec<String> {
    let mut names: Vec<_> = std::fs::read_dir(path)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

#[test]
fn compact_merges_runs_and_unflushed_changes_and_reclaims_old_files() {
    let directory = TempDirectory::new();
    let path = directory.database();
    for (key, value) in [("a", "old"), ("b", "deleted"), ("c", "see")] {
        success(&path, &["put", key, value]);
    }
    success(&path, &["flush"]);
    success(&path, &["put", "a", "middle"]);
    success(&path, &["delete", "b"]);
    success(&path, &["flush"]);
    success(&path, &["put", "a", "latest"]);
    success(&path, &["put", "d", "dee"]);
    let expected = success(&path, &["scan"]);
    let old_names = names(&path);
    let output = success(&path, &["compact"]);
    assert!(output.contains("OK compact (published)"));
    assert!(output.contains("tables_before=2, tables_after=1"));
    assert!(output.contains("memtable_entries=0, tables=1, wal_bytes=16"));
    assert_eq!(success(&path, &["scan"]), expected);
    assert_eq!(success(&path, &["get", "a"]), "value=\"latest\"\n");
    assert_eq!(success(&path, &["get", "b"]), "NOT_FOUND\n");
    let new_names = names(&path);
    assert_eq!(new_names.len(), 4); // LOCK, manifest, one SST and one WAL.
    for old in old_names.into_iter().filter(|name| name.starts_with('g')) {
        assert!(!new_names.contains(&old), "retained obsolete file {old}");
    }
    assert!(success(&path, &["compact"]).contains("removed_files=0"));
    success(&path, &["delete", "a"]);
    success(&path, &["compact"]);
    assert_eq!(success(&path, &["get", "a"]), "NOT_FOUND\n");
    success(&path, &["put", "a", "reinserted"]);
    success(&path, &["flush"]);
    success(&path, &["compact"]);
    assert_eq!(success(&path, &["get", "a"]), "value=\"reinserted\"\n");
}

#[test]
fn gc_removes_only_managed_orphans_and_keeps_current_data_and_unknown_files() {
    let directory = TempDirectory::new();
    let path = directory.database();
    success(&path, &["put", "key", "value"]);
    success(&path, &["flush"]);
    for name in ["notes.txt", "g00000000000000000999.unknown"] {
        std::fs::write(path.join(name), b"user data").unwrap();
    }
    let orphan = path.join("g00000000000000000999.manifest.tmp");
    std::fs::write(&orphan, b"unpublished partial data").unwrap();
    let output = success(&path, &["gc"]);
    assert!(output.contains("OK gc"));
    assert!(output.contains("removed_files=2")); // Old WAL and explicit orphan.
    assert!(!orphan.exists());
    assert_eq!(std::fs::read(path.join("notes.txt")).unwrap(), b"user data");
    assert_eq!(
        std::fs::read(path.join("g00000000000000000999.unknown")).unwrap(),
        b"user data"
    );
    assert_eq!(success(&path, &["get", "key"]), "value=\"value\"\n");
    assert!(success(&path, &["gc"]).contains("removed_files=0"));
}

#[test]
fn compact_and_gc_reject_extra_arguments_and_legacy_mode_before_creating_files() {
    let directory = TempDirectory::new();
    let path = directory.database();
    for command in ["compact", "gc"] {
        for extra in ["extra", "--limit", "--reverse"] {
            let output = invoke(&path, &[command, extra]);
            assert!(!output.status.success());
            assert!(!path.exists());
        }
        let legacy = directory.0.join("legacy.wal");
        let output = Command::new(env!("CARGO_BIN_EXE_hoke"))
            .arg(&legacy)
            .arg(command)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(!legacy.exists());
    }
}

#[test]
fn cleanup_failure_reports_published_compaction_and_remains_retryable() {
    let directory = TempDirectory::new();
    let path = directory.database();
    success(&path, &["put", "key", "value"]);
    success(&path, &["flush"]);
    success(&path, &["put", "key", "newest"]);
    // remove_file must fail on an orphan directory rather than recursively deleting it.
    let blocked = path.join("g00000000000000000999.wal");
    std::fs::create_dir(&blocked).unwrap();
    let output = invoke(&path, &["compact"]);
    assert!(!output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stdout.contains("OK compact (published)"));
    assert!(stdout.contains("memtable_entries=0, tables=1, wal_bytes=16"));
    assert!(stderr.contains("compaction published; cleanup incomplete"));
    assert!(stderr.contains("g00000000000000000999.wal"));
    assert_eq!(success(&path, &["get", "key"]), "value=\"newest\"\n");
    let output = invoke(&path, &["gc"]);
    assert!(!output.status.success());
    assert!(!String::from_utf8(output.stdout).unwrap().contains("OK gc"));
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("garbage collection cleanup incomplete")
    );
    std::fs::remove_dir(&blocked).unwrap();
    assert!(success(&path, &["gc"]).contains("OK gc"));
    assert_eq!(success(&path, &["get", "key"]), "value=\"newest\"\n");
}
