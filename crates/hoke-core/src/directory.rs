use crate::storage::Storage;
use std::io;

pub trait Directory {
    type File: Storage;
    fn open_existing(&self, name: &str) -> io::Result<Self::File>;
    fn create_new(&self, name: &str) -> io::Result<Self::File>;
    fn list(&self) -> io::Result<Vec<String>>;
    fn rename(&self, from: &str, to: &str) -> io::Result<()>;

    fn remove_file(&self, _name: &str) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "file removal is unsupported by this directory adapter",
        ))
    }
    fn sync_directory(&self) -> io::Result<()>;
}
