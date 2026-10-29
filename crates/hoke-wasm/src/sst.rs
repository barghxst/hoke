use std::collections::BTreeMap;

use hoke_core::sstable::{
    Lookup, MAX_ENTRIES, MAX_MERGE_SOURCES, SSTable, merge_tables, write_table,
};
use hoke_core::storage::Storage;

use wasm_bindgen::prelude::*;

use crate::binding_error;
use crate::limits::{MAX_KEY_BYTES, MAX_PACKET_BYTES, MAX_VALUE_BYTES, TOMBSTONE};
use crate::scan::indexed_scan_options;
use crate::storage::JsStorage;

#[wasm_bindgen]
pub struct WasmSst {
    table: Option<SSTable<JsStorage>>,
}

fn packet_u32(packet: &[u8], cursor: &mut usize) -> Result<u32, JsError> {
    let bytes = packet
        .get(*cursor..cursor.saturating_add(4))
        .ok_or_else(|| binding_error("truncated SST packet"))?;
    *cursor += 4;
    Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
}

fn sst_entries(packet: &[u8]) -> Result<BTreeMap<Vec<u8>, Option<Vec<u8>>>, JsError> {
    if packet.len() > MAX_PACKET_BYTES {
        return Err(binding_error("SST packet exceeds 32 MiB byte budget"));
    }
    let mut cursor = 0;
    let count = packet_u32(packet, &mut cursor)? as usize;
    if count > MAX_ENTRIES {
        return Err(binding_error("SST packet exceeds entry limit"));
    }

    if count > (packet.len() - cursor) / 8 {
        return Err(binding_error("truncated SST packet entries"));
    }
    let mut entries = BTreeMap::new();
    for _ in 0..count {
        let key_len = packet_u32(packet, &mut cursor)? as usize;
        let value_len = packet_u32(packet, &mut cursor)?;
        if key_len > MAX_KEY_BYTES {
            return Err(binding_error("SST key exceeds 65536 bytes"));
        }
        if value_len != TOMBSTONE && value_len as usize > MAX_VALUE_BYTES {
            return Err(binding_error("SST value exceeds 16 MiB"));
        }
        let end = cursor
            .checked_add(key_len)
            .and_then(|end| {
                end.checked_add(if value_len == TOMBSTONE {
                    0
                } else {
                    value_len as usize
                })
            })
            .filter(|end| *end <= packet.len())
            .ok_or_else(|| binding_error("truncated SST packet payload"))?;
        let key = &packet[cursor..cursor + key_len];
        if entries
            .last_key_value()
            .is_some_and(|(previous, _): (&Vec<u8>, _)| previous.as_slice() >= key)
        {
            return Err(binding_error("SST packet keys are not strictly sorted"));
        }
        let value = (value_len != TOMBSTONE).then(|| packet[cursor + key_len..end].to_vec());
        entries.insert(key.to_vec(), value);
        cursor = end;
    }
    if cursor != packet.len() {
        return Err(binding_error("trailing SST packet bytes"));
    }
    Ok(entries)
}

#[wasm_bindgen]
impl WasmSst {
    #[wasm_bindgen(constructor)]
    pub fn new(adapter: JsValue) -> Result<WasmSst, JsError> {
        let table = SSTable::open(JsStorage(adapter.unchecked_into())).map_err(binding_error)?;
        Ok(Self { table: Some(table) })
    }

    pub fn create(adapter: JsValue, packet: &[u8]) -> Result<WasmSst, JsError> {
        let entries = sst_entries(packet)?;
        let mut storage = JsStorage(adapter.unchecked_into());
        write_table(&mut storage, &entries).map_err(binding_error)?;
        storage.flush().map_err(binding_error)?;
        storage.sync_all().map_err(binding_error)?;
        let table = SSTable::open(storage).map_err(binding_error)?;
        Ok(Self { table: Some(table) })
    }

    pub fn merge(destination: JsValue, sources: Vec<JsValue>) -> Result<WasmSst, JsError> {
        if sources.len() > MAX_MERGE_SOURCES {
            return Err(binding_error("too many SST merge sources (maximum 64)"));
        }
        let mut tables = Vec::new();
        tables
            .try_reserve_exact(sources.len())
            .map_err(|_| binding_error("could not allocate SST merge sources"))?;
        for source in sources {
            tables.push(SSTable::open(JsStorage(source.unchecked_into())).map_err(binding_error)?);
        }
        let mut storage = JsStorage(destination.unchecked_into());
        merge_tables(&mut storage, &mut tables).map_err(binding_error)?;
        storage.flush().map_err(binding_error)?;
        storage.sync_all().map_err(binding_error)?;
        let table = SSTable::open(storage).map_err(binding_error)?;
        Ok(Self { table: Some(table) })
    }

    pub fn lookup(&mut self, key: &[u8]) -> Result<Vec<u8>, JsError> {
        if key.len() > MAX_KEY_BYTES {
            return Err(binding_error("SST key exceeds 65536 bytes"));
        }
        let table = self
            .table
            .as_mut()
            .ok_or_else(|| binding_error("SST is closed"))?;
        match table.get(key).map_err(binding_error)? {
            Lookup::Missing => Ok(vec![0]),
            Lookup::Deleted => Ok(vec![1]),
            Lookup::Value(value) => {
                let mut result = Vec::new();
                result
                    .try_reserve_exact(1 + value.len())
                    .map_err(|_| binding_error("could not allocate SST lookup"))?;
                result.push(2);
                result.extend_from_slice(&value);
                Ok(result)
            }
        }
    }

    pub fn scan(
        &mut self,
        start: Option<Vec<u8>>,
        end: Option<Vec<u8>>,
        prefix: Option<Vec<u8>>,
        flags: u8,
        limit: u32,
    ) -> Result<Vec<u8>, JsError> {
        let options = indexed_scan_options(start, end, prefix, flags, limit)?;
        let table = self
            .table
            .as_mut()
            .ok_or_else(|| binding_error("SST is closed"))?;
        let mut packet = vec![0; 4];
        let mut count = 0u32;
        let mut positions = table.range_positions(&options.start, &options.end);
        while count < limit {
            let position = if options.reverse {
                positions.next_back()
            } else {
                positions.next()
            };
            let Some(position) = position else { break };
            let key = table
                .key_at(position)
                .ok_or_else(|| binding_error("SST index lost its key"))?
                .to_vec();
            if options
                .prefix
                .as_ref()
                .is_some_and(|prefix| !key.starts_with(prefix))
            {
                continue;
            }
            let value = match table.get(&key).map_err(binding_error)? {
                Lookup::Missing => return Err(binding_error("SST index lost its record")),
                Lookup::Deleted => None,
                Lookup::Value(value) => Some(value),
            };
            let added = 8 + key.len() + value.as_ref().map_or(0, Vec::len);
            if packet
                .len()
                .checked_add(added)
                .is_none_or(|size| size > MAX_PACKET_BYTES)
            {
                return Err(binding_error("SST scan packet exceeds 32 MiB byte budget"));
            }
            packet
                .try_reserve_exact(added)
                .map_err(|_| binding_error("could not allocate SST scan packet"))?;
            packet.extend_from_slice(&(key.len() as u32).to_le_bytes());
            packet.extend_from_slice(
                &value
                    .as_ref()
                    .map_or(TOMBSTONE, |value| value.len() as u32)
                    .to_le_bytes(),
            );
            packet.extend_from_slice(&key);
            if let Some(value) = value {
                packet.extend_from_slice(&value);
            }
            count += 1;
        }
        packet[..4].copy_from_slice(&count.to_le_bytes());
        Ok(packet)
    }

    pub fn close(&mut self) {
        self.table.take();
    }
}
