use crate::storage::Storage;

use super::{Lookup, MAX_MERGE_SOURCES, SSTable, SstError, TableWriter, format::copy};

pub fn merge_tables<S: Storage, O: Storage>(
    destination: &mut O,
    sources: &mut [SSTable<S>],
) -> Result<usize, SstError> {
    if sources.len() > MAX_MERGE_SOURCES {
        return Err(SstError::Invalid("too many merge sources"));
    }

    let mut positions = [0usize; MAX_MERGE_SOURCES];
    let mut writer = TableWriter::new(destination)?;
    loop {
        let mut winner: Option<usize> = None;
        for (source, table) in sources.iter().enumerate() {
            let Some(key) = table.key_at(positions[source]) else {
                continue;
            };
            if winner.is_none_or(|previous| {
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
