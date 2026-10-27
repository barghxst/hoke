//! Multi-file storage for immutable runs and manifest publication.
use crate::storage::Storage;
use std::io;

/// Owns an exclusive database-directory lock for its entire lifetime.
/// Implementors must not create missing referenced files or overwrite create_new.
/// Rename must atomically replace the destination in the same directory; failure
/// may leave the old or new name visible and requires reopen before mutations.
pub trait Directory {
    type File: Storage;
    fn open_existing(&self, name: &str) -> io::Result<Self::File>;
    fn create_new(&self, name: &str) -> io::Result<Self::File>;
    fn list(&self) -> io::Result<Vec<String>>;
    fn rename(&self, from: &str, to: &str) -> io::Result<()>;
    /// Remove one file without following it into a directory or synchronizing
    /// the parent directory. Callers must establish publication and cleanup
    /// durability barriers explicitly through `sync_directory`.
    ///
    /// Existing adapters may omit this optional garbage-collection primitive.
    fn remove_file(&self, _name: &str) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "file removal is unsupported by this directory adapter",
        ))
    }
    fn sync_directory(&self) -> io::Result<()>;
}
