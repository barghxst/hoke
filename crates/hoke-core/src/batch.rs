use crate::{DbError, wal};

/// Maximum number of input operations, including repeated keys.
pub const MAX_BATCH_OPERATIONS: usize = 1024;
/// Maximum sum of input key/value lengths, including repeated keys.
pub const MAX_BATCH_BYTES: usize = 32 * 1024 * 1024;

/// One operation in an atomic directory-backed database batch.
/// Operations run in slice order; the last operation on a key wins.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BatchOperation {
    Put { key: Vec<u8>, value: Vec<u8> },
    Delete { key: Vec<u8> },
}

/// Validate all input without reading or writing storage.
/// Database::write_batch calls this as well; CLI callers can validate before open.
pub fn validate_batch(operations: &[BatchOperation]) -> Result<(), DbError> {
    if operations.len() > MAX_BATCH_OPERATIONS {
        return Err(DbError::Limit("batch exceeds 1024 operations"));
    }
    let mut bytes = 0_usize;
    for operation in operations {
        let (key, value): (&[u8], &[u8]) = match operation {
            BatchOperation::Put { key, value } => (key, value),
            BatchOperation::Delete { key } => (key, &[]),
        };
        if key.len() > wal::MAX_KEY_BYTES {
            return Err(DbError::Limit("batch key exceeds 64 KiB"));
        }
        if value.len() > wal::MAX_VALUE_BYTES {
            return Err(DbError::Limit("batch value exceeds 16 MiB"));
        }
        bytes = bytes
            .checked_add(key.len())
            .and_then(|n| n.checked_add(value.len()))
            .ok_or(DbError::Limit("batch byte length overflow"))?;
        if bytes > MAX_BATCH_BYTES {
            return Err(DbError::Limit("batch exceeds 32 MiB of key/value bytes"));
        }
    }
    Ok(())
}
