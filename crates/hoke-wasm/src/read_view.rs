use hoke_core::read::{get_from_sources, scan_sources_with_budget};
use hoke_core::sstable::{MAX_MERGE_SOURCES, SSTable};

use wasm_bindgen::prelude::*;

use crate::binding_error;
use crate::limits::{MAX_KEY_BYTES, MAX_PACKET_BYTES};
use crate::scan::{indexed_scan_options, pack_live_rows};
use crate::storage::JsStorage;
use crate::wal::WasmWal;

#[wasm_bindgen]
pub struct WasmReadView {
    tables: Option<Vec<SSTable<JsStorage>>>,
}

#[wasm_bindgen]
impl WasmReadView {
    #[wasm_bindgen(constructor)]
    pub fn new(adapters: Vec<JsValue>) -> Result<WasmReadView, JsError> {
        if adapters.len() > MAX_MERGE_SOURCES {
            return Err(binding_error("too many read view sources (maximum 64)"));
        }
        let mut tables = Vec::new();
        tables
            .try_reserve_exact(adapters.len())
            .map_err(|_| binding_error("could not allocate read view sources"))?;
        for adapter in adapters {
            tables.push(SSTable::open(JsStorage(adapter.unchecked_into())).map_err(binding_error)?);
        }
        Ok(Self {
            tables: Some(tables),
        })
    }

    pub fn get(&mut self, wal: &WasmWal, key: &[u8]) -> Result<Option<Vec<u8>>, JsError> {
        if key.len() > MAX_KEY_BYTES {
            return Err(binding_error("read view key exceeds 65536 bytes"));
        }
        let tables = self
            .tables
            .as_mut()
            .ok_or_else(|| binding_error("read view is closed"))?;
        let engine = wal.engine()?;
        get_from_sources(engine, tables, key).map_err(binding_error)
    }

    pub fn scan(
        &mut self,
        wal: &WasmWal,
        start: Option<Vec<u8>>,
        end: Option<Vec<u8>>,
        prefix: Option<Vec<u8>>,
        flags: u8,
        limit: u32,
    ) -> Result<Vec<u8>, JsError> {
        let options = indexed_scan_options(start, end, prefix, flags, limit)?;
        let tables = self
            .tables
            .as_mut()
            .ok_or_else(|| binding_error("read view is closed"))?;
        let engine = wal.engine()?;
        let rows = scan_sources_with_budget(engine, tables, options, MAX_PACKET_BYTES)
            .map_err(binding_error)?;
        pack_live_rows(rows)
    }

    pub fn close(&mut self) {
        self.tables.take();
    }
}
