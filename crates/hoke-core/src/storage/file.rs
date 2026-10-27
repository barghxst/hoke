use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

use super::Storage;

/// An exclusively owned file with cooperative, nonblocking writer exclusion.
///
/// The lock lasts until this value is dropped. It protects cooperating openers,
/// not code that ignores locks or replaces/unlinks the path. Opening creates a
/// missing file, but does not create or synchronize its parent directory.
#[derive(Debug)]
pub struct FileStorage {
    file: File,
}

impl FileStorage {
    /// Open or create a file without truncating existing bytes, then lock it.
    ///
    /// An already locked file produces an error immediately. No file handle is
    /// exposed or cloned, so the owner cannot accidentally release its lock.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        Self::open_with_options(path.as_ref(), true, false)
    }

    /// Open an existing file without creating a missing referenced file.
    pub fn open_existing(path: impl AsRef<Path>) -> io::Result<Self> {
        Self::open_with_options(path.as_ref(), false, false)
    }

    /// Create a new file, rejecting a collision without changing existing bytes.
    pub fn create_new(path: impl AsRef<Path>) -> io::Result<Self> {
        Self::open_with_options(path.as_ref(), false, true)
    }

    fn open_with_options(path: &Path, create: bool, create_new: bool) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(create)
            .create_new(create_new)
            .truncate(false)
            .open(path)?;
        file.try_lock().map_err(io::Error::from)?;
        Ok(Self { file })
    }
}

fn check_range(offset: u64, len: usize) -> io::Result<()> {
    let len = u64::try_from(len)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "buffer length exceeds u64"))?;
    offset.checked_add(len).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "offset plus length overflows u64",
        )
    })?;
    Ok(())
}

impl Storage for FileStorage {
    fn len(&self) -> io::Result<u64> {
        Ok(self.file.metadata()?.len())
    }

    fn read_at(&mut self, offset: u64, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        // Avoid a platform-specific seek error for valid offsets past EOF.
        if offset >= self.len()? {
            return Ok(0);
        }
        check_range(offset, buffer.len())?;
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.read(buffer)
    }

    fn write_all_at(&mut self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        check_range(offset, bytes.len())?;
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.write_all(bytes)
    }

    fn set_len(&mut self, len: u64) -> io::Result<()> {
        self.file.set_len(len)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }

    fn sync_all(&mut self) -> io::Result<()> {
        self.file.sync_all()
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TempDirectory(PathBuf);

    impl TempDirectory {
        fn new() -> Self {
            loop {
                let path = std::env::temp_dir().join(format!(
                    "hoke-file-test-{}-{}",
                    std::process::id(),
                    NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
                ));
                match fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("create test directory: {error}"),
                }
            }
        }

        fn file(&self) -> PathBuf {
            self.0.join("storage")
        }
    }

    impl Drop for TempDirectory {
        fn drop(&mut self) {
            // Test files are closed first by reverse declaration/drop order.
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn read_bytes(storage: &mut FileStorage) -> Vec<u8> {
        let mut bytes = vec![0; usize::try_from(storage.len().unwrap()).unwrap()];
        let mut position = 0;
        while position < bytes.len() {
            let count = storage
                .read_at(position as u64, &mut bytes[position..])
                .unwrap();
            assert_ne!(count, 0);
            position += count;
        }
        bytes
    }

    #[test]
    fn persists_bytes_after_close_and_reopen_without_truncation() {
        let directory = TempDirectory::new();
        {
            let mut storage = FileStorage::open(directory.file()).unwrap();
            assert!(storage.is_empty().unwrap());
            storage.write_all_at(0, b"original").unwrap();
            storage.write_all_at(1, b"XY").unwrap();
            storage.flush().unwrap();
            storage.sync_all().unwrap();
        }
        let mut reopened = FileStorage::open(directory.file()).unwrap();
        assert_eq!(reopened.len().unwrap(), 8);
        assert_eq!(read_bytes(&mut reopened), b"oXYginal");
    }

    #[test]
    fn lock_conflict_preserves_contents_and_drop_releases_lock() {
        let directory = TempDirectory::new();
        let mut first = FileStorage::open(directory.file()).unwrap();
        first.write_all_at(0, b"preserve me").unwrap();
        assert!(FileStorage::open(directory.file()).is_err());
        assert_eq!(read_bytes(&mut first), b"preserve me");
        first.write_all_at(11, b"!").unwrap();
        // A failed opener must not release the original owner's lock either.
        assert!(FileStorage::open(directory.file()).is_err());
        drop(first);
        let mut next = FileStorage::open(directory.file()).unwrap();
        assert_eq!(read_bytes(&mut next), b"preserve me!");
    }

    #[test]
    fn existing_only_and_create_new_preserve_missing_and_existing_paths() {
        let directory = TempDirectory::new();
        assert_eq!(
            FileStorage::open_existing(directory.file())
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
        assert!(!directory.file().exists());
        let mut storage = FileStorage::create_new(directory.file()).unwrap();
        storage.write_all_at(0, b"preserve").unwrap();
        assert_eq!(
            FileStorage::create_new(directory.file())
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        assert!(FileStorage::open_existing(directory.file()).is_err());
        drop(storage);
        let mut reopened = FileStorage::open_existing(directory.file()).unwrap();
        assert_eq!(read_bytes(&mut reopened), b"preserve");
    }

    #[test]
    fn invalid_paths_report_errors_without_creating_parents() {
        let directory = TempDirectory::new();
        let missing_parent = directory.0.join("missing");
        assert!(FileStorage::open(missing_parent.join("storage")).is_err());
        assert!(!missing_parent.exists());
        assert!(FileStorage::open(&directory.0).is_err());
    }

    #[test]
    fn offset_reads_allow_short_reads_and_preserve_buffer_after_eof() {
        let directory = TempDirectory::new();
        let mut storage = FileStorage::open(directory.file()).unwrap();
        storage.write_all_at(0, b"abcd").unwrap();
        let mut buffer = [0xff; 8];
        let count = storage.read_at(2, &mut buffer).unwrap();
        assert!(count > 0 && count <= 2);
        assert_eq!(&buffer[..count], &b"cd"[..count]);
        assert!(buffer[count..].iter().all(|byte| *byte == 0xff));
        let mut untouched = [0xfe; 2];
        assert_eq!(storage.read_at(4, &mut untouched).unwrap(), 0);
        assert_eq!(storage.read_at(u64::MAX - 2, &mut untouched).unwrap(), 0);
        assert_eq!(untouched, [0xfe; 2]);
        assert_eq!(read_bytes(&mut storage), b"abcd");
    }

    #[test]
    fn gaps_and_extension_are_zero_filled_and_truncation_discards_bytes() {
        let directory = TempDirectory::new();
        let mut storage = FileStorage::open(directory.file()).unwrap();
        storage.write_all_at(3, b"ab").unwrap();
        assert_eq!(read_bytes(&mut storage), b"\0\0\0ab");
        storage.set_len(7).unwrap();
        assert_eq!(read_bytes(&mut storage), b"\0\0\0ab\0\0");
        storage.set_len(4).unwrap();
        assert_eq!(read_bytes(&mut storage), b"\0\0\0a");
        storage.set_len(6).unwrap();
        assert_eq!(read_bytes(&mut storage), b"\0\0\0a\0\0");
        storage.set_len(0).unwrap();
        assert!(storage.is_empty().unwrap());
    }

    #[test]
    fn empty_operations_at_extreme_offsets_have_no_effect() {
        let directory = TempDirectory::new();
        let mut storage = FileStorage::open(directory.file()).unwrap();
        storage.write_all_at(u64::MAX, b"").unwrap();
        assert!(storage.is_empty().unwrap());
        storage.write_all_at(0, b"keep").unwrap();
        storage.write_all_at(u64::MAX, b"").unwrap();
        assert_eq!(storage.read_at(u64::MAX, &mut []).unwrap(), 0);
        assert_eq!(read_bytes(&mut storage), b"keep");
    }

    #[test]
    fn write_overflow_and_reads_past_eof_preserve_bytes_and_buffers() {
        let directory = TempDirectory::new();
        let mut storage = FileStorage::open(directory.file()).unwrap();
        storage.write_all_at(0, b"keep").unwrap();
        for (offset, length) in [(u64::MAX, 1), (u64::MAX - 1, 2)] {
            let mut bytes = vec![0xff; length];
            let error = storage.write_all_at(offset, &bytes).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
            assert_eq!(storage.read_at(offset, &mut bytes).unwrap(), 0);
            assert!(bytes.iter().all(|byte| *byte == 0xff));
            assert_eq!(read_bytes(&mut storage), b"keep");
        }
    }
}
