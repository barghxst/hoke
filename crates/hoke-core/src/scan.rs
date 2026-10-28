use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::ops::{Bound, Range};

use crate::Engine;
use crate::database::DbError;
use crate::sstable::{Lookup, SSTable};
use crate::storage::Storage;

/// Owned byte-key/value pair, independent of the database after the scan returns.
pub type KeyValue = (Vec<u8>, Vec<u8>);

/// Bounds and prefix are intersected. Default returns at most 100 live keys.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanOptions {
    pub start: Bound<Vec<u8>>,
    pub end: Bound<Vec<u8>>,
    pub prefix: Option<Vec<u8>>,
    pub reverse: bool,
    pub limit: usize,
}
impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            start: Bound::Unbounded,
            end: Bound::Unbounded,
            prefix: None,
            reverse: false,
            limit: 100,
        }
    }
}
impl ScanOptions {
    /// Reject reversed explicit bounds without reading storage.
    pub fn validate(&self) -> Result<(), DbError> {
        if let (Some(start), Some(end)) = (bound_key(&self.start), bound_key(&self.end))
            && start > end
        {
            return Err(DbError::InvalidRange);
        }
        Ok(())
    }
}
fn bound_key(bound: &Bound<Vec<u8>>) -> Option<&[u8]> {
    match bound {
        Bound::Included(key) | Bound::Excluded(key) => Some(key),
        Bound::Unbounded => None,
    }
}
fn empty(start: &Bound<Vec<u8>>, end: &Bound<Vec<u8>>) -> bool {
    match (bound_key(start), bound_key(end)) {
        (Some(start_key), Some(end_key)) => {
            start_key > end_key
                || (start_key == end_key
                    && (matches!(start, Bound::Excluded(_)) || matches!(end, Bound::Excluded(_))))
        }
        _ => false,
    }
}
fn prefix_end(prefix: &[u8]) -> Option<Vec<u8>> {
    let position = prefix.iter().rposition(|byte| *byte != u8::MAX)?;
    let mut upper = prefix[..=position].to_vec();
    upper[position] += 1;
    Some(upper)
}
fn intersect_prefix(options: &mut ScanOptions) {
    let Some(prefix) = options.prefix.take() else {
        return;
    };
    if bound_key(&options.start).is_none_or(|key| key < prefix.as_slice()) {
        options.start = Bound::Included(prefix.clone());
    }
    if let Some(upper) = prefix_end(&prefix)
        && bound_key(&options.end).is_none_or(|key| key >= upper.as_slice())
    {
        options.end = Bound::Excluded(upper);
    }
}

type KeyIter<'a> = Box<dyn Iterator<Item = &'a [u8]> + 'a>;
enum Cursor<'a> {
    Table {
        table: usize,
        positions: Range<usize>,
    },
    Memory(KeyIter<'a>),
}
impl Cursor<'_> {
    fn next_key<S: Storage>(&mut self, tables: &[SSTable<S>], reverse: bool) -> Option<Vec<u8>> {
        match self {
            Self::Table { table, positions } => {
                let position = if reverse {
                    positions.next_back()
                } else {
                    positions.next()
                }?;
                tables[*table].key_at(position).map(<[u8]>::to_vec)
            }
            Self::Memory(keys) => keys.next().map(<[u8]>::to_vec),
        }
    }
}
#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum OrderedKey {
    Ascending(Reverse<Vec<u8>>),
    Descending(Vec<u8>),
}
impl OrderedKey {
    fn new(key: Vec<u8>, reverse: bool) -> Self {
        if reverse {
            Self::Descending(key)
        } else {
            Self::Ascending(Reverse(key))
        }
    }
    fn bytes(&self) -> &[u8] {
        match self {
            Self::Ascending(Reverse(key)) | Self::Descending(key) => key,
        }
    }
    fn into_bytes(self) -> Vec<u8> {
        match self {
            Self::Ascending(Reverse(key)) | Self::Descending(key) => key,
        }
    }
}
fn advance<S: Storage>(
    source: usize,
    cursors: &mut [Cursor<'_>],
    tables: &[SSTable<S>],
    heap: &mut BinaryHeap<(OrderedKey, usize)>,
    reverse: bool,
) {
    if let Some(key) = cursors[source].next_key(tables, reverse) {
        heap.push((OrderedKey::new(key, reverse), source));
    }
}

pub(crate) fn scan_sources<S: Storage>(
    wal: &Engine<S>,
    tables: &mut [SSTable<S>],
    options: ScanOptions,
) -> Result<Vec<KeyValue>, DbError> {
    let mut result = Vec::new();
    visit_sources(wal, tables, options, |key, value| {
        result
            .try_reserve(1)
            .map_err(|_| DbError::AllocationFailed)?;
        result.push((key, value));
        Ok(())
    })?;
    Ok(result)
}

// Share the ordering/version resolution without materialising compaction values.
pub(crate) fn visit_sources<W: Storage, T: Storage>(
    wal: &Engine<W>,
    tables: &mut [SSTable<T>],
    mut options: ScanOptions,
    mut visit: impl FnMut(Vec<u8>, Vec<u8>) -> Result<(), DbError>,
) -> Result<(), DbError> {
    options.validate()?;
    if options.limit == 0
        || options
            .prefix
            .as_ref()
            .is_some_and(|p| p.len() > crate::wal::MAX_KEY_BYTES)
    {
        return Ok(());
    }
    intersect_prefix(&mut options);
    if empty(&options.start, &options.end) {
        return Ok(());
    }
    let table_count = tables.len();
    let mut cursors: Vec<Cursor<'_>> = tables
        .iter()
        .enumerate()
        .map(|(table, run)| Cursor::Table {
            table,
            positions: run.range_positions(&options.start, &options.end),
        })
        .collect();
    cursors.push(Cursor::Memory(wal.value_keys(
        &options.start,
        &options.end,
        options.reverse,
    )));
    cursors.push(Cursor::Memory(wal.deleted_keys(
        &options.start,
        &options.end,
        options.reverse,
    )));
    let mut heap = BinaryHeap::new();
    for source in 0..cursors.len() {
        advance(source, &mut cursors, tables, &mut heap, options.reverse);
    }
    let mut emitted = 0;
    while let Some((ordered, source)) = heap.pop() {
        let key = ordered.into_bytes();
        let mut winner = source;
        advance(source, &mut cursors, tables, &mut heap, options.reverse);
        // Resolve all older versions before reading a value. Newer sources win,
        // independent of traversal direction. Each source has unique sorted keys.
        while heap.peek().is_some_and(|(head, _)| head.bytes() == key) {
            let Some((_, duplicate)) = heap.pop() else {
                break;
            };
            winner = winner.max(duplicate);
            advance(duplicate, &mut cursors, tables, &mut heap, options.reverse);
        }
        let value = if winner >= table_count {
            wal.lookup_change(&key)
                .ok_or_else(|| DbError::Corruption("memtable cursor lost its key".into()))?
                .map(<[u8]>::to_vec)
        } else {
            match tables[winner].get(&key).map_err(DbError::Sst)? {
                Lookup::Value(value) => Some(value),
                Lookup::Deleted => None,
                Lookup::Missing => {
                    return Err(DbError::Corruption("SST cursor lost its key".into()));
                }
            }
        };
        if let Some(value) = value {
            visit(key, value)?;
            emitted += 1;
            if emitted == options.limit {
                break;
            }
        }
    }
    Ok(())
}
