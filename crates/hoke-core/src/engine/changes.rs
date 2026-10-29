use std::collections::{BTreeMap, BTreeSet};
use std::ops::Bound;

use super::{Engine, Error};
use crate::storage::Storage;
use crate::wal::Operation;

pub(super) fn apply(
    values: &mut BTreeMap<Vec<u8>, Vec<u8>>,
    deleted: &mut BTreeSet<Vec<u8>>,
    change_bytes: &mut usize,
    operation: Operation,
) {
    let (key, value) = match &operation {
        Operation::Put { key, value } => (key.as_slice(), Some(value.as_slice())),
        Operation::Delete { key } => (key.as_slice(), None),
    };
    let previous = change_size(values, deleted, key);
    let next_bytes = *change_bytes - previous + key.len() + value.map_or(0, <[u8]>::len);
    match operation {
        Operation::Put { key, value } => {
            deleted.remove(&key);
            values.insert(key, value);
        }
        Operation::Delete { key } => {
            values.remove(&key);
            deleted.insert(key);
        }
    }
    *change_bytes = next_bytes;
}

fn change_size(
    values: &BTreeMap<Vec<u8>, Vec<u8>>,
    deleted: &BTreeSet<Vec<u8>>,
    key: &[u8],
) -> usize {
    if let Some(value) = values.get(key) {
        key.len() + value.len()
    } else if deleted.contains(key) {
        key.len()
    } else {
        0
    }
}

impl<S: Storage> Engine<S> {
    pub(crate) fn lookup_change(&self, key: &[u8]) -> Option<Option<&[u8]>> {
        if let Some(value) = self.values.get(key) {
            return Some(Some(value));
        }
        if self.deleted.contains(key) {
            return Some(None);
        }
        None
    }
    pub(crate) fn value_keys<'a>(
        &'a self,
        start: &Bound<Vec<u8>>,
        end: &Bound<Vec<u8>>,
        reverse: bool,
    ) -> Box<dyn Iterator<Item = &'a [u8]> + 'a> {
        let keys = self
            .values
            .range::<Vec<u8>, _>((start.as_ref(), end.as_ref()))
            .map(|(key, _)| key.as_slice());
        if reverse {
            Box::new(keys.rev())
        } else {
            Box::new(keys)
        }
    }
    pub(crate) fn deleted_keys<'a>(
        &'a self,
        start: &Bound<Vec<u8>>,
        end: &Bound<Vec<u8>>,
        reverse: bool,
    ) -> Box<dyn Iterator<Item = &'a [u8]> + 'a> {
        let keys = self
            .deleted
            .range::<Vec<u8>, _>((start.as_ref(), end.as_ref()))
            .map(|key| key.as_slice());
        if reverse {
            Box::new(keys.rev())
        } else {
            Box::new(keys)
        }
    }
    pub(crate) fn change_count(&self) -> usize {
        self.values.len() + self.deleted.len()
    }

    pub(crate) fn change_bytes(&self) -> usize {
        self.change_bytes
    }
    pub(crate) fn projected_change_bytes(&self, key: &[u8], value: Option<&[u8]>) -> usize {
        self.change_bytes - change_size(&self.values, &self.deleted, key)
            + key.len()
            + value.map_or(0, <[u8]>::len)
    }
    pub(crate) fn snapshot_changes(&self) -> Result<BTreeMap<Vec<u8>, Option<Vec<u8>>>, Error> {
        self.check_writable()?;
        let mut changes: BTreeMap<_, _> = self
            .values
            .iter()
            .map(|(key, value)| (key.clone(), Some(value.clone())))
            .collect();
        for key in &self.deleted {
            changes.insert(key.clone(), None);
        }
        Ok(changes)
    }
}

impl Engine<crate::storage::MemoryStorage> {
    pub(crate) fn apply_committed(&mut self, operation: Operation) {
        apply(
            &mut self.values,
            &mut self.deleted,
            &mut self.change_bytes,
            operation,
        );
    }
}
