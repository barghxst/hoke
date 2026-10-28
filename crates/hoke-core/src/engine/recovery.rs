//! Checked WAL replay and repair of physically incomplete EOF tails.

use std::collections::{BTreeMap, BTreeSet};
use std::io;

use super::changes::apply;
use super::error::{corrupt, io_error};
use super::{Engine, Error, sync};
use crate::storage::Storage;
use crate::wal::{self, CodecError};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Recovery {
    pub records_replayed: u64,
    pub discarded_tail_bytes: u64,
}

fn read_exact<S: Storage>(storage: &mut S, offset: u64, buffer: &mut [u8]) -> Result<(), Error> {
    let mut read = 0;
    while read < buffer.len() {
        let position = offset
            .checked_add(read as u64)
            .ok_or_else(|| corrupt(offset, "read offset overflow"))?;
        let count = storage
            .read_at(position, &mut buffer[read..])
            .map_err(io_error("read WAL"))?;
        if count == 0 {
            return Err(Error::Io {
                operation: "read WAL",
                source: io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "WAL shorter than its reported length",
                ),
            });
        }
        if count > buffer.len() - read {
            return Err(corrupt(position, "storage violated read contract"));
        }
        read += count;
    }
    Ok(())
}
impl<S: Storage> Engine<S> {
    /// Open an exclusively owned storage and replay checked records.
    ///
    /// Only a physically incomplete final frame is discarded. A full bad header,
    /// checksum mismatch or sequence gap fails without rewriting the file.
    /// Creating a new file does not yet synchronize its parent directory entry.
    pub fn open(mut storage: S) -> Result<Self, Error> {
        let mut length = storage.len().map_err(io_error("read WAL length"))?;
        if length == 0 {
            storage
                .write_all_at(0, &wal::encode_file_header())
                .map_err(io_error("initialize WAL"))?;
            sync(&mut storage)?;
            length = wal::FILE_HEADER_LEN as u64;
        }
        if length < wal::FILE_HEADER_LEN as u64 {
            return Err(corrupt(
                0,
                "incomplete file header; automatic repair is unsafe",
            ));
        }
        let mut file_header = [0; wal::FILE_HEADER_LEN];
        read_exact(&mut storage, 0, &mut file_header)?;
        if let Err(error) = wal::decode_file_header(&file_header) {
            return Err(match error {
                CodecError::UnsupportedVersion(version) => Error::UnsupportedVersion(version),
                other => corrupt(0, other),
            });
        }
        let mut values = BTreeMap::new();
        let mut deleted = BTreeSet::new();
        let mut change_bytes = 0;
        let mut sequence = 0_u64;
        let mut offset = wal::FILE_HEADER_LEN as u64;
        let mut recovery = Recovery::default();
        while offset < length {
            let remaining = length - offset;
            if remaining < wal::FRAME_HEADER_LEN as u64 {
                break;
            }
            let mut header = [0; wal::FRAME_HEADER_LEN];
            read_exact(&mut storage, offset, &mut header)?;
            let decoded =
                wal::decode_frame_header(&header).map_err(|error| corrupt(offset, error))?;
            let expected = sequence
                .checked_add(1)
                .ok_or_else(|| corrupt(offset, "sequence overflow"))?;
            if decoded.sequence != expected {
                return Err(corrupt(
                    offset,
                    format!("expected sequence {expected}, got {}", decoded.sequence),
                ));
            }
            let frame_len = decoded.frame_len() as u64;
            if remaining < frame_len {
                break;
            }
            let buffer_len = decoded.payload_len() + wal::TRAILER_LEN;
            let mut payload = Vec::new();
            payload
                .try_reserve_exact(buffer_len)
                .map_err(|_| Error::AllocationFailed)?;
            payload.resize(buffer_len, 0);
            read_exact(
                &mut storage,
                offset + wal::FRAME_HEADER_LEN as u64,
                &mut payload,
            )?;
            let (next_sequence, operation) =
                wal::decode_frame(&header, &payload).map_err(|error| corrupt(offset, error))?;
            apply(&mut values, &mut deleted, &mut change_bytes, operation);
            sequence = next_sequence;
            offset = offset
                .checked_add(frame_len)
                .ok_or_else(|| corrupt(offset, "frame offset overflow"))?;
            recovery.records_replayed += 1;
        }
        if offset < length {
            // Parsing the complete prefix succeeded; only then repair a short EOF tail.
            storage
                .set_len(offset)
                .map_err(io_error("truncate incomplete WAL tail"))?;
            sync(&mut storage)?;
            recovery.discarded_tail_bytes = length - offset;
        }
        Ok(Self {
            storage,
            values,
            deleted,
            change_bytes,
            sequence,
            wal_len: offset,
            poisoned: false,
            recovery,
        })
    }
}
