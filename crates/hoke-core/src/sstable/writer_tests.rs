use super::*;
use std::io;

use crate::sstable::{Lookup, SSTable, test_support::*};
use crate::storage::MemoryStorage;

#[test]
fn refuses_nonempty_and_preflights_limits_before_writing() {
    let mut storage = sample();
    let original = bytes(sample());
    assert!(write_table(&mut storage, &BTreeMap::new()).is_err());
    assert_eq!(bytes(storage), original);
    for entries in [
        BTreeMap::from([(vec![0; wal::MAX_KEY_BYTES + 1], None)]),
        BTreeMap::from([(vec![], Some(vec![0; wal::MAX_VALUE_BYTES + 1]))]),
    ] {
        let mut storage = MemoryStorage::new();
        assert!(write_table(&mut storage, &entries).is_err());
        assert!(storage.is_empty().unwrap());
    }
}

#[test]
fn index_limit_checked_before_destination_write() {
    let entries = (0..1024u32)
        .map(|i| {
            let mut key = vec![0; wal::MAX_KEY_BYTES];
            key[..4].copy_from_slice(&i.to_be_bytes());
            (key, None)
        })
        .collect();
    let mut storage = MemoryStorage::new();
    assert!(write_table(&mut storage, &entries).is_err());
    assert!(storage.is_empty().unwrap());
}

#[test]
fn streaming_writer_roundtrips_reused_buffers_and_matches_v1_bytes() {
    let entries = BTreeMap::from([
        (vec![], Some(vec![])),
        (b"a".to_vec(), Some(b"first".to_vec())),
        (b"b".to_vec(), None),
        (vec![255, 0], Some(vec![0, 255])),
    ]);
    let mut storage = MemoryStorage::new();
    let mut writer = TableWriter::new(&mut storage).unwrap();
    let mut scratch = Vec::new();
    for (key, value) in &entries {
        scratch.clear();
        if let Some(value) = value {
            scratch.extend_from_slice(value);
        }
        writer
            .append(key, value.as_ref().map(|_| scratch.as_slice()))
            .unwrap();
        scratch.fill(42);
    }
    assert_eq!(writer.finish().unwrap(), 4);
    assert_eq!(bytes(storage), bytes(sample()));
    let mut empty = MemoryStorage::new();
    assert_eq!(TableWriter::new(&mut empty).unwrap().finish().unwrap(), 0);
    assert_eq!(
        SSTable::open(empty).unwrap().get(b"anything").unwrap(),
        Lookup::Missing
    );
}

#[test]
fn streaming_writer_refuses_nonempty_and_unsorted_or_duplicate_keys() {
    let mut nonempty = sample();
    assert!(TableWriter::new(&mut nonempty).is_err());
    assert_eq!(bytes(nonempty), bytes(sample()));
    for second in [b"b".as_slice(), b"a"] {
        let mut storage = MemoryStorage::new();
        let mut writer = TableWriter::new(&mut storage).unwrap();
        writer.append(b"b", Some(b"value")).unwrap();
        assert!(writer.append(second, None).is_err());
        assert!(writer.append(b"z", None).is_err());
        assert!(writer.finish().is_err());
        assert!(SSTable::open(storage).is_err());
    }
}

#[test]
fn streaming_writer_enforces_each_format_bound_without_appending() {
    let mut storage = MemoryStorage::new();
    let mut writer = TableWriter::new(&mut storage).unwrap();
    assert!(matches!(
        writer.append(&vec![0; wal::MAX_KEY_BYTES + 1], None),
        Err(SstError::Invalid("key too large"))
    ));
    drop(writer);
    assert_eq!(storage.len().unwrap(), HEADER_LEN as u64);

    let mut storage = MemoryStorage::new();
    let mut writer = TableWriter::new(&mut storage).unwrap();
    assert!(matches!(
        writer.append(b"", Some(&vec![0; wal::MAX_VALUE_BYTES + 1])),
        Err(SstError::Invalid("value too large"))
    ));
    drop(writer);
    assert_eq!(storage.len().unwrap(), HEADER_LEN as u64);

    let mut storage = MemoryStorage::new();
    let mut writer = TableWriter::new(&mut storage).unwrap();
    // Exercise boundary counters without writing a million records.
    writer.count = MAX_ENTRIES;
    assert!(matches!(
        writer.append(b"", None),
        Err(SstError::Invalid("too many entries"))
    ));
    drop(writer);
    assert_eq!(storage.len().unwrap(), HEADER_LEN as u64);

    let mut storage = MemoryStorage::new();
    let mut writer = TableWriter::new(&mut storage).unwrap();
    writer.index.resize(MAX_INDEX_BYTES - 11, 0);
    assert!(matches!(
        writer.append(b"", None),
        Err(SstError::Invalid("index too large"))
    ));
    assert!(writer.finish().is_err());
    assert_eq!(storage.len().unwrap(), HEADER_LEN as u64);
}

#[test]
fn streaming_write_errors_are_propagated_and_prevent_finish_after_append_error() {
    struct FailWrites {
        storage: MemoryStorage,
        writes: usize,
        fail_at: usize,
    }
    impl Storage for FailWrites {
        fn len(&self) -> io::Result<u64> {
            self.storage.len()
        }
        fn read_at(&mut self, offset: u64, bytes: &mut [u8]) -> io::Result<usize> {
            self.storage.read_at(offset, bytes)
        }
        fn write_all_at(&mut self, offset: u64, bytes: &[u8]) -> io::Result<()> {
            self.writes += 1;
            if self.writes == self.fail_at {
                return Err(io::Error::other("injected write"));
            }
            self.storage.write_all_at(offset, bytes)
        }
        fn set_len(&mut self, len: u64) -> io::Result<()> {
            self.storage.set_len(len)
        }
        fn flush(&mut self) -> io::Result<()> {
            self.storage.flush()
        }
        fn sync_all(&mut self) -> io::Result<()> {
            self.storage.sync_all()
        }
    }
    // Placeholder, two records, index, index CRC and final header.
    for fail_at in 1..=6 {
        let mut storage = FailWrites {
            storage: MemoryStorage::new(),
            writes: 0,
            fail_at,
        };
        let result = (|| {
            let mut writer = TableWriter::new(&mut storage)?;
            for key in [b"a", b"b"] {
                if let Err(error) = writer.append(key, Some(b"value")) {
                    assert!(writer.finish().is_err());
                    return Err(error);
                }
            }
            writer.finish()
        })();
        assert!(matches!(result, Err(SstError::Io(_))), "write {fail_at}");
    }
}
