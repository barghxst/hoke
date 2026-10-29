use std::fs;
use std::path::PathBuf;

use super::Result;

pub(crate) struct Config {
    pub(super) dir: PathBuf,
    pub(super) keys: usize,
    pub(super) key_bytes: usize,
    pub(super) value_bytes: usize,
    pub(super) scan_limit: usize,
    pub(super) scan_iterations: usize,
    pub(super) batch_size: usize,
    pub(super) seed: u64,
}
impl Config {
    pub(crate) fn parse() -> Result<Option<Self>> {
        let args: Vec<_> = std::env::args_os().skip(1).collect();
        if args.len() == 1 && args[0] == "--help" {
            println!(
                "hoke-bench --dir <existing-empty-directory> [--keys 1000] [--key-bytes 16] [--value-bytes 256] [--scan-limit 100] [--scan-iterations 50] [--batch-size 100] [--seed 1]"
            );
            return Ok(None);
        }
        let mut config = Self {
            dir: PathBuf::new(),
            keys: 1000,
            key_bytes: 16,
            value_bytes: 256,
            scan_limit: 100,
            scan_iterations: 50,
            batch_size: 100,
            seed: 1,
        };
        let mut seen = std::collections::BTreeSet::new();
        let mut pairs = args.chunks_exact(2);
        for pair in &mut pairs {
            let name = pair[0].to_str().ok_or("non-UTF8 option")?;
            if !seen.insert(name.to_owned()) {
                return Err(format!("duplicate option: {name}").into());
            }
            if name == "--dir" {
                config.dir = PathBuf::from(&pair[1]);
                continue;
            }
            let value = pair[1].to_str().ok_or("non-UTF8 number")?;
            if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                return Err("expected unsigned decimal number".into());
            }
            if name == "--seed" {
                config.seed = value.parse()?;
                continue;
            }
            let number: usize = value.parse()?;
            match name {
                "--keys" => config.keys = number,
                "--key-bytes" => config.key_bytes = number,
                "--value-bytes" => config.value_bytes = number,
                "--scan-limit" => config.scan_limit = number,
                "--scan-iterations" => config.scan_iterations = number,
                "--batch-size" => config.batch_size = number,
                _ => return Err(format!("unknown option: {name}").into()),
            }
        }
        if !pairs.remainder().is_empty() {
            return Err("missing option value".into());
        }
        if config.dir.as_os_str().is_empty() {
            return Err("--dir is required".into());
        }
        if !(1..=100_000).contains(&config.keys)
            || !(8..=65_536).contains(&config.key_bytes)
            || config.value_bytes > 16_777_216
            || !(1..=config.keys).contains(&config.scan_limit)
            || !(1..=10_000).contains(&config.scan_iterations)
            || !(1..=1024).contains(&config.batch_size)
        {
            return Err("benchmark option outside supported bounds".into());
        }
        if config.keys.div_ceil(config.batch_size) + 2 > 4096 {
            return Err("batch workload exceeds 4096 SSTables including preparation".into());
        }
        let raw = config
            .key_bytes
            .checked_add(config.value_bytes)
            .ok_or("byte overflow")?;
        if raw
            .checked_mul(config.batch_size.min(config.keys))
            .ok_or("byte overflow")?
            > 32 * 1024 * 1024
        {
            return Err("batch payload exceeds 32 MiB".into());
        }
        let prepared = config
            .key_bytes
            .checked_mul(3)
            .and_then(|n| {
                config
                    .value_bytes
                    .checked_mul(2)
                    .and_then(|v| n.checked_add(v))
            })
            .and_then(|n| n.checked_add(16))
            .and_then(|n| n.checked_mul(config.keys))
            .ok_or("prepared byte overflow")?;
        if prepared > 128 * 1024 * 1024 {
            return Err("prepared data exceeds 128 MiB".into());
        }
        if !config.dir.is_dir() || fs::read_dir(&config.dir)?.next().is_some() {
            return Err("--dir must be an existing empty directory".into());
        }
        Ok(Some(config))
    }
    pub(crate) fn json(&self) -> String {
        format!(
            "{{\"kind\":\"config\",\"schema\":1,\"keys\":{},\"key_bytes\":{},\"value_bytes\":{},\"scan_limit\":{},\"scan_iterations\":{},\"batch_size\":{},\"seed\":{},\"profile\":\"{}\",\"cache_policy\":\"warm_no_eviction\",\"durability\":\"sync_all\"}}",
            self.keys,
            self.key_bytes,
            self.value_bytes,
            self.scan_limit,
            self.scan_iterations,
            self.batch_size,
            self.seed,
            if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            }
        )
    }
}
