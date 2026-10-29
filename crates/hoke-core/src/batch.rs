use crate::{DbError, wal};

pub const MAX_BATCH_OPERATIONS: usize = 1024;

pub const MAX_BATCH_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BatchOperation {
    Put { key: Vec<u8>, value: Vec<u8> },
    Delete { key: Vec<u8> },
}

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
