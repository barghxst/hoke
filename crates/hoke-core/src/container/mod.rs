//! Append-only container engine and publication journal (ADR 0018–0019).
pub(crate) mod codec;
mod database;
mod journal;
pub(crate) mod storage;

pub use codec::{
    CodecError, MAX_FILE_BYTES, MAX_PENDING_SEGMENTS, MAX_ROOT_REFS, MAX_SEGMENT_BYTES, REF_LEN,
    Root, SegmentKind, SegmentRef, decode_ref, decode_root, encode_ref, encode_root,
};
pub use database::{ContainerDatabase, ContainerDbError, ContainerStats};
pub use journal::{Journal, JournalError, JournalRecovery};
