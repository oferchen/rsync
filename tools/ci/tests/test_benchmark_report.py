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
