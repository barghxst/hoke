//! Resolve ordered immutable runs into a new candidate, retaining tombstones.
use crate::storage::Storage;

use super::{Lookup, MAX_MERGE_SOURCES, SSTable, SstError, TableWriter, format::copy};

/// Merge immutable tables ordered from oldest to newest into an empty destination.
///
/// Each key appears once, with the newest source winning. Winning tombstones are
/// always retained so that merging a subset of runs cannot resurrect older data.
/// Only winning records are read and checksum-validated; shadowed payloads are not
/// scrubbed. Memory is bounded by source cursors, one record, and the writer index.
///
/// The caller must provide distinct storage for the destination and keep every
/// source immutable. This does not flush, synchronize, publish, or modify sources.
/// An error may leave a partial candidate that must be discarded. Excess source
/// counts are rejected before any destination access; empty input writes an empty
/// valid table. Output capacity limits are enforced while streaming.
pub fn merge_tables<S: Storage, O: Storage>(
    destination: &mut O,
    sources: &mut [SSTable<S>],
) -> Result<usize, SstError> {
    if sources.len() > MAX_MERGE_SOURCES {
        return Err(SstError::Invalid("too many merge sources"));
    }
    // The small, fixed fan-in avoids allocating or copying heap keys. Cursors
    // refer directly to the immutable indexes, never to buffered record values.
    let mut positions = [0usize; MAX_MERGE_SOURCES];
    let mut writer = TableWriter::new(destination)?;
    loop {
        let mut winner: Option<usize> = None;
        for (source, table) in sources.iter().enumerate() {
            let Some(key) = table.key_at(positions[source]) else {
                continue;
            };
            if winner.is_none_or(|previous| {
                // Later sources win ties because iteration is oldest to newest.
                key <= sources[previous].key_at(positions[previous]).unwrap()
            }) {
                winner = Some(source);
            }
        }
        let Some(winner) = winner else {
            return writer.finish();
        };
        let key = copy(sources[winner].key_at(positions[winner]).unwrap())?;
        match sources[winner].get(&key)? {
            Lookup::Value(value) => writer.append(&key, Some(&value))?,
            Lookup::Deleted => writer.append(&key, None)?,
            Lookup::Missing => return Err(SstError::Invalid("merge key missing from index")),
        }
        for (source, table) in sources.iter().enumerate() {
            if table.key_at(positions[source]) == Some(key.as_slice()) {
                positions[source] += 1;
            }
        }
    }
}
