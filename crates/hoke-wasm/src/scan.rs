//! Shared scan request validation and transient live-row response framing.

use std::ops::Bound;

use hoke_core::ScanOptions;

use wasm_bindgen::prelude::*;

use crate::binding_error;
use crate::limits::MAX_KEY_BYTES;
pub(crate) use crate::limits::MAX_PACKET_BYTES;

pub(crate) fn pack_live_rows(rows: Vec<(Vec<u8>, Vec<u8>)>) -> Result<Vec<u8>, JsError> {
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

pub(crate) fn scan_options(
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
    Ok(options)
}

pub(crate) fn indexed_scan_options(
    start: Option<Vec<u8>>,
    end: Option<Vec<u8>>,
    prefix: Option<Vec<u8>>,
    flags: u8,
    limit: u32,
) -> Result<ScanOptions, JsError> {
    let mut options = scan_options(start, end, prefix, flags, limit)?;
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
