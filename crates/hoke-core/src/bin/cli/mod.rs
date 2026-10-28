//! Native CLI orchestration: validate first, then choose one storage backend.
mod arguments;
mod directory;
mod wal;

use std::{env, error::Error};

pub(super) fn run() -> Result<(), Box<dyn Error>> {
    let invocation = arguments::parse(env::args_os().skip(1))?;
    if invocation.directory_mode {
        return directory::run(
            invocation.path,
            &invocation.command,
            &invocation.arguments,
            invocation.scan_options,
            invocation.batch,
            invocation.database_options,
        );
    }
    wal::run(invocation.path, &invocation.command, &invocation.arguments)
}
