//! Exclusive WAL engine binding.

use hoke_core::Engine;

use wasm_bindgen::prelude::*;

use crate::binding_error;
use crate::scan::{MAX_PACKET_BYTES, pack_live_rows, scan_options};
use crate::storage::JsStorage;

/// An exclusive WAL engine over a synchronous Worker-side byte adapter.
/// The adapter's handle must remain open until this engine is closed/freed.
#[wasm_bindgen]
pub struct WasmWal {
    engine: Option<Engine<JsStorage>>,
}

impl WasmWal {
    /// Borrow the current overlay without exposing ownership to read views.
    pub(crate) fn engine(&self) -> Result<&Engine<JsStorage>, JsError> {
        self.engine
            .as_ref()
            .ok_or_else(|| binding_error("WAL engine is closed"))
    }
}

#[wasm_bindgen]
impl WasmWal {
    #[wasm_bindgen(constructor)]
    pub fn new(adapter: JsValue) -> Result<WasmWal, JsError> {
        let storage = JsStorage(adapter.unchecked_into());
        let engine = Engine::open(storage).map_err(binding_error)?;
        Ok(Self {
            engine: Some(engine),
        })
    }

    pub fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, JsError> {
        let engine = self
            .engine
            .as_ref()
            .ok_or_else(|| binding_error("WAL engine is closed"))?;
        Ok(engine.get(key).map(<[u8]>::to_vec))
    }

    /// Packed ephemeral scan response, independent of the durable WAL format.
    /// Flags: bit 0 excludes start, bit 1 includes end, bit 2 reverses order.
    pub fn scan(
        &self,
        start: Option<Vec<u8>>,
        end: Option<Vec<u8>>,
        prefix: Option<Vec<u8>>,
        flags: u8,
        limit: u32,
    ) -> Result<Vec<u8>, JsError> {
        let options = scan_options(start, end, prefix, flags, limit)?;
        let engine = self
            .engine
            .as_ref()
            .ok_or_else(|| binding_error("WAL engine is closed"))?;
        let rows = engine
            .scan_with_budget(options, MAX_PACKET_BYTES)
            .map_err(binding_error)?;
        pack_live_rows(rows)
    }

    pub fn put(&mut self, key: &[u8], value: &[u8]) -> Result<(), JsError> {
        self.engine
            .as_mut()
            .ok_or_else(|| binding_error("WAL engine is closed"))?
            .put(key, value)
            .map_err(binding_error)
    }

    pub fn delete(&mut self, key: &[u8]) -> Result<(), JsError> {
        self.engine
            .as_mut()
            .ok_or_else(|| binding_error("WAL engine is closed"))?
            .delete(key)
            .map_err(binding_error)
    }

    /// Synchronize and drop the engine even when final synchronization fails.
    /// This does not close the JS handle; its Worker owner must do so in finally.
    pub fn close(&mut self) -> Result<(), JsError> {
        match self.engine.take() {
            Some(engine) => engine.close().map_err(binding_error),
            None => Ok(()),
        }
    }
}
