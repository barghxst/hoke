//! Shared exclusive handle and immutable bounded views of segment payloads.
use std::{cell::RefCell, io, rc::Rc};

use super::{CodecError, SegmentRef, codec};
use crate::storage::Storage;

pub(crate) struct SharedStorage<S: Storage>(Rc<RefCell<S>>);

impl<S: Storage> Clone for SharedStorage<S> {
    fn clone(&self) -> Self {
        Self(Rc::clone(&self.0))
    }
}
impl<S: Storage> SharedStorage<S> {
    pub(crate) fn new(storage: S) -> Self {
        Self(Rc::new(RefCell::new(storage)))
    }
    pub(crate) fn try_into_inner(self) -> Result<S, Self> {
        Rc::try_unwrap(self.0)
            .map(RefCell::into_inner)
            .map_err(Self)
    }
}
fn borrowed(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(format!("container storage already borrowed: {error}"))
}
impl<S: Storage> Storage for SharedStorage<S> {
    fn len(&self) -> io::Result<u64> {
        self.0.try_borrow().map_err(borrowed)?.len()
    }
    fn read_at(&mut self, offset: u64, buffer: &mut [u8]) -> io::Result<usize> {
        self.0
            .try_borrow_mut()
            .map_err(borrowed)?
            .read_at(offset, buffer)
    }
    fn write_all_at(&mut self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        self.0
            .try_borrow_mut()
            .map_err(borrowed)?
            .write_all_at(offset, bytes)
    }
    fn set_len(&mut self, len: u64) -> io::Result<()> {
        self.0.try_borrow_mut().map_err(borrowed)?.set_len(len)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.try_borrow_mut().map_err(borrowed)?.flush()
    }
    fn sync_all(&mut self) -> io::Result<()> {
        self.0.try_borrow_mut().map_err(borrowed)?.sync_all()
    }
}

pub(crate) struct ReadSegment<S: Storage> {
    storage: SharedStorage<S>,
    offset: u64,
    length: u64,
}
impl<S: Storage> ReadSegment<S> {
    /// The coordinator must select a reference validated by the journal.
    pub(crate) fn new(
        storage: SharedStorage<S>,
        reference: SegmentRef,
    ) -> Result<Self, CodecError> {
        codec::encode_ref(&reference)?;
        Ok(Self {
            storage,
            offset: reference.offset + codec::RECORD_HEADER_LEN as u64,
            length: reference.payload_len,
        })
    }
}
fn readonly() -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, "immutable container segment")
}
impl<S: Storage> Storage for ReadSegment<S> {
    fn len(&self) -> io::Result<u64> {
        Ok(self.length)
    }
    fn read_at(&mut self, offset: u64, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() || offset >= self.length {
            return Ok(0);
        }
        let count = (self.length - offset).min(buffer.len() as u64) as usize;
        self.storage
            .read_at(self.offset + offset, &mut buffer[..count])
    }
    fn write_all_at(&mut self, _offset: u64, _bytes: &[u8]) -> io::Result<()> {
        Err(readonly())
    }
    fn set_len(&mut self, _len: u64) -> io::Result<()> {
        Err(readonly())
    }
    fn flush(&mut self) -> io::Result<()> {
        Err(readonly())
    }
    fn sync_all(&mut self) -> io::Result<()> {
        Err(readonly())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        container::{Journal, SegmentKind},
        storage::MemoryStorage,
    };

    #[test]
    fn bounded_views_never_expose_adjacent_records_or_allow_mutations() {
        let shared = SharedStorage::new(MemoryStorage::new());
        let mut journal = Journal::open(shared.clone()).unwrap();
        let reference = journal
            .append_segment(SegmentKind::Sst, b"payload")
            .unwrap();
        journal.append_segment(SegmentKind::Sst, b"secret").unwrap();
        let mut view = ReadSegment::new(shared.clone(), reference).unwrap();
        assert_eq!(view.len().unwrap(), 7);
        let mut bytes = [0x77; 20];
        assert_eq!(view.read_at(2, &mut bytes).unwrap(), 5);
        assert_eq!(&bytes[..5], b"yload");
        assert_eq!(bytes[5], 0x77);
        assert_eq!(view.read_at(u64::MAX, &mut bytes).unwrap(), 0);
        assert_eq!(view.read_at(7, &mut bytes).unwrap(), 0);
        for error in [
            view.write_all_at(0, b"x"),
            view.set_len(1),
            view.flush(),
            view.sync_all(),
        ] {
            assert_eq!(error.unwrap_err().kind(), io::ErrorKind::Unsupported);
        }
        assert!(
            ReadSegment::new(
                shared,
                SegmentRef {
                    offset: u64::MAX,
                    ..reference
                }
            )
            .is_err()
        );
    }

    #[test]
    fn shared_nonclone_storage_returns_borrow_errors_and_releases_ownership() {
        let shared = SharedStorage::new(MemoryStorage::new());
        let mut clone = shared.clone();
        let guard = shared.0.borrow_mut();
        assert!(clone.len().is_err());
        assert!(clone.read_at(0, &mut [0]).is_err());
        assert!(clone.write_all_at(0, b"x").is_err());
        assert!(clone.set_len(0).is_err());
        assert!(clone.flush().is_err());
        assert!(clone.sync_all().is_err());
        drop(guard);
        let shared = shared.try_into_inner().err().unwrap();
        clone.write_all_at(0, b"ok").unwrap();
        drop(clone);
        assert_eq!(shared.try_into_inner().ok().unwrap().len().unwrap(), 2);
    }
}
