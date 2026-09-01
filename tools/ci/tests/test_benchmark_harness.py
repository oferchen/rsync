#!/usr/bin/env python3
"""Regression tests for the release benchmark harness itself.

Three defects motivated these, and each is guarded by asserting the value a
correct implementation produces *and* a value only the broken one could:

1. The published `benchmark_results.json` for v0.6.4 recorded
   `oc_rsync_version = '3.4.4'` for a 0.6.4 build. `\\b(\\d+\\.\\d+\\.\\d+)\\b`
   cannot match inside `v0.6.4` -- there is no word boundary between `v` and
   `0` -- so the first number the pattern accepted was the *wire
   compatibility* version from the next banner line. Asserting only "the
   release is 0.6.4" would pass on a parser that read the right line by luck,
   so these tests also assert the compatibility version is reported, and
   separately.

2. An initial-transfer cell reset its destination once and then timed six
   runs, and `representative_secs` discards the first as the warm-up. The
   published "Initial sync" figure was therefore the median of five no-change
   runs. The guard is on *how many times* the reset hook fires.

3. Only the primary baseline existed. The guard is that a second baseline is
   parsed, ordered, and kept distinct from the first.
"""

from __future__ import annotations

import importlib.util
import os
import shutil
import stat
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[3]
SCRIPTS = REPO_ROOT / ".github" / "scripts"

# benchmark.py resolves its baselines at import time and exits when none are
# named, which is the behaviour CI depends on. Point it at a binary that
# certainly exists so the module can be imported and its helpers exercised.
os.environ.setdefault("UPSTREAM_RSYNC", "probe=/bin/echo")
sys.path.insert(0, str(SCRIPTS))


def load_script(name: str):
    spec = importlib.util.spec_from_file_location(name, SCRIPTS / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


bench = load_script("benchmark")
env = load_script("benchmark_env")


OC_BANNER = """\
oc-rsync v0.6.4 (revision #9d99155e1) protocol version 32
Compatible with rsync 3.4.4 wire protocol
Built in Rust 2024 for aarch64-linux
"""

OC_VV = '{"program": "oc-rsync", "version": "0.6.4", "protocol": "32.0"}'

UPSTREAM_BANNER = "rsync  version 3.5.0  protocol version 32\n"


def fake_binary(tmp: Path, name: str, banner: str, vv: str | None) -> str:
    """A stand-in binary that prints a fixed --version / -VV document."""
    path = tmp / name
    vv_body = f'cat <<"EOF"\n{vv}\nEOF' if vv else "exit 1"
    path.write_text(
        "#!/bin/sh\n"
        'if [ "$1" = "-VV" ]; then\n'
        f"{vv_body}\n"
        "else\n"
        f'cat <<"EOF"\n{banner}EOF\n'
        "fi\n"
    )
    path.chmod(path.stat().st_mode | stat.S_IXUSR)
    return str(path)


class OcRsyncVersionLabel(unittest.TestCase):
    """oc-rsync's release and its wire compatibility are two different facts."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.oc = fake_binary(
            Path(self.tmp.name), "oc-rsync", OC_BANNER, OC_VV
        )

    def tearDown(self):
        self.tmp.cleanup()

    def test_release_is_oc_rsyncs_own_version(self):
        release, _ = bench.oc_rsync_versions(self.oc)
        self.assertEqual(release, "0.6.4")
        self.assertNotEqual(
            release, "3.4.4", "the wire compatibility version was published "
            "as oc-rsync's release"
        )

    def test_wire_compatibility_is_reported_separately(self):
        _, compat = bench.oc_rsync_versions(self.oc)
        self.assertEqual(compat, "3.4.4")

    def test_banner_is_the_fallback_when_the_json_document_is_absent(self):
        """A build whose -VV cannot be parsed must still name its release."""
        oc = fake_binary(
            Path(self.tmp.name), "oc-rsync-nojson", OC_BANNER, None
        )
        release, compat = bench.oc_rsync_versions(oc)
        self.assertEqual(release, "0.6.4")
        self.assertEqual(compat, "3.4.4")

    def test_the_old_greedy_parser_would_have_failed_this_fixture(self):
        """Pin the mechanism, so the fixture cannot silently stop reproducing.

        If this ever returns 0.6.4, the banner no longer has the shape the
        bug needed and the tests above stop being regression tests.
        """
        self.assertEqual(bench.binary_version(self.oc), "3.4.4")

    def test_upstream_binaries_still_use_the_leading_number_rule(self):
        up = fake_binary(
            Path(self.tmp.name), "rsync", UPSTREAM_BANNER, None
        )
        self.assertEqual(bench.binary_version(up), "3.5.0")


class BaselineParsing(unittest.TestCase):
    def test_labelled_entries_keep_their_order(self):
        baselines = bench.parse_baselines(
            "3.5.0=/bin/echo,3.4.4=/bin/echo"
        )
        self.assertEqual([b.label for b in baselines], ["3.5.0", "3.4.4"])

    def test_paths_are_absolute_for_the_remote_end(self):
        """SSH cells pass the path to the remote shell, whose cwd is not ours."""
        baselines = bench.parse_baselines("x=./some/rsync")
        self.assertTrue(baselines[0].path.startswith("/"))

    def test_a_bare_path_takes_its_label_from_the_binary(self):
        with tempfile.TemporaryDirectory() as tmp:
            up = fake_binary(Path(tmp), "rsync", UPSTREAM_BANNER, None)
            baselines = bench.parse_baselines(up)
        self.assertEqual([b.label for b in baselines], ["3.5.0"])

    def test_a_bare_command_name_resolves_through_path(self):
        """`UPSTREAM_RSYNC=rsync` means the system rsync, not ./rsync."""
        baselines = bench.parse_baselines("echo")
        self.assertEqual(baselines[0].path, shutil.which("echo"))

    def test_duplicate_labels_are_refused(self):
        with self.assertRaises(SystemExit):
            bench.parse_baselines("3.5.0=/bin/echo,3.5.0=/bin/true")

    def test_an_empty_spec_yields_no_baselines(self):
        self.assertEqual(bench.parse_baselines("  "), [])

    def test_labels_become_filesystem_safe_directory_slugs(self):
        baseline = bench.parse_baselines("3.5.0-rc/1=/bin/echo")[0]
        self.assertEqual(baseline.slug, "3.5.0-rc_1")


class PerRunReset(unittest.TestCase):
    """An initial-transfer cell must start empty on every run, not just run 1."""

    def test_before_each_fires_once_per_run(self):
        calls = []
        bench.benchmark("true", runs=4, before_each=lambda: calls.append(1))
        self.assertEqual(
            len(calls), 4,
            "resetting once per cell leaves runs 2..n measuring a no-change "
            "sync, and the warm-up run that did the real work is discarded",
        )

    def test_no_hook_means_no_reset(self):
        result = bench.benchmark("true", runs=2)
        self.assertEqual(result["runs"], 2)

    def test_rss_runner_also_resets_per_run(self):
        if not os.access("/usr/bin/time", os.X_OK):
            self.skipTest("/usr/bin/time is not available")
        calls = []
        bench.benchmark_rss("true", runs=3, before_each=lambda: calls.append(1))
        self.assertEqual(len(calls), 3)


class Statistics(unittest.TestCase):
    def test_spread_is_the_min_to_max_range_over_the_median(self):
        # The warm-up run (1.0) is dropped, so the median is that of
        # [2.0, 3.0, 4.0] = 3.0; min/max still span every run, so the spread
        # is (4.0 - 1.0) / 3.0 = 100%.
        s = bench.stats([1.0, 2.0, 3.0, 4.0])
        self.assertEqual(s["mean"], 3.0)
        self.assertEqual(s["min"], 1.0)
        self.assertEqual(s["max"], 4.0)
        self.assertEqual(s["runs"], 4)
        self.assertAlmostEqual(s["spread_pct"], 100.0)

    def test_a_repeatable_cell_reports_no_spread(self):
        self.assertEqual(bench.stats([1.0, 1.0, 1.0])["spread_pct"], 0.0)

    def test_throughput_is_corpus_bytes_over_elapsed(self):
        series = {"mean": 2.0}
        bench.add_throughput(series, 1024 * 1024 * 100)
        self.assertEqual(series["corpus_mibps"], 50.0)

    def test_throughput_is_omitted_when_the_corpus_size_is_unknown(self):
        series = {"mean": 2.0}
        bench.add_throughput(series, None)
        self.assertNotIn("corpus_mibps", series)


class ComparisonRows(unittest.TestCase):
    def setUp(self):
        self.saved = bench.BASELINES, bench.PRIMARY
        bench.BASELINES = [
            bench.Baseline("3.5.0", "/bin/echo"),
            bench.Baseline("3.4.4", "/bin/true"),
        ]
        bench.PRIMARY = bench.BASELINES[0]

    def tearDown(self):
        bench.BASELINES, bench.PRIMARY = self.saved

    def _row(self, **kwargs):
        results = {"tests": []}
        seen = []

        def runner(cmd, runs=1, before_each=None):
            seen.append(cmd)
            if before_each is not None:
                before_each()
            return {"mean": float(len(seen)), "min": 1.0, "max": 1.0}

        row = bench.compare(
            results,
            "id",
            "name",
            "local",
            lambda b: f"up:{b.label}",
            lambda b: f"oc:{b.label}",
            runner=runner,
            **kwargs,
        )
        return row, seen

    def test_every_baseline_gets_its_own_upstream_series(self):
        row, seen = self._row()
        self.assertEqual(sorted(row["upstreams"]), ["3.4.4", "3.5.0"])
        self.assertIn("up:3.5.0", seen)
        self.assertIn("up:3.4.4", seen)

    def test_the_legacy_fields_describe_the_primary_baseline(self):
        row, _ = self._row()
        self.assertEqual(row["upstream"], row["upstreams"]["3.5.0"])
        self.assertEqual(row["ratio"], row["ratios"]["3.5.0"])

    def test_a_local_cell_times_oc_rsync_once(self):
        """The baseline is the other contestant, not the peer."""
        _, seen = self._row()
        self.assertEqual([c for c in seen if c.startswith("oc:")], ["oc:3.5.0"])

    def test_a_peer_cell_times_oc_rsync_against_each_baseline(self):
        row, seen = self._row(oc_per_baseline=True)
        self.assertEqual(
            [c for c in seen if c.startswith("oc:")], ["oc:3.5.0", "oc:3.4.4"]
        )
        self.assertEqual(sorted(row["oc_rsync_per_baseline"]), ["3.4.4", "3.5.0"])

    def test_ratios_differ_when_the_baselines_differ(self):
        row, _ = self._row()
        self.assertNotEqual(
            row["ratios"]["3.5.0"], row["ratios"]["3.4.4"],
            "both baselines were divided into the same upstream time",
        )


class FailedRuns(unittest.TestCase):
    """A command that errors out in a millisecond is not a fast command."""

    def test_a_nonzero_exit_is_counted(self):
        result = bench.benchmark("exit 4", runs=3)
        self.assertEqual(result["failures"], 3)
        self.assertTrue(bench.series_failed(result))

    def test_a_clean_run_records_no_failure_key(self):
        result = bench.benchmark("true", runs=2)
        self.assertNotIn("failures", result)
        self.assertFalse(bench.series_failed(result))

    def test_a_failing_baseline_marks_the_row(self):
        saved = bench.BASELINES, bench.PRIMARY
        bench.BASELINES = [bench.Baseline("3.5.0", "/bin/echo")]
        bench.PRIMARY = bench.BASELINES[0]
        try:
            results = {"tests": []}
            row = bench.compare(
                results, "compress_zstd", "zstd", "compression",
                lambda b: "exit 4",
                lambda b: "true",
                runs=2,
            )
        finally:
            bench.BASELINES, bench.PRIMARY = saved
        self.assertEqual(row["failed_series"], ["3.5.0"])

    def test_a_sound_row_carries_no_failure_marker(self):
        saved = bench.BASELINES, bench.PRIMARY
        bench.BASELINES = [bench.Baseline("3.5.0", "/bin/echo")]
        bench.PRIMARY = bench.BASELINES[0]
        try:
            row = bench.compare(
                {"tests": []}, "local", "local", "local",
                lambda b: "true",
                lambda b: "true",
                runs=2,
            )
        finally:
            bench.BASELINES, bench.PRIMARY = saved
        self.assertNotIn("failed_series", row)


class SendZcProbe(unittest.TestCase):
    def test_kernel_version_parsing(self):
        self.assertEqual(env.parse_kernel_version("7.0.0-29-generic"), (7, 0))
        self.assertEqual(env.parse_kernel_version("6.8.0-45-azure"), (6, 8))
        self.assertIsNone(env.parse_kernel_version("weird"))

    def test_probe_reports_the_opcode_it_asked_about(self):
        probe = env.probe_send_zc()
        self.assertEqual(probe["opcode"], 47)
        self.assertEqual(probe["kernel_floor"], "6.0")
        self.assertIsInstance(probe["supported"], bool)

    def test_support_requires_both_the_floor_and_the_opcode(self):
        """Neither half alone is enough; state both, and conjoin them."""
        probe = dict(env.probe_send_zc())
        if probe["supported"]:
            self.assertTrue(probe["meets_kernel_floor"])
            self.assertTrue(probe["opcode_advertised"])
        else:
            self.assertFalse(
                probe["meets_kernel_floor"] and probe["opcode_advertised"]
            )

    def test_an_unsupported_kernel_is_stated_loudly(self):
        verdict = env.send_zc_verdict(
            {
                "io_uring_send_zc": {
                    "supported": False,
                    "kernel_release": "5.4.0",
                    "detail": "below the 6.0 floor",
                }
            }
        )
        self.assertIn("SEND_ZC UNAVAILABLE", verdict)
        self.assertIn("No figure below is evidence", verdict)

    def test_a_supported_kernel_still_names_a_binary_that_cannot_use_it(self):
        verdict = env.send_zc_verdict(
            {
                "io_uring_send_zc": {
                    "supported": True,
                    "kernel_release": "7.0.0",
                    "detail": "op 47 advertised",
                },
                "oc_rsync_send_zc_dispatch": "compiled out",
            }
        )
        self.assertIn("advertises IORING_OP_SEND_ZC", verdict)
        self.assertIn("does not dispatch it", verdict)


class ModuleEntryPoint(unittest.TestCase):
    def test_benchmark_py_refuses_to_run_without_a_baseline(self):
        """Naming the version in two places is what let them drift apart."""
        environ = dict(os.environ, UPSTREAM_RSYNC="")
        proc = subprocess.run(
            [sys.executable, str(SCRIPTS / "benchmark.py")],
            capture_output=True, text=True, env=environ,
        )
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("UPSTREAM_RSYNC is not set", proc.stderr)

    def test_a_missing_baseline_binary_is_refused(self):
        environ = dict(os.environ, UPSTREAM_RSYNC="9.9.9=/nonexistent/rsync")
        proc = subprocess.run(
            [sys.executable, str(SCRIPTS / "benchmark.py")],
            capture_output=True, text=True, env=environ,
        )
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("not found", proc.stderr)


if __name__ == "__main__":
    unittest.main(verbosity=2)
