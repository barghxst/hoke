mod bench;

use std::fmt::Write as _;

use bench::{Config, Result, run};

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
