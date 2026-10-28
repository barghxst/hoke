use super::*;
use crate::container::codec::FILE_HEADER_LEN;
use crate::storage::MemoryStorage;
use std::io;

#[test]
fn validates_payload_before_explicit_tail_repair_and_blocks_writes() {
    let mut journal = Journal::open(MemoryStorage::new()).unwrap();
    let reference = journal.append_segment(SegmentKind::Wal, b"known").unwrap();
    journal.commit(&[], &[reference]).unwrap();
    let mut storage = journal.into_storage();
    let length = storage.len().unwrap();
    storage.write_all_at(length, &[1, 2, 3]).unwrap();
    let mut journal = Journal::open_deferred(storage).unwrap();
    assert_eq!(journal.storage.len().unwrap(), length + 3);
    assert_eq!(journal.read_segment(&reference).unwrap(), b"known");
    assert!(
        journal
            .append_segment(SegmentKind::Wal, b"blocked")
            .is_err()
    );
    assert!(journal.commit(&[], &[]).is_err());
    assert!(!journal.is_poisoned());
    assert_eq!(journal.storage.len().unwrap(), length + 3);
    journal.repair_tail().unwrap();
    assert_eq!(journal.storage.len().unwrap(), length);
    assert_eq!(journal.recovery().discarded_tail_bytes, 3);
    journal.repair_tail().unwrap();
    journal.commit(&[], &[reference]).unwrap();
    assert_eq!(journal.root().unwrap().generation, 2);
}

struct FailRepair {
    inner: MemoryStorage,
    fail_sync: bool,
}
impl Storage for FailRepair {
    fn len(&self) -> io::Result<u64> {
        self.inner.len()
    }
    fn read_at(&mut self, offset: u64, bytes: &mut [u8]) -> io::Result<usize> {
        self.inner.read_at(offset, bytes)
    }
    fn write_all_at(&mut self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        self.inner.write_all_at(offset, bytes)
    }
    fn set_len(&mut self, length: u64) -> io::Result<()> {
        if !self.fail_sync {
            return Err(io::Error::other("truncate failed"));
        }
        self.inner.set_len(length)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
    fn sync_all(&mut self) -> io::Result<()> {
        Err(io::Error::other("sync failed"))
    }
}
#[test]
fn failed_tail_truncate_or_sync_poison_the_journal() {
    for fail_sync in [false, true] {
        let journal = Journal::open(MemoryStorage::new()).unwrap();
        let mut inner = journal.into_storage();
        inner.write_all_at(FILE_HEADER_LEN as u64, b"tail").unwrap();
        let mut journal = Journal::open_deferred(FailRepair { inner, fail_sync }).unwrap();
        assert!(matches!(
            journal.repair_tail(),
            Err(JournalError::Io { .. })
        ));
        assert!(journal.is_poisoned());
        assert!(matches!(
            journal.commit(&[], &[]),
            Err(JournalError::Poisoned)
        ));
        assert!(matches!(journal.repair_tail(), Err(JournalError::Poisoned)));
    }
}
