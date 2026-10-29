use std::fmt;

pub const FILE_HEADER_LEN: usize = 32;
pub const RECORD_HEADER_LEN: usize = 32;
pub const TRAILER_LEN: usize = 4;
pub const REF_LEN: usize = 24;
pub const ROOT_PREFIX_LEN: usize = 24;
pub const MAX_SEGMENT_BYTES: u64 = 256 * 1024 * 1024;
pub const MAX_ROOT_REFS: usize = 4096;
pub const MAX_PENDING_SEGMENTS: usize = 4096;
pub const MAX_FILE_BYTES: u64 = (1 << 53) - 1;
pub const MAX_ROOT_BYTES: usize = ROOT_PREFIX_LEN + REF_LEN * MAX_ROOT_REFS;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum SegmentKind {
    Wal = 1,
    Sst = 2,
}

impl TryFrom<u8> for SegmentKind {
    type Error = CodecError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Wal),
            2 => Ok(Self::Sst),
            _ => Err(CodecError::Invalid("unknown segment kind")),
        }
    }
}

impl SegmentKind {
    pub fn record_kind(self) -> RecordKind {
        match self {
            Self::Wal => RecordKind::Wal,
            Self::Sst => RecordKind::Sst,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum RecordKind {
    Wal = 1,
    Sst = 2,
    Root = 3,
}

impl TryFrom<u8> for RecordKind {
    type Error = CodecError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Wal),
            2 => Ok(Self::Sst),
            3 => Ok(Self::Root),
            _ => Err(CodecError::Invalid("unknown record kind")),
        }
    }
}

impl RecordKind {
    pub fn segment_kind(self) -> Option<SegmentKind> {
        match self {
            Self::Wal => Some(SegmentKind::Wal),
            Self::Sst => Some(SegmentKind::Sst),
            Self::Root => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SegmentRef {
    pub kind: SegmentKind,
    pub offset: u64,
    pub payload_len: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Root {
    pub generation: u64,
    pub tables: Vec<SegmentRef>,
    pub wal: Vec<SegmentRef>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordHeader {
    pub kind: RecordKind,
    pub sequence: u64,
    pub payload_len: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CodecError {
    Invalid(&'static str),
    UnsupportedVersion(u32),
    Allocation,
}

impl fmt::Display for CodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(reason) => write!(f, "invalid container: {reason}"),
            Self::UnsupportedVersion(version) => {
                write!(f, "unsupported container version {version}")
            }
            Self::Allocation => f.write_str("container metadata allocation failed"),
        }
    }
}

impl std::error::Error for CodecError {}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

fn check_crc_header(bytes: &[u8]) -> Result<(), CodecError> {
    if bytes.len() != 32 {
        return Err(CodecError::Invalid("header length"));
    }
    if crc32fast::hash(&bytes[..28]) != u32_at(bytes, 28) {
        return Err(CodecError::Invalid("header checksum"));
    }
    Ok(())
}

fn finish_header(bytes: &mut [u8; 32]) {
    let checksum = crc32fast::hash(&bytes[..28]);
    bytes[28..].copy_from_slice(&checksum.to_le_bytes());
}

pub fn encode_file_header() -> [u8; FILE_HEADER_LEN] {
    let mut bytes = [0; FILE_HEADER_LEN];
    bytes[..8].copy_from_slice(b"HOKECNT\0");
    bytes[8..12].copy_from_slice(&1_u32.to_le_bytes());
    bytes[12..16].copy_from_slice(&(FILE_HEADER_LEN as u32).to_le_bytes());
    finish_header(&mut bytes);
    bytes
}

pub fn decode_file_header(bytes: &[u8]) -> Result<(), CodecError> {
    check_crc_header(bytes)?;
    if &bytes[..8] != b"HOKECNT\0" {
        return Err(CodecError::Invalid("file magic"));
    }
    let version = u32_at(bytes, 8);
    if version != 1 {
        return Err(CodecError::UnsupportedVersion(version));
    }
    if u32_at(bytes, 12) != FILE_HEADER_LEN as u32 {
        return Err(CodecError::Invalid("file header length"));
    }
    if bytes[16..28].iter().any(|byte| *byte != 0) {
        return Err(CodecError::Invalid("file reserved bytes"));
    }
    Ok(())
}

fn validate_record(header: &RecordHeader) -> Result<(), CodecError> {
    if header.sequence == 0 {
        return Err(CodecError::Invalid("zero record sequence"));
    }
    match header.kind {
        RecordKind::Wal | RecordKind::Sst if header.payload_len > MAX_SEGMENT_BYTES => {
            Err(CodecError::Invalid("segment payload limit"))
        }
        RecordKind::Root
            if header.payload_len < ROOT_PREFIX_LEN as u64
                || header.payload_len > MAX_ROOT_BYTES as u64
                || !(header.payload_len - ROOT_PREFIX_LEN as u64)
                    .is_multiple_of(REF_LEN as u64) =>
        {
            Err(CodecError::Invalid("root payload length"))
        }
        _ => Ok(()),
    }
}

pub fn encode_record_header(header: &RecordHeader) -> Result<[u8; RECORD_HEADER_LEN], CodecError> {
    validate_record(header)?;
    let mut bytes = [0; RECORD_HEADER_LEN];
    bytes[..4].copy_from_slice(b"HKR1");
    bytes[4] = header.kind as u8;
    bytes[8..16].copy_from_slice(&header.sequence.to_le_bytes());
    bytes[16..24].copy_from_slice(&header.payload_len.to_le_bytes());
    finish_header(&mut bytes);
    Ok(bytes)
}

pub fn decode_record_header(bytes: &[u8]) -> Result<RecordHeader, CodecError> {
    check_crc_header(bytes)?;
    if &bytes[..4] != b"HKR1" {
        return Err(CodecError::Invalid("record magic"));
    }
    if bytes[5..8]
        .iter()
        .chain(&bytes[24..28])
        .any(|byte| *byte != 0)
    {
        return Err(CodecError::Invalid("record reserved bytes"));
    }
    let header = RecordHeader {
        kind: RecordKind::try_from(bytes[4])?,
        sequence: u64_at(bytes, 8),
        payload_len: u64_at(bytes, 16),
    };
    validate_record(&header)?;
    Ok(header)
}

fn validate_ref(reference: &SegmentRef) -> Result<(), CodecError> {
    if reference.payload_len > MAX_SEGMENT_BYTES {
        return Err(CodecError::Invalid("segment payload limit"));
    }
    if reference.offset < FILE_HEADER_LEN as u64 {
        return Err(CodecError::Invalid("segment offset before records"));
    }
    let end = reference
        .offset
        .checked_add(RECORD_HEADER_LEN as u64)
        .and_then(|value| value.checked_add(reference.payload_len))
        .and_then(|value| value.checked_add(TRAILER_LEN as u64));
    if end.is_none_or(|value| value > MAX_FILE_BYTES) {
        return Err(CodecError::Invalid("segment end offset limit"));
    }
    Ok(())
}

pub fn encode_ref(reference: &SegmentRef) -> Result<[u8; REF_LEN], CodecError> {
    validate_ref(reference)?;
    let mut bytes = [0; REF_LEN];
    bytes[0] = reference.kind as u8;
    bytes[8..16].copy_from_slice(&reference.offset.to_le_bytes());
    bytes[16..24].copy_from_slice(&reference.payload_len.to_le_bytes());
    Ok(bytes)
}

pub fn decode_ref(bytes: &[u8]) -> Result<SegmentRef, CodecError> {
    if bytes.len() != REF_LEN {
        return Err(CodecError::Invalid("reference length"));
    }
    if bytes[1..8].iter().any(|byte| *byte != 0) {
        return Err(CodecError::Invalid("reference reserved bytes"));
    }
    let reference = SegmentRef {
        kind: SegmentKind::try_from(bytes[0])?,
        offset: u64_at(bytes, 8),
        payload_len: u64_at(bytes, 16),
    };
    validate_ref(&reference)?;
    Ok(reference)
}

pub fn encode_root(root: &Root) -> Result<Vec<u8>, CodecError> {
    let count = root.tables.len().checked_add(root.wal.len());
    if root.generation == 0 || count.is_none_or(|count| count > MAX_ROOT_REFS) {
        return Err(CodecError::Invalid("root generation or reference count"));
    }
    for (index, reference) in root.tables.iter().chain(&root.wal).enumerate() {
        validate_ref(reference)?;
        let expected = if index < root.tables.len() {
            SegmentKind::Sst
        } else {
            SegmentKind::Wal
        };
        if reference.kind != expected {
            return Err(CodecError::Invalid("root reference kind"));
        }
        if root
            .tables
            .iter()
            .chain(&root.wal)
            .take(index)
            .any(|prior| prior.offset == reference.offset)
        {
            return Err(CodecError::Invalid("duplicate root offset"));
        }
    }
    let len = ROOT_PREFIX_LEN + count.unwrap() * REF_LEN;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(len)
        .map_err(|_| CodecError::Allocation)?;
    bytes.extend_from_slice(&root.generation.to_le_bytes());
    bytes.extend_from_slice(&(root.tables.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&(root.wal.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&[0; 8]);
    for reference in root.tables.iter().chain(&root.wal) {
        bytes.extend_from_slice(&encode_ref(reference)?);
    }
    Ok(bytes)
}

pub fn decode_root(bytes: &[u8]) -> Result<Root, CodecError> {
    if !(ROOT_PREFIX_LEN..=MAX_ROOT_BYTES).contains(&bytes.len()) {
        return Err(CodecError::Invalid("root payload length"));
    }
    let generation = u64_at(bytes, 0);
    let table_count = u32_at(bytes, 8) as usize;
    let wal_count = u32_at(bytes, 12) as usize;
    let count = table_count.checked_add(wal_count);
    if generation == 0 || count.is_none_or(|count| count > MAX_ROOT_REFS) {
        return Err(CodecError::Invalid("root generation or reference count"));
    }
    if bytes[16..24].iter().any(|byte| *byte != 0) {
        return Err(CodecError::Invalid("root reserved bytes"));
    }
    if bytes.len() != ROOT_PREFIX_LEN + count.unwrap() * REF_LEN {
        return Err(CodecError::Invalid("root reference count mismatch"));
    }
    let encoded_refs = &bytes[ROOT_PREFIX_LEN..];
    for (index, encoded) in encoded_refs.chunks_exact(REF_LEN).enumerate() {
        let reference = decode_ref(encoded)?;
        let expected = if index < table_count {
            SegmentKind::Sst
        } else {
            SegmentKind::Wal
        };
        if reference.kind != expected {
            return Err(CodecError::Invalid("root reference kind"));
        }
        if encoded_refs
            .chunks_exact(REF_LEN)
            .take(index)
            .any(|prior| u64_at(prior, 8) == reference.offset)
        {
            return Err(CodecError::Invalid("duplicate root offset"));
        }
    }
    let mut tables = Vec::new();
    let mut wal = Vec::new();
    tables
        .try_reserve_exact(table_count)
        .map_err(|_| CodecError::Allocation)?;
    wal.try_reserve_exact(wal_count)
        .map_err(|_| CodecError::Allocation)?;
    for (index, encoded) in encoded_refs.chunks_exact(REF_LEN).enumerate() {
        let reference = decode_ref(encoded)?;
        if index < table_count {
            tables.push(reference);
        } else {
            wal.push(reference);
        }
    }
    Ok(Root {
        generation,
        tables,
        wal,
    })
}

#[cfg(test)]
#[path = "codec_tests.rs"]
mod tests;
