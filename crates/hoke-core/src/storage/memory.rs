use std::io;

use super::Storage;

#[derive(Debug, Default)]
pub struct MemoryStorage {
    data: Vec<u8>,
}

impl MemoryStorage {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.data
    }

    fn resize(&mut self, len: usize) -> io::Result<()> {
        if len > self.data.len() {
            self.data
                .try_reserve_exact(len - self.data.len())
                .map_err(|error| io::Error::new(io::ErrorKind::OutOfMemory, error))?;
        }
        self.data.resize(len, 0);
        Ok(())
    }
}

impl Storage for MemoryStorage {
    fn len(&self) -> io::Result<u64> {
        u64::try_from(self.data.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "length does not fit in u64"))
    }

    fn read_at(&mut self, offset: u64, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() || offset >= self.len()? {
            return Ok(0);
        }
        let start = usize::try_from(offset).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "offset does not fit in usize")
        })?;
        let count = buffer.len().min(self.data.len() - start);
        buffer[..count].copy_from_slice(&self.data[start..start + count]);
        Ok(count)
    }

    fn write_all_at(&mut self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        let count = u64::try_from(bytes.len()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "length does not fit in u64")
        })?;
        let end = offset.checked_add(count).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "offset + length overflow")
        })?;
        if bytes.is_empty() {
            return Ok(());
        }
        let end = usize::try_from(end).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "length does not fit in usize")
        })?;
        if end > self.data.len() {
            self.resize(end)?;
        }

        let start = end - bytes.len();
        self.data[start..end].copy_from_slice(bytes);
        Ok(())
    }

    fn set_len(&mut self, len: u64) -> io::Result<()> {
        let len = usize::try_from(len).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "length does not fit in usize")
        })?;
        self.resize(len)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn sync_all(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_and_default_are_empty() {
        for mut storage in [MemoryStorage::new(), MemoryStorage::default()] {
            assert_eq!(storage.len().unwrap(), 0);
            assert!(storage.is_empty().unwrap());
            assert_eq!(storage.read_at(0, &mut [1; 2]).unwrap(), 0);
        }
    }

    #[test]
    fn reads_use_explicit_offsets_and_leave_unused_buffer_unchanged() {
        let mut storage = MemoryStorage::new();
        storage.write_all_at(0, b"abcdef").unwrap();
        let mut buffer = [9; 5];
        assert_eq!(storage.read_at(4, &mut buffer).unwrap(), 2);
        assert_eq!(buffer, [b'e', b'f', 9, 9, 9]);
        assert_eq!(storage.read_at(1, &mut buffer[..3]).unwrap(), 3);
        assert_eq!(&buffer[..3], b"bcd");
        assert_eq!(storage.read_at(1, &mut buffer[..3]).unwrap(), 3);
        assert_eq!(&buffer[..3], b"bcd");
        for offset in [6, 7, u64::MAX] {
            assert_eq!(storage.read_at(offset, &mut buffer).unwrap(), 0);
        }
        assert_eq!(storage.read_at(u64::MAX, &mut []).unwrap(), 0);
    }

    #[test]
    fn writes_overwrite_and_fill_gaps_with_zeros() {
        let mut storage = MemoryStorage::new();
        storage.write_all_at(0, b"abc").unwrap();
        storage.write_all_at(1, b"Z").unwrap();
        assert_eq!(storage.data, b"aZc");
        storage.write_all_at(5, &[0, 255]).unwrap();
        assert_eq!(storage.data, [b'a', b'Z', b'c', 0, 0, 0, 255]);
        assert_eq!(storage.len().unwrap(), 7);
        assert!(!storage.is_empty().unwrap());
    }

    #[test]
    fn truncation_discards_bytes_and_extension_zero_fills() {
        let mut storage = MemoryStorage::new();
        storage.write_all_at(0, b"abcdef").unwrap();
        storage.set_len(2).unwrap();
        storage.set_len(6).unwrap();
        assert_eq!(storage.data, [b'a', b'b', 0, 0, 0, 0]);
        storage.set_len(6).unwrap();
        assert_eq!(storage.data, [b'a', b'b', 0, 0, 0, 0]);
        storage.set_len(0).unwrap();
        assert!(storage.is_empty().unwrap());
        storage.set_len(2).unwrap();
        assert_eq!(storage.data, [0, 0]);
    }

    #[test]
    fn empty_writes_do_not_extend_or_change_contents() {
        let mut storage = MemoryStorage::new();
        storage.write_all_at(u64::MAX, &[]).unwrap();
        assert!(storage.is_empty().unwrap());
        storage.write_all_at(0, b"abc").unwrap();
        for offset in [0, 1, 3, 100, u64::MAX] {
            storage.write_all_at(offset, &[]).unwrap();
            assert_eq!(storage.data, b"abc");
        }
    }

    #[test]
    fn overflowing_write_fails_before_mutation() {
        let mut storage = MemoryStorage::new();
        storage.write_all_at(0, b"original").unwrap();
        let error = storage.write_all_at(u64::MAX, &[1]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(storage.data, b"original");
        let error = storage.write_all_at(u64::MAX - 1, &[1, 2]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(storage.data, b"original");
    }

    #[test]
    fn impossible_allocations_return_errors_without_mutation() {
        let mut storage = MemoryStorage::new();
        storage.write_all_at(0, b"original").unwrap();

        assert!(storage.set_len(u64::MAX).is_err());
        assert_eq!(storage.data, b"original");
        assert!(storage.write_all_at(u64::MAX - 1, &[1]).is_err());
        assert_eq!(storage.data, b"original");
    }

    #[test]
    fn flush_and_sync_are_successful_volatile_no_ops() {
        let mut storage = MemoryStorage::new();
        storage.write_all_at(0, b"volatile").unwrap();
        storage.flush().unwrap();
        storage.sync_all().unwrap();
        assert_eq!(storage.data, b"volatile");
        assert!(MemoryStorage::new().is_empty().unwrap());
    }
}
