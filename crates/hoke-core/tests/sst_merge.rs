use std::{cell::Cell, collections::BTreeMap, io, rc::Rc};

use hoke_core::{
    sstable::{Lookup, MAX_MERGE_SOURCES, SSTable, SstError, merge_tables, write_table},
    storage::{MemoryStorage, Storage},
};

#[derive(Default)]
struct Access {
    fail_reads: Cell<bool>,
    reads: Cell<usize>,
    mutations: Cell<usize>,
}

struct ObservedStorage {
    inner: MemoryStorage,
    access: Rc<Access>,
    write_limit: Option<usize>,
}

impl ObservedStorage {
    fn new(inner: MemoryStorage) -> Self {
        Self {
            inner,
            access: Rc::default(),
            write_limit: None,
        }
    }
}

impl Storage for ObservedStorage {
    fn len(&self) -> io::Result<u64> {
        self.inner.len()
    }
    fn read_at(&mut self, offset: u64, bytes: &mut [u8]) -> io::Result<usize> {
        self.access.reads.set(self.access.reads.get() + 1);
        if self.access.fail_reads.get() {
            return Err(io::Error::other("injected read failure"));
        }
        // Force short reads so streaming merge also exercises exact reads.
        let len = bytes.len().min(7);
        self.inner.read_at(offset, &mut bytes[..len])
    }
    fn write_all_at(&mut self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        let writes = self.access.mutations.get();
        self.access.mutations.set(writes + 1);
        if self.write_limit.is_some_and(|limit| writes >= limit) {
            return Err(io::Error::other("injected output failure"));
        }
        self.inner.write_all_at(offset, bytes)
    }
    fn set_len(&mut self, len: u64) -> io::Result<()> {
        self.access.mutations.set(self.access.mutations.get() + 1);
        self.inner.set_len(len)
    }
    fn flush(&mut self) -> io::Result<()> {
        panic!("merge must not flush")
    }
    fn sync_all(&mut self) -> io::Result<()> {
        panic!("merge must not sync")
    }
}

type Rows = BTreeMap<Vec<u8>, Option<Vec<u8>>>;

fn source(rows: Rows) -> (SSTable<ObservedStorage>, Rc<Access>) {
    let mut memory = MemoryStorage::new();
    write_table(&mut memory, &rows).unwrap();
    let storage = ObservedStorage::new(memory);
    let access = storage.access.clone();
    let table = SSTable::open(storage).unwrap();
    access.reads.set(0);
    (table, access)
}

fn row(key: &[u8], value: Option<&[u8]>) -> (Vec<u8>, Option<Vec<u8>>) {
    (key.to_vec(), value.map(<[u8]>::to_vec))
}

fn assert_rows(storage: MemoryStorage, expected: &Rows) {
    let mut table = SSTable::open(storage).unwrap();
    assert_eq!(table.len(), expected.len());
    for (position, (key, value)) in expected.iter().enumerate() {
        assert_eq!(table.key_at(position), Some(key.as_slice()));
        let lookup = value.clone().map_or(Lookup::Deleted, Lookup::Value);
        assert_eq!(table.get(key).unwrap(), lookup);
    }
}

#[test]
fn newest_wins_and_deletions_and_reinserts_survive_partial_compaction() {
    let (old, old_access) = source(Rows::from([
        row(b"a", Some(b"old")),
        row(b"b", Some(b"deleted")),
        row(b"c", Some(b"original")),
    ]));
    let (middle, middle_access) = source(Rows::from([
        row(b"a", Some(b"middle")),
        row(b"b", None),
        row(b"c", None),
    ]));
    let (new, new_access) = source(Rows::from([
        row(b"a", Some(b"latest")),
        row(b"c", Some(b"reinsert")),
    ]));
    let mut output = MemoryStorage::new();
    assert_eq!(
        merge_tables(&mut output, &mut [old, middle, new]).unwrap(),
        3
    );
    assert_rows(
        output,
        &Rows::from([
            row(b"a", Some(b"latest")),
            row(b"b", None),
            row(b"c", Some(b"reinsert")),
        ]),
    );
    for access in [old_access, middle_access, new_access] {
        assert_eq!(access.mutations.get(), 0);
    }
}

#[test]
fn empty_input_and_empty_runs_produce_valid_tables() {
    let mut output = MemoryStorage::new();
    let sources: &mut [SSTable<MemoryStorage>] = &mut [];
    assert_eq!(merge_tables(&mut output, sources).unwrap(), 0);
    assert_rows(output, &Rows::new());

    let (empty, _) = source(Rows::new());
    let (nonempty, _) = source(Rows::from([row(b"x", None)]));
    let (empty_newest, _) = source(Rows::new());
    let mut output = MemoryStorage::new();
    assert_eq!(
        merge_tables(&mut output, &mut [empty, nonempty, empty_newest]).unwrap(),
        1
    );
    assert_rows(output, &Rows::from([row(b"x", None)]));
}

#[test]
fn binary_order_and_empty_values_are_preserved() {
    let expected = Rows::from([
        row(b"", Some(b"")),
        row(&[0, 255], None),
        row(&[255], Some(&[0, 255, 0])),
        row(&[255, 0], Some(b"")),
    ]);
    let (a, _) = source(Rows::from([row(&[255], None), row(b"", Some(b"old"))]));
    let (b, _) = source(expected.clone());
    let mut output = MemoryStorage::new();
    assert_eq!(
        merge_tables(&mut output, &mut [a, b]).unwrap(),
        expected.len()
    );
    assert_rows(output, &expected);
}

#[test]
fn shadowed_payload_is_never_read() {
    let (old, old_access) = source(Rows::from([row(b"same", Some(b"shadowed"))]));
    old_access.fail_reads.set(true);
    let (new, _) = source(Rows::from([row(b"same", None)]));
    let mut output = MemoryStorage::new();
    assert_eq!(merge_tables(&mut output, &mut [old, new]).unwrap(), 1);
    assert_eq!(old_access.reads.get(), 0);
    assert_rows(output, &Rows::from([row(b"same", None)]));
}

#[test]
fn winning_record_read_failure_propagates_without_source_mutation() {
    let (table, access) = source(Rows::from([row(b"x", Some(b"value"))]));
    access.fail_reads.set(true);
    let mut output = MemoryStorage::new();
    assert!(matches!(
        merge_tables(&mut output, &mut [table]),
        Err(SstError::Io(_))
    ));
    assert_eq!(access.mutations.get(), 0);
    assert!(SSTable::open(output).is_err());
}

#[test]
fn every_output_write_failure_preserves_sources_and_propagates() {
    // Initialization, records, index, index CRC and header replacement.
    for limit in 0..6 {
        let (table, access) = source(Rows::from([row(b"a", Some(b"one")), row(b"b", None)]));
        let mut output = ObservedStorage::new(MemoryStorage::new());
        output.write_limit = Some(limit);
        assert!(
            matches!(
                merge_tables(&mut output, &mut [table]),
                Err(SstError::Io(_))
            ),
            "limit={limit}"
        );
        assert_eq!(access.mutations.get(), 0);
        assert!(SSTable::open(output.inner).is_err());
    }
}

#[test]
fn source_cap_is_checked_before_any_destination_write() {
    let mut sources: Vec<_> = (0..=MAX_MERGE_SOURCES)
        .map(|_| source(Rows::new()).0)
        .collect();
    let mut output = ObservedStorage::new(MemoryStorage::new());
    assert!(matches!(
        merge_tables(&mut output, &mut sources),
        Err(SstError::Invalid("too many merge sources"))
    ));
    assert_eq!(output.access.mutations.get(), 0);
    assert!(output.inner.is_empty().unwrap());
    sources.pop();
    assert_eq!(merge_tables(&mut output, &mut sources).unwrap(), 0);
    assert!(SSTable::open(output.inner).unwrap().is_empty());
}

#[test]
fn nonempty_output_is_untouched() {
    let mut output = MemoryStorage::new();
    output.write_all_at(0, b"existing").unwrap();
    let mut output = ObservedStorage::new(output);
    let mut sources: [SSTable<MemoryStorage>; 0] = [];
    assert!(matches!(
        merge_tables(&mut output, &mut sources),
        Err(SstError::Invalid("destination is not empty"))
    ));
    assert_eq!(output.access.mutations.get(), 0);
    let mut bytes = [0; 8];
    output.inner.read_at(0, &mut bytes).unwrap();
    assert_eq!(&bytes, b"existing");
}

#[test]
fn deterministic_overlapping_run_trace_matches_reference_map() {
    let mut expected = Rows::new();
    let mut sources = Vec::new();
    for generation in 0..MAX_MERGE_SOURCES {
        let mut rows = Rows::new();
        for ordinal in 0..37 {
            let key = vec![((generation * 17 + ordinal * 13) % 97) as u8, 0, 255];
            let value =
                ((generation + ordinal) % 5 != 0).then(|| vec![generation as u8, ordinal as u8]);
            rows.insert(key.clone(), value.clone());
            expected.insert(key, value);
        }
        sources.push(source(rows).0);
    }
    let mut output = MemoryStorage::new();
    assert_eq!(
        merge_tables(&mut output, &mut sources).unwrap(),
        expected.len()
    );
    assert_rows(output, &expected);
}
