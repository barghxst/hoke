//! Opaque container segment publication and root metadata binding.

use hoke_core::container::{
    Journal, MAX_ROOT_REFS, REF_LEN, SegmentKind, SegmentRef, decode_ref, encode_ref, encode_root,
};

use wasm_bindgen::prelude::*;

use crate::binding_error;
use crate::storage::JsStorage;

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
