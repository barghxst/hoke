//! A synchronous byte-storage contract, independent of platform APIs.
//!
//! Offsets are explicit: callers never share an implicit cursor. This contract is
//! deliberately below the engine's key/value API. It does not define a file format.

use std::io;

#[cfg(not(target_family = "wasm"))]
mod file;
mod memory;

#[cfg(not(target_family = "wasm"))]
pub use file::FileStorage;
pub use memory::MemoryStorage;

/// A single exclusively owned byte store used by the storage engines.
///
/// A successful write writes all supplied bytes; a failed write may have written
/// a prefix. Callers must not assume rollback or continue committing after an I/O
/// error without a recovery protocol. No backend may silently fall back to memory.
///
/// `flush` drains implementation buffers; only `sync_all` requests persistent
/// storage synchronization. The in-memory test backend is always volatile.
/// Neither method alone makes a newly created directory entry durable.
pub trait Storage {
    /// Return the current byte length, propagating metadata errors.
    fn len(&self) -> io::Result<u64>;

    fn is_empty(&self) -> io::Result<bool> {
        Ok(self.len()? == 0)
    }

    /// Read up to buffer.len() bytes. Return zero at/past EOF or for empty buffers.
    /// Short reads are allowed, just as for `std::io::Read`.
    fn read_at(&mut self, offset: u64, buffer: &mut [u8]) -> io::Result<usize>;

    /// Write all bytes at offset. A gap is zero-filled; empty writes do not extend.
    /// Offset + length overflow returns InvalidInput before changing bytes.
    fn write_all_at(&mut self, offset: u64, bytes: &[u8]) -> io::Result<()>;

    /// Truncate or extend with zeros; persistence still requires flush/sync_all.
    fn set_len(&mut self, len: u64) -> io::Result<()>;

    /// Drain implementation buffers. This is not a durability boundary.
    fn flush(&mut self) -> io::Result<()>;

    /// Request synchronization of file contents/metadata; propagate failures.
    fn sync_all(&mut self) -> io::Result<()>;
}
