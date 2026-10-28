//! Execute the WAL-only command set and report recovered records.
use std::{error::Error, path::PathBuf};

use hoke_core::{Engine, storage::FileStorage};

pub(super) fn run(
    path: PathBuf,
    command: &str,
    arguments: &[String],
) -> Result<(), Box<dyn Error>> {
    let mut engine = Engine::open(FileStorage::open(path)?)?;
    let recovery = engine.recovery();
    eprintln!(
        "replayed={}, discarded_tail_bytes={}",
        recovery.records_replayed, recovery.discarded_tail_bytes
    );
    match command {
        "put" => {
            engine.put(arguments[0].as_bytes(), arguments[1].as_bytes())?;
            println!("OK put (synced)");
        }
        "delete" => {
            engine.delete(arguments[0].as_bytes())?;
            println!("OK delete (synced)");
        }
        "get" => match engine.get(arguments[0].as_bytes()) {
            Some(value) => println!("value={:?}", String::from_utf8_lossy(value)),
            None => println!("NOT_FOUND"),
        },
        "info" => println!("keys={}", engine.len()),
        _ => unreachable!(),
    }
    engine.close()?;
    Ok(())
}
