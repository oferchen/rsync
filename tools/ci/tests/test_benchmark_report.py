#!/usr/bin/env python3
"""Regression tests for the release benchmark report's peak-RSS columns.

Why this exists: benchmark_rss() records `peak_rss_kb` for every mode, not
just the `memory` mode, but the report published only elapsed time. A mode
could therefore get 3x faster while using 3x the memory and the release
notes would show only the win. The report now prints both.

The trap being guarded is the one benchmark_chart.py already had to fix
once: describing a BYTES metric in DURATION language. A peak-RSS ratio
rendered as "slower 3.11x" is meaningless -- memory is not fast -- so these
tests assert the wording, not just the presence of a number. They also
assert the magnitude is derived from `peak_rss_kb` and not from `mean`,
because a report that still read `mean` but formatted it as bytes would
satisfy a suffix-only check while being exactly as wrong as before.
"""

from __future__ import annotations

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[3]
REPORT = REPO_ROOT / ".github" / "scripts" / "benchmark_report.py"


def render(results: dict) -> str:
    """Run benchmark_report.py against `results` and return its markdown."""
    with tempfile.TemporaryDirectory() as tmp:
        (Path(tmp) / "benchmark_results.json").write_text(json.dumps(results))
        proc = subprocess.run(
            [sys.executable, str(REPORT)],
            cwd=tmp,
            capture_output=True,
            text=True,
        )
    if proc.returncode != 0:
        raise AssertionError(f"report failed ({proc.returncode}):\n{proc.stderr}")
    return proc.stdout


def results_with(upstream: dict, oc_rsync: dict, ratio: float = 0.5) -> dict:
    """A minimal single-mode result set, so each test states only what it varies."""
    return {
        "upstream_version": "3.5.0",
        "test_data": {"size_mb": 512, "files": 2000},
        "summary": {
            "by_mode": {"local": ratio},
            "avg_ratio": ratio,
            "best_ratio": ratio,
            "worst_ratio": ratio,
        },
        "tests": [
            {
                "mode": "local",
                "name": "512MB local",
                "ratio": ratio,
                "upstream": upstream,
                "oc_rsync": oc_rsync,
            }
        ],
    }


class PeakRssColumns(unittest.TestCase):
    def test_rss_is_reported_for_a_timing_mode(self):
        """A duration mode must still publish the peak RSS it already measured."""
        out = render(
            results_with(
                {"mean": 2.986, "peak_rss_kb": 7924},
                {"mean": 0.925, "peak_rss_kb": 24680},
            )
        )
        self.assertIn("7.74 MiB", out)
        self.assertIn("24.10 MiB", out)

    def test_rss_magnitude_comes_from_peak_rss_kb_not_mean(self):
        """Guard the chart's original bug: reading `mean` and calling it bytes.

        `mean` here is 2.986/0.925; if either were formatted as a size the
        output would be a sub-KiB value, never the MiB figures above.
        """
        out = render(
            results_with(
                {"mean": 2.986, "peak_rss_kb": 7924},
                {"mean": 0.925, "peak_rss_kb": 24680},
            )
        )
        self.assertNotIn("0 KiB", out)
        self.assertIn("7.74 MiB", out)

    def test_higher_memory_is_not_described_as_slower(self):
        """A BYTES ratio must not borrow DURATION wording."""
        out = render(
            results_with(
                {"mean": 2.986, "peak_rss_kb": 7924},
                {"mean": 0.925, "peak_rss_kb": 24680},
            )
        )
        self.assertIn("higher 3.11x", out)
        self.assertNotIn("slower 3.11x", out)

    def test_lower_memory_reads_as_lower(self):
        out = render(
            results_with(
                {"mean": 1.0, "peak_rss_kb": 20000},
                {"mean": 1.0, "peak_rss_kb": 10000},
                ratio=1.0,
            )
        )
        self.assertIn("lower 0.50x", out)

    def test_memory_within_noise_reads_as_same(self):
        out = render(
            results_with(
                {"mean": 1.0, "peak_rss_kb": 10000},
                {"mean": 1.0, "peak_rss_kb": 10100},
                ratio=1.0,
            )
        )
        self.assertIn("~same 1.01x", out)

    def test_missing_rss_degrades_to_a_dash_and_keeps_the_row(self):
        """A run whose RSS could not be parsed must not invent a number.

        It must also not drop the row: the timing comparison is still valid
        and silently omitting it would understate the mode count.
        """
        out = render(results_with({"mean": 1.0}, {"mean": 0.9}, ratio=0.9))
        self.assertIn("512MB local", out)
        self.assertIn("| - | - | - |", out)


def dual_baseline_results() -> dict:
    """A two-baseline result set whose numbers cannot be confused.

    Every magnitude is distinct and the legacy single-baseline `upstream` /
    `ratio` fields deliberately carry the *primary* baseline's values, so a
    renderer that ignored the new `upstreams` / `ratios` keys and fell back to
    the legacy pair would render the 3.5.0 figures twice -- visible as the
    3.4.4 numbers going missing rather than as a crash.
    """
    up_350 = {"mean": 4.000, "min": 3.9, "max": 4.1, "spread_pct": 5.0,
              "peak_rss_kb": 8192, "corpus_mibps": 25.0}
    up_344 = {"mean": 2.000, "min": 1.9, "max": 2.1, "spread_pct": 10.0,
              "peak_rss_kb": 4096, "corpus_mibps": 50.0}
    oc = {"mean": 1.000, "min": 0.9, "max": 1.1, "spread_pct": 20.0,
          "peak_rss_kb": 16384, "corpus_mibps": 100.0}
    return {
        "upstream_version": "3.5.0",
        "oc_rsync_version": "0.6.4",
        "oc_rsync_wire_compat_version": "3.4.4",
        "baselines": [
            {"label": "3.5.0", "version": "3.5.0", "path": "/a", "primary": True},
            {"label": "3.4.4", "version": "3.4.4", "path": "/b", "primary": False},
        ],
        "environment": {
            "platform": "linux",
            "kernel_release": "5.4.0-generic",
            "machine": "x86_64",
            "cpu_count": 4,
            "io_uring_send_zc": {
                "supported": False,
                "kernel_release": "5.4.0-generic",
                "detail": "below the 6.0 floor",
            },
            "oc_rsync_send_zc_dispatch": "compiled out",
        },
        "test_data": {"size_mb": 100, "files": 1000},
        "summary": {
            "by_mode": {"local": 0.25},
            "avg_ratio": 0.25,
            "best_ratio": 0.25,
            "worst_ratio": 0.25,
            "by_baseline": {
                "3.5.0": {"avg_ratio": 0.25, "best_ratio": 0.25,
                          "worst_ratio": 0.25, "by_mode": {"local": 0.25}},
                "3.4.4": {"avg_ratio": 0.50, "best_ratio": 0.50,
                          "worst_ratio": 0.50, "by_mode": {"local": 0.50}},
            },
        },
        "tests": [
            {
                "id": "local_initial",
                "name": "Initial sync",
                "mode": "local",
                "upstreams": {"3.5.0": up_350, "3.4.4": up_344},
                "ratios": {"3.5.0": 0.25, "3.4.4": 0.50},
                "upstream": up_350,
                "oc_rsync": oc,
                "ratio": 0.25,
                "corpus_bytes": 100 * 1024 * 1024,
            }
        ],
    }


class DualBaselineColumns(unittest.TestCase):
    """Both upstream releases must reach the published table."""

    @classmethod
    def setUpClass(cls):
        cls.out = render(dual_baseline_results())

    def test_both_baselines_are_named_in_the_header(self):
        self.assertIn("rsync 3.5.0", self.out)
        self.assertIn("rsync 3.4.4", self.out)

    def test_each_baselines_own_timing_is_rendered(self):
        self.assertIn("4.000s", self.out)
        self.assertIn("2.000s", self.out)

    def test_the_secondary_ratio_comes_from_ratios_not_the_legacy_field(self):
        """0.50 exists only under `ratios`; 0.25 is the legacy `ratio`."""
        self.assertIn("faster 0.50x", self.out)
        self.assertIn("faster 0.25x", self.out)

    def test_the_summary_states_a_verdict_per_baseline(self):
        self.assertIn("vs rsync 3.5.0", self.out)
        self.assertIn("vs rsync 3.4.4", self.out)

    def test_spread_travels_with_the_median(self):
        self.assertIn("4.000s ±5%", self.out)
        self.assertIn("1.000s ±20%", self.out)

    def test_corpus_rate_is_published(self):
        self.assertIn("MiB/s", self.out)
        self.assertIn("100", self.out)

    def test_oc_rsync_is_labelled_with_its_own_release(self):
        """The published v0.6.4 results said `oc_rsync_version = '3.4.4'`."""
        self.assertIn("oc-rsync 0.6.4", self.out)
        self.assertNotIn("oc-rsync 3.4.4", self.out)

    def test_wire_compatibility_is_stated_as_a_separate_fact(self):
        self.assertIn("wire-compatible with rsync 3.4.4", self.out)

    def test_the_environment_the_numbers_came_from_is_stated(self):
        self.assertIn("5.4.0-generic", self.out)
        self.assertIn("x86_64", self.out)

    def test_a_kernel_without_send_zc_is_called_out(self):
        """Numbers from such a kernel say nothing about the zero-copy path."""
        self.assertIn("SEND_ZC UNAVAILABLE", self.out)


class FailedCells(unittest.TestCase):
    """A cell whose binary errored out must not read as a performance result."""

    def _results(self):
        data = dual_baseline_results()
        t = data["tests"][0]
        t["id"] = "compress_zstd_initial"
        t["failed_series"] = ["3.5.0"]
        # The harness leaves such a row out of the averages, so the summary
        # names it rather than silently dropping it.
        data["summary"]["excluded_tests"] = ["compress_zstd_initial"]
        return data

    def test_the_row_says_which_side_failed(self):
        out = render(self._results())
        self.assertIn("failed: 3.5.0", out)

    def test_the_summary_names_what_it_excluded(self):
        out = render(self._results())
        self.assertIn("compress_zstd_initial", out)
        self.assertIn("excluded from these averages", out)

    def test_a_sound_run_carries_no_exclusion_notice(self):
        out = render(dual_baseline_results())
        self.assertNotIn("excluded from these averages", out)
        self.assertNotIn("failed:", out)


class UpstreamVersionLabel(unittest.TestCase):
    def test_version_label_comes_from_the_measured_binary(self):
        """The column header must name the build actually benchmarked.

        benchmark.py probes `<binary> --version`, so a hardcoded header would
        silently mislabel the comparison whenever the pinned oracle moves.
        """
        results = results_with(
            {"mean": 1.0, "peak_rss_kb": 1000},
            {"mean": 1.0, "peak_rss_kb": 1000},
            ratio=1.0,
        )
        results["upstream_version"] = "9.9.9"
        out = render(results)
        self.assertIn("rsync 9.9.9", out)
        self.assertNotIn("rsync 3.5.0", out)


if __name__ == "__main__":
    unittest.main(verbosity=2)
