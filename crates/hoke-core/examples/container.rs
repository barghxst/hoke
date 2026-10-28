//! Small UTF-8 CLI for the experimental single-file container database.
use std::{error::Error, io, path::PathBuf, process::ExitCode};

use hoke_core::{
    BatchOperation, ContainerDatabase, ScanOptions, storage::FileStorage, validate_batch,
};

const USAGE: &str = "container <path.hkc> get KEY | put KEY VALUE | delete KEY | batch [put KEY VALUE | delete KEY]... | scan | flush | compact | info";
enum Command {
    Get(Vec<u8>),
    Batch(Vec<BatchOperation>),
    Scan,
    Flush,
    Compact,
    Info,
}
fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, USAGE)
}
fn operation(args: &[String], at: &mut usize) -> Result<BatchOperation, io::Error> {
    let tag = args.get(*at).ok_or_else(invalid)?;
    let key = args.get(*at + 1).ok_or_else(invalid)?.as_bytes().to_vec();
    match tag.as_str() {
        "put" => {
            let value = args.get(*at + 2).ok_or_else(invalid)?.as_bytes().to_vec();
            *at += 3;
            Ok(BatchOperation::Put { key, value })
        }
        "delete" => {
            *at += 2;
            Ok(BatchOperation::Delete { key })
        }
        _ => Err(invalid()),
    }
}
fn run() -> Result<(), Box<dyn Error>> {
    let mut raw = std::env::args_os().skip(1);
    let path = PathBuf::from(raw.next().ok_or_else(invalid)?);
    let args: Vec<String> = raw
        .map(|value| value.into_string().map_err(|_| invalid()))
        .collect::<Result<_, _>>()?;
    let command = match args.first().map(String::as_str) {
        Some("get") if args.len() == 2 => {
            if args[1].len() > 65536 {
                return Err(invalid().into());
            }
            Command::Get(args[1].as_bytes().to_vec())
        }
        Some("put" | "delete") => {
            let mut at = 0;
            let op = operation(&args, &mut at)?;
            if at != args.len() {
                return Err(invalid().into());
            }
            Command::Batch(vec![op])
        }
        Some("batch") => {
            let mut at = 1;
            let mut operations = Vec::new();
            while at < args.len() {
                operations.push(operation(&args, &mut at)?);
                if operations.len() > 1024 {
                    return Err(invalid().into());
                }
            }
            Command::Batch(operations)
        }
        Some("scan") if args.len() == 1 => Command::Scan,
        Some("flush") if args.len() == 1 => Command::Flush,
        Some("compact") if args.len() == 1 => Command::Compact,
        Some("info") if args.len() == 1 => Command::Info,
        _ => return Err(invalid().into()),
    };
    // Parse and validate the complete command before creating or locking a file.
    if let Command::Batch(operations) = &command {
        validate_batch(operations)?;
    }
    let storage = if matches!(&command, Command::Batch(operations) if !operations.is_empty()) {
        FileStorage::open(&path)?
    } else {
        FileStorage::open_existing(&path)?
    };
    let mut database = ContainerDatabase::open(storage)?;
    match command {
        Command::Get(key) => match database.get(&key)? {
            Some(value) => println!("value={:?}", String::from_utf8_lossy(&value)),
            None => println!("missing"),
        },
        Command::Batch(operations) => {
            database.write_batch(&operations)?;
            println!("OK batch (committed), operations={}", operations.len());
        }
        Command::Scan => {
            let options = ScanOptions {
                limit: 1000,
                ..Default::default()
            };
            for (key, value) in database.scan_with_budget(options, 32 * 1024 * 1024)? {
                println!(
                    "key={:?}, value={:?}",
                    String::from_utf8_lossy(&key),
                    String::from_utf8_lossy(&value)
                );
            }
        }
        Command::Flush => {
            database.flush()?;
            println!("OK flush (published)");
        }
        Command::Compact => {
            database.compact()?;
            println!("OK compact (published; append-only file)");
        }
        Command::Info => {
            let stats = database.stats();
            println!(
                "generation={}, tables={}, wal_segments={}, memtable_entries={}, memtable_bytes={}",
                stats.generation,
                stats.table_count,
                stats.wal_segments,
                stats.memtable_entries,
                stats.memtable_bytes
            );
        }
    }
    Ok(())
}
fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
