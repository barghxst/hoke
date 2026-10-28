//! WAL-only engine facade. Recovery, durable writes, and derived memory state
//! live in separate modules while sharing one exclusively owned storage.

mod changes;
mod error;
mod recovery;
#[cfg(test)]
mod tests;
mod write;

pub use error::Error;
pub use recovery::Recovery;

use std::collections::{BTreeMap, BTreeSet};

use crate::storage::Storage;
use error::io_error;

/// Experimental WAL-only key/value engine; no SSTables, compaction or transactions.
///
/// Keys and values are arbitrary bytes. Each successful mutation writes and syncs
/// its WAL frame before becoming visible. An I/O error leaves the write outcome
/// unknown and poisons further writes; previously published values remain readable.
/// MemoryStorage implements the same protocol but is explicitly not durable.
pub struct Engine<S: Storage> {
    storage: S,
    values: BTreeMap<Vec<u8>, Vec<u8>>,
    deleted: BTreeSet<Vec<u8>>,
    change_bytes: usize,
    sequence: u64,
    wal_len: u64,
    poisoned: bool,
    recovery: Recovery,
}

fn sync<S: Storage>(storage: &mut S) -> Result<(), Error> {
    storage.flush().map_err(io_error("flush WAL"))?;
    storage.sync_all().map_err(io_error("sync WAL"))
}

impl<S: Storage> Engine<S> {
    pub fn get(&self, key: &[u8]) -> Option<&[u8]> {
        self.values.get(key).map(Vec::as_slice)
    }

    /// Return ordered live entries with a bounded total key/value payload.
    ///
    /// This reads the recovered in-memory WAL state without storage I/O or
    /// mutation. A budget error discards the entire result, including any prefix
    /// already collected. The budget excludes allocation and cursor overhead.
    pub fn scan_with_budget(
        &self,
        options: crate::ScanOptions,
        max_bytes: usize,
    ) -> Result<Vec<crate::KeyValue>, crate::DbError> {
        let mut result = Vec::new();
        let mut bytes = 0_usize;
        crate::scan::visit_sources::<S, S>(self, &mut [], options, |key, value| {
            bytes = bytes
                .checked_add(key.len())
                .and_then(|total| total.checked_add(value.len()))
                .filter(|total| *total <= max_bytes)
                .ok_or(crate::DbError::Limit("scan payload exceeds byte budget"))?;
            result
                .try_reserve(1)
                .map_err(|_| crate::DbError::AllocationFailed)?;
            result.push((key, value));
            Ok(())
        })?;
        Ok(result)
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
    pub fn recovery(&self) -> Recovery {
        self.recovery
    }

    fn check_writable(&self) -> Result<(), Error> {
        if self.poisoned {
            Err(Error::Poisoned)
        } else {
            Ok(())
        }
    }

    pub(crate) fn byte_len(&self) -> u64 {
        self.wal_len
    }
}
