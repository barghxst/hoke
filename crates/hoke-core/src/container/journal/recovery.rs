//! Validate journal history and defer incomplete-tail repair until requested.
use super::{
    Journal, JournalError, JournalRecovery,
    io::{checksum, io_error, payload_buffer, read_exact, record_end, sync},
};
use crate::container::{
    MAX_FILE_BYTES, MAX_PENDING_SEGMENTS, SegmentRef,
    codec::{self, FILE_HEADER_LEN, RECORD_HEADER_LEN, RecordKind},
};
use crate::storage::Storage;
use std::collections::BTreeMap;

impl<S: Storage> Journal<S> {
    pub fn open(storage: S) -> Result<Self, JournalError> {
        let mut journal = Self::open_deferred(storage)?;
        journal.repair_tail()?;
        Ok(journal)
    }

    /// Validate the outer journal without repairing its incomplete EOF tail.
    /// Callers may validate immutable payloads before explicitly enabling writes.
    pub fn open_deferred(mut storage: S) -> Result<Self, JournalError> {
        let length = storage.len().map_err(io_error("container length"))?;
        if length > MAX_FILE_BYTES {
            return Err(JournalError::Limit("file bytes"));
        }
        if length == 0 {
            storage
                .write_all_at(0, &codec::encode_file_header())
                .map_err(io_error("initialize container"))?;
            sync(&mut storage)?;
        } else {
            if length < FILE_HEADER_LEN as u64 {
                return Err(JournalError::Corruption("incomplete file header"));
            }
            let mut bytes = [0; FILE_HEADER_LEN];
            read_exact(&mut storage, 0, &mut bytes)?;
            codec::decode_file_header(&bytes)?;
        }
        let mut journal = Self {
            storage,
            end: FILE_HEADER_LEN as u64,
            sequence: 0,
            root: None,
            known: BTreeMap::new(),
            pending: 0,
            poisoned: false,
            tail_pending: false,
            recovery: JournalRecovery::default(),
        };
        while journal.end < length {
            if length - journal.end < RECORD_HEADER_LEN as u64 {
                break;
            }
            let mut bytes = [0; RECORD_HEADER_LEN];
            read_exact(&mut journal.storage, journal.end, &mut bytes)?;
            let header = codec::decode_record_header(&bytes)?;
            if header.sequence != journal.next_sequence()? {
                return Err(JournalError::Corruption("nonconsecutive record sequence"));
            }
            let end = record_end(journal.end, header.payload_len)?;
            if end > length {
                break;
            }
            let mut root_bytes = if header.kind == RecordKind::Root {
                Some(payload_buffer(header.payload_len)?)
            } else {
                None
            };
            checksum(
                &mut journal.storage,
                journal.end,
                &bytes,
                header.payload_len,
                root_bytes.as_deref_mut(),
            )?;
            if let Some(bytes) = root_bytes {
                let root = codec::decode_root(&bytes)?;
                journal.validate_root(&root)?;
                journal.publish(root);
            } else {
                if journal.pending == MAX_PENDING_SEGMENTS {
                    return Err(JournalError::Limit("pending segments"));
                }
                let reference = SegmentRef {
                    kind: header.kind.segment_kind().expect("segment record"),
                    offset: journal.end,
                    payload_len: header.payload_len,
                };
                journal.known.insert(reference.offset, reference);
                journal.pending += 1;
            }
            journal.end = end;
            journal.sequence = header.sequence;
            journal.recovery.records_scanned += 1;
        }
        if length > journal.end {
            journal.recovery.discarded_tail_bytes = length - journal.end;
            journal.tail_pending = true;
        }
        Ok(journal)
    }
    /// Repair only a validated outer incomplete tail; failures poison this handle.
    pub fn repair_tail(&mut self) -> Result<(), JournalError> {
        self.readable()?;
        if !self.tail_pending {
            return Ok(());
        }
        let result = self
            .storage
            .set_len(self.end)
            .map_err(io_error("truncate container tail"))
            .and_then(|()| sync(&mut self.storage));
        if result.is_err() {
            self.poisoned = true;
        }
        result?;
        self.tail_pending = false;
        Ok(())
    }
}
