//! Read a caller-owned WAL and immutable sorted runs without publishing state.
//!
//! Tables must be ordered from oldest to newest. The WAL is newer than every
//! table, including its tombstones. These helpers never write, flush, or repair
//! storage; the caller remains responsible for source selection and ownership.

use crate::sstable::{Lookup, SSTable};
use crate::storage::Storage;
use crate::{DbError, Engine, KeyValue, ScanOptions};

/// Read only the newest matching version, without falling back after an error.
pub fn get_from_sources<S: Storage>(
    wal: &Engine<S>,
    tables: &mut [SSTable<S>],
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

/// Merge live rows with an all-or-error budget on their key/value payload.
///
/// The budget excludes packet framing, allocations, indexes and cursor overhead.
/// A selected SST record may be decoded before the budget is checked. Invalid
/// bounds fail before storage reads; a valid zero limit performs no reads.
pub fn scan_sources_with_budget<S: Storage>(
    wal: &Engine<S>,
    tables: &mut [SSTable<S>],
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
