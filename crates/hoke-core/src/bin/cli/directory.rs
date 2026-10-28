//! Execute directory-backed commands and report publication versus cleanup outcomes.
use std::{error::Error, path::PathBuf};

use hoke_core::{
    BatchOperation, CleanupReport, Database, DatabaseOptions, NativeDirectory, ScanOptions,
};

pub(super) fn run(
    path: PathBuf,
    command: &str,
    arguments: &[String],
    scan_options: Option<ScanOptions>,
    batch: Option<Vec<BatchOperation>>,
    database_options: DatabaseOptions,
) -> Result<(), Box<dyn Error>> {
    let mut db = Database::open_with_options(NativeDirectory::open(path)?, database_options)?;
    let mut cleanup_failure = None;
    match command {
        "batch" => {
            let operations = batch.expect("validated batch operations");
            db.write_batch(&operations)?;
            println!("OK batch (published)");
            println!("operations={}", operations.len());
        }
        "put" => {
            db.put(arguments[0].as_bytes(), arguments[1].as_bytes())?;
            println!("OK put (synced)");
        }
        "delete" => {
            db.delete(arguments[0].as_bytes())?;
            println!("OK delete (synced)");
        }
        "get" => match db.get(arguments[0].as_bytes())? {
            Some(value) => println!("value={:?}", String::from_utf8_lossy(&value)),
            None => println!("NOT_FOUND"),
        },
        "flush" => {
            db.flush()?;
            println!("OK flush");
        }
        "compact" => {
            let report = db.compact()?;
            println!("OK compact (published)");
            println!(
                "tables_before={}, tables_after={}",
                report.tables_before, report.tables_after
            );
            if !print_cleanup(&report.cleanup) {
                cleanup_failure = Some("compaction published; cleanup incomplete");
            }
        }
        "gc" => {
            let report = db.collect_garbage()?;
            if print_cleanup(&report) {
                println!("OK gc");
            } else {
                cleanup_failure = Some("garbage collection cleanup incomplete");
            }
        }
        "scan" | "prefix" | "range" => {
            let options = scan_options.expect("validated scan options");
            let rows = if command == "range" {
                db.range(options)?
            } else {
                db.scan(options)?
            };
            if rows.is_empty() {
                println!("EMPTY");
            }
            for (key, value) in rows {
                println!(
                    "key={:?}, value={:?}",
                    String::from_utf8_lossy(&key),
                    String::from_utf8_lossy(&value)
                );
            }
        }
        "info" => {}
        _ => unreachable!(),
    }
    if matches!(command, "flush" | "info" | "compact" | "gc" | "batch") {
        let stats = db.stats();
        println!(
            "memtable_entries={}, tables={}, wal_bytes={}, memtable_bytes={}",
            stats.memtable_entries, stats.table_count, stats.wal_bytes, stats.memtable_bytes
        );
    }
    db.close()?;
    if let Some(error) = cleanup_failure {
        return Err(error.into());
    }
    Ok(())
}

fn print_cleanup(report: &CleanupReport) -> bool {
    println!("removed_files={}", report.removed_files);
    for error in &report.errors {
        match &error.file {
            Some(file) => eprintln!(
                "cleanup error: {} ({file:?}): {}",
                error.operation, error.source
            ),
            None => eprintln!("cleanup error: {}: {}", error.operation, error.source),
        }
    }
    report.errors.is_empty()
}
