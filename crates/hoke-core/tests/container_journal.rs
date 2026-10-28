use std::cell::RefCell;
use std::io;
use std::rc::Rc;

use hoke_core::container::{Journal, JournalError, MAX_PENDING_SEGMENTS, SegmentKind, SegmentRef};
use hoke_core::storage::{MemoryStorage, Storage};

#[derive(Default)]
struct State {
    bytes: Vec<u8>,
    events: Vec<&'static str>,
    fail_at: Option<usize>,
    reads: usize,
    max_read: usize,
}
#[derive(Clone, Default)]
struct Adapter(Rc<RefCell<State>>);
impl Adapter {
    fn event(&self, name: &'static str) -> io::Result<()> {
        let mut state = self.0.borrow_mut();
        state.events.push(name);
        if state.fail_at == Some(state.events.len()) {
            return Err(io::Error::other("injected"));
        }
        Ok(())
    }
    fn reset(&self) {
        let mut s = self.0.borrow_mut();
        s.events.clear();
        s.fail_at = None;
    }
}
impl Storage for Adapter {
    fn len(&self) -> io::Result<u64> {
        Ok(self.0.borrow().bytes.len() as u64)
    }
    fn read_at(&mut self, offset: u64, out: &mut [u8]) -> io::Result<usize> {
        let mut s = self.0.borrow_mut();
        s.reads += 1;
        s.max_read = s.max_read.max(out.len());
        let offset = offset as usize;
        let n = out.len().min(s.bytes.len().saturating_sub(offset));
        if n > 0 {
            out[..n].copy_from_slice(&s.bytes[offset..offset + n]);
        }
        Ok(n)
    }
    fn write_all_at(&mut self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        // A failed append is allowed to leave a prefix.
        if let Err(error) = self.event("write") {
            let mut s = self.0.borrow_mut();
            let end = offset as usize + bytes.len().min(3);
            let length = end.max(s.bytes.len());
            s.bytes.resize(length, 0);
            s.bytes[offset as usize..end].copy_from_slice(&bytes[..end - offset as usize]);
            return Err(error);
        }
        let mut s = self.0.borrow_mut();
        let end = offset as usize + bytes.len();
        let length = end.max(s.bytes.len());
        s.bytes.resize(length, 0);
        s.bytes[offset as usize..end].copy_from_slice(bytes);
        Ok(())
    }
    fn set_len(&mut self, length: u64) -> io::Result<()> {
        self.event("truncate")?;
        self.0.borrow_mut().bytes.resize(length as usize, 0);
        Ok(())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.event("flush")
    }
    fn sync_all(&mut self) -> io::Result<()> {
        self.event("sync")
    }
}

#[test]
fn initializes_once_and_commits_with_two_barriers() {
    let adapter = Adapter::default();
    let mut journal = Journal::open(adapter.clone()).unwrap();
    assert_eq!(adapter.0.borrow().events, ["write", "flush", "sync"]);
    assert!(journal.root().is_none());
    adapter.reset();
    let a = journal.append_segment(SegmentKind::Wal, b"a").unwrap();
    assert_eq!(adapter.0.borrow().events, ["write", "write", "write"]);
    adapter.reset();
    journal.commit(&[], &[a]).unwrap();
    assert_eq!(
        adapter.0.borrow().events,
        ["flush", "sync", "write", "write", "write", "flush", "sync"]
    );
    assert_eq!(journal.root().unwrap().generation, 1);
    let mut reopened = Journal::open(journal.into_storage()).unwrap();
    assert_eq!(reopened.read_segment(&a).unwrap(), b"a");
    assert_eq!(reopened.recovery().records_scanned, 2);
}

#[test]
fn complete_pending_records_survive_reopen_without_becoming_root() {
    let mut journal = Journal::open(MemoryStorage::new()).unwrap();
    let a = journal.append_segment(SegmentKind::Sst, b"table").unwrap();
    journal.commit(&[a], &[]).unwrap();
    let pending = journal
        .append_segment(SegmentKind::Wal, b"pending")
        .unwrap();
    let mut reopened = Journal::open(journal.into_storage()).unwrap();
    assert_eq!(reopened.root().unwrap().tables, [a]);
    assert!(reopened.root().unwrap().wal.is_empty());
    assert_eq!(reopened.read_segment(&pending).unwrap(), b"pending");
    reopened.commit(&[a], &[pending]).unwrap();
    assert_eq!(reopened.root().unwrap().generation, 2);
}

#[test]
fn dropped_and_forged_references_are_rejected_without_io_or_poison() {
    let adapter = Adapter::default();
    let mut journal = Journal::open(adapter.clone()).unwrap();
    let a = journal
        .append_segment(SegmentKind::Wal, b"obsolete")
        .unwrap();
    journal.commit(&[], &[a]).unwrap();
    journal.commit(&[], &[]).unwrap();
    let b = journal
        .append_segment(SegmentKind::Wal, b"current")
        .unwrap();
    adapter.reset();
    for bad in [
        a,
        SegmentRef {
            offset: b.offset + 1,
            ..b
        },
        SegmentRef {
            payload_len: 1,
            ..b
        },
        SegmentRef {
            kind: SegmentKind::Sst,
            ..b
        },
    ] {
        assert!(journal.commit(&[], &[bad]).is_err());
        assert!(journal.read_segment(&bad).is_err());
        assert!(!journal.is_poisoned());
        assert!(adapter.0.borrow().events.is_empty());
    }
    assert!(journal.commit(&[], &[b, b]).is_err());
    assert!(adapter.0.borrow().events.is_empty());
    journal.commit(&[], &[b]).unwrap();
    let mut reopened = Journal::open(journal.into_storage()).unwrap();
    assert!(reopened.commit(&[], &[a]).is_err());
}

#[test]
fn every_truncated_record_tail_recovers_but_existing_partial_header_does_not() {
    let adapter = Adapter::default();
    let mut journal = Journal::open(adapter.clone()).unwrap();
    journal.commit(&[], &[]).unwrap();
    let prefix = adapter.0.borrow().bytes.clone();
    journal
        .append_segment(SegmentKind::Wal, b"payload")
        .unwrap();
    let full = adapter.0.borrow().bytes.clone();
    for end in prefix.len() + 1..full.len() {
        let candidate = Adapter::default();
        candidate.0.borrow_mut().bytes = full[..end].to_vec();
        let recovered = Journal::open(candidate.clone()).unwrap();
        assert_eq!(candidate.0.borrow().bytes, prefix);
        assert_eq!(
            recovered.recovery().discarded_tail_bytes,
            (end - prefix.len()) as u64
        );
        assert_eq!(candidate.0.borrow().events, ["truncate", "flush", "sync"]);
    }
    for end in 1..32 {
        let candidate = Adapter::default();
        candidate.0.borrow_mut().bytes = full[..end].to_vec();
        assert!(Journal::open(candidate.clone()).is_err());
        assert_eq!(candidate.0.borrow().bytes, full[..end]);
        assert!(candidate.0.borrow().events.is_empty());
    }
}

#[test]
fn corruption_never_falls_back_or_truncates_even_with_extra_tail() {
    let adapter = Adapter::default();
    let mut journal = Journal::open(adapter.clone()).unwrap();
    let reference = journal.append_segment(SegmentKind::Wal, b"abc").unwrap();
    journal.commit(&[], &[reference]).unwrap();
    let original = adapter.0.borrow().bytes.clone();
    for at in [0, 28, 32, 60, 64, 67, original.len() - 1] {
        let candidate = Adapter::default();
        let mut bytes = original.clone();
        bytes[at] ^= 1;
        bytes.extend_from_slice(b"tail");
        candidate.0.borrow_mut().bytes = bytes.clone();
        assert!(Journal::open(candidate.clone()).is_err(), "offset {at}");
        assert_eq!(candidate.0.borrow().bytes, bytes);
        assert!(candidate.0.borrow().events.is_empty());
    }
}

#[test]
fn every_commit_io_failure_poisoned_and_unacknowledged_root_can_recover() {
    for fail in 1..=7 {
        let adapter = Adapter::default();
        let mut journal = Journal::open(adapter.clone()).unwrap();
        let reference = journal.append_segment(SegmentKind::Wal, b"value").unwrap();
        adapter.reset();
        adapter.0.borrow_mut().fail_at = Some(fail);
        assert!(matches!(
            journal.commit(&[], &[reference]),
            Err(JournalError::Io { .. })
        ));
        assert!(journal.is_poisoned());
        assert!(journal.root().is_none());
        let events = adapter.0.borrow().events.len();
        assert!(matches!(
            journal.append_segment(SegmentKind::Wal, b"x"),
            Err(JournalError::Poisoned)
        ));
        assert!(matches!(
            journal.commit(&[], &[]),
            Err(JournalError::Poisoned)
        ));
        assert_eq!(adapter.0.borrow().events.len(), events);
        adapter.reset();
        let reopened = Journal::open(journal.into_storage()).unwrap();
        // A root fully appended before a failed flush/sync is recoverable despite no ACK.
        assert_eq!(reopened.root().is_some(), fail >= 6);
    }
}

#[test]
fn append_failure_poisoned_and_partial_append_recovers() {
    for fail in 1..=3 {
        let adapter = Adapter::default();
        let mut journal = Journal::open(adapter.clone()).unwrap();
        adapter.reset();
        adapter.0.borrow_mut().fail_at = Some(fail);
        assert!(journal.append_segment(SegmentKind::Wal, b"hello").is_err());
        assert!(journal.is_poisoned());
        adapter.reset();
        let reopened = Journal::open(journal.into_storage()).unwrap();
        assert!(reopened.root().is_none());
        assert_eq!(adapter.0.borrow().bytes.len(), 32);
    }
}

#[test]
fn recovery_streams_large_payload_and_read_rechecks_crc() {
    let adapter = Adapter::default();
    let mut journal = Journal::open(adapter.clone()).unwrap();
    let reference = journal
        .append_segment(SegmentKind::Sst, &vec![7; 150_000])
        .unwrap();
    journal.commit(&[reference], &[]).unwrap();
    adapter.0.borrow_mut().max_read = 0;
    let mut reopened = Journal::open(journal.into_storage()).unwrap();
    assert!(adapter.0.borrow().max_read <= 64 * 1024);
    adapter.0.borrow_mut().bytes[reference.offset as usize + 32] ^= 1;
    assert!(matches!(
        reopened.read_segment(&reference),
        Err(JournalError::Corruption("record checksum"))
    ));
    assert!(reopened.is_poisoned());
    adapter.reset();
    assert!(matches!(
        reopened.commit(&[], &[]),
        Err(JournalError::Poisoned)
    ));
    assert!(matches!(
        reopened.append_segment(SegmentKind::Wal, b"later"),
        Err(JournalError::Poisoned)
    ));
    assert!(adapter.0.borrow().events.is_empty());
}

#[test]
fn pending_cap_resets_after_commit_and_empty_segments_are_valid() {
    let mut journal = Journal::open(MemoryStorage::new()).unwrap();
    for _ in 0..MAX_PENDING_SEGMENTS {
        journal.append_segment(SegmentKind::Wal, &[]).unwrap();
    }
    assert!(matches!(
        journal.append_segment(SegmentKind::Wal, &[]),
        Err(JournalError::Limit(_))
    ));
    assert!(!journal.is_poisoned());
    journal.commit(&[], &[]).unwrap();
    let next = journal.append_segment(SegmentKind::Sst, &[]).unwrap();
    assert!(journal.read_segment(&next).unwrap().is_empty());
}

fn rewrite_record_checksums(bytes: &mut [u8], offset: usize) {
    let header_crc = crc32fast::hash(&bytes[offset..offset + 28]);
    bytes[offset + 28..offset + 32].copy_from_slice(&header_crc.to_le_bytes());
    let length = u64::from_le_bytes(bytes[offset + 16..offset + 24].try_into().unwrap()) as usize;
    let trailer = offset + 32 + length;
    let frame_crc = crc32fast::hash(&bytes[offset..trailer]);
    bytes[trailer..trailer + 4].copy_from_slice(&frame_crc.to_le_bytes());
}

#[test]
fn checksum_valid_semantic_corruption_is_rejected_unchanged() {
    let adapter = Adapter::default();
    let mut journal = Journal::open(adapter.clone()).unwrap();
    let a = journal.append_segment(SegmentKind::Wal, b"abc").unwrap();
    let first_root = adapter.0.borrow().bytes.len();
    journal.commit(&[], &[a]).unwrap();
    let original = adapter.0.borrow().bytes.clone();
    for (offset, position, replacement) in [
        (a.offset as usize, a.offset as usize + 8, 2_u64), // sequence skips 1
        (first_root, first_root + 32, 2_u64),              // generation skips 1
        (first_root, first_root + 32 + 24 + 8, a.offset + 32), // payload interior
        (first_root, first_root + 32 + 24 + 8, first_root as u64), // root, not segment
        (first_root, first_root + 32 + 24 + 8, original.len() as u64), // future
    ] {
        let mut bytes = original.clone();
        bytes[position..position + 8].copy_from_slice(&replacement.to_le_bytes());
        rewrite_record_checksums(&mut bytes, offset);
        let candidate = Adapter::default();
        candidate.0.borrow_mut().bytes = bytes.clone();
        assert!(Journal::open(candidate.clone()).is_err());
        assert!(candidate.0.borrow().events.is_empty());
        assert_eq!(candidate.0.borrow().bytes, bytes);
    }
}

#[test]
fn recovered_root_cannot_resurrect_previously_dropped_segment() {
    let adapter = Adapter::default();
    let mut journal = Journal::open(adapter.clone()).unwrap();
    let a = journal.append_segment(SegmentKind::Wal, b"old").unwrap();
    journal.commit(&[], &[a]).unwrap();
    journal.commit(&[], &[]).unwrap();
    let b = journal.append_segment(SegmentKind::Wal, b"new").unwrap();
    let last_root = adapter.0.borrow().bytes.len();
    journal.commit(&[], &[b]).unwrap();
    let mut bytes = adapter.0.borrow().bytes.clone();
    let offset_position = last_root + 32 + 24 + 8;
    bytes[offset_position..offset_position + 8].copy_from_slice(&a.offset.to_le_bytes());
    rewrite_record_checksums(&mut bytes, last_root);
    adapter.0.borrow_mut().bytes = bytes.clone();
    adapter.reset();
    assert!(Journal::open(adapter.clone()).is_err());
    assert!(adapter.0.borrow().events.is_empty());
    assert_eq!(adapter.0.borrow().bytes, bytes);
}

#[test]
fn tail_repair_failures_are_reported_and_into_storage_does_not_commit() {
    let base = Adapter::default();
    let mut journal = Journal::open(base.clone()).unwrap();
    journal
        .append_segment(SegmentKind::Wal, b"unsynced")
        .unwrap();
    base.reset();
    let _storage = journal.into_storage();
    assert!(base.0.borrow().events.is_empty());
    let mut bytes = base.0.borrow().bytes.clone();
    bytes.extend_from_slice(b"partial");
    for fail in 1..=3 {
        let adapter = Adapter::default();
        adapter.0.borrow_mut().bytes = bytes.clone();
        adapter.0.borrow_mut().fail_at = Some(fail);
        assert!(matches!(
            Journal::open(adapter.clone()),
            Err(JournalError::Io { .. })
        ));
        assert_eq!(adapter.0.borrow().events.len(), fail);
    }
}

#[test]
fn invalid_known_header_poisoned_but_unknown_reference_does_not_read() {
    let adapter = Adapter::default();
    let mut journal = Journal::open(adapter.clone()).unwrap();
    let reference = journal.append_segment(SegmentKind::Wal, b"value").unwrap();
    let reads = adapter.0.borrow().reads;
    assert!(
        journal
            .read_segment(&SegmentRef {
                offset: reference.offset + 1,
                ..reference
            })
            .is_err()
    );
    assert!(!journal.is_poisoned());
    assert_eq!(adapter.0.borrow().reads, reads);
    journal.commit(&[], &[reference]).unwrap();
    adapter.0.borrow_mut().bytes[reference.offset as usize + 28] ^= 1;
    assert!(matches!(
        journal.read_segment(&reference),
        Err(JournalError::Codec(_))
    ));
    assert!(journal.is_poisoned());
    adapter.reset();
    assert!(matches!(
        journal.commit(&[], &[]),
        Err(JournalError::Poisoned)
    ));
    assert!(adapter.0.borrow().events.is_empty());
}
