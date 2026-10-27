"""Contract tests for the native benchmark runner (no timing thresholds)."""
import argparse
import copy
import io
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import bench


CONFIG = dict(bench.DEFAULTS, keys=20, scan_limit=10, scan_iterations=2, batch_size=8)


def raw_run(config=None):
    config = config or CONFIG
    header = dict(config, kind="config", schema=1, profile="release",
                  cache_policy="warm_no_eviction", durability="sync_all")
    rows = [header]
    for case in bench.CASES:
        calls, items = bench.expected_counts(case, config)
        rows.append(dict(schema=1, kind="measurement", case=case, calls=calls,
                         items=items if items is not None else config["keys"],
                         samples_ns=list(range(1, calls + 1)), elapsed_ns=calls * (calls + 1) // 2,
                         before=dict.fromkeys(bench.SNAPSHOT_FIELDS, 0),
                         after=dict.fromkeys(bench.SNAPSHOT_FIELDS, 0)))
        rows[-1]["before"]["wal_bytes"] = 16
        rows[-1]["after"]["wal_bytes"] = 16
    return rows


def encode(rows):
    return "\n".join(json.dumps(row) for row in rows) + "\n"


class ParsingTests(unittest.TestCase):
    def test_complete_run(self):
        self.assertEqual(set(bench.parse_run(encode(raw_run()), CONFIG)), set(bench.CASES))

    def test_partial_duplicate_unknown_and_malformed(self):
        rows = raw_run()
        cases = [rows[:-1], rows + [rows[-1]], [rows[0], rows[1]] + rows[1:-1]]
        unknown = copy.deepcopy(rows)
        unknown[1]["case"] = "unknown"
        cases.append(unknown)
        for records in cases:
            with self.subTest(records=records), self.assertRaises(ValueError):
                bench.parse_run(encode(records), CONFIG)
        with self.assertRaises(ValueError):
            bench.parse_run("{broken", CONFIG)

    def test_invalid_fields(self):
        mutations = [
            (0, "seed", True), (0, "keys", CONFIG["keys"] + 1),
            (0, "cache_policy", "cold"), (0, "schema", True),
            (1, "schema", 2), (1, "calls", True), (1, "items", -1),
            (1, "samples_ns", [float("nan")] * CONFIG["keys"]),
            (1, "elapsed_ns", 1), (1, "calls", CONFIG["keys"] - 1),
            (1, "samples_ns", [0]), (1, "elapsed_ns", 2 ** 65),
        ]
        for index, key, value in mutations:
            with self.subTest(key=key, value=value):
                rows = raw_run()
                rows[index][key] = value
                with self.assertRaises(ValueError):
                    bench.parse_run(encode(rows), CONFIG)

    def test_snapshot_validation(self):
        for field, value in (("files", True), ("wal_bytes", 15), ("bytes", -1), ("memtable_entries", CONFIG["keys"] + 1)):
            rows = raw_run()
            rows[1]["before"][field] = value
            with self.subTest(field=field), self.assertRaises(ValueError):
                bench.parse_run(encode(rows), CONFIG)
        rows[1]["before"] = {}
        with self.assertRaises(ValueError):
            bench.parse_run(encode(rows), CONFIG)

    def test_workload_counts(self):
        for case in ("batch_put", "scan_forward", "get_sst_missing"):
            rows = raw_run()
            row = next(row for row in rows if row.get("case") == case)
            row["items"] += 1
            with self.subTest(case=case), self.assertRaises(ValueError):
                bench.parse_run(encode(rows), CONFIG)


class SummaryTests(unittest.TestCase):
    def test_known_quantiles_and_rates(self):
        run = bench.parse_run(encode(raw_run()), CONFIG)
        summary = bench.summarise([run, run])
        row = summary["cases"][0]
        self.assertEqual(row["sample_count"], 40)
        self.assertEqual(row["latency_ns"]["p50"], 10.5)
        self.assertAlmostEqual(row["latency_ns"]["p95"], 19.05)
        self.assertIsNone(row["latency_ns"]["p99"])
        self.assertEqual(row["calls_per_run"], [20, 20])
        self.assertEqual(row["elapsed_ns"], {"min": 210, "median": 210, "max": 210})
        self.assertAlmostEqual(row["calls_per_second"]["median"], 20e9 / 210)

    def test_thresholds_zero_and_batch_units(self):
        run = bench.parse_run(encode(raw_run()), CONFIG)
        for record in run.values():
            record["samples_ns"] = [0] * record["calls"]
            record["elapsed_ns"] = 0
        summary = bench.summarise([run] * 5)
        first = summary["cases"][0]
        self.assertEqual(first["latency_ns"]["p99"], 0)
        self.assertIsNone(first["calls_per_second"]["median"])
        batch = next(row for row in summary["cases"] if row["case"] == "batch_put")
        self.assertEqual(batch["calls_per_run"], [3] * 5)
        self.assertEqual(batch["items_per_run"], [20] * 5)
        self.assertIsNone(batch["latency_ns"]["p95"])
        with self.assertRaises(ValueError):
            bench.summarise([])


class RunnerTests(unittest.TestCase):
    def args(self, root):
        return argparse.Namespace(**CONFIG, runs=2, workdir=str(root), output=str(root / "results"), storage_label="test")

    def test_bad_arguments_leave_no_output(self):
        for field, value in (("runs", 0), ("keys", 0), ("seed", -1), ("key_bytes", 7), ("scan_limit", 21)):
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                args = self.args(root)
                setattr(args, field, value)
                with self.assertRaises(ValueError):
                    bench.execute(args)
                self.assertFalse(Path(args.output).exists())
        with self.assertRaises(ValueError):
            bench.validate_config(dict(CONFIG, keys=100000, value_bytes=1000))
        with self.assertRaises(ValueError):
            bench.validate_config(dict(CONFIG, keys=4095, batch_size=1))

    def test_existing_output_preserved(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            args = self.args(root)
            Path(args.output).mkdir()
            marker = Path(args.output) / "user-data"
            marker.write_text("keep")
            with self.assertRaises(ValueError):
                bench.execute(args)
            self.assertEqual(marker.read_text(), "keep")

    def run_mocked(self, mode):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            args = self.args(root)
            calls = []
            def run(command, **kwargs):
                calls.append(command)
                if "build" in command:
                    if mode == "build-failure":
                        raise subprocess.CalledProcessError(1, command)
                else:
                    if mode == "child-failure":
                        kwargs["stdout"].write("partial\n")
                        raise subprocess.CalledProcessError(2, command)
                    rows = raw_run()
                    if mode == "bad-results":
                        rows = rows[:-1]
                    kwargs["stdout"].write(encode(rows))
                return subprocess.CompletedProcess(command, 0)
            with patch.object(bench, "environment", return_value={}), patch.object(bench.subprocess, "run", side_effect=run), patch.object(bench.subprocess, "check_output", return_value=json.dumps({"target_directory": str(root / "custom-target")})), patch("sys.stdout", new=io.StringIO()), patch("sys.stderr", new=io.StringIO()):
                if mode == "success":
                    bench.execute(args)
                else:
                    with self.assertRaises((ValueError, subprocess.SubprocessError)):
                        bench.execute(args)
            output = Path(args.output)
            metadata = json.loads((output / "metadata.json").read_text())
            self.assertEqual(metadata["state"], "complete" if mode == "success" else "failed")
            self.assertEqual((output / "summary.json").exists(), mode == "success")
            self.assertEqual(len([call for call in calls if "build" in call]), 1)
            if mode == "success":
                self.assertEqual(len(list(output.glob("run-*.jsonl"))), 2)
                self.assertTrue(str(calls[-1][0]).startswith(str(root / "custom-target")))
            if mode == "child-failure":
                self.assertEqual((output / "run-001.jsonl").read_text(), "partial\n")
            self.assertEqual(sorted(path.name for path in root.iterdir()), ["results"])

    def test_complete_state_and_custom_target(self):
        self.run_mocked("success")

    def test_failed_state_retains_raw_and_omits_summary(self):
        for mode in ("build-failure", "child-failure", "bad-results"):
            with self.subTest(mode=mode):
                self.run_mocked(mode)


if __name__ == "__main__":
    unittest.main()
