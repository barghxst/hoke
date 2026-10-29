use crate::sstable::{Lookup, SSTable};
use crate::storage::Storage;
use crate::{DbError, Engine, KeyValue, ScanOptions};

pub fn get_from_sources<S: Storage>(
    wal: &Engine<S>,
    tables: &mut [SSTable<S>],
    key: &[u8],
) -> Result<Option<Vec<u8>>, DbError> {
    get_from_mixed_sources(wal, tables, key)
}

pub fn get_from_mixed_sources<W: Storage, T: Storage>(
    wal: &Engine<W>,
    tables: &mut [SSTable<T>],
    key: &[u8],
) -> Result<Option<Vec<u8>>, DbError> {
    if let Some(value) = wal.lookup_change(key) {
        return Ok(value.map(<[u8]>::to_vec));
    }
    for table in tables.iter_mut().rev() {
        match table.get(key).map_err(DbError::Sst)? {
            Lookup::Missing => {}
            Lookup::Deleted => return Ok(None),
            Lookup::Value(value) => return Ok(Some(value)),
        }
    }
    Ok(None)
}

pub fn scan_sources_with_budget<S: Storage>(
    wal: &Engine<S>,
    tables: &mut [SSTable<S>],
    options: ScanOptions,
    max_payload_bytes: usize,
) -> Result<Vec<KeyValue>, DbError> {
    scan_mixed_sources_with_budget(wal, tables, options, max_payload_bytes)
}

pub fn scan_mixed_sources_with_budget<W: Storage, T: Storage>(
    wal: &Engine<W>,
    tables: &mut [SSTable<T>],
    options: ScanOptions,
    max_payload_bytes: usize,
) -> Result<Vec<KeyValue>, DbError> {
    let mut result = Vec::new();
    let mut bytes = 0_usize;
    crate::scan::visit_sources(wal, tables, options, |key, value| {
        bytes = bytes
            .checked_add(key.len())
            .and_then(|total| total.checked_add(value.len()))
            .filter(|total| *total <= max_payload_bytes)
            .ok_or(DbError::Limit("scan payload exceeds byte budget"))?;
        result
            .try_reserve(1)
            .map_err(|_| DbError::AllocationFailed)?;
        result.push((key, value));
        Ok(())
    })?;
    Ok(result)
}

#[cfg(test)]
mod mixed_tests {
    use super::*;
    use crate::{
        container::{
            Journal, SegmentKind,
            storage::{ReadSegment, SharedStorage},
        },
        sstable::write_table,
        storage::MemoryStorage,
    };
    use std::collections::BTreeMap;

    #[test]
    fn mixed_memory_delta_and_container_segment_resolve_versions_and_budget() {
        let mut payload = MemoryStorage::new();
        write_table(
            &mut payload,
            &BTreeMap::from([
                (b"a".to_vec(), Some(b"old".to_vec())),
                (b"b".to_vec(), Some(b"gone".to_vec())),
                (b"c".to_vec(), Some(b"table".to_vec())),
            ]),
        )
        .unwrap();
        let mut bytes = vec![0; payload.len().unwrap() as usize];
        let count = payload.read_at(0, &mut bytes).unwrap();
        assert_eq!(count, bytes.len());
        let shared = SharedStorage::new(MemoryStorage::new());
        let mut journal = Journal::open(shared.clone()).unwrap();
        let reference = journal.append_segment(SegmentKind::Sst, &bytes).unwrap();
        journal.commit(&[reference], &[]).unwrap();
        let table = SSTable::open(ReadSegment::new(shared, reference).unwrap()).unwrap();
        let mut tables = [table];
        let mut wal = Engine::open(MemoryStorage::new()).unwrap();
        wal.apply_committed(crate::wal::Operation::Put {
            key: b"a".to_vec(),
            value: b"new".to_vec(),
        });
        wal.apply_committed(crate::wal::Operation::Delete { key: b"b".to_vec() });
        assert_eq!(
            get_from_mixed_sources(&wal, &mut tables, b"a").unwrap(),
            Some(b"new".to_vec())
        );
        assert_eq!(
            get_from_mixed_sources(&wal, &mut tables, b"b").unwrap(),
            None
        );
        assert_eq!(
            get_from_mixed_sources(&wal, &mut tables, b"c").unwrap(),
            Some(b"table".to_vec())
        );
        let expected = vec![
            (b"a".to_vec(), b"new".to_vec()),
            (b"c".to_vec(), b"table".to_vec()),
        ];
        assert_eq!(
            scan_mixed_sources_with_budget(&wal, &mut tables, ScanOptions::default(), 10).unwrap(),
            expected
        );
        assert!(matches!(
            scan_mixed_sources_with_budget(&wal, &mut tables, ScanOptions::default(), 9),
            Err(DbError::Limit(_))
        ));
        assert_eq!(
            scan_mixed_sources_with_budget(
                &wal,
                &mut tables,
                ScanOptions {
                    reverse: true,
                    ..Default::default()
                },
                10
            )
            .unwrap(),
            expected.into_iter().rev().collect::<Vec<_>>()
        );
    }
}
