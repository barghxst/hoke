//! Container-backed database operations and transient batch request decoding.

use hoke_core::BatchOperation;
use hoke_core::container::ContainerDatabase;

use wasm_bindgen::prelude::*;

use crate::binding_error;
use crate::limits::{MAX_KEY_BYTES, MAX_PACKET_BYTES, MAX_VALUE_BYTES};
use crate::scan::{indexed_scan_options, pack_live_rows};
use crate::storage::JsStorage;

/// Container-backed KV coordinator over a caller-owned synchronous handle.
/// Mutations publish roots; close neither commits nor closes the JS handle.
#[wasm_bindgen]
pub struct WasmContainerDb {
    database: Option<ContainerDatabase<JsStorage>>,
}

// This framing belongs to the JS/WASM call boundary, never to persistent data.
fn container_batch(packet: &[u8]) -> Result<Vec<BatchOperation>, JsError> {
    let count = packet
        .get(..4)
        .ok_or_else(|| binding_error("batch packet is missing its count"))?;
    let count = u32::from_le_bytes(count.try_into().unwrap()) as usize;
    if count > 1024 {
        return Err(binding_error("batch exceeds 1024 operations"));
    }
    let mut operations = Vec::with_capacity(count);
    let mut cursor = 4_usize;
    let mut bytes = 0_usize;
    for _ in 0..count {
        let header = packet
            .get(cursor..cursor + 9)
            .ok_or_else(|| binding_error("truncated batch operation header"))?;
        let tag = header[0];
        let key_len = u32::from_le_bytes(header[1..5].try_into().unwrap()) as usize;
        let value_len = u32::from_le_bytes(header[5..9].try_into().unwrap()) as usize;
        if tag != 1 && tag != 2 {
            return Err(binding_error("unknown batch operation tag"));
        }
        if tag == 2 && value_len != 0 {
            return Err(binding_error(
                "delete batch operation must have zero value length",
            ));
        }
        if key_len > MAX_KEY_BYTES || value_len > MAX_VALUE_BYTES {
            return Err(binding_error("batch key/value exceeds 64 KiB/16 MiB limit"));
        }
        bytes = bytes
            .checked_add(key_len)
            .and_then(|size| size.checked_add(value_len))
            .filter(|size| *size <= MAX_PACKET_BYTES)
            .ok_or_else(|| binding_error("batch exceeds 32 MiB key/value budget"))?;
        cursor += 9;
        let end = cursor
            .checked_add(key_len)
            .and_then(|size| size.checked_add(value_len))
            .ok_or_else(|| binding_error("batch packet length overflow"))?;
        let body = packet
            .get(cursor..end)
            .ok_or_else(|| binding_error("truncated batch operation payload"))?;
        let key = body[..key_len].to_vec();
        operations.push(if tag == 1 {
            BatchOperation::Put {
                key,
                value: body[key_len..].to_vec(),
            }
        } else {
            BatchOperation::Delete { key }
        });
        cursor = end;
    }
    if cursor != packet.len() {
        return Err(binding_error("trailing bytes in batch packet"));
    }
    Ok(operations)
}

#[wasm_bindgen]
impl WasmContainerDb {
    #[wasm_bindgen(constructor)]
    pub fn new(adapter: JsValue) -> Result<WasmContainerDb, JsError> {
        let database =
            ContainerDatabase::open(JsStorage(adapter.unchecked_into())).map_err(binding_error)?;
        Ok(Self {
            database: Some(database),
        })
    }

    pub fn get(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>, JsError> {
        self.database
            .as_mut()
            .ok_or_else(|| binding_error("container database is closed"))?
            .get(key)
            .map_err(binding_error)
    }

    pub fn put(&mut self, key: &[u8], value: &[u8]) -> Result<(), JsError> {
        self.database
            .as_mut()
            .ok_or_else(|| binding_error("container database is closed"))?
            .put(key, value)
            .map_err(binding_error)
    }

    pub fn delete(&mut self, key: &[u8]) -> Result<(), JsError> {
        self.database
            .as_mut()
            .ok_or_else(|| binding_error("container database is closed"))?
            .delete(key)
            .map_err(binding_error)
    }

    pub fn write_batch(&mut self, packet: &[u8]) -> Result<(), JsError> {
        let database = self
            .database
            .as_mut()
            .ok_or_else(|| binding_error("container database is closed"))?;
        let operations = container_batch(packet)?;
        database.write_batch(&operations).map_err(binding_error)
    }

    /// Same packed LIVE rows and scan flags as WasmWal.scan.
    pub fn scan(
        &mut self,
        start: Option<Vec<u8>>,
        end: Option<Vec<u8>>,
        prefix: Option<Vec<u8>>,
        flags: u8,
        limit: u32,
    ) -> Result<Vec<u8>, JsError> {
        let options = indexed_scan_options(start, end, prefix, flags, limit)?;
        let rows = self
            .database
            .as_mut()
            .ok_or_else(|| binding_error("container database is closed"))?
            .scan_with_budget(options, MAX_PACKET_BYTES)
            .map_err(binding_error)?;
        pack_live_rows(rows)
    }

    pub fn flush(&mut self) -> Result<(), JsError> {
        self.database
            .as_mut()
            .ok_or_else(|| binding_error("container database is closed"))?
            .flush()
            .map_err(binding_error)
    }

    pub fn compact(&mut self) -> Result<(), JsError> {
        self.database
            .as_mut()
            .ok_or_else(|| binding_error("container database is closed"))?
            .compact()
            .map_err(binding_error)
    }

    /// Five u64LE counters: generation, SSTs, WALs, dirty entries, dirty bytes.
    pub fn stats(&self) -> Result<Vec<u8>, JsError> {
        let stats = self
            .database
            .as_ref()
            .ok_or_else(|| binding_error("container database is closed"))?
            .stats();
        let mut packet = Vec::with_capacity(40);
        for value in [
            stats.generation,
            stats.table_count as u64,
            stats.wal_segments as u64,
            stats.memtable_entries as u64,
            stats.memtable_bytes as u64,
        ] {
            packet.extend_from_slice(&value.to_le_bytes());
        }
        Ok(packet)
    }

    pub fn is_poisoned(&self) -> Result<bool, JsError> {
        self.database
            .as_ref()
            .map(ContainerDatabase::is_poisoned)
            .ok_or_else(|| binding_error("container database is closed"))
    }

    pub fn close(&mut self) {
        self.database.take();
    }
}
