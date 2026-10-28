//! Shared byte fixtures for reader validation and writer compatibility checks.
use std::collections::BTreeMap;

use crate::storage::{MemoryStorage, Storage};

use super::{
    format::{read_exact, u64_at},
    write_table,
};

pub(super) fn sample() -> MemoryStorage {
    let entries = BTreeMap::from([
        (vec![], Some(vec![])),
        (b"a".to_vec(), Some(b"first".to_vec())),
        (b"b".to_vec(), None),
        (vec![255, 0], Some(vec![0, 255])),
    ]);
    let mut storage = MemoryStorage::new();
    write_table(&mut storage, &entries).unwrap();
    storage
}
pub(super) fn bytes(mut storage: MemoryStorage) -> Vec<u8> {
    let mut result = vec![0; storage.len().unwrap() as usize];
    read_exact(&mut storage, 0, &mut result).unwrap();
    result
}
pub(super) fn from_bytes(bytes: &[u8]) -> MemoryStorage {
    let mut storage = MemoryStorage::new();
    storage.write_all_at(0, bytes).unwrap();
    storage
}
pub(super) fn repair_header(bytes: &mut [u8]) {
    let crc = crc32fast::hash(&bytes[..28]);
    bytes[28..32].copy_from_slice(&crc.to_le_bytes());
}
pub(super) fn repair_index(bytes: &mut [u8]) {
    let start = u64_at(bytes, 20) as usize;
    let end = bytes.len() - 4;
    let crc = crc32fast::hash(&bytes[start..end]);
    bytes[end..].copy_from_slice(&crc.to_le_bytes());
}
