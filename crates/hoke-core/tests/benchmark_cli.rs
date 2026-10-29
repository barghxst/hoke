use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct TempDirectory(PathBuf);

impl TempDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "hoke-benchmark-cli-{}-{}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for TempDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn small_arguments() -> Vec<String> {
    [
        "--keys",
        "8",
        "--key-bytes",
        "8",
        "--value-bytes",
        "16",
        "--scan-limit",
        "3",
        "--scan-iterations",
        "2",
        "--batch-size",
        "3",
        "--seed",
        "7",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

fn invoke(path: &Path, arguments: &[String]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_hoke-bench"))
        .arg("--dir")
        .arg(path)
        .args(arguments)
        .output()
        .unwrap()
}

fn field<'a>(record: &'a str, name: &str) -> &'a str {
    let marker = format!("\"{name}\"");
    let (_, rest) = record.split_once(&marker).unwrap_or_else(|| {
        panic!("missing field {name}: {record}");
    });
    rest.trim_start()
        .strip_prefix(':')
        .expect("field separator")
        .trim_start()
}

fn number(record: &str, name: &str) -> u128 {
    let value = field(record, name);
    let end = value
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(value.len());
    value[..end].parse().expect("unsigned numeric field")
}

fn string<'a>(record: &'a str, name: &str) -> &'a str {
    field(record, name)
        .strip_prefix('"')
        .expect("string field")
        .split_once('"')
        .expect("string terminator")
        .0
}

fn object<'a>(record: &'a str, name: &str) -> &'a str {
    let value = field(record, name);
    assert!(value.starts_with('{'));
    let end = value.find('}').expect("flat snapshot object");
    &value[..=end]
}

fn samples(record: &str) -> Vec<u128> {
    let value = field(record, "samples_ns");
    let inner = value
        .strip_prefix('[')
        .expect("sample array")
        .split_once(']')
        .expect("sample terminator")
        .0;
    if inner.trim().is_empty() {
        Vec::new()
    } else {
        inner
            .split(',')
            .map(|sample| sample.trim().parse().expect("numeric sample"))
            .collect()
    }
}

fn directory_files(path: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    for entry in std::fs::read_dir(path).unwrap() {
        let entry = entry.unwrap();
        assert!(entry.file_type().unwrap().is_file());
        files.insert(entry.path(), std::fs::read(entry.path()).unwrap());
    }
    files
}

#[test]
fn smoke_reports_every_case_and_consistent_samples_and_disk_reclamation() {
    let directory = TempDirectory::new();
    let output = invoke(&directory.0, &small_arguments());
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let records: Vec<_> = stdout.lines().collect();
    assert_eq!(records.len(), 13, "{stdout}");
    assert_eq!(string(records[0], "kind"), "config");
    for (name, expected) in [
        ("schema", 1),
        ("keys", 8),
        ("key_bytes", 8),
        ("value_bytes", 16),
        ("scan_limit", 3),
        ("scan_iterations", 2),
        ("batch_size", 3),
        ("seed", 7),
    ] {
        assert_eq!(number(records[0], name), expected, "{name}");
    }
    for name in ["profile", "cache_policy", "durability"] {
        assert!(!string(records[0], name).is_empty());
    }
    let expected_calls = BTreeMap::from([
        ("put_sequential", 8),
        ("overwrite_random", 8),
        ("reopen_wal", 1),
        ("flush", 1),
        ("get_sst_random", 8),
        ("get_sst_missing", 8),
        ("scan_forward", 2),
        ("scan_reverse", 2),
        ("batch_put", 3),
        ("compact", 1),
        ("reopen_sst", 1),
        ("gc", 1),
    ]);
    let mut measurements = BTreeMap::new();
    for record in &records[1..] {
        assert!(record.starts_with('{') && record.ends_with('}'));
        assert_eq!(string(record, "kind"), "measurement");
        assert_eq!(number(record, "schema"), 1);
        let case = string(record, "case");
        assert!(measurements.insert(case, *record).is_none());
        let expected = *expected_calls.get(case).expect("known measurement case");
        assert_eq!(number(record, "calls"), expected, "{case}");
        let samples = samples(record);
        assert_eq!(samples.len() as u128, expected, "{case}");
        assert_eq!(samples.iter().sum::<u128>(), number(record, "elapsed_ns"));
        let _ = number(record, "items");
        for snapshot in ["before", "after"] {
            let snapshot = object(record, snapshot);
            for field in [
                "memtable_entries",
                "memtable_bytes",
                "table_count",
                "wal_bytes",
                "files",
                "bytes",
            ] {
                let _ = number(snapshot, field);
            }
        }
    }
    assert_eq!(measurements.len(), expected_calls.len());
    for case in [
        "put_sequential",
        "overwrite_random",
        "get_sst_random",
        "batch_put",
    ] {
        assert_eq!(number(measurements[case], "items"), 8, "{case}");
    }
    for case in ["scan_forward", "scan_reverse"] {
        assert_eq!(number(measurements[case], "items"), 6, "{case}");
    }
    let flushed = object(measurements["flush"], "after");
    assert_eq!(number(flushed, "memtable_entries"), 0);
    assert_eq!(number(flushed, "wal_bytes"), 16);
    assert_eq!(number(flushed, "table_count"), 1);
    for case in ["compact", "gc"] {
        let record = measurements[case];
        let before = object(record, "before");
        let after = object(record, "after");
        assert!(number(after, "files") < number(before, "files"), "{case}");
        assert!(number(after, "bytes") < number(before, "bytes"), "{case}");
    }
}

#[test]
fn invalid_arguments_leave_existing_empty_directory_untouched() {
    let directory = TempDirectory::new();
    let mut cases = Vec::new();
    for (option, value) in [
        ("--keys", "0"),
        ("--keys", "100001"),
        ("--key-bytes", "7"),
        ("--key-bytes", "65537"),
        ("--value-bytes", "16777217"),
        ("--scan-limit", "0"),
        ("--scan-limit", "9"),
        ("--scan-iterations", "0"),
        ("--scan-iterations", "10001"),
        ("--batch-size", "0"),
        ("--batch-size", "1025"),
        ("--seed", "18446744073709551616"),
        ("--keys", "-1"),
        ("--keys", "+8"),
        ("--keys", "1.5"),
        ("--keys", "999999999999999999999999999999999999999"),
    ] {
        let mut arguments = small_arguments();
        let index = arguments.iter().position(|arg| arg == option).unwrap();
        arguments[index + 1] = String::from(value);
        cases.push(arguments);
    }
    for suffix in [
        vec!["--unknown"],
        vec!["--keys", "8"],
        vec!["--seed", "7"],
        vec!["--value-bytes"],
        vec!["unexpected"],
    ] {
        let mut arguments = small_arguments();
        arguments.extend(suffix.into_iter().map(String::from));
        cases.push(arguments);
    }

    for (keys, value_bytes, batch_size) in [
        ("4000", "20000", "3"),
        ("8", "16777216", "3"),
        ("4095", "16", "1"),
    ] {
        let mut arguments = small_arguments();
        for (option, value) in [
            ("--keys", keys),
            ("--value-bytes", value_bytes),
            ("--batch-size", batch_size),
        ] {
            let index = arguments.iter().position(|arg| arg == option).unwrap();
            arguments[index + 1] = String::from(value);
        }
        cases.push(arguments);
    }
    for arguments in cases {
        let output = invoke(&directory.0, &arguments);
        assert!(!output.status.success(), "accepted {arguments:?}");
        assert!(
            output.stdout.is_empty(),
            "partial success for {arguments:?}"
        );
        assert!(
            !output.stderr.is_empty(),
            "missing diagnostic for {arguments:?}"
        );
        assert!(
            directory_files(&directory.0).is_empty(),
            "mutated for {arguments:?}"
        );
    }
}

#[test]
fn nonempty_directory_and_file_are_preserved_and_missing_directory_is_not_created() {
    let directory = TempDirectory::new();
    let sentinel = directory.0.join("user-data");
    std::fs::write(&sentinel, b"do not replace\0\xff").unwrap();
    let original = directory_files(&directory.0);
    for path in [&directory.0, &sentinel, &directory.0.join("missing")] {
        let output = invoke(path, &small_arguments());
        assert!(!output.status.success(), "accepted {path:?}");
        assert!(output.stdout.is_empty());
        assert_eq!(directory_files(&directory.0), original);
    }
}

#[test]
fn help_succeeds_without_database_and_missing_directory_argument_fails() {
    let directory = TempDirectory::new();
    let output = Command::new(env!("CARGO_BIN_EXE_hoke-bench"))
        .arg("--help")
        .current_dir(&directory.0)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8(output.stdout).unwrap().contains("--dir"));
    for arguments in [vec![], vec!["--dir"]] {
        let output = Command::new(env!("CARGO_BIN_EXE_hoke-bench"))
            .args(arguments)
            .current_dir(&directory.0)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
    }
    assert!(directory_files(&directory.0).is_empty());
}

#[test]
fn smallest_dataset_accepts_empty_values_and_maximum_seed() {
    let directory = TempDirectory::new();
    let arguments: Vec<_> = [
        "--keys",
        "1",
        "--key-bytes",
        "8",
        "--value-bytes",
        "0",
        "--scan-limit",
        "1",
        "--scan-iterations",
        "1",
        "--batch-size",
        "1",
        "--seed",
        "18446744073709551615",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    let output = invoke(&directory.0, &arguments);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(stdout.lines().count(), 13);
    let config = stdout.lines().next().unwrap();
    assert_eq!(number(config, "value_bytes"), 0);
    assert_eq!(number(config, "seed"), u128::from(u64::MAX));
}
