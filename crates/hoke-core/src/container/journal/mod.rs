//! Append-only segment writes and root publication.
mod io;
mod recovery;
#[cfg(test)]
mod tests;
mod types;

use super::{
    CodecError, MAX_PENDING_SEGMENTS, MAX_ROOT_REFS, Root, SegmentKind, SegmentRef,
    codec::{self, RECORD_HEADER_LEN, RecordHeader, RecordKind, TRAILER_LEN},
};
use crate::storage::Storage;
use io::{checksum, io_error, payload_buffer, read_exact, record_end, sync};
use std::collections::BTreeMap;
pub use types::{JournalError, JournalRecovery};

/// Owns one exclusive byte store. Appending a segment does not publish or sync it.
/// Only a successful commit publishes a new root; failed I/O requires reopening.
pub struct Journal<S: Storage> {
    storage: S,
    end: u64,
    sequence: u64,
    root: Option<Root>,
    known: BTreeMap<u64, SegmentRef>,
    pending: usize,
    poisoned: bool,
    tail_pending: bool,
    recovery: JournalRecovery,
}
impl<S: Storage> Journal<S> {
    fn next_sequence(&self) -> Result<u64, JournalError> {
        self.sequence
            .checked_add(1)
            .ok_or(JournalError::Limit("record sequence"))
    }
    fn next_generation(&self) -> Result<u64, JournalError> {
        self.root
            .as_ref()
            .map_or(0, |r| r.generation)
            .checked_add(1)
            .ok_or(JournalError::Limit("root generation"))
    }
    fn readable(&self) -> Result<(), JournalError> {
        if self.poisoned {
            Err(JournalError::Poisoned)
        } else {
            Ok(())
        }
    }
    fn writable(&self) -> Result<(), JournalError> {
        self.readable()?;
        if self.tail_pending {
            return Err(JournalError::Corruption(
                "incomplete tail requires explicit repair",
            ));
        }
        Ok(())
    }
    fn validate_root(&self, root: &Root) -> Result<(), JournalError> {
        if root.generation != self.next_generation()? {
            return Err(JournalError::Corruption("nonconsecutive root generation"));
        }
        for reference in root.tables.iter().chain(&root.wal) {
            if self.known.get(&reference.offset) != Some(reference) {
                return Err(JournalError::Corruption("root references unknown segment"));
            }
        }
        Ok(())
    }
    fn publish(&mut self, root: Root) {
        self.known = root
            .tables
            .iter()
            .chain(&root.wal)
            .map(|r| (r.offset, *r))
            .collect();
        self.pending = 0;
        self.root = Some(root);
    }
    fn append_record(
        &mut self,
        bytes: &[u8; 32],
        payload: &[u8],
        end: u64,
    ) -> Result<(), JournalError> {
        let mut hash = crc32fast::Hasher::new();
        hash.update(bytes);
        hash.update(payload);
        let result = (|| {
            self.storage
                .write_all_at(self.end, bytes)
                .map_err(io_error("append container header"))?;
            self.storage
                .write_all_at(self.end + RECORD_HEADER_LEN as u64, payload)
                .map_err(io_error("append container payload"))?;
            self.storage
                .write_all_at(end - TRAILER_LEN as u64, &hash.finalize().to_le_bytes())
                .map_err(io_error("append container checksum"))
        })();
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }
    pub fn append_segment(
        &mut self,
        kind: SegmentKind,
        payload: &[u8],
    ) -> Result<SegmentRef, JournalError> {
        self.writable()?;
        if self.pending == MAX_PENDING_SEGMENTS {
            return Err(JournalError::Limit("pending segments"));
        }
        let sequence = self.next_sequence()?;
        let bytes = codec::encode_record_header(&RecordHeader {
            kind: kind.record_kind(),
            sequence,
            payload_len: payload.len() as u64,
        })?;
        let end = record_end(self.end, payload.len() as u64)?;
        let reference = SegmentRef {
            kind,
            offset: self.end,
            payload_len: payload.len() as u64,
        };
        self.append_record(&bytes, payload, end)?;
        self.end = end;
        self.sequence = sequence;
        self.pending += 1;
        self.known.insert(reference.offset, reference);
        Ok(reference)
    }
    pub fn commit(
        &mut self,
        tables: &[SegmentRef],
        wal: &[SegmentRef],
    ) -> Result<(), JournalError> {
        self.writable()?;
        if tables
            .len()
            .checked_add(wal.len())
            .is_none_or(|n| n > MAX_ROOT_REFS)
        {
            return Err(JournalError::Limit("root references"));
        }
        let root = Root {
            generation: self.next_generation()?,
            tables: tables.to_vec(),
            wal: wal.to_vec(),
        };
        let payload = codec::encode_root(&root)?;
        self.validate_root(&root)?;
        let sequence = self.next_sequence()?;
        let bytes = codec::encode_record_header(&RecordHeader {
            kind: RecordKind::Root,
            sequence,
            payload_len: payload.len() as u64,
        })?;
        let end = record_end(self.end, payload.len() as u64)?;
        let result = (|| {
            sync(&mut self.storage)?;
            self.append_record(&bytes, &payload, end)?;
            sync(&mut self.storage)
        })();
        if result.is_err() {
            self.poisoned = true;
        }
        result?;
        self.end = end;
        self.sequence = sequence;
        self.publish(root);
        Ok(())
    }
    pub fn read_segment(&mut self, reference: &SegmentRef) -> Result<Vec<u8>, JournalError> {
        self.readable()?;
        if self.known.get(&reference.offset) != Some(reference) {
            return Err(JournalError::Corruption("unknown segment reference"));
        }
        let result = (|| {
            let mut header_bytes = [0; RECORD_HEADER_LEN];
            read_exact(&mut self.storage, reference.offset, &mut header_bytes)?;
            let header = codec::decode_record_header(&header_bytes)?;
            if header.kind.segment_kind() != Some(reference.kind)
                || header.payload_len != reference.payload_len
            {
                return Err(JournalError::Corruption("segment header changed"));
            }
            let mut payload = payload_buffer(reference.payload_len)?;
            checksum(
                &mut self.storage,
                reference.offset,
                &header_bytes,
                reference.payload_len,
                Some(&mut payload),
            )?;
            Ok(payload)
        })();
        // An unknown caller-supplied ref is rejected above without touching
        // storage. A known record failing reread makes future publication unsafe.
        if matches!(
            &result,
            Err(JournalError::Io { .. } | JournalError::Corruption(_))
        ) || matches!(&result, Err(JournalError::Codec(error)) if !matches!(error, CodecError::Allocation))
        {
            self.poisoned = true;
        }
        result
    }
    pub fn root(&self) -> Option<&Root> {
        self.root.as_ref()
    }
    pub fn recovery(&self) -> JournalRecovery {
        self.recovery
    }
    pub fn is_poisoned(&self) -> bool {
        self.poisoned
    }
    pub fn into_storage(self) -> S {
        self.storage
    }
}
