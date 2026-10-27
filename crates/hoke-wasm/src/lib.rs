//! Worker-only WAL bindings. The caller owns and closes the OPFS handle.
//!
//! No filesystem fallback is provided. The adapter must implement the synchronous
//! methods documented below; all exceptions are returned as engine I/O errors.

use std::io;
use std::ops::Bound;

use hoke_core::storage::Storage;
use hoke_core::{Engine, ScanOptions};
use wasm_bindgen::prelude::*;

const MAX_JS_INTEGER: u64 = (1_u64 << 53) - 1;
static ZERO_CHUNK: [u8; 64 * 1024] = [0; 64 * 1024];

#[wasm_bindgen]
extern "C" {
    type JsAdapter;

    #[wasm_bindgen(method, catch, js_name = getSize)]
    fn get_size(this: &JsAdapter) -> Result<f64, JsValue>;

    #[wasm_bindgen(method, catch, js_name = readAt)]
    fn read_bytes(this: &JsAdapter, buffer: &mut [u8], at: f64) -> Result<f64, JsValue>;

    #[wasm_bindgen(method, catch, js_name = writeAt)]
    fn write_bytes(this: &JsAdapter, buffer: &[u8], at: f64) -> Result<f64, JsValue>;

    #[wasm_bindgen(method, catch, js_name = truncate)]
    fn truncate(this: &JsAdapter, size: f64) -> Result<(), JsValue>;

    #[wasm_bindgen(method, catch, js_name = flush)]
    fn sync_handle(this: &JsAdapter) -> Result<(), JsValue>;
}

struct JsStorage(JsAdapter);

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn js_io(error: JsValue) -> io::Error {
    io::Error::other(
        error
            .as_string()
            .unwrap_or_else(|| format!("JavaScript storage exception: {error:?}")),
    )
}

fn checked_number(number: f64) -> io::Result<u64> {
    if !number.is_finite()
        || number < 0.0
        || number.fract() != 0.0
        || number > MAX_JS_INTEGER as f64
    {
        return Err(invalid("storage result is not a nonnegative safe integer"));
    }
    Ok(number as u64)
}

fn checked_offset(offset: u64) -> io::Result<f64> {
    if offset > MAX_JS_INTEGER {
        return Err(invalid("storage offset exceeds JavaScript safe integers"));
    }
    Ok(offset as f64)
}

fn checked_end(offset: u64, length: usize) -> io::Result<u64> {
    let length = u64::try_from(length).map_err(|_| invalid("buffer length exceeds u64"))?;
    let end = offset
        .checked_add(length)
        .ok_or_else(|| invalid("storage offset plus length overflows u64"))?;
    checked_offset(end)?;
    Ok(end)
}

fn checked_count(number: f64, available: usize) -> io::Result<usize> {
    let count = checked_number(number)?;
    if count > available as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "storage adapter returned more bytes than the supplied buffer",
        ));
    }
    Ok(count as usize)
}

impl JsStorage {
    fn write_bytes_at(&self, mut offset: u64, mut bytes: &[u8]) -> io::Result<()> {
        while !bytes.is_empty() {
            let count = checked_count(
                self.0
                    .write_bytes(bytes, checked_offset(offset)?)
                    .map_err(js_io)?,
                bytes.len(),
            )?;
            if count == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "storage adapter made no write progress",
                ));
            }
            offset += count as u64;
            bytes = &bytes[count..];
        }
        Ok(())
    }

    fn zero_fill(&self, mut from: u64, end: u64) -> io::Result<()> {
        while from < end {
            let length = (end - from).min(ZERO_CHUNK.len() as u64) as usize;
            self.write_bytes_at(from, &ZERO_CHUNK[..length])?;
            from += length as u64;
        }
        Ok(())
    }
}

impl Storage for JsStorage {
    fn len(&self) -> io::Result<u64> {
        checked_number(self.0.get_size().map_err(js_io)?)
    }

    fn read_at(&mut self, offset: u64, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() || offset >= self.len()? {
            return Ok(0);
        }
        checked_end(offset, buffer.len())?;
        let count = self
            .0
            .read_bytes(buffer, checked_offset(offset)?)
            .map_err(js_io)?;
        checked_count(count, buffer.len())
    }

    fn write_all_at(&mut self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        checked_end(offset, bytes.len())?;
        let old_len = self.len()?;
        if offset > old_len {
            self.zero_fill(old_len, offset)?;
        }
        self.write_bytes_at(offset, bytes)
    }

    fn set_len(&mut self, length: u64) -> io::Result<()> {
        let size = checked_offset(length)?;
        let old_len = self.len()?;
        self.0.truncate(size).map_err(js_io)?;
        if length > old_len {
            self.zero_fill(old_len, length)?;
        }
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        // No Rust-side buffering. The OPFS flush is the sync_all boundary.
        Ok(())
    }

    fn sync_all(&mut self) -> io::Result<()> {
        self.0.sync_handle().map_err(js_io)
    }
}

/// An exclusive WAL engine over a synchronous Worker-side byte adapter.
/// The adapter's handle must remain open until this engine is closed/freed.
#[wasm_bindgen]
pub struct WasmWal {
    engine: Option<Engine<JsStorage>>,
}

fn binding_error(error: impl std::fmt::Display) -> JsError {
    JsError::new(&error.to_string())
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
        const MAX_PACKET_BYTES: usize = 32 * 1024 * 1024;
        if flags & !7 != 0 {
            return Err(binding_error("invalid scan flags"));
        }
        if limit > 1000 {
            return Err(binding_error("scan limit exceeds 1000"));
        }
        if [&start, &end, &prefix]
            .iter()
            .any(|bound| bound.as_ref().is_some_and(|key| key.len() > 65536))
        {
            return Err(binding_error("scan bound or prefix exceeds 65536 bytes"));
        }
        let options = ScanOptions {
            start: match start {
                Some(key) if flags & 1 != 0 => Bound::Excluded(key),
                Some(key) => Bound::Included(key),
                None => Bound::Unbounded,
            },
            end: match end {
                Some(key) if flags & 2 != 0 => Bound::Included(key),
                Some(key) => Bound::Excluded(key),
                None => Bound::Unbounded,
            },
            prefix,
            reverse: flags & 4 != 0,
            limit: limit as usize,
        };
        options.validate().map_err(binding_error)?;
        let engine = self
            .engine
            .as_ref()
            .ok_or_else(|| binding_error("WAL engine is closed"))?;
        let rows = engine
            .scan_with_budget(options, MAX_PACKET_BYTES)
            .map_err(binding_error)?;
        let packet_bytes = rows.iter().try_fold(4_usize, |total, (key, value)| {
            total
                .checked_add(8)
                .and_then(|size| size.checked_add(key.len()))
                .and_then(|size| size.checked_add(value.len()))
                .filter(|size| *size <= MAX_PACKET_BYTES)
                .ok_or_else(|| binding_error("scan packet exceeds 32 MiB byte budget"))
        })?;
        let mut packet = Vec::new();
        packet
            .try_reserve_exact(packet_bytes)
            .map_err(|_| binding_error("could not allocate scan packet"))?;
        packet.extend_from_slice(&(rows.len() as u32).to_le_bytes());
        for (key, value) in rows {
            packet.extend_from_slice(&(key.len() as u32).to_le_bytes());
            packet.extend_from_slice(&(value.len() as u32).to_le_bytes());
            packet.extend_from_slice(&key);
            packet.extend_from_slice(&value);
        }
        Ok(packet)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn javascript_integer_boundaries_are_checked_before_io() {
        assert_eq!(checked_number(0.0).unwrap(), 0);
        assert_eq!(
            checked_number(MAX_JS_INTEGER as f64).unwrap(),
            MAX_JS_INTEGER
        );
        for value in [-1.0, 0.5, f64::NAN, f64::INFINITY, (1_u64 << 53) as f64] {
            assert!(checked_number(value).is_err());
        }
        assert!(checked_offset(MAX_JS_INTEGER).is_ok());
        assert!(checked_offset(MAX_JS_INTEGER + 1).is_err());
        assert!(checked_end(MAX_JS_INTEGER, 1).is_err());
        assert!(checked_end(u64::MAX, 1).is_err());
        assert_eq!(checked_end(MAX_JS_INTEGER - 1, 1).unwrap(), MAX_JS_INTEGER);
    }

    #[test]
    fn adapter_counts_cannot_exceed_the_slice() {
        assert_eq!(checked_count(2.0, 2).unwrap(), 2);
        assert_eq!(checked_count(0.0, 0).unwrap(), 0);
        for value in [3.0, -1.0, 0.5, f64::NAN, f64::INFINITY] {
            assert!(checked_count(value, 2).is_err());
        }
    }
}
