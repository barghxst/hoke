use std::fs;
use std::path::Path;
use std::time::Instant;

use super::{Db, Result};

pub(super) struct Snapshot {
    memtable_entries: usize,
    memtable_bytes: usize,
    table_count: usize,
    wal_bytes: u64,
    files: u64,
    bytes: u64,
}
impl Snapshot {
    pub(super) fn read(db: &Db, path: &Path) -> Result<Self> {
        let stats = db.stats();
        let (files, bytes) = disk(path)?;
        Ok(Self {
            memtable_entries: stats.memtable_entries,
            memtable_bytes: stats.memtable_bytes,
            table_count: stats.table_count,
            wal_bytes: stats.wal_bytes,
            files,
            bytes,
        })
    }
    pub(crate) fn json(&self) -> String {
        format!(
            "{{\"memtable_entries\":{},\"memtable_bytes\":{},\"table_count\":{},\"wal_bytes\":{},\"files\":{},\"bytes\":{}}}",
            self.memtable_entries,
            self.memtable_bytes,
            self.table_count,
            self.wal_bytes,
            self.files,
            self.bytes
        )
    }
}
fn disk(path: &Path) -> Result<(u64, u64)> {
    let mut files = 0_u64;
    let mut bytes = 0_u64;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let meta = entry.metadata()?;
        if meta.is_file() {
            files += 1;
            bytes = bytes.checked_add(meta.len()).ok_or("disk byte overflow")?;
        } else if meta.is_dir() {
            let (f, b) = disk(&entry.path())?;
            files += f;
            bytes = bytes.checked_add(b).ok_or("disk byte overflow")?;
        } else {
            return Err("unexpected non-regular benchmark file".into());
        }
    }
    Ok((files, bytes))
}
pub(crate) struct Measurement {
    pub(super) case: &'static str,
    pub(super) samples: Vec<u128>,
    pub(super) items: usize,
    pub(super) before: Snapshot,
    pub(super) after: Snapshot,
}
impl Measurement {
    pub(crate) fn json(&self) -> String {
        let samples = self
            .samples
            .iter()
            .map(u128::to_string)
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "{{\"kind\":\"measurement\",\"schema\":1,\"case\":\"{}\",\"calls\":{},\"items\":{},\"elapsed_ns\":{},\"samples_ns\":[{}],\"before\":{},\"after\":{}}}",
            self.case,
            self.samples.len(),
            self.items,
            self.samples.iter().sum::<u128>(),
            samples,
            self.before.json(),
            self.after.json()
        )
    }
}
pub(super) fn timed<T>(samples: &mut Vec<u128>, action: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let result = action();
    let elapsed = start.elapsed().as_nanos();
    samples.push(elapsed);
    result
}
pub(super) fn measure<T>(
    db: &mut Db,
    path: &Path,
    case: &'static str,
    items: usize,
    action: impl FnOnce(&mut Db, &mut Vec<u128>) -> Result<T>,
    output: &mut Vec<Measurement>,
) -> Result<T> {
    let before = Snapshot::read(db, path)?;
    let mut samples = Vec::new();
    let value = action(db, &mut samples)?;
    let after = Snapshot::read(db, path)?;
    output.push(Measurement {
        case,
        samples,
        items,
        before,
        after,
    });
    Ok(value)
}
