#!/usr/bin/env python3
"""Reproducible native benchmarks; only Python's standard library is required."""

import argparse
import datetime
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import statistics
import subprocess
import sys
import tempfile
import uuid

ROOT = Path(__file__).resolve().parents[1]
CASES = (
    "put_sequential", "overwrite_random", "reopen_wal", "flush",
    "get_sst_random", "get_sst_missing", "scan_forward", "scan_reverse",
    "batch_put", "compact", "reopen_sst", "gc",
)
DEFAULTS = dict(keys=1000, key_bytes=16, value_bytes=256, scan_limit=100,
                scan_iterations=50, batch_size=100, seed=1)
SNAPSHOT_FIELDS = ("memtable_entries", "memtable_bytes", "table_count", "wal_bytes", "files", "bytes")


def integer(value, name, minimum=0, maximum=(1 << 64) - 1):
    if type(value) is not int or value < minimum or (maximum is not None and value > maximum):
        raise ValueError(f"{name}: invalid integer {value!r}")
    return value


def validate_config(config):
    for name, low, high in (
        ("keys", 1, 100000), ("key_bytes", 8, 65536),
        ("value_bytes", 0, 16777216), ("scan_limit", 1, config["keys"]),
        ("scan_iterations", 1, 10000), ("batch_size", 1, 1024),
        ("seed", 0, (1 << 64) - 1),
    ):
        integer(config[name], name, low, high)
    if config["keys"] * (3 * config["key_bytes"] + 2 * config["value_bytes"] + 16) > 128 * 1024 * 1024:
        raise ValueError("prepared key/value data exceeds 128 MiB")
    if min(config["keys"], config["batch_size"]) * (config["key_bytes"] + config["value_bytes"]) > 32 * 1024 * 1024:
        raise ValueError("batch raw data exceeds 32 MiB")
    if (config["keys"] + config["batch_size"] - 1) // config["batch_size"] + 2 > 4096:
        raise ValueError("batch workload exceeds 4096 SSTables including compaction preparation")


def expected_counts(case, config):
    keys = config["keys"]
    if case in ("put_sequential", "overwrite_random", "get_sst_random", "get_sst_missing"):
        return keys, keys
    if case in ("scan_forward", "scan_reverse"):
        return config["scan_iterations"], config["scan_iterations"] * config["scan_limit"]
    if case == "batch_put":
        return math.ceil(keys / config["batch_size"]), keys
    return 1, None


def parse_run(raw, config):
    """Reject partial, ambiguous or malformed replicas before summarising."""
    lines = raw.splitlines()
    if len(lines) != len(CASES) + 1 or any(not line.strip() for line in lines):
        raise ValueError("expected one config and twelve measurement records")
    records = [json.loads(line) for line in lines]
    header = records[0]
    if not isinstance(header, dict) or header.get("kind") != "config" or type(header.get("schema")) is not int or header["schema"] != 1:
        raise ValueError("invalid config schema")
    for key, value in {**config, "profile": "release", "cache_policy": "warm_no_eviction", "durability": "sync_all"}.items():
        if header.get(key) != value or (type(value) is int and type(header.get(key)) is not int):
            raise ValueError(f"config mismatch: {key}")
    result = {}
    for record in records[1:]:
        if not isinstance(record, dict) or record.get("kind") != "measurement" or type(record.get("schema")) is not int or record["schema"] != 1:
            raise ValueError("invalid measurement schema")
        case = record.get("case")
        if case not in CASES or case in result:
            raise ValueError(f"unknown or duplicate case: {case}")
        calls = integer(record.get("calls"), "calls", 1)
        items = integer(record.get("items"), "items")
        elapsed = integer(record.get("elapsed_ns"), "elapsed_ns")
        samples = record.get("samples_ns")
        if not isinstance(samples, list) or len(samples) != calls:
            raise ValueError(f"{case}: samples count differs from calls")
        for sample in samples:
            integer(sample, "sample")
        if sum(samples) != elapsed:
            raise ValueError(f"{case}: elapsed_ns differs from sum(samples_ns)")
        expected_calls, expected_items = expected_counts(case, config)
        if calls != expected_calls or (expected_items is not None and items != expected_items):
            raise ValueError(f"{case}: workload counts differ from config")
        for name in ("before", "after"):
            snapshot = record.get(name)
            if not isinstance(snapshot, dict) or set(snapshot) != set(SNAPSHOT_FIELDS):
                raise ValueError(f"{case}: invalid {name} snapshot")
            for field in SNAPSHOT_FIELDS:
                integer(snapshot[field], field)
            if snapshot["memtable_entries"] > config["keys"] or snapshot["wal_bytes"] < 16:
                raise ValueError(f"{case}: impossible {name} snapshot")
        result[case] = record
    if set(result) != set(CASES):
        raise ValueError("missing measurement cases")
    return result


def quantile(samples, fraction):
    """Linear interpolation (same definition as common inclusive quantiles)."""
    ordered = sorted(samples)
    position = (len(ordered) - 1) * fraction
    low = math.floor(position)
    high = math.ceil(position)
    return ordered[low] + (ordered[high] - ordered[low]) * (position - low)


def spread(values):
    if any(value is None for value in values):
        return {"min": None, "median": None, "max": None}
    return {"min": min(values), "median": statistics.median(values), "max": max(values)}


def summarise(runs):
    if not runs:
        raise ValueError("no complete runs")
    result = {"schema": 1, "runs": len(runs), "cases": []}
    for case in CASES:
        rows = [run[case] for run in runs]
        samples = [sample for row in rows for sample in row["samples_ns"]]
        result["cases"].append({
            "case": case, "calls_per_run": [row["calls"] for row in rows],
            "items_per_run": [row["items"] for row in rows], "sample_count": len(samples),
            "latency_ns": {"p50": quantile(samples, .5),
                           "p95": quantile(samples, .95) if len(samples) >= 20 else None,
                           "p99": quantile(samples, .99) if len(samples) >= 100 else None},
            "elapsed_ns": spread([row["elapsed_ns"] for row in rows]),
            "calls_per_second": spread([row["calls"] * 1e9 / row["elapsed_ns"] if row["elapsed_ns"] else None for row in rows]),
            "items_per_second": spread([row["items"] * 1e9 / row["elapsed_ns"] if row["elapsed_ns"] else None for row in rows]),
        })
    return result


def command_output(command):
    try:
        proc = subprocess.run(command, cwd=ROOT, capture_output=True, text=True, check=False)
        return proc.stdout.strip() if proc.returncode == 0 else None
    except OSError:
        return None


def source_fingerprint():
    digest = hashlib.sha256()
    paths = [ROOT / name for name in ("Cargo.toml", "Cargo.lock", "scripts/bench.py")]
    paths.extend((ROOT / "crates").rglob("*.rs"))
    paths.extend((ROOT / "crates").rglob("Cargo.toml"))
    for path in sorted(paths):
        if path.is_file():
            digest.update(str(path.relative_to(ROOT)).encode())
            digest.update(b"\0")
            digest.update(path.read_bytes())
    return digest.hexdigest()


def write_json(path, value):
    temporary = path.with_suffix(".tmp")
    temporary.write_text(json.dumps(value, indent=2, allow_nan=False) + "\n", encoding="utf-8")
    temporary.replace(path)


def environment(workdir):
    cpu = platform.processor()
    try:
        for line in Path("/proc/cpuinfo").read_text().splitlines():
            if line.startswith("model name"):
                cpu = line.split(":", 1)[1].strip()
                break
    except OSError:
        pass
    return {"utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
            "os": platform.platform(), "cpu": cpu, "cpu_count": os.cpu_count(),
            "python": sys.version, "rustc": command_output(["rustc", "-Vv"]),
            "cargo": command_output(["cargo", "-V"]),
            "git_head": command_output(["git", "rev-parse", "HEAD"]),
            "git_status": command_output(["git", "status", "--porcelain"]),
            "source_sha256": source_fingerprint(), "workdir": str(workdir),
            "filesystem_findmnt": command_output(["findmnt", "--target", str(workdir), "--json"]),
            "filesystem_df": command_output(["df", "-PT", str(workdir)])}


def print_summary(summary):
    print(f"{'case':<22} {'p50 us':>12} {'median ms':>12} {'calls/s':>12} {'items/s':>12}")
    for row in summary["cases"]:
        def rate(name):
            value = row[name]["median"]
            return f"{value:.1f}" if value is not None else "n/a"
        print(f"{row['case']:<22} {row['latency_ns']['p50'] / 1000:>12.2f} {row['elapsed_ns']['median'] / 1e6:>12.2f} {rate('calls_per_second'):>12} {rate('items_per_second'):>12}")


def execute(args):
    config = {name: getattr(args, name) for name in DEFAULTS}
    validate_config(config)
    integer(args.runs, "runs", 1, 20)
    workdir = Path(args.workdir).expanduser().resolve()
    if not workdir.is_dir():
        raise ValueError("--workdir must be an existing directory")
    output = Path(args.output).expanduser().absolute() if args.output else ROOT / "bench-results" / (datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%SZ-") + uuid.uuid4().hex[:8])
    if output.exists() or output.is_symlink():
        raise ValueError("--output must be a new directory")
    output.mkdir(parents=True, exist_ok=False)
    metadata = {"schema": 1, "state": "running", "config": config, "runs": args.runs,
                "storage_label": args.storage_label, "environment": environment(workdir)}
    write_json(output / "metadata.json", metadata)
    try:
        with (output / "build.stdout").open("w") as stdout, (output / "build.stderr").open("w") as stderr:
            subprocess.run(["cargo", "build", "--release", "--locked", "--bin", "hoke-bench", "-p", "hoke-core"], cwd=ROOT, stdout=stdout, stderr=stderr, check=True)
        cargo_metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--format-version", "1", "--no-deps", "--locked"], cwd=ROOT, text=True))
        binary = Path(cargo_metadata["target_directory"]) / "release" / ("hoke-bench.exe" if os.name == "nt" else "hoke-bench")
        runs = []
        for index in range(1, args.runs + 1):
            command = [str(binary)]
            for name, value in config.items():
                command.extend(["--" + name.replace("_", "-"), str(value)])
            with tempfile.TemporaryDirectory(prefix="hoke-bench-", dir=workdir) as directory:
                command.extend(["--dir", directory])
                raw_path = output / f"run-{index:03d}.jsonl"
                with raw_path.open("w") as stdout, (output / f"run-{index:03d}.stderr").open("w") as stderr:
                    subprocess.run(command, cwd=ROOT, stdout=stdout, stderr=stderr, check=True)
                runs.append(parse_run(raw_path.read_text(), config))
        summary = summarise(runs)
        write_json(output / "summary.json", summary)
        metadata["state"] = "complete"
        metadata["completed_utc"] = datetime.datetime.now(datetime.timezone.utc).isoformat()
        write_json(output / "metadata.json", metadata)
        print_summary(summary)
        print(f"Results: {output}")
        return output
    except BaseException as error:
        (output / "summary.json").unlink(missing_ok=True)
        metadata["state"] = "failed"
        metadata["error"] = f"{type(error).__name__}: {error}"
        print(f"Failed results: {output}; build diagnostics: {output / 'build.stderr'}", file=sys.stderr)
        write_json(output / "metadata.json", metadata)
        raise


def parser():
    result = argparse.ArgumentParser(description=__doc__)
    result.add_argument("--runs", type=int, default=3)
    for name, default in DEFAULTS.items():
        result.add_argument("--" + name.replace("_", "-"), type=int, default=default)
    result.add_argument("--workdir", default=tempfile.gettempdir())
    result.add_argument("--output")
    result.add_argument("--storage-label")
    return result


def main():
    try:
        execute(parser().parse_args())
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        print(f"benchmark failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
