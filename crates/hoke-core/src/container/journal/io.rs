//! Checked I/O and streaming record checksums.
use super::JournalError;
use crate::container::CodecError;
use crate::container::{
    MAX_FILE_BYTES,
    codec::{RECORD_HEADER_LEN, TRAILER_LEN},
};
use crate::storage::Storage;
use std::io;

pub(super) fn io_error(operation: &'static str) -> impl FnOnce(io::Error) -> JournalError {
    move |source| JournalError::Io { operation, source }
}
pub(super) fn read_exact<S: Storage>(
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
pub(super) fn sync<S: Storage>(storage: &mut S) -> Result<(), JournalError> {
    storage.flush().map_err(io_error("flush container"))?;
    storage.sync_all().map_err(io_error("sync container"))
}
pub(super) fn record_end(offset: u64, length: u64) -> Result<u64, JournalError> {
    offset
        .checked_add(RECORD_HEADER_LEN as u64)
        .and_then(|n| n.checked_add(length))
        .and_then(|n| n.checked_add(TRAILER_LEN as u64))
        .filter(|n| *n <= MAX_FILE_BYTES)
        .ok_or(JournalError::Limit("file bytes"))
}
pub(super) fn payload_buffer(length: u64) -> Result<Vec<u8>, JournalError> {
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
pub(super) fn checksum<S: Storage>(
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
