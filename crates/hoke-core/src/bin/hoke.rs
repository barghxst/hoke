//! Native CLI entry point; validation and execution belong to the CLI domain.
mod cli;

use std::process::ExitCode;

fn main() -> ExitCode {
    match cli::run() {
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
