//! WAL mutation protocol: validate, append and sync, then publish memory state.

use super::changes::apply;
use super::error::io_error;
use super::{Engine, Error, sync};
use crate::storage::Storage;
use crate::wal::{self, Operation};

impl<S: Storage> Engine<S> {
    pub fn put(&mut self, key: &[u8], value: &[u8]) -> Result<(), Error> {
        self.check_writable()?;
        if key.len() > wal::MAX_KEY_BYTES || value.len() > wal::MAX_VALUE_BYTES {
            return Err(Error::InvalidInput(
                "key or value exceeds WAL size limit".into(),
            ));
        }
        self.commit(Operation::Put {
            key: key.to_vec(),
            value: value.to_vec(),
        })
    }

    pub fn delete(&mut self, key: &[u8]) -> Result<(), Error> {
        self.check_writable()?;
        if key.len() > wal::MAX_KEY_BYTES {
            return Err(Error::InvalidInput("key exceeds WAL size limit".into()));
        }
        self.commit(Operation::Delete { key: key.to_vec() })
    }

    fn commit(&mut self, operation: Operation) -> Result<(), Error> {
        self.check_writable()?;
        let sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| Error::InvalidInput("sequence overflow".into()))?;
        let frame = wal::encode_frame(sequence, &operation)
            .map_err(|error| Error::InvalidInput(error.to_string()))?;
        let new_len = self
            .wal_len
            .checked_add(frame.len() as u64)
            .ok_or_else(|| Error::InvalidInput("WAL offset overflow".into()))?;
        let result = self
            .storage
            .write_all_at(self.wal_len, &frame)
            .map_err(io_error("append WAL"))
            .and_then(|()| sync(&mut self.storage));
        if let Err(error) = result {
            self.poisoned = true;
            return Err(error);
        }
        apply(
            &mut self.values,
            &mut self.deleted,
            &mut self.change_bytes,
            operation,
        );
        self.sequence = sequence;
        self.wal_len = new_len;
        Ok(())
    }

    /// Explicitly check final synchronization and release the storage/lock.
    /// A poisoned engine must instead be dropped and reopened for recovery.
    pub fn close(mut self) -> Result<(), Error> {
        self.check_writable()?;
        sync(&mut self.storage)
    }
}
