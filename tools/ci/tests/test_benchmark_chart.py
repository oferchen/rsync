#!/usr/bin/env python3
"""Regression tests for the release benchmark chart and report renderers.

Why this exists: benchmark.py measures two different things.  Most modes
measure elapsed time, but the `memory` mode measures peak resident set size
and reports it under a separate `peak_rss_kb` key, leaving `mean` holding
the run's duration.  The chart read `mean` for every mode, so the published
"Memory Usage" row plotted durations, labelled them in milliseconds, and
annotated them with a time ratio -- advertising a speed-up where oc-rsync
was in fact using several times more memory than upstream.  Nothing caught
it because nothing rendered the chart outside CI.

These tests therefore assert on the rendered SVG text rather than on
internal helpers, and they assert magnitudes derived from `peak_rss_kb`
rather than merely the presence of a unit suffix: a chart that still read
`mean` but formatted it as bytes would satisfy a suffix-only check while
being just as wrong.
"""

from __future__ import annotations

import importlib.util
import json
import re
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[3]
SCRIPTS = REPO_ROOT / ".github" / "scripts"


def load_script(name: str):
    """Import one of the .github/scripts modules by path.

    They are standalone CI scripts rather than an importable package, so
    they are loaded explicitly instead of through sys.path.
    """
    spec = importlib.util.spec_from_file_location(name, SCRIPTS / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    # Registered before execution so the dataclasses in the module can
    # resolve their own postponed annotations.
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


bc = load_script("benchmark_chart")

# Synthetic results with hand-picked magnitudes that make a metric mix-up
# unmistakable:
#   - memory RSS is 8 MiB vs 64 MiB, an 8.0x regression;
#   - memory `mean` says the opposite (oc-rsync twice as quick), and the
#     JSON `ratio` (0.5) is that time ratio, exactly as benchmark.py emits;
#   - the durations are chosen so that leaking them into the memory row
#     would print recognisable strings ("500 ms", "250 ms").
MEMORY_UPSTREAM_RSS_KB = 8192
MEMORY_OC_RSS_KB = 65536
SYNTHETIC = {
    "upstream_version": "3.4.4",
    "test_data": {"size_mb": 100, "files": 1000},
    "summary": {"avg_ratio": 0.5, "best_ratio": 0.2, "worst_ratio": 1.0,
                "by_mode": {}},
    "tests": [
        {
            "id": "local_initial",
            "name": "Initial sync",
            "mode": "local",
            "upstream": {"mean": 2.0, "min": 2.0, "max": 2.0},
            "oc_rsync": {"mean": 0.4, "min": 0.4, "max": 0.4},
            "ratio": 0.2,
        },
        {
            "id": "memory_initial",
            "name": "Initial sync",
            "mode": "memory",
            "upstream": {
                "mean": 0.5, "min": 0.5, "max": 0.5,
                "peak_rss_kb": MEMORY_UPSTREAM_RSS_KB,
                "avg_rss_kb": MEMORY_UPSTREAM_RSS_KB,
            },
            "oc_rsync": {
                "mean": 0.25, "min": 0.25, "max": 0.25,
                "peak_rss_kb": MEMORY_OC_RSS_KB,
                "avg_rss_kb": MEMORY_OC_RSS_KB,
            },
            "ratio": 0.5,
        },
    ],
}

# Any label the duration formatter would emit.
DURATION_LABEL = re.compile(r"\d+\s*ms\b|\d+\.\d{2}s\b|\d+\.\d{3}s\b")
# Any label the byte formatter would emit.
BYTE_LABEL = re.compile(r"\d[\d.]*\s*(KiB|MiB|GiB)\b")


def texts(svg: str) -> list[str]:
    """Every rendered text run: bar labels, axis labels, hover titles."""
    return [
        (m.group(1) or m.group(2) or "").strip()
        for m in re.finditer(r"<title>([^<]*)</title>|>([^<>]*)</text>", svg)
    ]


def mode_section(svg: str, header: str, next_header: str | None) -> str:
    """The slice of the SVG between one mode header and the next."""
    start = svg.index(">" + header + "<")
    if next_header is None:
        return svg[start:]
    end = svg.find(">" + next_header + "<", start)
    return svg[start:end if end > 0 else len(svg)]


class ModeUnitDeclarations(unittest.TestCase):
    """Every mode must state its unit, so none can silently inherit one."""

    def test_every_ordered_mode_declares_a_unit(self):
        missing = [m for m in bc.MODE_ORDER if m not in bc.MODE_UNITS]
        self.assertEqual(
            missing, [],
            "a mode without a MODE_UNITS entry would fall back to whatever "
            "the caller assumes, which is how peak RSS became milliseconds",
        )

    def test_memory_mode_is_a_byte_metric(self):
        self.assertIs(bc.MODE_UNITS["memory"], bc.MetricUnit.BYTES)

    def test_transfer_modes_are_duration_metrics(self):
        for mode in ("local", "ssh_pull", "daemon_push", "sparse"):
            self.assertIs(bc.MODE_UNITS[mode], bc.MetricUnit.DURATION, mode)


class MetricExtraction(unittest.TestCase):
    """A byte mode must read peak RSS, never the timing keys."""

    def test_byte_unit_reads_peak_rss_not_mean(self):
        series = {"mean": 0.5, "peak_rss_kb": 8192}
        self.assertEqual(
            bc.metric_value(series, bc.MetricUnit.BYTES), 8192 * 1024
        )

    def test_duration_unit_reads_mean(self):
        series = {"mean": 0.5, "peak_rss_kb": 8192}
        self.assertEqual(bc.metric_value(series, bc.MetricUnit.DURATION), 0.5)

    def test_missing_rss_reports_zero_rather_than_aborting(self):
        # /usr/bin/time output can fail to parse; the chart still has to
        # render the other thirteen modes.
        self.assertEqual(bc.metric_value({"mean": 0.5}, bc.MetricUnit.BYTES), 0.0)


class Formatters(unittest.TestCase):
    """The two units must not share a formatter."""

    def test_time_formatting_is_unchanged(self):
        self.assertEqual(bc.fmt_time(0.042), "42 ms")
        self.assertEqual(bc.fmt_time(1.234), "1.23s")

    def test_bytes_use_binary_prefixes(self):
        self.assertEqual(bc.fmt_bytes(512 * 1024), "512 KiB")
        self.assertEqual(bc.fmt_bytes(8192 * 1024), "8.00 MiB")
        self.assertEqual(bc.fmt_bytes(1536 * 1024 * 1024), "1.50 GiB")

    def test_units_dispatch_to_distinct_formatters(self):
        value = 8192 * 1024
        self.assertNotEqual(
            bc.fmt_value(value, bc.MetricUnit.BYTES),
            bc.fmt_value(value, bc.MetricUnit.DURATION),
            "a single shared formatter is the defect under test",
        )

    def test_hover_text_keeps_the_measured_precision(self):
        self.assertEqual(bc.fmt_value_exact(2.986, bc.MetricUnit.DURATION), "2.986s")
        self.assertEqual(
            bc.fmt_value_exact(7924 * 1024, bc.MetricUnit.BYTES), "7924 KiB"
        )


class RatioWording(unittest.TestCase):
    """Spending more memory is not being slower."""

    def test_duration_ratio_reads_as_speed(self):
        self.assertEqual(
            bc.ratio_text(0.5, bc.MetricUnit.DURATION)[0], "2.0x faster"
        )
        self.assertEqual(
            bc.ratio_text(2.0, bc.MetricUnit.DURATION)[0], "2.0x slower"
        )

    def test_byte_ratio_reads_as_memory(self):
        self.assertEqual(
            bc.ratio_text(8.0, bc.MetricUnit.BYTES)[0], "8.0x more memory"
        )
        self.assertEqual(
            bc.ratio_text(0.5, bc.MetricUnit.BYTES)[0], "2.0x less memory"
        )

    def test_within_noise_is_neutral_in_both_units(self):
        for unit in (bc.MetricUnit.DURATION, bc.MetricUnit.BYTES):
            self.assertEqual(bc.ratio_text(1.0, unit)[0], "~same", unit)

    def test_missing_byte_ratio_does_not_claim_a_win(self):
        # The ">100x faster" fallback exists because benchmark.py rounds a
        # sub-millisecond time ratio to 0.00.  Peak RSS is measured in whole
        # kilobytes and has no such rounding floor, so a non-positive byte
        # ratio only ever means the measurement is missing.
        self.assertEqual(bc.ratio_text(0.0, bc.MetricUnit.BYTES)[0], "no data")
        self.assertEqual(
            bc.ratio_text(0.0, bc.MetricUnit.DURATION)[0], ">100x faster"
        )

    def test_byte_row_ratio_is_derived_from_plotted_values(self):
        # benchmark.py's `ratio` field is a time ratio for every mode --
        # benchmark_report.py renders it under a "Time Ratio" heading -- so a
        # byte row must compute its own.
        row = SYNTHETIC["tests"][1]
        self.assertEqual(
            bc.pair_ratio(row, bc.MetricUnit.BYTES, 8.0, 64.0), 8.0
        )
        self.assertEqual(
            bc.pair_ratio(row, bc.MetricUnit.DURATION, 8.0, 64.0), 0.5
        )


class AxisLabels(unittest.TestCase):
    """Axis labels are formatted at a different call site from bar labels."""

    def _grid_labels(self, unit, max_val):
        builder = bc.ChartBuilder(bc.CHART_WIDTH, 200)
        scale = bc.BAR_AREA_WIDTH / (max_val * 1.1)
        builder.add_grid(max_val, scale, 10, 100, unit)
        labels = texts(builder.render())
        self.assertTrue(labels, "grid produced no labels to assert on")
        return labels

    def test_duration_axis_is_labelled_in_time(self):
        labels = self._grid_labels(bc.MetricUnit.DURATION, 2.0)
        self.assertTrue(all(DURATION_LABEL.search(t) for t in labels), labels)
        self.assertFalse(any(BYTE_LABEL.search(t) for t in labels), labels)

    def test_byte_axis_is_labelled_in_bytes(self):
        labels = self._grid_labels(bc.MetricUnit.BYTES, 64.0 * 1024 * 1024)
        self.assertTrue(all(BYTE_LABEL.search(t) for t in labels), labels)
        self.assertFalse(
            any(DURATION_LABEL.search(t) for t in labels),
            f"axis reverted to the duration formatter: {labels}",
        )


class RenderedChart(unittest.TestCase):
    """End-to-end assertions on the SVG the release actually publishes."""

    @classmethod
    def setUpClass(cls):
        cls.svg = bc.generate_chart(SYNTHETIC)
        cls.memory = mode_section(cls.svg, "Memory Usage", None)
        cls.local = mode_section(cls.svg, "Local Copy", "Memory Usage")

    def test_memory_row_plots_the_peak_rss_magnitudes(self):
        # 8192 KiB and 65536 KiB, not the 0.5s / 0.25s durations beside them.
        self.assertIn("8.00 MiB", self.memory)
        self.assertIn("64.00 MiB", self.memory)

    def test_memory_row_carries_no_duration_label(self):
        offenders = [t for t in texts(self.memory) if DURATION_LABEL.search(t)]
        self.assertEqual(
            offenders, [],
            "peak RSS rendered through the duration formatter",
        )
        for leaked in ("500 ms", "250 ms"):
            self.assertNotIn(leaked, self.memory)

    def test_memory_row_reports_the_regression_not_a_speedup(self):
        self.assertIn("8.0x more memory", self.memory)
        self.assertNotIn("faster", self.memory)
        self.assertNotIn("slower", self.memory)

    def test_memory_hover_text_shows_the_measured_kilobytes(self):
        self.assertIn(f"{MEMORY_UPSTREAM_RSS_KB} KiB", self.memory)
        self.assertIn(f"{MEMORY_OC_RSS_KB} KiB", self.memory)

    def test_timing_row_is_still_rendered_as_time(self):
        self.assertIn("2.00s", self.local)
        self.assertIn("400 ms", self.local)
        self.assertIn("5.0x faster", self.local)

    def test_timing_row_carries_no_byte_label(self):
        offenders = [t for t in texts(self.local) if BYTE_LABEL.search(t)]
        self.assertEqual(offenders, [], "duration rendered through fmt_bytes")

    def test_chart_survives_missing_rss_measurements(self):
        data = json.loads(json.dumps(SYNTHETIC))
        for side in ("upstream", "oc_rsync"):
            data["tests"][1][side].pop("peak_rss_kb")
        section = mode_section(bc.generate_chart(data), "Memory Usage", None)
        self.assertIn("no data", section)


class RenderedReport(unittest.TestCase):
    """The markdown report already handled RSS correctly; pin that."""

    @classmethod
    def setUpClass(cls):
        with tempfile.TemporaryDirectory() as tmp:
            (Path(tmp) / "benchmark_results.json").write_text(
                json.dumps(SYNTHETIC)
            )
            cls.report = subprocess.run(
                [sys.executable, str(SCRIPTS / "benchmark_report.py")],
                cwd=tmp, capture_output=True, text=True, check=True,
            ).stdout

    def test_memory_table_reports_rss_columns(self):
        self.assertIn("8.0MB", self.report)
        self.assertIn("64.0MB", self.report)

    def test_memory_table_names_its_ratio_column_as_time(self):
        # The chart derives its own byte ratio precisely because this column
        # is, and remains, a time ratio.
        self.assertIn("Time Ratio", self.report)


if __name__ == "__main__":
    unittest.main(verbosity=2)
