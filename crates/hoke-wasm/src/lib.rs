//! Worker-only WAL, immutable SST, and container journal bindings. The caller owns and closes the OPFS handle.
//!
//! No filesystem fallback is provided. The adapter must implement the synchronous
//! methods documented below; all exceptions are returned as engine I/O errors.

use std::collections::BTreeMap;
use std::io;
use std::ops::Bound;

use hoke_core::container::{
    ContainerDatabase, Journal, MAX_ROOT_REFS, REF_LEN, SegmentKind, SegmentRef, decode_ref,
    encode_ref, encode_root,
};
use hoke_core::read::{get_from_sources, scan_sources_with_budget};
use hoke_core::sstable::{
    Lookup, MAX_ENTRIES, MAX_MERGE_SOURCES, SSTable, merge_tables, write_table,
};
use hoke_core::storage::Storage;
use hoke_core::{BatchOperation, Engine, ScanOptions};
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

const MAX_PACKET_BYTES: usize = 32 * 1024 * 1024;
const MAX_KEY_BYTES: usize = 65536;
const MAX_VALUE_BYTES: usize = 16 * 1024 * 1024;
const TOMBSTONE: u32 = u32::MAX;

fn pack_live_rows(rows: Vec<(Vec<u8>, Vec<u8>)>) -> Result<Vec<u8>, JsError> {
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

/// An immutable native-format SST over a caller-owned synchronous byte adapter.
/// Creation synchronizes one candidate file; it does not publish a database commit.
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
    // Each entry needs at least the two lengths, including empty tombstones.
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

fn sst_scan_options(
    start: Option<Vec<u8>>,
    end: Option<Vec<u8>>,
    prefix: Option<Vec<u8>>,
    flags: u8,
    limit: u32,
) -> Result<ScanOptions, JsError> {
    if flags & !7 != 0 {
        return Err(binding_error("invalid scan flags"));
    }
    if limit > 1000 {
        return Err(binding_error("scan limit exceeds 1000"));
    }
    if [&start, &end, &prefix]
        .iter()
        .any(|key| key.as_ref().is_some_and(|key| key.len() > MAX_KEY_BYTES))
    {
        return Err(binding_error("scan bound or prefix exceeds 65536 bytes"));
    }
    let mut options = ScanOptions {
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
    if let Some(prefix) = &options.prefix {
        let start = match &options.start {
            Bound::Included(key) | Bound::Excluded(key) => Some(key),
            Bound::Unbounded => None,
        };
        if start.is_none_or(|key| key < prefix) {
            options.start = Bound::Included(prefix.clone());
        }
        if let Some(position) = prefix.iter().rposition(|byte| *byte != 255) {
            let mut upper = prefix[..=position].to_vec();
            upper[position] += 1;
            let end = match &options.end {
                Bound::Included(key) | Bound::Excluded(key) => Some(key),
                Bound::Unbounded => None,
            };
            if end.is_none_or(|key| key >= &upper) {
                options.end = Bound::Excluded(upper);
            }
        }
    }
    Ok(options)
}

#[wasm_bindgen]
impl WasmSst {
    #[wasm_bindgen(constructor)]
    pub fn new(adapter: JsValue) -> Result<WasmSst, JsError> {
        let table = SSTable::open(JsStorage(adapter.unchecked_into())).map_err(binding_error)?;
        Ok(Self { table: Some(table) })
    }

    /// Validate the complete ephemeral packet before touching the destination.
    /// A write/sync error may leave an incomplete candidate, never a published run.
    pub fn create(adapter: JsValue, packet: &[u8]) -> Result<WasmSst, JsError> {
        let entries = sst_entries(packet)?;
        let mut storage = JsStorage(adapter.unchecked_into());
        write_table(&mut storage, &entries).map_err(binding_error)?;
        storage.flush().map_err(binding_error)?;
        storage.sync_all().map_err(binding_error)?;
        let table = SSTable::open(storage).map_err(binding_error)?;
        Ok(Self { table: Some(table) })
    }

    /// Stream a candidate from immutable sources ordered oldest to newest.
    /// All source indexes are opened before destination I/O; winning record CRCs
    /// are checked during the merge. Errors can leave an unpublished candidate.
    /// The caller must supply distinct destination/source files and close handles.
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

    /// Tag 0 means missing, 1 tombstone, 2 followed by bytes means a live value.
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

    /// Packed indexed rows, including tombstones as value length 0xffffffff.
    /// Flags match WasmWal.scan; the packet is not a durable file format.
    pub fn scan(
        &mut self,
        start: Option<Vec<u8>>,
        end: Option<Vec<u8>>,
        prefix: Option<Vec<u8>>,
        flags: u8,
        limit: u32,
    ) -> Result<Vec<u8>, JsError> {
        let options = sst_scan_options(start, end, prefix, flags, limit)?;
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

    /// Release the immutable index without closing the caller-owned JS handle.
    pub fn close(&mut self) {
        self.table.take();
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

/// A live WAL overlay over immutable SST sources ordered oldest to newest.
/// Table indexes are owned here; adapter handles remain owned by the caller.
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
        let engine = wal
            .engine
            .as_ref()
            .ok_or_else(|| binding_error("WAL engine is closed"))?;
        get_from_sources(engine, tables, key).map_err(binding_error)
    }

    /// Packed live rows using the same framing and flags as WasmWal.scan.
    pub fn scan(
        &mut self,
        wal: &WasmWal,
        start: Option<Vec<u8>>,
        end: Option<Vec<u8>>,
        prefix: Option<Vec<u8>>,
        flags: u8,
        limit: u32,
    ) -> Result<Vec<u8>, JsError> {
        let options = sst_scan_options(start, end, prefix, flags, limit)?;
        let tables = self
            .tables
            .as_mut()
            .ok_or_else(|| binding_error("read view is closed"))?;
        let engine = wal
            .engine
            .as_ref()
            .ok_or_else(|| binding_error("WAL engine is closed"))?;
        let rows = scan_sources_with_budget(engine, tables, options, MAX_PACKET_BYTES)
            .map_err(binding_error)?;
        pack_live_rows(rows)
    }

    /// Drop immutable indexes without closing caller-owned JS handles.
    pub fn close(&mut self) {
        self.tables.take();
    }
}

/// Append-only container publication over one caller-owned synchronous handle.
/// Segment bytes remain opaque; close never commits or closes the JS handle.
#[wasm_bindgen]
pub struct WasmJournal {
    journal: Option<Journal<JsStorage>>,
}

fn journal_refs(packet: &[u8]) -> Result<Vec<SegmentRef>, JsError> {
    if !packet.len().is_multiple_of(REF_LEN) {
        return Err(binding_error(
            "container reference list must contain complete 24-byte descriptors",
        ));
    }
    let count = packet.len() / REF_LEN;
    if count > MAX_ROOT_REFS {
        return Err(binding_error(
            "container reference list exceeds 4096 entries",
        ));
    }
    let mut refs = Vec::new();
    refs.try_reserve_exact(count)
        .map_err(|_| binding_error("could not allocate container references"))?;
    for descriptor in packet.chunks_exact(REF_LEN) {
        refs.push(decode_ref(descriptor).map_err(binding_error)?);
    }
    Ok(refs)
}

#[wasm_bindgen]
impl WasmJournal {
    #[wasm_bindgen(constructor)]
    pub fn new(adapter: JsValue) -> Result<WasmJournal, JsError> {
        let journal = Journal::open(JsStorage(adapter.unchecked_into())).map_err(binding_error)?;
        Ok(Self {
            journal: Some(journal),
        })
    }

    /// Append an immutable opaque segment without publishing a logical commit.
    pub fn append(&mut self, kind: f64, payload: &[u8]) -> Result<Vec<u8>, JsError> {
        let journal = self
            .journal
            .as_mut()
            .ok_or_else(|| binding_error("container journal is closed"))?;
        if kind != 1.0 && kind != 2.0 {
            return Err(binding_error(
                "unknown segment kind; expected 1 (WAL) or 2 (SST)",
            ));
        }
        let kind = SegmentKind::try_from(kind as u8).map_err(binding_error)?;
        let reference = journal
            .append_segment(kind, payload)
            .map_err(binding_error)?;
        Ok(encode_ref(&reference).map_err(binding_error)?.to_vec())
    }

    /// Validate descriptors, then publish a root through two file sync barriers.
    /// Any error after publication starts has an unknown outcome until reopen.
    pub fn commit(&mut self, table_refs: &[u8], wal_refs: &[u8]) -> Result<(), JsError> {
        let journal = self
            .journal
            .as_mut()
            .ok_or_else(|| binding_error("container journal is closed"))?;
        let table_count = table_refs.len() / REF_LEN;
        let wal_count = wal_refs.len() / REF_LEN;
        if table_count.saturating_add(wal_count) > MAX_ROOT_REFS {
            return Err(binding_error("container root exceeds 4096 references"));
        }
        let tables = journal_refs(table_refs)?;
        let wal = journal_refs(wal_refs)?;
        journal.commit(&tables, &wal).map_err(binding_error)
    }

    /// Encoded root metadata, or an empty vector before the first commit.
    pub fn root(&self) -> Result<Vec<u8>, JsError> {
        let journal = self
            .journal
            .as_ref()
            .ok_or_else(|| binding_error("container journal is closed"))?;
        match journal.root() {
            Some(root) => encode_root(root).map_err(binding_error),
            None => Ok(Vec::new()),
        }
    }

    /// Read only a known active/pending descriptor, checking its payload CRC.
    pub fn read(&mut self, reference: &[u8]) -> Result<Vec<u8>, JsError> {
        let journal = self
            .journal
            .as_mut()
            .ok_or_else(|| binding_error("container journal is closed"))?;
        let reference = decode_ref(reference).map_err(binding_error)?;
        journal.read_segment(&reference).map_err(binding_error)
    }

    pub fn discarded_tail_bytes(&self) -> Result<u64, JsError> {
        self.journal
            .as_ref()
            .map(|journal| journal.recovery().discarded_tail_bytes)
            .ok_or_else(|| binding_error("container journal is closed"))
    }

    pub fn records_scanned(&self) -> Result<u64, JsError> {
        self.journal
            .as_ref()
            .map(|journal| journal.recovery().records_scanned)
            .ok_or_else(|| binding_error("container journal is closed"))
    }

    pub fn is_poisoned(&self) -> Result<bool, JsError> {
        self.journal
            .as_ref()
            .map(Journal::is_poisoned)
            .ok_or_else(|| binding_error("container journal is closed"))
    }

    /// Drop Rust state only. This neither synchronizes nor closes the JS handle.
    pub fn close(&mut self) {
        self.journal.take();
    }
}

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
        let options = sst_scan_options(start, end, prefix, flags, limit)?;
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
