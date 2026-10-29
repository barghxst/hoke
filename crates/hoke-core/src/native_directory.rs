use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Component, Path, PathBuf};

use crate::directory::Directory;
use crate::storage::FileStorage;

#[derive(Debug)]
pub struct NativeDirectory {
    path: PathBuf,
    _lock: File,
}

impl NativeDirectory {
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = std::path::absolute(path)?;
        let created = match fs::create_dir(&path) {
            Ok(()) => true,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if !path.is_dir() {
                    return Err(io::Error::new(
                        io::ErrorKind::NotADirectory,
                        "database path is not a directory",
                    ));
                }
                false
            }
            Err(error) => return Err(error),
        };
        if created {
            let parent = path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            sync_path(parent)?;
        }

        let path = fs::canonicalize(path)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path.join("LOCK"))?;
        lock.try_lock().map_err(io::Error::from)?;
        Ok(Self { path, _lock: lock })
    }

    fn file_path(&self, name: &str) -> io::Result<PathBuf> {
        let mut components = Path::new(name).components();
        let single_component =
            matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none();
        if !single_component
            || name.is_empty()
            || name == "."
            || name == ".."
            || name == "LOCK"
            || name.contains('/')
            || name.contains('\\')
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "file name must be a single non-reserved component",
            ));
        }
        Ok(self.path.join(name))
    }
}

#[cfg(unix)]
fn sync_path(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_path(_path: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "native directory synchronization is unsupported on this platform",
    ))
}

impl Directory for NativeDirectory {
    type File = FileStorage;

    fn open_existing(&self, name: &str) -> io::Result<Self::File> {
        FileStorage::open_existing(self.file_path(name)?)
    }

    fn create_new(&self, name: &str) -> io::Result<Self::File> {
        FileStorage::create_new(self.file_path(name)?)
    }

    fn list(&self) -> io::Result<Vec<String>> {
        let mut names = Vec::new();
        for entry in fs::read_dir(&self.path)? {
            let name = entry?.file_name().into_string().map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "non-UTF-8 database file name")
            })?;
            if name != "LOCK" {
                names.push(name);
            }
        }
        names.sort();
        Ok(names)
    }

    fn rename(&self, from: &str, to: &str) -> io::Result<()> {
        fs::rename(self.file_path(from)?, self.file_path(to)?)
    }

    fn remove_file(&self, name: &str) -> io::Result<()> {
        fs::remove_file(self.file_path(name)?)
    }

    fn sync_directory(&self) -> io::Result<()> {
        sync_path(&self.path)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::storage::Storage;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Temp(PathBuf);

    impl Temp {
        fn new() -> Self {
            loop {
                let path = std::env::temp_dir().join(format!(
                    "hoke-directory-test-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
                match fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("create directory: {error}"),
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
    fn directory_lock_is_exclusive_until_drop() {
        let temp = Temp::new();
        let directory = NativeDirectory::open(&temp.0).unwrap();
        assert!(NativeDirectory::open(&temp.0).is_err());
        assert!(NativeDirectory::open(&temp.0).is_err());
        drop(directory);
        NativeDirectory::open(&temp.0).unwrap();
    }

    #[test]
    fn creates_only_final_directory_and_syncs_it() {
        let temp = Temp::new();
        let path = temp.0.join("database");
        let directory = NativeDirectory::open(&path).unwrap();
        assert!(path.is_dir());
        directory.sync_directory().unwrap();
        assert!(NativeDirectory::open(temp.0.join("missing/database")).is_err());
        assert!(!temp.0.join("missing").exists());
        fs::write(temp.0.join("plain-file"), b"keep").unwrap();
        assert!(NativeDirectory::open(temp.0.join("plain-file")).is_err());
    }

    #[test]
    fn missing_referenced_file_is_not_created_and_collisions_preserve_bytes() {
        let temp = Temp::new();
        let directory = NativeDirectory::open(&temp.0).unwrap();
        assert_eq!(
            directory.open_existing("missing").unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        assert!(!temp.0.join("missing").exists());
        let mut file = directory.create_new("existing").unwrap();
        file.write_all_at(0, b"keep").unwrap();
        assert_eq!(
            directory.create_new("existing").unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert!(directory.open_existing("existing").is_err());
        drop(file);
        assert_eq!(fs::read(temp.0.join("existing")).unwrap(), b"keep");
        directory.open_existing("existing").unwrap();
    }

    #[test]
    fn rejects_traversal_and_lock_names_before_filesystem_operations() {
        let temp = Temp::new();
        let directory = NativeDirectory::open(&temp.0).unwrap();
        for name in [
            "",
            ".",
            "..",
            "LOCK",
            "../outside",
            "/absolute",
            "sub/file",
            "sub\\file",
        ] {
            assert_eq!(
                directory.create_new(name).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
            assert_eq!(
                directory.open_existing(name).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
            assert_eq!(
                directory.rename(name, "valid").unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
            assert_eq!(
                directory.rename("valid", name).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
            assert_eq!(
                directory.remove_file(name).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
        }
        assert_eq!(directory.list().unwrap(), Vec::<String>::new());
    }

    #[test]
    fn rename_replaces_target_and_leaves_other_files_unchanged() {
        let temp = Temp::new();
        let directory = NativeDirectory::open(&temp.0).unwrap();
        fs::write(temp.0.join("MANIFEST"), b"old").unwrap();
        fs::write(temp.0.join("next.tmp"), b"new").unwrap();
        fs::write(temp.0.join("unrelated"), b"keep").unwrap();
        directory.rename("next.tmp", "MANIFEST").unwrap();
        directory.sync_directory().unwrap();
        assert_eq!(fs::read(temp.0.join("MANIFEST")).unwrap(), b"new");
        assert_eq!(fs::read(temp.0.join("unrelated")).unwrap(), b"keep");
        assert!(!temp.0.join("next.tmp").exists());
        assert!(directory.rename("missing", "MANIFEST").is_err());
        assert_eq!(fs::read(temp.0.join("MANIFEST")).unwrap(), b"new");
        assert_eq!(directory.list().unwrap(), ["MANIFEST", "unrelated"]);
    }

    #[test]
    fn list_reports_non_utf8_names() {
        use std::os::unix::ffi::OsStringExt;
        let temp = Temp::new();
        let directory = NativeDirectory::open(&temp.0).unwrap();
        let name = std::ffi::OsString::from_vec(vec![0xff]);
        fs::write(temp.0.join(name), b"bytes").unwrap();
        assert_eq!(
            directory.list().unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn remove_file_removes_only_the_named_file() {
        let temp = Temp::new();
        let directory = NativeDirectory::open(&temp.0).unwrap();
        fs::write(temp.0.join("obsolete.sst"), b"old").unwrap();
        fs::write(temp.0.join("active.sst"), b"keep").unwrap();
        directory.remove_file("obsolete.sst").unwrap();
        directory.sync_directory().unwrap();
        assert!(!temp.0.join("obsolete.sst").exists());
        assert_eq!(fs::read(temp.0.join("active.sst")).unwrap(), b"keep");
        assert!(temp.0.join("LOCK").is_file());
        assert!(NativeDirectory::open(&temp.0).is_err());
        assert_eq!(
            directory.remove_file("obsolete.sst").unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn remove_file_rejects_directories_without_removing_children() {
        let temp = Temp::new();
        let directory = NativeDirectory::open(&temp.0).unwrap();
        fs::create_dir(temp.0.join("nested")).unwrap();
        fs::write(temp.0.join("nested/child"), b"keep").unwrap();
        assert!(directory.remove_file("nested").is_err());
        assert_eq!(fs::read(temp.0.join("nested/child")).unwrap(), b"keep");
        fs::create_dir(temp.0.join("empty")).unwrap();
        assert!(directory.remove_file("empty").is_err());
        assert!(temp.0.join("empty").is_dir());
    }

    #[test]
    fn remove_file_unlinks_symlink_without_removing_its_target() {
        use std::os::unix::fs::symlink;
        let temp = Temp::new();
        let directory = NativeDirectory::open(&temp.0).unwrap();
        fs::create_dir(temp.0.join("target")).unwrap();
        fs::write(temp.0.join("target/child"), b"keep").unwrap();
        symlink("target", temp.0.join("link")).unwrap();
        directory.remove_file("link").unwrap();
        assert!(!temp.0.join("link").exists());
        assert_eq!(fs::read(temp.0.join("target/child")).unwrap(), b"keep");
    }
}
