//! Synchronous JavaScript byte adapter and safe-integer I/O boundary.

use std::io;

use hoke_core::storage::Storage;
use wasm_bindgen::prelude::*;

const MAX_JS_INTEGER: u64 = (1_u64 << 53) - 1;
static ZERO_CHUNK: [u8; 64 * 1024] = [0; 64 * 1024];

#[wasm_bindgen]
extern "C" {
    pub(crate) type JsAdapter;

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

pub(crate) struct JsStorage(pub(crate) JsAdapter);

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
