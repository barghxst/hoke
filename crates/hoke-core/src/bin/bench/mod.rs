mod batch;
mod config;
mod dataset;
mod measurement;
mod point;

use std::collections::BTreeMap;
use std::error::Error;
use std::path::Path;

use hoke_core::{Database, NativeDirectory, ScanOptions};

pub(super) use config::Config;
use dataset::Dataset;
use measurement::Measurement;

pub(super) type Result<T> = std::result::Result<T, Box<dyn Error>>;
type Db = Database<NativeDirectory>;
type Model = BTreeMap<Vec<u8>, Vec<u8>>;

fn open(path: &Path) -> Result<Db> {
    Ok(Database::open(NativeDirectory::open(path)?)?)
}
fn verify(db: &mut Db, model: &Model) -> Result<()> {
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
pub(super) fn run(c: &Config) -> Result<Vec<Measurement>> {
    let data = Dataset::prepare(c);
    let path = c.dir.join("point");
    let batch_path = c.dir.join("batch");
    let mut output = Vec::new();

    let _point_model = point::run(c, &data, &path, &mut output)?;
    batch::run(c, &data, &batch_path, &mut output)?;
    Ok(output)
}
