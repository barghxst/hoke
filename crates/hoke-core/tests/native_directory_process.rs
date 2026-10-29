#![cfg(unix)]

use hoke_core::{Database, NativeDirectory};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);
const CHILD_ROOT: &str = "HOKE_DIRECTORY_CWD_CHILD_ROOT";

struct Temp(PathBuf);

impl Temp {
    fn new() -> Self {
        loop {
            let path = std::env::temp_dir().join(format!(
                "hoke-directory-cwd-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create temporary directory: {error}"),
            }
        }
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn relative_directory_remains_locked_to_its_original_location_after_cwd_change() {
    let temp = Temp::new();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "cwd_child", "--nocapture"])
        .env(CHILD_ROOT, &temp.0)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "cwd regression failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn cwd_child() {
    let Some(root) = std::env::var_os(CHILD_ROOT) else {
        return;
    };
    let root = PathBuf::from(root);
    let original = root.join("original");
    let other = root.join("other");
    fs::create_dir(&original).unwrap();
    fs::create_dir(&other).unwrap();
    let mut other_db = Database::open(NativeDirectory::open(other.join("db")).unwrap()).unwrap();
    other_db.put(b"other", b"must remain").unwrap();

    std::env::set_current_dir(&original).unwrap();
    let mut db = Database::open(NativeDirectory::open("db").unwrap()).unwrap();
    db.put(b"before", b"first acknowledged").unwrap();
    std::env::set_current_dir(&other).unwrap();
    db.flush().unwrap();
    db.put(b"after", b"second acknowledged").unwrap();
    db.compact().unwrap();
    db.close().unwrap();

    assert_eq!(
        other_db.get(b"other").unwrap(),
        Some(b"must remain".to_vec())
    );
    assert_eq!(other_db.get(b"before").unwrap(), None);
    other_db.close().unwrap();
    let mut other_db = Database::open(NativeDirectory::open(other.join("db")).unwrap()).unwrap();
    assert_eq!(
        other_db.get(b"other").unwrap(),
        Some(b"must remain".to_vec())
    );
    assert_eq!(other_db.get(b"before").unwrap(), None);
    other_db.close().unwrap();

    let mut db = Database::open(NativeDirectory::open(original.join("db")).unwrap()).unwrap();
    assert_eq!(
        db.get(b"before").unwrap(),
        Some(b"first acknowledged".to_vec())
    );
    assert_eq!(
        db.get(b"after").unwrap(),
        Some(b"second acknowledged".to_vec())
    );
    db.close().unwrap();
    std::env::set_current_dir(root).unwrap();
}
