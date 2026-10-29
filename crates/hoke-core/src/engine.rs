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
