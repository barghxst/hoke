//! Bounded temporary SST output; no persistent publication occurs here.
use super::ContainerDbError;
use crate::container::MAX_SEGMENT_BYTES;
use crate::storage::{MemoryStorage, Storage};
use std::io;

#[derive(Default)]
pub(super) struct CappedStorage {
    inner: MemoryStorage,
    pub(super) exceeded: bool,
}
impl CappedStorage {
    fn check(&mut self, end: u64) -> io::Result<()> {
        if end > MAX_SEGMENT_BYTES {
            self.exceeded = true;
            return Err(io::Error::other("SST staging cap"));
        }
        Ok(())
    }
    pub(super) fn bytes(self) -> Result<Vec<u8>, ContainerDbError> {
        Ok(self.inner.into_bytes())
    }
}
impl Storage for CappedStorage {
    fn len(&self) -> io::Result<u64> {
        self.inner.len()
    }
    fn read_at(&mut self, offset: u64, buffer: &mut [u8]) -> io::Result<usize> {
        self.inner.read_at(offset, buffer)
    }
    fn write_all_at(&mut self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        self.check(
            offset
                .checked_add(bytes.len() as u64)
                .ok_or_else(|| io::Error::other("staging offset"))?,
        )?;
        self.inner.write_all_at(offset, bytes)
    }
    fn set_len(&mut self, len: u64) -> io::Result<()> {
        self.check(len)?;
        self.inner.set_len(len)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
    fn sync_all(&mut self) -> io::Result<()> {
        self.inner.sync_all()
    }
}
