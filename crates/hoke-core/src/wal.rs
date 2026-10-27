//! The version 1 WAL codec. This module performs no storage operations.
use std::error::Error;
use std::fmt;

pub(crate) const FILE_HEADER_LEN: usize = 16;
pub(crate) const FRAME_HEADER_LEN: usize = 28;
pub(crate) const TRAILER_LEN: usize = 4;
pub(crate) const MAX_KEY_BYTES: usize = 64 * 1024;
pub(crate) const MAX_VALUE_BYTES: usize = 16 * 1024 * 1024;
const MAGIC: &[u8; 8] = b"STOWWAL\0";
const VERSION: u32 = 1;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Operation {
    Put { key: Vec<u8>, value: Vec<u8> },
    Delete { key: Vec<u8> },
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CodecError {
    InvalidMagic,
    UnsupportedVersion(u32),
    FileHeaderChecksum,
    FrameHeaderChecksum,
    FrameChecksum,
    ZeroSequence,
    UnknownOperation(u8),
    ReservedBytes,
    KeyTooLarge(usize),
    ValueTooLarge(usize),
    DeleteWithValue,
    InvalidPayloadLength { declared: usize, expected: usize },
    InvalidFrameLength { actual: usize, expected: usize },
    Allocation,
}

impl fmt::Display for CodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMagic => f.write_str("invalid WAL magic"),
            Self::UnsupportedVersion(v) => write!(f, "unsupported WAL version {v}"),
            Self::FileHeaderChecksum => f.write_str("WAL file header checksum mismatch"),
            Self::FrameHeaderChecksum => f.write_str("WAL frame header checksum mismatch"),
            Self::FrameChecksum => f.write_str("WAL frame checksum mismatch"),
            Self::ZeroSequence => f.write_str("WAL sequence must be at least 1"),
            Self::UnknownOperation(tag) => write!(f, "unknown WAL operation {tag}"),
            Self::ReservedBytes => f.write_str("WAL reserved bytes must be zero"),
            Self::KeyTooLarge(len) => write!(f, "WAL key length {len} exceeds {MAX_KEY_BYTES}"),
            Self::ValueTooLarge(len) => {
                write!(f, "WAL value length {len} exceeds {MAX_VALUE_BYTES}")
            }
            Self::DeleteWithValue => f.write_str("WAL delete must have no value"),
            Self::InvalidPayloadLength { declared, expected } => write!(
                f,
                "WAL payload length {declared} differs from key + value length {expected}"
            ),
            Self::InvalidFrameLength { actual, expected } => write!(
                f,
                "WAL payload and trailer length {actual} differs from expected {expected}"
            ),
            Self::Allocation => f.write_str("could not allocate WAL buffer"),
        }
    }
}
impl Error for CodecError {}

#[derive(Debug)]
pub(crate) struct FrameHeader {
    pub(crate) sequence: u64,
    pub(crate) key_len: usize,
    pub(crate) value_len: usize,
    kind: u8,
}
impl FrameHeader {
    pub(crate) fn is_delete(&self) -> bool {
        self.kind == 2
    }
    pub(crate) fn payload_len(&self) -> usize {
        self.key_len + self.value_len
    }
    pub(crate) fn frame_len(&self) -> usize {
        FRAME_HEADER_LEN + self.payload_len() + TRAILER_LEN
    }
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

pub(crate) fn encode_file_header() -> [u8; FILE_HEADER_LEN] {
    let mut bytes = [0; FILE_HEADER_LEN];
    bytes[..8].copy_from_slice(MAGIC);
    bytes[8..12].copy_from_slice(&VERSION.to_le_bytes());
    let crc = crc32fast::hash(&bytes[..12]);
    bytes[12..].copy_from_slice(&crc.to_le_bytes());
    bytes
}

pub(crate) fn decode_file_header(bytes: &[u8; FILE_HEADER_LEN]) -> Result<(), CodecError> {
    if &bytes[..8] != MAGIC {
        return Err(CodecError::InvalidMagic);
    }
    if crc32fast::hash(&bytes[..12]) != u32_at(bytes, 12) {
        return Err(CodecError::FileHeaderChecksum);
    }
    let version = u32_at(bytes, 8);
    if version != VERSION {
        return Err(CodecError::UnsupportedVersion(version));
    }
    Ok(())
}

fn check_lengths(key_len: usize, value_len: usize) -> Result<(), CodecError> {
    if key_len > MAX_KEY_BYTES {
        return Err(CodecError::KeyTooLarge(key_len));
    }
    if value_len > MAX_VALUE_BYTES {
        return Err(CodecError::ValueTooLarge(value_len));
    }
    Ok(())
}

pub(crate) fn decode_frame_header(
    bytes: &[u8; FRAME_HEADER_LEN],
) -> Result<FrameHeader, CodecError> {
    // Do not trust lengths (or allocate from them) before validating the header CRC.
    if crc32fast::hash(&bytes[..24]) != u32_at(bytes, 24) {
        return Err(CodecError::FrameHeaderChecksum);
    }
    let sequence = u64::from_le_bytes([
        bytes[4], bytes[5], bytes[6], bytes[7], bytes[8], bytes[9], bytes[10], bytes[11],
    ]);
    if sequence == 0 {
        return Err(CodecError::ZeroSequence);
    }
    let kind = bytes[12];
    if kind != 1 && kind != 2 {
        return Err(CodecError::UnknownOperation(kind));
    }
    if bytes[13..16] != [0; 3] {
        return Err(CodecError::ReservedBytes);
    }
    let key_len = u32_at(bytes, 16) as usize;
    let value_len = u32_at(bytes, 20) as usize;
    check_lengths(key_len, value_len)?;
    if kind == 2 && value_len != 0 {
        return Err(CodecError::DeleteWithValue);
    }
    let declared = u32_at(bytes, 0) as usize;
    let expected = key_len + value_len;
    if declared != expected {
        return Err(CodecError::InvalidPayloadLength { declared, expected });
    }
    Ok(FrameHeader {
        sequence,
        key_len,
        value_len,
        kind,
    })
}

pub(crate) fn encode_frame(sequence: u64, operation: &Operation) -> Result<Vec<u8>, CodecError> {
    if sequence == 0 {
        return Err(CodecError::ZeroSequence);
    }
    let (kind, key, value): (u8, &[u8], &[u8]) = match operation {
        Operation::Put { key, value } => (1, key, value),
        Operation::Delete { key } => (2, key, &[]),
    };
    check_lengths(key.len(), value.len())?;
    let payload_len = key.len() + value.len();
    let mut header = [0; FRAME_HEADER_LEN];
    header[..4].copy_from_slice(&(payload_len as u32).to_le_bytes());
    header[4..12].copy_from_slice(&sequence.to_le_bytes());
    header[12] = kind;
    header[16..20].copy_from_slice(&(key.len() as u32).to_le_bytes());
    header[20..24].copy_from_slice(&(value.len() as u32).to_le_bytes());
    let crc = crc32fast::hash(&header[..24]);
    header[24..].copy_from_slice(&crc.to_le_bytes());
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(FRAME_HEADER_LEN + payload_len + TRAILER_LEN)
        .map_err(|_| CodecError::Allocation)?;
    bytes.extend_from_slice(&header);
    bytes.extend_from_slice(key);
    bytes.extend_from_slice(value);
    let crc = crc32fast::hash(&bytes);
    bytes.extend_from_slice(&crc.to_le_bytes());
    Ok(bytes)
}

fn copy_bytes(bytes: &[u8]) -> Result<Vec<u8>, CodecError> {
    let mut result = Vec::new();
    result
        .try_reserve_exact(bytes.len())
        .map_err(|_| CodecError::Allocation)?;
    result.extend_from_slice(bytes);
    Ok(result)
}

pub(crate) fn decode_frame(
    header: &[u8; FRAME_HEADER_LEN],
    payload_and_crc: &[u8],
) -> Result<(u64, Operation), CodecError> {
    let decoded = decode_frame_header(header)?;
    let expected = decoded.payload_len() + TRAILER_LEN;
    if payload_and_crc.len() != expected {
        return Err(CodecError::InvalidFrameLength {
            actual: payload_and_crc.len(),
            expected,
        });
    }
    let payload = &payload_and_crc[..decoded.payload_len()];
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(header);
    hasher.update(payload);
    if hasher.finalize() != u32_at(payload_and_crc, decoded.payload_len()) {
        return Err(CodecError::FrameChecksum);
    }
    let key = copy_bytes(&payload[..decoded.key_len])?;
    let operation = if decoded.kind == 1 {
        Operation::Put {
            key,
            value: copy_bytes(&payload[decoded.key_len..])?,
        }
    } else {
        Operation::Delete { key }
    };
    Ok((decoded.sequence, operation))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(frame: &[u8]) -> &[u8; FRAME_HEADER_LEN] {
        frame[..FRAME_HEADER_LEN].try_into().unwrap()
    }
    fn repair_header(bytes: &mut [u8; FRAME_HEADER_LEN]) {
        let crc = crc32fast::hash(&bytes[..24]);
        bytes[24..].copy_from_slice(&crc.to_le_bytes());
    }
    fn empty_put() -> Operation {
        Operation::Put {
            key: vec![],
            value: vec![],
        }
    }

    #[test]
    fn standard_crc_vector() {
        assert_eq!(crc32fast::hash(b"123456789"), 0xcbf4_3926);
    }

    #[test]
    fn file_header_roundtrip_and_corruption() {
        let valid = encode_file_header();
        assert_eq!(
            valid,
            [
                83, 84, 79, 87, 87, 65, 76, 0, 1, 0, 0, 0, 199, 185, 108, 252
            ]
        );
        decode_file_header(&valid).unwrap();
        let mut bad = valid;
        bad[0] ^= 1;
        assert_eq!(decode_file_header(&bad), Err(CodecError::InvalidMagic));
        bad = valid;
        bad[12] ^= 1;
        assert_eq!(
            decode_file_header(&bad),
            Err(CodecError::FileHeaderChecksum)
        );
        bad = valid;
        bad[8..12].copy_from_slice(&99u32.to_le_bytes());
        let crc = crc32fast::hash(&bad[..12]);
        bad[12..].copy_from_slice(&crc.to_le_bytes());
        assert_eq!(
            decode_file_header(&bad),
            Err(CodecError::UnsupportedVersion(99))
        );
    }

    #[test]
    fn golden_frame_endianness_and_lengths() {
        let frame = encode_frame(
            0x0102_0304_0506_0708,
            &Operation::Put {
                key: b"k".to_vec(),
                value: b"ab".to_vec(),
            },
        )
        .unwrap();
        assert_eq!(
            &frame[..24],
            &[
                3, 0, 0, 0, 8, 7, 6, 5, 4, 3, 2, 1, 1, 0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0
            ]
        );
        // Fixture checksums were independently computed with Python zlib.crc32.
        assert_eq!(
            frame,
            [
                3, 0, 0, 0, 8, 7, 6, 5, 4, 3, 2, 1, 1, 0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 84, 209,
                90, 210, 107, 97, 98, 45, 161, 180, 6
            ]
        );
        let decoded = decode_frame_header(header(&frame)).unwrap();
        assert_eq!(decoded.payload_len(), 3);
        assert_eq!(decoded.frame_len(), frame.len());
    }

    #[test]
    fn roundtrips_binary_empty_and_boundary_values() {
        for operation in [
            empty_put(),
            Operation::Delete { key: vec![] },
            Operation::Put {
                key: (0..=255).collect(),
                value: (0..=255).rev().collect(),
            },
            Operation::Delete {
                key: vec![0, 255, 0],
            },
            Operation::Put {
                key: vec![255; MAX_KEY_BYTES],
                value: vec![0; MAX_VALUE_BYTES],
            },
        ] {
            let frame = encode_frame(u64::MAX, &operation).unwrap();
            let (sequence, actual) =
                decode_frame(header(&frame), &frame[FRAME_HEADER_LEN..]).unwrap();
            assert_eq!(sequence, u64::MAX);
            assert_eq!(actual, operation);
        }
    }

    #[test]
    fn rejects_corruption_before_lengths_or_allocation() {
        let frame = encode_frame(1, &empty_put()).unwrap();
        let mut bytes = *header(&frame);
        bytes[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(
            decode_frame_header(&bytes),
            Err(CodecError::FrameHeaderChecksum)
        ));
        repair_header(&mut bytes);
        assert!(matches!(
            decode_frame_header(&bytes),
            Err(CodecError::KeyTooLarge(_))
        ));
        for index in 0..frame.len() {
            let mut bad = frame.clone();
            bad[index] ^= 1;
            assert!(decode_frame(header(&bad), &bad[FRAME_HEADER_LEN..]).is_err());
        }
        let frame = encode_frame(
            1,
            &Operation::Put {
                key: vec![1, 2],
                value: vec![3, 4],
            },
        )
        .unwrap();
        for index in FRAME_HEADER_LEN..frame.len() {
            let mut bad = frame.clone();
            bad[index] ^= 1;
            assert_eq!(
                decode_frame(header(&bad), &bad[FRAME_HEADER_LEN..]),
                Err(CodecError::FrameChecksum)
            );
        }
    }

    #[test]
    fn validates_semantics_with_correct_header_checksum() {
        let frame = encode_frame(1, &empty_put()).unwrap();
        for (index, value, expected) in [
            (4, 0, CodecError::ZeroSequence),
            (12, 3, CodecError::UnknownOperation(3)),
            (13, 1, CodecError::ReservedBytes),
            (
                0,
                1,
                CodecError::InvalidPayloadLength {
                    declared: 1,
                    expected: 0,
                },
            ),
        ] {
            let mut bytes = *header(&frame);
            bytes[index] = value;
            repair_header(&mut bytes);
            assert_eq!(decode_frame_header(&bytes).unwrap_err(), expected);
        }
        let mut bytes = *header(&frame);
        bytes[12] = 2;
        bytes[20] = 1;
        repair_header(&mut bytes);
        assert!(matches!(
            decode_frame_header(&bytes),
            Err(CodecError::DeleteWithValue)
        ));
        bytes = *header(&frame);
        bytes[20..24].copy_from_slice(&((MAX_VALUE_BYTES + 1) as u32).to_le_bytes());
        repair_header(&mut bytes);
        assert!(matches!(
            decode_frame_header(&bytes),
            Err(CodecError::ValueTooLarge(_))
        ));
    }

    #[test]
    fn validates_encode_limits_and_zero_sequence() {
        assert_eq!(encode_frame(0, &empty_put()), Err(CodecError::ZeroSequence));
        let huge_key = Operation::Delete {
            key: vec![0; MAX_KEY_BYTES + 1],
        };
        assert_eq!(
            encode_frame(1, &huge_key),
            Err(CodecError::KeyTooLarge(MAX_KEY_BYTES + 1))
        );
        let huge_value = Operation::Put {
            key: vec![],
            value: vec![0; MAX_VALUE_BYTES + 1],
        };
        assert_eq!(
            encode_frame(1, &huge_value),
            Err(CodecError::ValueTooLarge(MAX_VALUE_BYTES + 1))
        );
    }

    #[test]
    fn rejects_every_truncation_and_trailing_bytes() {
        let frame = encode_frame(
            1,
            &Operation::Put {
                key: vec![1, 2],
                value: vec![3, 4],
            },
        )
        .unwrap();
        for length in 0..frame.len() - FRAME_HEADER_LEN {
            assert!(matches!(
                decode_frame(
                    header(&frame),
                    &frame[FRAME_HEADER_LEN..FRAME_HEADER_LEN + length]
                ),
                Err(CodecError::InvalidFrameLength { .. })
            ));
        }
        let mut payload = frame[FRAME_HEADER_LEN..].to_vec();
        payload.push(0);
        assert!(matches!(
            decode_frame(header(&frame), &payload),
            Err(CodecError::InvalidFrameLength { .. })
        ));
    }
}
