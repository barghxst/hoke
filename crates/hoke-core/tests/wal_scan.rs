use std::cell::Cell;
use std::collections::BTreeMap;
use std::io;
use std::ops::Bound;
use std::rc::Rc;

use hoke_core::storage::{MemoryStorage, Storage};
use hoke_core::{DbError, Engine, ScanOptions};

struct WatchedStorage {
    memory: MemoryStorage,
    forbid_io: Rc<Cell<bool>>,
}
impl WatchedStorage {
    fn check(&self) {
        assert!(!self.forbid_io.get(), "scan must not access storage");
    }
}
impl Storage for WatchedStorage {
    fn len(&self) -> io::Result<u64> {
        self.check();
        self.memory.len()
    }
    fn read_at(&mut self, offset: u64, bytes: &mut [u8]) -> io::Result<usize> {
        self.check();
        self.memory.read_at(offset, bytes)
    }
    fn write_all_at(&mut self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        self.check();
        self.memory.write_all_at(offset, bytes)
    }
    fn set_len(&mut self, len: u64) -> io::Result<()> {
        self.check();
        self.memory.set_len(len)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.check();
        self.memory.flush()
    }
    fn sync_all(&mut self) -> io::Result<()> {
        self.check();
        self.memory.sync_all()
    }
}

fn matches(key: &[u8], options: &ScanOptions) -> bool {
    let start = match &options.start {
        Bound::Unbounded => true,
        Bound::Included(bound) => key >= bound.as_slice(),
        Bound::Excluded(bound) => key > bound.as_slice(),
    };
    let end = match &options.end {
        Bound::Unbounded => true,
        Bound::Included(bound) => key <= bound.as_slice(),
        Bound::Excluded(bound) => key < bound.as_slice(),
    };
    start
        && end
        && options
            .prefix
            .as_ref()
            .is_none_or(|prefix| key.starts_with(prefix))
}

#[test]
fn deterministic_mutations_match_model_for_bounds_prefix_reverse_and_limits() {
    let forbid_io = Rc::new(Cell::new(false));
    let mut engine = Engine::open(WatchedStorage {
        memory: MemoryStorage::new(),
        forbid_io: forbid_io.clone(),
    })
    .unwrap();
    let keys = [
        vec![],
        vec![0],
        vec![0, 255],
        vec![1],
        vec![255],
        vec![255, 255],
    ];
    let mut model = BTreeMap::new();
    for step in 0..137 {
        let key = &keys[(step * 7 + step / 9) % keys.len()];
        if step % 5 == 0 {
            engine.delete(key).unwrap();
            model.remove(key);
        } else {
            let value = vec![step as u8, 0, 255];
            engine.put(key, &value).unwrap();
            model.insert(key.clone(), value);
        }
    }

    engine.put(b"", b"").unwrap();
    model.insert(vec![], vec![]);
    engine.put(&[255, 255], b"last").unwrap();
    model.insert(vec![255, 255], b"last".to_vec());
    forbid_io.set(true);
    let bounds = [
        Bound::Unbounded,
        Bound::Included(vec![]),
        Bound::Excluded(vec![0]),
        Bound::Included(vec![1]),
        Bound::Excluded(vec![255, 255]),
    ];
    for start in &bounds {
        for end in &bounds {
            for prefix in [
                None,
                Some(vec![]),
                Some(vec![0]),
                Some(vec![255]),
                Some(vec![255, 255]),
            ] {
                for reverse in [false, true] {
                    for limit in [0, 1, 3, 100] {
                        let options = ScanOptions {
                            start: start.clone(),
                            end: end.clone(),
                            prefix: prefix.clone(),
                            reverse,
                            limit,
                        };
                        if options.validate().is_err() {
                            assert!(matches!(
                                engine.scan_with_budget(options, usize::MAX),
                                Err(DbError::InvalidRange)
                            ));
                            continue;
                        }
                        let mut expected: Vec<_> = model
                            .iter()
                            .filter(|(key, _)| matches(key, &options))
                            .map(|(key, value)| (key.clone(), value.clone()))
                            .collect();
                        if reverse {
                            expected.reverse();
                        }
                        expected.truncate(limit);
                        assert_eq!(
                            engine
                                .scan_with_budget(options.clone(), usize::MAX)
                                .unwrap(),
                            expected,
                            "{options:?}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn budget_exact_boundary_rejects_whole_result_and_preserves_state() {
    let mut engine = Engine::open(MemoryStorage::new()).unwrap();
    engine.put(b"a", b"123").unwrap();
    engine.put(b"b", b"45").unwrap();
    assert!(matches!(
        engine.scan_with_budget(ScanOptions::default(), 6),
        Err(DbError::Limit(_))
    ));
    let mut rows = engine.scan_with_budget(ScanOptions::default(), 7).unwrap();
    assert_eq!(
        rows,
        vec![
            (b"a".to_vec(), b"123".to_vec()),
            (b"b".to_vec(), b"45".to_vec())
        ]
    );
    rows[0].1.fill(0);
    assert_eq!(engine.get(b"a"), Some(b"123".as_slice()));
    engine.put(b"c", b"ok").unwrap();
    assert_eq!(engine.get(b"c"), Some(b"ok".as_slice()));
}

#[test]
fn zero_budget_allows_empty_payload_and_zero_limit_but_validates_bounds() {
    let mut engine = Engine::open(MemoryStorage::new()).unwrap();
    engine.put(b"", b"").unwrap();
    assert_eq!(
        engine.scan_with_budget(ScanOptions::default(), 0).unwrap(),
        vec![(vec![], vec![])]
    );
    let options = ScanOptions {
        limit: 0,
        ..ScanOptions::default()
    };
    assert!(
        engine
            .scan_with_budget(options.clone(), 0)
            .unwrap()
            .is_empty()
    );
    let invalid = ScanOptions {
        start: Bound::Included(b"z".to_vec()),
        end: Bound::Excluded(b"a".to_vec()),
        ..options
    };
    assert!(matches!(
        engine.scan_with_budget(invalid, 0),
        Err(DbError::InvalidRange)
    ));
}

#[test]
fn tombstones_do_not_consume_limit_or_payload() {
    let mut engine = Engine::open(MemoryStorage::new()).unwrap();
    engine.put(b"a", b"old").unwrap();
    engine.put(b"b", b"new").unwrap();
    engine.delete(b"a").unwrap();
    engine.delete(b"missing").unwrap();
    let rows = engine
        .scan_with_budget(
            ScanOptions {
                limit: 1,
                ..ScanOptions::default()
            },
            4,
        )
        .unwrap();
    assert_eq!(rows, vec![(b"b".to_vec(), b"new".to_vec())]);
}
