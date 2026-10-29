use std::collections::BTreeMap;
use std::path::Path;

use hoke_core::ScanOptions;

use super::config::Config;
use super::dataset::Dataset;
use super::measurement::{Measurement, Snapshot, measure, timed};
use super::{Model, Result, open, verify};

pub(super) fn run(
    c: &Config,
    data: &Dataset,
    path: &Path,
    output: &mut Vec<Measurement>,
) -> Result<Model> {
    let Dataset {
        keys,
        missing,
        values,
        order,
        ..
    } = data;
    let mut db = open(path)?;
    let mut model = BTreeMap::new();
    measure(
        &mut db,
        path,
        "put_sequential",
        c.keys,
        |db, s| {
            for i in 0..c.keys {
                timed(s, || db.put(&keys[i], &values[i]))?;
            }
            Ok(())
        },
        output,
    )?;
    for i in 0..c.keys {
        model.insert(keys[i].clone(), values[i].clone());
    }
    verify(&mut db, &model)?;
    measure(
        &mut db,
        path,
        "overwrite_random",
        c.keys,
        |db, s| {
            for &i in order {
                timed(s, || db.put(&keys[i], &values[(i + 1) % c.keys]))?;
            }
            Ok(())
        },
        output,
    )?;
    for i in 0..c.keys {
        model.insert(keys[i].clone(), values[(i + 1) % c.keys].clone());
    }
    verify(&mut db, &model)?;
    let before = Snapshot::read(&db, path)?;
    db.close()?;
    let mut samples = Vec::new();
    db = timed(&mut samples, || open(path))?;
    let after = Snapshot::read(&db, path)?;
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
        path,
        "flush",
        c.keys,
        |db, s| timed(s, || db.flush()).map_err(Into::into),
        output,
    )?;
    verify(&mut db, &model)?;
    measure(
        &mut db,
        path,
        "get_sst_random",
        c.keys,
        |db, s| {
            for &i in order {
                let result = timed(s, || db.get(&keys[i]))?;
                if result.as_ref() != model.get(&keys[i]) {
                    return Err("get mismatch".into());
                }
            }
            Ok(())
        },
        output,
    )?;
    measure(
        &mut db,
        path,
        "get_sst_missing",
        c.keys,
        |db, s| {
            for &i in order {
                if timed(s, || db.get(&missing[i]))?.is_some() {
                    return Err("missing key found".into());
                }
            }
            Ok(())
        },
        output,
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
            path,
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
            output,
        )?;
    }
    let removed_files = measure(
        &mut db,
        path,
        "gc",
        0,
        |db, s| {
            let report = timed(s, || db.collect_garbage())?;
            if !report.errors.is_empty() || report.removed_files == 0 {
                return Err("GC did not reclaim obsolete WAL".into());
            }
            Ok(report.removed_files)
        },
        output,
    )?;
    output.last_mut().ok_or("missing GC measurement")?.items = removed_files;
    verify(&mut db, &model)?;
    db.close()?;
    Ok(model)
}
