use crate::storage::Storage;

use super::SstError;

pub(super) const HEADER_LEN: usize = 32;
pub(super) const MAGIC: &[u8; 8] = b"STOWSST\0";
pub(super) const VERSION: u32 = 1;
pub(super) fn buffer(len: usize) -> Result<Vec<u8>, SstError> {
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(len)
        .map_err(|_| SstError::Allocation)?;
    bytes.resize(len, 0);
    Ok(bytes)
}
pub(super) fn copy(bytes: &[u8]) -> Result<Vec<u8>, SstError> {
    let mut result = buffer(bytes.len())?;
    result.copy_from_slice(bytes);
    Ok(result)
}
pub(super) fn read_exact<S: Storage>(
    storage: &mut S,
    mut offset: u64,
    mut bytes: &mut [u8],
) -> Result<(), SstError> {
    while !bytes.is_empty() {
        let count = storage.read_at(offset, bytes)?;
        if count == 0 {
            return Err(SstError::Invalid("truncated file"));
        }
        if count > bytes.len() {
            return Err(SstError::Invalid("storage returned invalid read length"));
        }
        offset = offset
            .checked_add(count as u64)
            .ok_or(SstError::Invalid("offset overflow"))?;
        bytes = &mut bytes[count..];
    }
    Ok(())
}
pub(super) fn u32_at(bytes: &[u8], start: usize) -> u32 {
    u32::from_le_bytes(bytes[start..start + 4].try_into().unwrap())
}
pub(super) fn u64_at(bytes: &[u8], start: usize) -> u64 {
    u64::from_le_bytes(bytes[start..start + 8].try_into().unwrap())
}
pub(super) fn header(count: usize, index_offset: u64) -> [u8; HEADER_LEN] {
    let mut bytes = [0; HEADER_LEN];
    bytes[..8].copy_from_slice(MAGIC);
    bytes[8..12].copy_from_slice(&VERSION.to_le_bytes());
    bytes[12..20].copy_from_slice(&(count as u64).to_le_bytes());
    bytes[20..28].copy_from_slice(&index_offset.to_le_bytes());
    let crc = crc32fast::hash(&bytes[..28]);
    bytes[28..].copy_from_slice(&crc.to_le_bytes());
    bytes
}
