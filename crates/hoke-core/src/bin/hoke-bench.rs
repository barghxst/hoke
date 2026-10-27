//! Reproducible native API benchmark; see docs/adr/0009-native-benchmarks.md.
use hoke_core::{BatchOperation, Database, NativeDirectory, ScanOptions};
use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

type Result<T> = std::result::Result<T, Box<dyn Error>>;
type Db = Database<NativeDirectory>;

struct Config {
    dir: PathBuf,
    keys: usize,
    key_bytes: usize,
    value_bytes: usize,
    scan_limit: usize,
    scan_iterations: usize,
    batch_size: usize,
    seed: u64,
}
impl Config {
    fn parse() -> Result<Option<Self>> {
        let args: Vec<_> = std::env::args_os().skip(1).collect();
        if args.len() == 1 && args[0] == "--help" {
            println!(
                "hoke-bench --dir <existing-empty-directory> [--keys 1000] [--key-bytes 16] [--value-bytes 256] [--scan-limit 100] [--scan-iterations 50] [--batch-size 100] [--seed 1]"
            );
            return Ok(None);
        }
        let mut config = Self {
            dir: PathBuf::new(),
            keys: 1000,
            key_bytes: 16,
            value_bytes: 256,
            scan_limit: 100,
            scan_iterations: 50,
            batch_size: 100,
            seed: 1,
        };
        let mut seen = std::collections::BTreeSet::new();
        let mut pairs = args.chunks_exact(2);
        for pair in &mut pairs {
            let name = pair[0].to_str().ok_or("non-UTF8 option")?;
            if !seen.insert(name.to_owned()) {
                return Err(format!("duplicate option: {name}").into());
            }
            if name == "--dir" {
                config.dir = PathBuf::from(&pair[1]);
                continue;
            }
            let value = pair[1].to_str().ok_or("non-UTF8 number")?;
            if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                return Err("expected unsigned decimal number".into());
            }
            if name == "--seed" {
                config.seed = value.parse()?;
                continue;
            }
            let number: usize = value.parse()?;
            match name {
                "--keys" => config.keys = number,
                "--key-bytes" => config.key_bytes = number,
                "--value-bytes" => config.value_bytes = number,
                "--scan-limit" => config.scan_limit = number,
                "--scan-iterations" => config.scan_iterations = number,
                "--batch-size" => config.batch_size = number,
                _ => return Err(format!("unknown option: {name}").into()),
            }
        }
        if !pairs.remainder().is_empty() {
            return Err("missing option value".into());
        }
        if config.dir.as_os_str().is_empty() {
            return Err("--dir is required".into());
        }
        if !(1..=100_000).contains(&config.keys)
            || !(8..=65_536).contains(&config.key_bytes)
            || config.value_bytes > 16_777_216
            || !(1..=config.keys).contains(&config.scan_limit)
            || !(1..=10_000).contains(&config.scan_iterations)
            || !(1..=1024).contains(&config.batch_size)
        {
            return Err("benchmark option outside supported bounds".into());
        }
        if config.keys.div_ceil(config.batch_size) + 2 > 4096 {
            return Err("batch workload exceeds 4096 SSTables including preparation".into());
        }
        let raw = config
            .key_bytes
            .checked_add(config.value_bytes)
            .ok_or("byte overflow")?;
        if raw
            .checked_mul(config.batch_size.min(config.keys))
            .ok_or("byte overflow")?
            > 32 * 1024 * 1024
        {
            return Err("batch payload exceeds 32 MiB".into());
        }
        let prepared = config
            .key_bytes
            .checked_mul(3)
            .and_then(|n| {
                config
                    .value_bytes
                    .checked_mul(2)
                    .and_then(|v| n.checked_add(v))
            })
            .and_then(|n| n.checked_add(16))
            .and_then(|n| n.checked_mul(config.keys))
            .ok_or("prepared byte overflow")?;
        if prepared > 128 * 1024 * 1024 {
            return Err("prepared data exceeds 128 MiB".into());
        }
        if !config.dir.is_dir() || fs::read_dir(&config.dir)?.next().is_some() {
            return Err("--dir must be an existing empty directory".into());
        }
        Ok(Some(config))
    }
    fn json(&self) -> String {
        format!(
            "{{\"kind\":\"config\",\"schema\":1,\"keys\":{},\"key_bytes\":{},\"value_bytes\":{},\"scan_limit\":{},\"scan_iterations\":{},\"batch_size\":{},\"seed\":{},\"profile\":\"{}\",\"cache_policy\":\"warm_no_eviction\",\"durability\":\"sync_all\"}}",
            self.keys,
            self.key_bytes,
            self.value_bytes,
            self.scan_limit,
            self.scan_iterations,
            self.batch_size,
            self.seed,
            if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            }
        )
    }
}
struct Snapshot {
    memtable_entries: usize,
    memtable_bytes: usize,
    table_count: usize,
    wal_bytes: u64,
    files: u64,
    bytes: u64,
}
impl Snapshot {
    fn read(db: &Db, path: &Path) -> Result<Self> {
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
    fn json(&self) -> String {
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
struct Measurement {
    case: &'static str,
    samples: Vec<u128>,
    items: usize,
    before: Snapshot,
    after: Snapshot,
}
impl Measurement {
    fn json(&self) -> String {
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
fn timed<T>(samples: &mut Vec<u128>, action: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let result = action();
    let elapsed = start.elapsed().as_nanos();
    samples.push(elapsed);
    result
}
fn open(path: &Path) -> Result<Db> {
    Ok(Database::open(NativeDirectory::open(path)?)?)
}
fn verify(db: &mut Db, model: &BTreeMap<Vec<u8>, Vec<u8>>) -> Result<()> {
    let actual = db.scan(ScanOptions {
        limit: usize::MAX,
        ..Default::default()
    })?;
    if actual.len() != model.len()
        || actual
            .iter()
            .map(|(key, value)| (key, value))
            .ne(model.iter())
    {
        return Err("database differs from benchmark model".into());
    }
    for (key, value) in model {
        if db.get(key)?.as_ref() != Some(value) {
            return Err("point validation failed".into());
        }
    }
    Ok(())
}
fn measure<T>(
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
fn run(c: &Config) -> Result<Vec<Measurement>> {
    let keys: Vec<Vec<u8>> = (0..c.keys)
        .map(|i| {
            let mut key = vec![0x55; c.key_bytes];
            key[..8].copy_from_slice(&(i as u64).to_be_bytes());
            key
        })
        .collect();
    let missing: Vec<Vec<u8>> = (0..c.keys)
        .map(|i| {
            let mut key = keys[i].clone();
            key[..8].copy_from_slice(&((i + c.keys) as u64).to_be_bytes());
            key
        })
        .collect();
    let values: Vec<Vec<u8>> = (0..c.keys)
        .map(|i| {
            (0..c.value_bytes)
                .map(|j| (i.wrapping_mul(31).wrapping_add(j) % 251) as u8)
                .collect()
        })
        .collect();
    let mut order: Vec<usize> = (0..c.keys).collect();
    let mut state = c.seed;
    for i in (1..order.len()).rev() {
        state = state.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        z ^= z >> 31;
        order.swap(i, (z % (i as u64 + 1)) as usize);
    }
    let batches: Vec<Vec<BatchOperation>> = keys
        .chunks(c.batch_size)
        .zip(values.chunks(c.batch_size))
        .map(|(ks, vs)| {
            ks.iter()
                .zip(vs)
                .map(|(key, value)| BatchOperation::Put {
                    key: key.clone(),
                    value: value.clone(),
                })
                .collect()
        })
        .collect();
    let path = c.dir.join("point");
    let batch_path = c.dir.join("batch");
    let mut db = open(&path)?;
    let mut output = Vec::new();
    let mut model = BTreeMap::new();
    measure(
        &mut db,
        &path,
        "put_sequential",
        c.keys,
        |db, s| {
            for i in 0..c.keys {
                timed(s, || db.put(&keys[i], &values[i]))?;
            }
            Ok(())
        },
        &mut output,
    )?;
    for i in 0..c.keys {
        model.insert(keys[i].clone(), values[i].clone());
    }
    verify(&mut db, &model)?;
    measure(
        &mut db,
        &path,
        "overwrite_random",
        c.keys,
        |db, s| {
            for &i in &order {
                timed(s, || db.put(&keys[i], &values[(i + 1) % c.keys]))?;
            }
            Ok(())
        },
        &mut output,
    )?;
    for i in 0..c.keys {
        model.insert(keys[i].clone(), values[(i + 1) % c.keys].clone());
    }
    verify(&mut db, &model)?;
    let before = Snapshot::read(&db, &path)?;
    db.close()?;
    let mut samples = Vec::new();
    db = timed(&mut samples, || open(&path))?;
    let after = Snapshot::read(&db, &path)?;
    output.push(Measurement {
        case: "reopen_wal",
        samples,
        items: c.keys,
        before,
        after,
    });
    verify(&mut db, &model)?;
    measure(
        &mut db,
        &path,
        "flush",
        c.keys,
        |db, s| timed(s, || db.flush()).map_err(Into::into),
        &mut output,
    )?;
    verify(&mut db, &model)?;
    measure(
        &mut db,
        &path,
        "get_sst_random",
        c.keys,
        |db, s| {
            for &i in &order {
                let result = timed(s, || db.get(&keys[i]))?;
                if result.as_ref() != model.get(&keys[i]) {
                    return Err("get mismatch".into());
                }
            }
            Ok(())
        },
        &mut output,
    )?;
    measure(
        &mut db,
        &path,
        "get_sst_missing",
        c.keys,
        |db, s| {
            for &i in &order {
                if timed(s, || db.get(&missing[i]))?.is_some() {
                    return Err("missing key found".into());
                }
            }
            Ok(())
        },
        &mut output,
    )?;
    for reverse in [false, true] {
        let options = ScanOptions {
            reverse,
            limit: c.scan_limit,
            ..Default::default()
        };
        let mut expected: Vec<_> = model.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        if reverse {
            expected.reverse();
        }
        expected.truncate(c.scan_limit);
        measure(
            &mut db,
            &path,
            if reverse {
                "scan_reverse"
            } else {
                "scan_forward"
            },
            expected.len() * c.scan_iterations,
            |db, s| {
                for _ in 0..c.scan_iterations {
                    let opts = options.clone();
                    let result = timed(s, || db.scan(opts))?;
                    if result != expected {
                        return Err("scan mismatch".into());
                    }
                }
                Ok(())
            },
            &mut output,
        )?;
    }
    let removed_files = measure(
        &mut db,
        &path,
        "gc",
        0,
        |db, s| {
            let report = timed(s, || db.collect_garbage())?;
            if !report.errors.is_empty() || report.removed_files == 0 {
                return Err("GC did not reclaim obsolete WAL".into());
            }
            Ok(report.removed_files)
        },
        &mut output,
    )?;
    output.last_mut().ok_or("missing GC measurement")?.items = removed_files;
    verify(&mut db, &model)?;
    db.close()?;
    let mut db = open(&batch_path)?;
    let mut model = BTreeMap::new();
    measure(
        &mut db,
        &batch_path,
        "batch_put",
        c.keys,
        |db, s| {
            for batch in &batches {
                timed(s, || db.write_batch(batch))?;
            }
            Ok(())
        },
        &mut output,
    )?;
    for i in 0..c.keys {
        model.insert(keys[i].clone(), values[i].clone());
    }
    verify(&mut db, &model)?;
    // Deliberately stale runs and tombstones, prepared and verified outside timing.
    for &i in order.iter().take(c.keys.min(64)) {
        db.put(&keys[i], &values[(i + 1) % c.keys])?;
        model.insert(keys[i].clone(), values[(i + 1) % c.keys].clone());
    }
    db.flush()?;
    for i in (0..c.keys).step_by(3) {
        db.delete(&keys[i])?;
        model.remove(&keys[i]);
    }
    db.flush()?;
    verify(&mut db, &model)?;
    measure(
        &mut db,
        &batch_path,
        "compact",
        model.len(),
        |db, s| {
            let report = timed(s, || db.compact())?;
            if !report.cleanup.errors.is_empty() {
                return Err("compaction cleanup failed".into());
            }
            Ok(())
        },
        &mut output,
    )?;
    verify(&mut db, &model)?;
    let before = Snapshot::read(&db, &batch_path)?;
    db.close()?;
    let mut samples = Vec::new();
    let mut db = timed(&mut samples, || open(&batch_path))?;
    let after = Snapshot::read(&db, &batch_path)?;
    output.push(Measurement {
        case: "reopen_sst",
        samples,
        items: model.len(),
        before,
        after,
    });
    verify(&mut db, &model)?;
    db.close()?;
    Ok(output)
}
fn main() {
    let result = (|| -> Result<()> {
        let Some(config) = Config::parse()? else {
            return Ok(());
        };
        let measurements = run(&config)?;
        let mut output = config.json();
        output.push('\n');
        for measurement in measurements {
            writeln!(output, "{}", measurement.json())?;
        }
        use std::io::Write as _;
        std::io::stdout().lock().write_all(output.as_bytes())?;
        Ok(())
    })();
    if let Err(error) = result {
        eprintln!("hoke-bench: {error}");
        std::process::exit(1);
    }
}
