use std::io;

#[cfg(not(target_family = "wasm"))]
mod file;
mod memory;

#[cfg(not(target_family = "wasm"))]
pub use file::FileStorage;
pub use memory::MemoryStorage;

pub trait Storage {
    fn len(&self) -> io::Result<u64>;

    fn is_empty(&self) -> io::Result<bool> {
        Ok(self.len()? == 0)
    }

    fn read_at(&mut self, offset: u64, buffer: &mut [u8]) -> io::Result<usize>;

    fn write_all_at(&mut self, offset: u64, bytes: &[u8]) -> io::Result<()>;

    fn set_len(&mut self, len: u64) -> io::Result<()>;

    fn flush(&mut self) -> io::Result<()>;

    fn sync_all(&mut self) -> io::Result<()>;
}
