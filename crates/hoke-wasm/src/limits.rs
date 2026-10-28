//! Limits for transient JS/WASM packets and scan requests.

pub(crate) const MAX_PACKET_BYTES: usize = 32 * 1024 * 1024;
pub(crate) const MAX_KEY_BYTES: usize = 65536;
pub(crate) const MAX_VALUE_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const TOMBSTONE: u32 = u32::MAX;
