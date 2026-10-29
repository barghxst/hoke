use super::*;
use std::{collections::BTreeMap, error::Error, io};

use crate::sstable::{test_support::*, write_table};
use crate::storage::MemoryStorage;

#[test]
fn index_ranges_match_byte_bounds_including_tombstone_keys() {
    let table = SSTable::open(sample()).unwrap();
    let keys: [&[u8]; 4] = [b"", b"a", b"b", &[255, 0]];
    for (position, key) in keys.iter().enumerate() {
        assert_eq!(table.key_at(position), Some(*key));
    }
    assert_eq!(table.key_at(keys.len()), None);
    assert_eq!(table.key_at(usize::MAX), None);

    let mut bounds = vec![Bound::Unbounded];
    for key in [
        vec![],
        vec![0],
        b"a".to_vec(),
        b"aa".to_vec(),
        b"b".to_vec(),
        vec![255],
        vec![255, 0],
        vec![255, 255],
    ] {
        bounds.push(Bound::Included(key.clone()));
        bounds.push(Bound::Excluded(key));
    }
    for start in &bounds {
        for end in &bounds {
            let positions = table.range_positions(start, end);
            assert!(positions.start <= positions.end);
            assert!(positions.end <= keys.len());
            let actual: Vec<_> = positions
                .map(|position| table.key_at(position).unwrap())
                .collect();
            let expected: Vec<_> = keys
                .iter()
                .copied()
                .filter(|key| match start {
                    Bound::Included(start) => *key >= start.as_slice(),
                    Bound::Excluded(start) => *key > start.as_slice(),
                    Bound::Unbounded => true,
                })
                .filter(|key| match end {
                    Bound::Included(end) => *key <= end.as_slice(),
                    Bound::Excluded(end) => *key < end.as_slice(),
                    Bound::Unbounded => true,
                })
                .collect();
            assert_eq!(actual, expected, "start={start:?}, end={end:?}");
        }
    }
}

#[test]
fn empty_table_index_ranges_are_empty() {
    let mut storage = MemoryStorage::new();
    write_table(&mut storage, &BTreeMap::new()).unwrap();
    let table = SSTable::open(storage).unwrap();
    assert_eq!(table.key_at(0), None);
    for bound in [
        Bound::Unbounded,
        Bound::Included(vec![]),
        Bound::Excluded(vec![255]),
    ] {
        assert_eq!(table.range_positions(&bound, &Bound::Unbounded), 0..0);
        assert_eq!(table.range_positions(&Bound::Unbounded, &bound), 0..0);
    }
}

#[test]
fn index_metadata_does_not_read_corrupt_record_payloads() {
    let mut raw = bytes(sample());

    let tombstone_offset = HEADER_LEN
        + wal::FRAME_HEADER_LEN
        + wal::TRAILER_LEN
        + wal::FRAME_HEADER_LEN
        + 1
        + b"first".len()
        + wal::TRAILER_LEN;
    raw[tombstone_offset + wal::FRAME_HEADER_LEN] ^= 1;
    let mut table = SSTable::open(from_bytes(&raw)).unwrap();
    assert_eq!(table.key_at(2), Some(b"b".as_slice()));
    assert_eq!(
        table.range_positions(
            &Bound::Included(b"b".to_vec()),
            &Bound::Included(b"b".to_vec())
        ),
        2..3
    );
    assert!(table.get(b"b").is_err());
}

#[test]
fn empty_and_binary_roundtrips_distinguish_missing_empty_and_deleted() {
    let mut storage = MemoryStorage::new();
    write_table(&mut storage, &BTreeMap::new()).unwrap();
    assert_eq!(storage.len().unwrap(), 36);
    assert_eq!(
        SSTable::open(storage).unwrap().get(b"absent").unwrap(),
        Lookup::Missing
    );
    let mut table = SSTable::open(sample()).unwrap();
    for (key, expected) in [
        (vec![], Lookup::Value(vec![])),
        (b"a".to_vec(), Lookup::Value(b"first".to_vec())),
        (b"b".to_vec(), Lookup::Deleted),
        (vec![255, 0], Lookup::Value(vec![0, 255])),
        (b"missing".to_vec(), Lookup::Missing),
    ] {
        assert_eq!(table.get(&key).unwrap(), expected);
    }
}

#[test]
fn rejects_every_truncation_and_trailing_bytes() {
    let valid = bytes(sample());
    for length in 0..valid.len() {
        assert!(
            SSTable::open(from_bytes(&valid[..length])).is_err(),
            "length {length}"
        );
    }
    let mut extra = valid;
    extra.push(0);
    assert!(SSTable::open(from_bytes(&extra)).is_err());
}

#[test]
fn rejects_corrupt_headers_and_index_but_payload_is_lazy() {
    let valid = bytes(sample());
    let index_offset = u64_at(&valid, 20) as usize;
    for i in (0..HEADER_LEN).chain(index_offset..valid.len()) {
        let mut bad = valid.clone();
        bad[i] ^= 1;
        assert!(SSTable::open(from_bytes(&bad)).is_err(), "byte {i}");
    }

    let second = HEADER_LEN + wal::FRAME_HEADER_LEN + wal::TRAILER_LEN;
    let mut bad = valid;
    bad[second + wal::FRAME_HEADER_LEN + 1] ^= 1;
    let mut table = SSTable::open(from_bytes(&bad)).unwrap();
    assert_eq!(table.get(b"b").unwrap(), Lookup::Deleted);
    assert!(table.get(b"a").is_err());
}

#[test]
fn rejects_repaired_header_limits_and_hostile_offsets() {
    let valid = bytes(sample());
    for (start, replacement) in [
        (12, (MAX_ENTRIES as u64 + 1)),
        (12, u64::MAX),
        (20, 0),
        (20, 31),
        (20, u64::MAX),
    ] {
        let mut bad = valid.clone();
        bad[start..start + 8].copy_from_slice(&replacement.to_le_bytes());
        repair_header(&mut bad);
        assert!(SSTable::open(from_bytes(&bad)).is_err());
    }
    let mut bad = valid;
    bad[8..12].copy_from_slice(&2u32.to_le_bytes());
    repair_header(&mut bad);
    assert!(SSTable::open(from_bytes(&bad)).is_err());
}

#[test]
fn repaired_index_checksum_does_not_bypass_semantics() {
    let valid = bytes(sample());
    let start = u64_at(&valid, 20) as usize;
    for replacement in [0, 31, 33, u64::MAX] {
        let mut bad = valid.clone();
        bad[start + 4..start + 12].copy_from_slice(&replacement.to_le_bytes());
        repair_index(&mut bad);
        assert!(SSTable::open(from_bytes(&bad)).is_err());
    }
    let mut bad = valid.clone();
    bad[start..start + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    repair_index(&mut bad);
    assert!(SSTable::open(from_bytes(&bad)).is_err());

    let mut bad = valid.clone();
    bad[start + 12 + 13 + 4] = b'a';
    repair_index(&mut bad);
    assert!(SSTable::open(from_bytes(&bad)).is_err());

    let mut bad = valid;
    bad[start + 12 + 4] = b'c';
    repair_index(&mut bad);
    assert!(SSTable::open(from_bytes(&bad)).is_err());
}

#[test]
fn lazy_decode_checks_key_against_index_even_with_valid_frame_crc() {
    let mut valid = bytes(sample());
    let offset = HEADER_LEN + wal::FRAME_HEADER_LEN + wal::TRAILER_LEN;
    let header: &[u8; wal::FRAME_HEADER_LEN] = valid[offset..offset + wal::FRAME_HEADER_LEN]
        .try_into()
        .unwrap();
    let len = wal::decode_frame_header(header).unwrap().frame_len();
    valid[offset + wal::FRAME_HEADER_LEN] = b'z';
    let crc = crc32fast::hash(&valid[offset..offset + len - 4]);
    valid[offset + len - 4..offset + len].copy_from_slice(&crc.to_le_bytes());
    let mut table = SSTable::open(from_bytes(&valid)).unwrap();
    assert!(table.get(b"a").is_err());
}

#[test]
fn validates_fixed_record_headers_at_open() {
    let valid = bytes(sample());
    for index in HEADER_LEN..HEADER_LEN + wal::FRAME_HEADER_LEN {
        let mut bad = valid.clone();
        bad[index] ^= 1;
        assert!(SSTable::open(from_bytes(&bad)).is_err());
    }
    let mut bad = valid;
    bad[HEADER_LEN + 4..HEADER_LEN + 12].copy_from_slice(&2u64.to_le_bytes());
    let crc = crc32fast::hash(&bad[HEADER_LEN..HEADER_LEN + 24]);
    bad[HEADER_LEN + 24..HEADER_LEN + 28].copy_from_slice(&crc.to_le_bytes());
    assert!(SSTable::open(from_bytes(&bad)).is_err());
}

#[test]
fn tombstone_metadata_distinguishes_delete_from_empty_live_values() {
    assert!(SSTable::open(sample()).unwrap().has_tombstones());
    for entries in [
        BTreeMap::new(),
        BTreeMap::from([
            (vec![], Some(vec![])),
            (b"a".to_vec(), Some(b"value".to_vec())),
        ]),
    ] {
        let mut storage = MemoryStorage::new();
        write_table(&mut storage, &entries).unwrap();
        assert!(!SSTable::open(storage).unwrap().has_tombstones());
    }
    let mut storage = MemoryStorage::new();
    write_table(&mut storage, &BTreeMap::from([(vec![], None)])).unwrap();
    assert!(SSTable::open(storage).unwrap().has_tombstones());
}

#[test]
fn bounded_short_reads_and_io_source() {
    struct Short(MemoryStorage);
    impl Storage for Short {
        fn len(&self) -> io::Result<u64> {
            self.0.len()
        }
        fn read_at(&mut self, offset: u64, bytes: &mut [u8]) -> io::Result<usize> {
            let len = bytes.len().min(3);
            self.0.read_at(offset, &mut bytes[..len])
        }
        fn write_all_at(&mut self, _: u64, _: &[u8]) -> io::Result<()> {
            unreachable!()
        }
        fn set_len(&mut self, _: u64) -> io::Result<()> {
            unreachable!()
        }
        fn flush(&mut self) -> io::Result<()> {
            unreachable!()
        }
        fn sync_all(&mut self) -> io::Result<()> {
            unreachable!()
        }
    }
    assert_eq!(
        SSTable::open(Short(sample())).unwrap().get(b"a").unwrap(),
        Lookup::Value(b"first".to_vec())
    );
    let error = SstError::from(io::Error::other("injected"));
    assert!(error.source().is_some());
    assert!(error.to_string().contains("injected"));
}
