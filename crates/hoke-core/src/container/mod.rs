//! Append-only container publication journal. See ADR 0018 for the byte contract.
use std::collections::BTreeMap;
use std::{fmt, io};

use crate::storage::Storage;
pub(crate) mod codec;
pub use codec::{
    CodecError, MAX_FILE_BYTES, MAX_PENDING_SEGMENTS, MAX_ROOT_REFS, MAX_SEGMENT_BYTES, REF_LEN,
    Root, SegmentKind, SegmentRef, decode_ref, decode_root, encode_ref, encode_root,
};
use codec::{FILE_HEADER_LEN, RECORD_HEADER_LEN, RecordHeader, RecordKind, TRAILER_LEN};

#[derive(Debug)]
pub enum JournalError {
    Io {
        operation: &'static str,
        source: io::Error,
    },
    Codec(CodecError),
    Corruption(&'static str),
    Limit(&'static str),
    Poisoned,
}
impl fmt::Display for JournalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { operation, source } => write!(f, "{operation}: {source}"),
            Self::Codec(error) => error.fmt(f),
            Self::Corruption(reason) => write!(f, "corrupt container: {reason}"),
            Self::Limit(reason) => write!(f, "container limit: {reason}"),
            Self::Poisoned => {
                f.write_str("container outcome unknown; reopen before further operations")
            }
        }
    }
}
impl std::error::Error for JournalError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Codec(error) => Some(error),
            _ => None,
        }
    }
}
impl From<CodecError> for JournalError {
    fn from(error: CodecError) -> Self {
        Self::Codec(error)
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct JournalRecovery {
    pub discarded_tail_bytes: u64,
    pub records_scanned: u64,
}

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
    recovery: JournalRecovery,
}
fn io_error(operation: &'static str) -> impl FnOnce(io::Error) -> JournalError {
    move |source| JournalError::Io { operation, source }
}
fn read_exact<S: Storage>(
    storage: &mut S,
    offset: u64,
    bytes: &mut [u8],
) -> Result<(), JournalError> {
    let mut done = 0;
    while done < bytes.len() {
        let count = storage
            .read_at(offset + done as u64, &mut bytes[done..])
            .map_err(io_error("read container"))?;
        if count == 0 {
            return Err(io_error("read container")(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "container shorter than reported length",
            )));
        }
        if count > bytes.len() - done {
            return Err(JournalError::Corruption("storage violated read contract"));
        }
        done += count;
    }
    Ok(())
}
fn sync<S: Storage>(storage: &mut S) -> Result<(), JournalError> {
    storage.flush().map_err(io_error("flush container"))?;
    storage.sync_all().map_err(io_error("sync container"))
}
fn record_end(offset: u64, length: u64) -> Result<u64, JournalError> {
    offset
        .checked_add(RECORD_HEADER_LEN as u64)
        .and_then(|n| n.checked_add(length))
        .and_then(|n| n.checked_add(TRAILER_LEN as u64))
        .filter(|n| *n <= MAX_FILE_BYTES)
        .ok_or(JournalError::Limit("file bytes"))
}
fn payload_buffer(length: u64) -> Result<Vec<u8>, JournalError> {
    let length =
        usize::try_from(length).map_err(|_| JournalError::Limit("payload address space"))?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length)
        .map_err(|_| JournalError::Codec(CodecError::Allocation))?;
    bytes.resize(length, 0);
    Ok(bytes)
}
/// Validate a complete record without allocating a segment payload during recovery.
fn checksum<S: Storage>(
    storage: &mut S,
    offset: u64,
    header: &[u8; 32],
    length: u64,
    mut output: Option<&mut [u8]>,
) -> Result<(), JournalError> {
    let mut hash = crc32fast::Hasher::new();
    hash.update(header);
    let mut scratch = [0; 64 * 1024];
    let mut done = 0;
    while done < length {
        let count = (length - done).min(scratch.len() as u64) as usize;
        read_exact(
            storage,
            offset + RECORD_HEADER_LEN as u64 + done,
            &mut scratch[..count],
        )?;
        hash.update(&scratch[..count]);
        if let Some(bytes) = output.as_deref_mut() {
            bytes[done as usize..done as usize + count].copy_from_slice(&scratch[..count]);
        }
        done += count as u64;
    }
    let mut trailer = [0; 4];
    read_exact(
        storage,
        offset + RECORD_HEADER_LEN as u64 + length,
        &mut trailer,
    )?;
    if hash.finalize() != u32::from_le_bytes(trailer) {
        return Err(JournalError::Corruption("record checksum"));
    }
    Ok(())
}
impl<S: Storage> Journal<S> {
    pub fn open(mut storage: S) -> Result<Self, JournalError> {
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
            journal
                .storage
                .set_len(journal.end)
                .map_err(io_error("truncate container tail"))?;
            sync(&mut journal.storage)?;
        }
        Ok(journal)
    }
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
    fn writable(&self) -> Result<(), JournalError> {
        if self.poisoned {
            Err(JournalError::Poisoned)
        } else {
            Ok(())
        }
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
        self.writable()?;
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
