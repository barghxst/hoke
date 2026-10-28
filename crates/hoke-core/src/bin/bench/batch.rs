//! Batch writes, stale-run preparation, compaction, and SST reopen.

use std::collections::BTreeMap;
use std::path::Path;

use super::config::Config;
use super::dataset::Dataset;
use super::measurement::{Measurement, Snapshot, measure, timed};
use super::{Result, open, verify};

pub(super) fn run(
    c: &Config,
    data: &Dataset,
    batch_path: &Path,
    output: &mut Vec<Measurement>,
) -> Result<()> {
    let Dataset {
        keys,
        values,
        order,
        batches,
        ..
    } = data;
    let mut db = open(batch_path)?;
    let mut model = BTreeMap::new();
    measure(
        &mut db,
        batch_path,
        "batch_put",
        c.keys,
        |db, s| {
            for batch in batches {
                timed(s, || db.write_batch(batch))?;
            }
            Ok(())
        },
        output,
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
        batch_path,
        "compact",
        model.len(),
        |db, s| {
            let report = timed(s, || db.compact())?;
            if !report.cleanup.errors.is_empty() {
                return Err("compaction cleanup failed".into());
            }
            Ok(())
        },
        output,
    )?;
    verify(&mut db, &model)?;
    let before = Snapshot::read(&db, batch_path)?;
    db.close()?;
    let mut samples = Vec::new();
    let mut db = timed(&mut samples, || open(batch_path))?;
    let after = Snapshot::read(&db, batch_path)?;
    output.push(Measurement {
        case: "reopen_sst",
        samples,
        items: model.len(),
        before,
        after,
    });
    verify(&mut db, &model)?;
    db.close()?;
    Ok(())
}
