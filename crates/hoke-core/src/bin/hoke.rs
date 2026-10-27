use std::env;
use std::error::Error;
use std::ops::Bound;
use std::path::PathBuf;
use std::process::ExitCode;

use hoke_core::storage::FileStorage;
use hoke_core::{
    BatchOperation, CleanupReport, Database, DatabaseOptions, Engine, NativeDirectory, ScanOptions,
    validate_batch,
};

fn run() -> Result<(), Box<dyn Error>> {
    let mut arguments = env::args_os().skip(1);
    let first = arguments.next().ok_or("missing path or --db")?;
    let directory_mode = first == "--db";
    let path = if directory_mode {
        PathBuf::from(arguments.next().ok_or("missing database directory")?)
    } else {
        PathBuf::from(first)
    };
    let mut command = arguments.next().ok_or("missing command")?;
    let mut database_options = DatabaseOptions::default();
    while directory_mode && (command == "--memtable-limit" || command == "--wal-limit") {
        let flag = command.to_str().expect("ASCII option");
        let duplicate = if flag == "--memtable-limit" {
            database_options.max_memtable_bytes.is_some()
        } else {
            database_options.max_wal_bytes.is_some()
        };
        if duplicate {
            return Err(format!("duplicate {flag}").into());
        }
        let value = arguments
            .next()
            .ok_or_else(|| format!("missing {flag} value"))?
            .into_string()
            .map_err(|_| format!("{flag} must be UTF-8"))?;
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(format!("{flag} must be a positive integer").into());
        }
        if flag == "--memtable-limit" {
            database_options.max_memtable_bytes = Some(
                value
                    .parse::<usize>()
                    .map_err(|_| "--memtable-limit exceeds usize")?,
            );
        } else {
            database_options.max_wal_bytes = Some(
                value
                    .parse::<u64>()
                    .map_err(|_| "--wal-limit exceeds u64")?,
            );
        }
        command = arguments.next().ok_or("missing command")?;
    }
    database_options.validate()?;
    let command = command.into_string().map_err(|_| "command must be UTF-8")?;
    // Validate the complete invocation before opening or creating any file.
    let arguments = arguments
        .map(|value| {
            value
                .into_string()
                .map_err(|_| "CLI keys/values must be UTF-8")
        })
        .collect::<Result<Vec<_>, _>>()?;
    let batch = if directory_mode && command == "batch" {
        let operations = parse_batch(&arguments)?;
        validate_batch(&operations)?;
        Some(operations)
    } else {
        None
    };
    let scan_options = if batch.is_some() {
        None
    } else if directory_mode && matches!(command.as_str(), "scan" | "prefix" | "range") {
        Some(parse_scan(&command, &arguments)?)
    } else {
        let expected = match command.as_str() {
            "put" => 2,
            "get" | "delete" => 1,
            "info" => 0,
            "flush" | "compact" | "gc" if directory_mode => 0,
            _ => return Err("unknown command for this storage mode".into()),
        };
        if arguments.len() != expected {
            return Err("wrong number of command arguments".into());
        }
        None
    };
    if directory_mode {
        return run_database(
            path,
            &command,
            &arguments,
            scan_options,
            batch,
            database_options,
        );
    }
    let mut engine = Engine::open(FileStorage::open(path)?)?;
    let recovery = engine.recovery();
    eprintln!(
        "replayed={}, discarded_tail_bytes={}",
        recovery.records_replayed, recovery.discarded_tail_bytes
    );
    match command.as_str() {
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

fn parse_batch(arguments: &[String]) -> Result<Vec<BatchOperation>, Box<dyn Error>> {
    let mut operations = Vec::new();
    let mut remaining = arguments;
    while let Some((operation, tail)) = remaining.split_first() {
        match operation.as_str() {
            "put" => {
                let [key, value, rest @ ..] = tail else {
                    return Err("batch put requires a key and value".into());
                };
                operations.push(BatchOperation::Put {
                    key: key.as_bytes().to_vec(),
                    value: value.as_bytes().to_vec(),
                });
                remaining = rest;
            }
            "delete" => {
                let [key, rest @ ..] = tail else {
                    return Err("batch delete requires a key".into());
                };
                operations.push(BatchOperation::Delete {
                    key: key.as_bytes().to_vec(),
                });
                remaining = rest;
            }
            _ => return Err("unknown batch operation: expected put or delete".into()),
        }
    }
    Ok(operations)
}

fn parse_scan(command: &str, arguments: &[String]) -> Result<ScanOptions, Box<dyn Error>> {
    let positional = match command {
        "prefix" => 1,
        "range" => 2,
        _ => 0,
    };
    if arguments.len() < positional
        || arguments[..positional]
            .iter()
            .any(|value| value.starts_with("--"))
    {
        return Err("missing scan arguments".into());
    }
    let mut options = ScanOptions::default();
    match command {
        "prefix" => options.prefix = Some(arguments[0].as_bytes().to_vec()),
        "range" => {
            if arguments[0].as_bytes() > arguments[1].as_bytes() {
                return Err("range start must not exceed end".into());
            }
            options.start = Bound::Included(arguments[0].as_bytes().to_vec());
            options.end = Bound::Excluded(arguments[1].as_bytes().to_vec());
        }
        _ => {}
    }
    let mut flags = arguments[positional..].iter();
    let mut has_limit = false;
    while let Some(flag) = flags.next() {
        match flag.as_str() {
            "--reverse" if !options.reverse => options.reverse = true,
            "--limit" if !has_limit => {
                let value = flags.next().ok_or("missing --limit value")?;
                if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                    return Err("--limit must be a nonnegative integer".into());
                }
                options.limit = value
                    .parse::<usize>()
                    .map_err(|_| "--limit exceeds usize")?;
                has_limit = true;
            }
            _ => return Err("unknown, duplicate, or extra scan argument".into()),
        }
    }
    Ok(options)
}

fn run_database(
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

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            eprintln!(
                "usage: hoke <path> put <key> <value> | get <key> | delete <key> | info (directory mode: --db <path> [--memtable-limit N] [--wal-limit N] <command>; also supports batch (put <key> <value> | delete <key>)* | flush | compact | gc | scan | prefix <prefix> | range <start> <end>, with --reverse and --limit N)"
            );
            ExitCode::FAILURE
        }
    }
}
