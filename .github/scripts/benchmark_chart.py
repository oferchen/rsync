#!/usr/bin/env python3
"""Generate an SVG benchmark chart from benchmark_results.json.

Pure Python -- no external dependencies.  Reads CI benchmark data and
produces a grouped horizontal bar chart comparing oc-rsync against upstream
rsync across all transfer modes.

Not every mode measures the same thing.  Most compare elapsed time, but the
`memory` mode compares peak resident set size, so each mode declares its
metric unit (`MODE_UNITS`) and that unit selects both the value read out of
the results JSON and the formatter used to label it.

Design patterns:
  - Builder (ChartBuilder) for incremental SVG construction
  - Data classes for typed, immutable layout geometry
  - Strategy for unit-driven value extraction, formatting and ratio wording
  - Strategy for adaptive text placement (inside vs outside bars)
  - Single Responsibility per function
"""

from __future__ import annotations

import argparse
import json
import math
import os
import sys
from dataclasses import dataclass
from enum import Enum
from html import escape

# ---------------------------------------------------------------------------
# Layout constants
# ---------------------------------------------------------------------------

CHART_WIDTH = 800
LEFT_MARGIN = 180
# Wide enough for the longest ratio annotation a multi-baseline row can
# produce ("vs 3.4.4 12.5x faster"), which would otherwise run off the
# right edge of the SVG.
RIGHT_MARGIN = 140
BAR_AREA_WIDTH = CHART_WIDTH - LEFT_MARGIN - RIGHT_MARGIN

TOP_MARGIN = 60
BOTTOM_MARGIN = 50

BAR_HEIGHT = 16
BAR_GAP = 4
GROUP_GAP = 12
MODE_HEADER_HEIGHT = 28
MODE_GAP = 16

MIN_BAR_WIDTH = 2
TEXT_INSIDE_THRESHOLD = 60

# Comparison thresholds on the oc_rsync / upstream ratio, in whichever unit
# the mode measures.  < FASTER: oc-rsync clearly better.  <= SAME_UPPER:
# within noise.  > SAME_UPPER: worse.
RATIO_FASTER_BELOW = 0.95
RATIO_SAME_UPPER = 1.05

# ---------------------------------------------------------------------------
# Colors
# ---------------------------------------------------------------------------

COLOR_UPSTREAM = "#6e7681"
COLOR_OC_RSYNC = "#58a6ff"
# One shade per upstream baseline, in the order the run measured them. Greys
# and browns stay visually subordinate to oc-rsync's blue, so a reader still
# sees which bar is the subject of the comparison when there are several
# baselines rather than one.
COLOR_BASELINES = (COLOR_UPSTREAM, "#a1887f", "#8d6e63", "#546e7a")
# Matching shades for oc-rsync when it had to be re-timed per baseline.
COLOR_OC_VARIANTS = ("#58a6ff", "#79c0ff", "#a5d6ff", "#cae8ff")
COLOR_PURE_RUST = "#58a6ff"
COLOR_OPENSSL = "#d2a8ff"
COLOR_STD_IO = "#da8b45"
COLOR_IO_URING = "#3fb950"
COLOR_SSH_UPSTREAM = "#6e7681"
COLOR_SSH_SUBPROCESS = "#56d4dd"
COLOR_SSH_RUSSH = "#ffa657"
COLOR_TITLE = "#e6edf3"
COLOR_SUBTITLE = "#8b949e"
COLOR_MODE_HEADER = "#e6edf3"
COLOR_LABEL = "#8b949e"
COLOR_TEXT_ON_BAR = "#ffffff"
COLOR_TEXT_OFF_BAR = "#8b949e"
COLOR_GRID = "#30363d"
COLOR_FASTER = "#3fb950"
COLOR_SAME = "#8b949e"
COLOR_SLOWER = "#f85149"
COLOR_BG = "#0d1117"

FONT = "Arial, Helvetica, sans-serif"
FONT_MONO = "monospace"

MODE_ORDER = [
    "local", "ssh_pull", "ssh_push", "daemon_pull", "daemon_push",
    "compression", "delta", "large_file", "many_small", "sparse",
    "memory", "checksum_openssl", "io_uring", "ssh_transport",
]
MODE_LABELS = {
    "local": "Local Copy",
    "ssh_pull": "SSH Pull",
    "ssh_push": "SSH Push",
    "daemon_pull": "Daemon Pull",
    "daemon_push": "Daemon Push",
    "compression": "Compression",
    "delta": "Delta Transfer",
    "large_file": "Large File (1GB)",
    "many_small": "Many Small Files (100K)",
    "sparse": "Sparse Files",
    "memory": "Memory Usage",
    "checksum_openssl": "Checksum: OpenSSL vs Pure Rust",
    "io_uring": "io_uring vs Standard I/O",
    "ssh_transport": (
        "SSH Transport: upstream vs oc-rsync subprocess vs oc-rsync russh"
    ),
}
MODE_CLI_HINTS = {
    "local": "rsync -av src/ dst/",
    "ssh_pull": "rsync -av host:src/ dst/",
    "ssh_push": "rsync -av src/ host:dst/",
    "daemon_pull": "rsync -av rsync://host/mod/ dst/",
    "daemon_push": "rsync -av src/ rsync://host/mod/",
    "compression": "rsync -avz / --compress-choice=zstd",
    "delta": "rsync -av (modified files)",
    "large_file": "rsync -av (1GB file)",
    "many_small": "rsync -av (100K x 100B files)",
    "sparse": "rsync -avS (sparse files)",
    "memory": "rsync -av (peak RSS measurement)",
    "checksum_openssl": "rsync -avc src/ dst/",
    "io_uring": "--io-uring vs --no-io-uring",
    "ssh_transport": (
        "upstream ssh vs oc-rsync host:path (subprocess) "
        "vs oc-rsync ssh://host/path (russh)"
    ),
}


class MetricUnit(Enum):
    """What a mode's bars measure.

    Only the two units the benchmark actually produces exist here: elapsed
    wall-clock time and peak resident set size.
    """

    DURATION = "duration"
    BYTES = "bytes"


# Every mode states its unit explicitly.  There is deliberately no default:
# a new mode must declare what it measures, because a mode silently
# inheriting "duration" is how peak RSS came to be plotted and labelled as
# elapsed time.
MODE_UNITS = {
    "local": MetricUnit.DURATION,
    "ssh_pull": MetricUnit.DURATION,
    "ssh_push": MetricUnit.DURATION,
    "daemon_pull": MetricUnit.DURATION,
    "daemon_push": MetricUnit.DURATION,
    "compression": MetricUnit.DURATION,
    "delta": MetricUnit.DURATION,
    "large_file": MetricUnit.DURATION,
    "many_small": MetricUnit.DURATION,
    "sparse": MetricUnit.DURATION,
    "memory": MetricUnit.BYTES,
    "checksum_openssl": MetricUnit.DURATION,
    "io_uring": MetricUnit.DURATION,
    "ssh_transport": MetricUnit.DURATION,
}

# Modes where bars represent alternative labels instead of upstream vs oc-rsync
OPENSSL_MODES = {"checksum_openssl"}
IO_URING_MODES = {"io_uring"}
SSH_TRANSPORT_MODES = {"ssh_transport"}

CLI_HINT_HEIGHT = 16

# ---------------------------------------------------------------------------
# Data classes
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class BarSpec:
    """One horizontal bar in the chart.

    `value` is the measured magnitude in the owning mode's unit -- seconds
    for a duration mode, bytes for a byte mode.  The field is deliberately
    unit-neutral; the unit lives on the enclosing `ModeGroup`.
    """

    y: float
    width: float
    value: float
    color: str
    series_label: str


@dataclass(frozen=True)
class TestPair:
    """One test scenario: its bars, left to right, and its ratio annotations.

    The bar count is not fixed. An upstream-comparison row carries one bar per
    baseline plus oc-rsync (and one oc-rsync bar per baseline when oc-rsync had
    to be re-timed against each peer); the 3-way SSH transport row carries its
    three transports. Modelling the row as a list rather than a fixed
    upstream/oc-rsync pair is what lets a second baseline be added without a
    third field, a fourth, and a rule about which ratio each one means.

    `annotations` are `(prefix, ratio)` pairs rendered right of the bars. A
    single unprefixed annotation is the one-baseline case.
    """

    name: str
    bars: tuple[BarSpec, ...]
    annotations: tuple[tuple[str, float], ...]
    center_y: float


@dataclass(frozen=True)
class ModeGroup:
    """A group of test pairs under one transfer mode header.

    `unit` is the metric every bar and ratio in this group is expressed in;
    it selects the label formatter and the ratio wording.
    """

    label: str
    header_y: float
    cli_hint: str
    cli_hint_y: float
    unit: MetricUnit
    tests: list[TestPair]


@dataclass(frozen=True)
class ChartLayout:
    """Complete layout geometry for the chart.

    `max_time` and `scale` describe a shared duration axis and are therefore
    derived from duration modes only -- a single axis spanning seconds and
    bytes would be meaningless.
    """

    groups: list[ModeGroup]
    chart_height: float
    content_bottom: float
    max_time: float
    scale: float


# ---------------------------------------------------------------------------
# Layout computation
# ---------------------------------------------------------------------------


def _is_three_way_ssh(mode: str, t: dict) -> bool:
    """True when this row should render as the 3-way SSH transport bar group."""
    return mode in SSH_TRANSPORT_MODES and "upstream_ssh" in t


def baseline_labels(data: dict) -> list[str]:
    """Upstream releases this run measured, in the order it measured them."""
    declared = data.get("baselines")
    if declared:
        return [b["label"] for b in declared]
    return [data.get("upstream_version") or "3.4.4"]


def upstream_series(t: dict, label: str) -> dict:
    return (t.get("upstreams") or {}).get(label) or t["upstream"]


def oc_series(t: dict, label: str) -> dict:
    return (t.get("oc_rsync_per_baseline") or {}).get(label) or t["oc_rsync"]


def metric_value(series: dict, unit: MetricUnit) -> float:
    """Read one series' measured magnitude in `unit` from a results entry.

    benchmark.py reports every series with the same timing keys (`mean`,
    `min`, `max`) and attaches peak resident set size under a separate
    `peak_rss_kb` key.  A byte-unit mode must therefore read `peak_rss_kb`:
    its `mean` is the run's elapsed time, not its memory use.

    `peak_rss_kb` is absent when /usr/bin/time could not be parsed; report
    zero so the row renders as visibly empty rather than aborting the chart.
    """
    if unit is MetricUnit.BYTES:
        return float(series.get("peak_rss_kb") or 0.0) * 1024.0
    return float(series["mean"])


def row_series(mode: str, t: dict, labels: list[str]) -> list[tuple]:
    """`(series, color, legend_label)` for every bar of one row, left to right.

    Modes that compare oc-rsync build variants against each other (OpenSSL,
    io_uring, SSH transport) have no baseline dimension: their bars are the
    variants. Only the upstream-comparison modes fan out over baselines.
    """
    if _is_three_way_ssh(mode, t):
        return [
            (t["upstream_ssh"], COLOR_SSH_UPSTREAM, "upstream (ssh subprocess)"),
            (
                t["oc_subprocess"],
                COLOR_SSH_SUBPROCESS,
                "oc-rsync (ssh subprocess)",
            ),
            (t["oc_russh"], COLOR_SSH_RUSSH, "oc-rsync (russh embedded)"),
        ]
    if mode in OPENSSL_MODES:
        return [
            (t["upstream"], COLOR_PURE_RUST, "pure Rust"),
            (t["oc_rsync"], COLOR_OPENSSL, "OpenSSL"),
        ]
    if mode in IO_URING_MODES:
        return [
            (t["upstream"], COLOR_STD_IO, "standard I/O"),
            (t["oc_rsync"], COLOR_IO_URING, "io_uring"),
        ]
    if mode in SSH_TRANSPORT_MODES:
        return [
            (t["upstream"], COLOR_SSH_SUBPROCESS, "subprocess"),
            (t["oc_rsync"], COLOR_SSH_RUSSH, "russh"),
        ]

    series = [
        (
            upstream_series(t, label),
            COLOR_BASELINES[i % len(COLOR_BASELINES)],
            f"upstream rsync {label}",
        )
        for i, label in enumerate(labels)
    ]
    if t.get("oc_rsync_per_baseline"):
        series += [
            (
                oc_series(t, label),
                COLOR_OC_VARIANTS[i % len(COLOR_OC_VARIANTS)],
                f"oc-rsync vs {label}",
            )
            for i, label in enumerate(labels)
        ]
    else:
        series.append((t["oc_rsync"], COLOR_OC_RSYNC, "oc-rsync"))
    return series


def metric_values(mode: str, t: dict, labels: list[str]) -> list[float]:
    """Measured magnitudes for every bar of one test row, in the mode's unit."""
    unit = MODE_UNITS[mode]
    return [metric_value(s, unit) for s, _, _ in row_series(mode, t, labels)]


def baseline_ratio(t: dict, label: str, unit: MetricUnit) -> float:
    """oc-rsync vs one named baseline, in the row's own unit.

    Same rule as `pair_ratio`: a byte-unit row derives its ratio from the
    magnitudes it plots, because the stored `ratio`/`ratios` fields are always
    elapsed-time ratios.
    """
    if unit is MetricUnit.BYTES:
        up = metric_value(upstream_series(t, label), unit)
        oc = metric_value(oc_series(t, label), unit)
        return oc / up if up > 0 else 0.0
    return (t.get("ratios") or {}).get(label, t.get("ratio", 0.0))


def row_annotations(
    mode: str, t: dict, labels: list[str], unit: MetricUnit, values: list[float]
) -> tuple[tuple[str, float], ...]:
    """`(prefix, ratio)` annotations rendered to the right of one row."""
    if _is_three_way_ssh(mode, t):
        return (
            ("oc/up", t.get("ratio_sub_vs_upstream", 0.0)),
            ("ru/oc", t.get("ratio_russh_vs_sub", t.get("ratio", 0.0))),
        )
    if mode in OPENSSL_MODES or mode in IO_URING_MODES or mode in SSH_TRANSPORT_MODES:
        return (("", t.get("ratio", 0.0)),)
    if len(labels) == 1:
        return (("", pair_ratio(t, unit, values[0], values[-1])),)
    return tuple(
        (f"vs {label}", baseline_ratio(t, label, unit)) for label in labels
    )


def pair_ratio(t: dict, unit: MetricUnit, up_v: float, oc_v: float) -> float:
    """oc-rsync vs upstream ratio for one row, in the row's own unit.

    The results JSON carries a single `ratio` field that benchmark.py always
    derives from elapsed time -- benchmark_report.py renders it under a
    column headed "Time Ratio" and prints peak RSS in separate columns.  A
    byte-unit row must therefore derive its ratio from the magnitudes it
    actually plots, otherwise the memory row is annotated with a speed-up
    that says nothing about memory.
    """
    if unit is MetricUnit.BYTES:
        return oc_v / up_v if up_v > 0 else 0.0
    return t.get("ratio", 0.0)


def compute_layout(
    tests_by_mode: dict[str, list[dict]], labels: list[str]
) -> ChartLayout:
    """Compute y-positions for every element and overall chart dimensions."""
    all_times = []
    for mode, mode_tests in tests_by_mode.items():
        if MODE_UNITS.get(mode) is not MetricUnit.DURATION:
            continue
        for t in mode_tests:
            all_times.extend(metric_values(mode, t, labels))

    max_time = max(all_times, default=0.0) or 1.0
    scale = BAR_AREA_WIDTH / (max_time * 1.1)

    y = TOP_MARGIN
    groups: list[ModeGroup] = []

    for i, mode in enumerate(MODE_ORDER):
        mode_tests = tests_by_mode.get(mode, [])
        if not mode_tests:
            continue

        unit = MODE_UNITS[mode]

        if groups:
            y += MODE_GAP

        # Per-group horizontal scale. A single global scale (driven by the one
        # largest test across every mode) crushes the small groups into
        # unreadable stubs while a few outliers stretch across, so each mode
        # group is scaled to its own largest bar instead. Absolute magnitudes
        # stay legible via the per-bar labels; the ratio column carries the
        # cross-group comparison. Per-group scaling is also what keeps modes
        # in different units from sharing an axis.
        group_values: list[float] = []
        for t in mode_tests:
            group_values.extend(metric_values(mode, t, labels))
        # `or 1.0` also covers an all-zero group (every measurement missing),
        # which would otherwise divide by zero.
        group_max = max(group_values, default=0.0) or 1.0
        group_scale = BAR_AREA_WIDTH / (group_max * 1.1)

        header_y = y + MODE_HEADER_HEIGHT * 0.7
        y += MODE_HEADER_HEIGHT

        cli_hint = MODE_CLI_HINTS.get(mode, "")
        cli_hint_y = y + 10
        if cli_hint:
            y += CLI_HINT_HEIGHT

        pairs: list[TestPair] = []
        for j, t in enumerate(mode_tests):
            if j > 0:
                y += GROUP_GAP

            series = row_series(mode, t, labels)
            values = [metric_value(s, unit) for s, _, _ in series]
            n = len(series)
            row_height = n * BAR_HEIGHT + (n - 1) * BAR_GAP
            center_y = y + row_height / 2

            bars = tuple(
                BarSpec(
                    y + k * (BAR_HEIGHT + BAR_GAP),
                    max(value * group_scale, MIN_BAR_WIDTH),
                    value,
                    color,
                    legend_label,
                )
                for k, ((_, color, legend_label), value) in enumerate(
                    zip(series, values)
                )
            )

            pairs.append(
                TestPair(
                    name=t["name"],
                    bars=bars,
                    annotations=row_annotations(mode, t, labels, unit, values),
                    center_y=center_y,
                )
            )

            y += row_height

        groups.append(ModeGroup(
            label=MODE_LABELS[mode],
            header_y=header_y,
            cli_hint=cli_hint,
            cli_hint_y=cli_hint_y,
            unit=unit,
            tests=pairs,
        ))

    content_bottom = y
    chart_height = content_bottom + BOTTOM_MARGIN

    return ChartLayout(
        groups=groups,
        chart_height=chart_height,
        content_bottom=content_bottom,
        max_time=max_time,
        scale=scale,
    )


# ---------------------------------------------------------------------------
# Formatting helpers
# ---------------------------------------------------------------------------


def fmt_time(seconds: float) -> str:
    """Human-readable timing: '42 ms' for <1s, '1.23s' for >=1s."""
    if seconds < 1.0:
        return f"{seconds * 1000:.0f} ms"
    return f"{seconds:.2f}s"


def fmt_bytes(num_bytes: float) -> str:
    """Human-readable size: '512 KiB' for <1 MiB, '12.34 MiB', '1.21 GiB'.

    IEC binary prefixes, because the underlying measurement is /usr/bin/time's
    peak resident set size, reported in 1024-byte kbytes; the digits therefore
    match the memory table in benchmark_report.py, which divides by 1024 too.
    Mirrors fmt_time's shape: whole units for the smallest step, two decimals
    once the value crosses into a larger one.
    """
    kib = num_bytes / 1024.0
    if kib < 1024.0:
        return f"{kib:.0f} KiB"
    mib = kib / 1024.0
    if mib < 1024.0:
        return f"{mib:.2f} MiB"
    return f"{mib / 1024.0:.2f} GiB"


def fmt_time_exact(seconds: float) -> str:
    """Unabbreviated timing for hover text: '2.986s'."""
    return f"{seconds:.3f}s"


def fmt_bytes_exact(num_bytes: float) -> str:
    """Unabbreviated size for hover text, in the measured unit: '7924 KiB'.

    /usr/bin/time reports peak RSS in whole kilobytes, so this is the raw
    measurement with no rounding of its own.
    """
    return f"{num_bytes / 1024:.0f} KiB"


# Strategy: the mode's unit selects the formatter.  One formatter for all
# modes is what put a duration under the "Memory Usage" header.  Bar and
# axis labels are abbreviated for width; hover text keeps full precision.
UNIT_FORMATTERS = {
    MetricUnit.DURATION: fmt_time,
    MetricUnit.BYTES: fmt_bytes,
}
UNIT_EXACT_FORMATTERS = {
    MetricUnit.DURATION: fmt_time_exact,
    MetricUnit.BYTES: fmt_bytes_exact,
}

# (better, worse) wording for a ratio in each unit.  Spending more RSS is
# not "slower", it is more memory.
RATIO_WORDS = {
    MetricUnit.DURATION: ("faster", "slower"),
    MetricUnit.BYTES: ("less memory", "more memory"),
}


def fmt_value(value: float, unit: MetricUnit) -> str:
    """Format a measured magnitude for a bar or axis label."""
    return UNIT_FORMATTERS[unit](value)


def fmt_value_exact(value: float, unit: MetricUnit) -> str:
    """Format a measured magnitude for hover text, without abbreviating."""
    return UNIT_EXACT_FORMATTERS[unit](value)


def ratio_text(ratio: float, unit: MetricUnit) -> tuple[str, str]:
    """Return (display_text, color) for an oc-rsync vs upstream annotation.

    Wording follows the unit: a duration ratio reads faster/slower, a memory
    ratio reads less/more memory.

    A non-positive ratio is only meaningful for durations, where benchmark.py
    rounds ratios to two decimals and so collapses a sub-millisecond ratio
    such as 250x faster to 0.0.  Peak RSS has no comparable rounding floor --
    it is measured in whole kilobytes -- so for a byte metric a non-positive
    ratio can only mean the measurement is missing, and claiming a win there
    would invent a result.
    """
    better, worse = RATIO_WORDS[unit]
    if ratio <= 0:
        if unit is MetricUnit.DURATION:
            return (f">100x {better}", COLOR_FASTER)
        return ("no data", COLOR_SAME)
    if ratio < RATIO_FASTER_BELOW:
        return (f"{1.0 / ratio:.1f}x {better}", COLOR_FASTER)
    if ratio <= RATIO_SAME_UPPER:
        return ("~same", COLOR_SAME)
    return (f"{ratio:.1f}x {worse}", COLOR_SLOWER)


def nice_grid_step(max_val: float, target_steps: int = 5) -> float:
    """Compute a visually pleasing grid interval."""
    if max_val <= 0:
        return 0.1
    raw = max_val / target_steps
    magnitude = 10 ** math.floor(math.log10(raw))
    residual = raw / magnitude
    if residual <= 1.5:
        nice = 1
    elif residual <= 3.5:
        nice = 2
    elif residual <= 7.5:
        nice = 5
    else:
        nice = 10
    return nice * magnitude


# ---------------------------------------------------------------------------
# SVG builder
# ---------------------------------------------------------------------------


class ChartBuilder:
    """Incrementally builds an SVG document from chart elements."""

    def __init__(self, width: float, height: float) -> None:
        self._parts: list[str] = []
        self._parts.append(
            f'<svg xmlns="http://www.w3.org/2000/svg" '
            f'width="{width}" height="{height}" '
            f'viewBox="0 0 {width} {height}" '
            f'font-family=\'{FONT}\'>'
        )
        self._parts.append(
            f'<rect width="{width}" height="{height}" fill="{COLOR_BG}"/>'
        )

    def add_title(self, title: str, subtitle: str) -> None:
        cx = CHART_WIDTH / 2
        self._parts.append(
            f'<text x="{cx}" y="24" text-anchor="middle" '
            f'font-size="16" font-weight="bold" fill="{COLOR_TITLE}">'
            f"{escape(title)}</text>"
        )
        self._parts.append(
            f'<text x="{cx}" y="44" text-anchor="middle" '
            f'font-size="11" fill="{COLOR_SUBTITLE}">'
            f"{escape(subtitle)}</text>"
        )

    def add_grid(
        self,
        max_time: float,
        scale: float,
        y_top: float,
        y_bottom: float,
        unit: MetricUnit = MetricUnit.DURATION,
    ) -> None:
        """Draw an axis of dashed gridlines labelled in `unit`.

        Axis labels are formatted at a different site from the bar labels, so
        they take the same unit-driven formatter; a grid drawn over a byte
        axis must not be labelled in milliseconds.
        """
        step = nice_grid_step(max_time)
        self._parts.append("<g>")
        val = step
        while val <= max_time * 1.05:
            x = LEFT_MARGIN + val * scale
            if x > CHART_WIDTH - RIGHT_MARGIN:
                break
            self._parts.append(
                f'<line x1="{x:.1f}" y1="{y_top}" '
                f'x2="{x:.1f}" y2="{y_bottom}" '
                f'stroke="{COLOR_GRID}" stroke-dasharray="4,4"/>'
            )
            self._parts.append(
                f'<text x="{x:.1f}" y="{y_bottom + 14}" '
                f'text-anchor="middle" font-size="10" fill="{COLOR_SUBTITLE}">'
                f"{fmt_value(val, unit)}</text>"
            )
            val += step
        self._parts.append("</g>")

    def add_mode_group(self, group: ModeGroup) -> None:
        self._parts.append(
            f'<text x="10" y="{group.header_y:.1f}" '
            f'font-size="13" font-weight="600" fill="{COLOR_MODE_HEADER}">'
            f"{escape(group.label)}</text>"
        )
        if group.cli_hint:
            self._parts.append(
                f'<text x="12" y="{group.cli_hint_y:.1f}" '
                f'font-size="10" font-family="{FONT_MONO}" fill="{COLOR_SUBTITLE}">'
                f"{escape(group.cli_hint)}</text>"
            )

        for pair in group.tests:
            label_y = pair.center_y + 4
            self._parts.append(
                f'<text x="{LEFT_MARGIN - 10}" y="{label_y:.1f}" '
                f'text-anchor="end" font-size="11" fill="{COLOR_LABEL}">'
                f"{escape(pair.name)}</text>"
            )
            for bar in pair.bars:
                self._add_bar(bar, group.unit)
            self._add_speedup(pair, group.unit)

    def add_legend(
        self,
        y: float,
        has_openssl: bool = False,
        has_io_uring: bool = False,
        has_ssh_transport: bool = False,
        has_ssh_3way: bool = False,
        baselines: tuple[str, ...] = ("3.4.4",),
    ) -> None:
        cx = CHART_WIDTH / 2
        self._parts.append(f'<g transform="translate({cx - 160}, {y:.0f})">')
        # One swatch per baseline, in measurement order, then oc-rsync. A
        # legend naming a single upstream release would leave the reader
        # guessing which grey bar is which release.
        row = 0
        for i, label in enumerate(baselines):
            ry = row * 18
            self._parts.append(
                f'<rect x="0" y="{ry}" width="12" height="12" rx="2" '
                f'fill="{COLOR_BASELINES[i % len(COLOR_BASELINES)]}"/>'
            )
            self._parts.append(
                f'<text x="16" y="{ry + 10}" font-size="11" fill="{COLOR_LABEL}">'
                f'upstream rsync {escape(label)}</text>'
            )
            if i == 0:
                self._parts.append(
                    f'<rect x="170" y="{ry}" width="12" height="12" rx="2" '
                    f'fill="{COLOR_OC_RSYNC}"/>'
                )
                self._parts.append(
                    f'<text x="186" y="{ry + 10}" font-size="11" '
                    f'fill="{COLOR_LABEL}">oc-rsync</text>'
                )
            row += 1
        row -= 1
        if has_openssl:
            row += 1
            ry = row * 18
            self._parts.append(
                f'<rect x="0" y="{ry}" width="12" height="12" rx="2" fill="{COLOR_PURE_RUST}"/>'
            )
            self._parts.append(
                f'<text x="16" y="{ry + 10}" font-size="11" fill="{COLOR_LABEL}">oc-rsync (pure Rust)</text>'
            )
            self._parts.append(
                f'<rect x="210" y="{ry}" width="12" height="12" rx="2" fill="{COLOR_OPENSSL}"/>'
            )
            self._parts.append(
                f'<text x="226" y="{ry + 10}" font-size="11" fill="{COLOR_LABEL}">oc-rsync (OpenSSL)</text>'
            )
        if has_io_uring:
            row += 1
            ry = row * 18
            self._parts.append(
                f'<rect x="0" y="{ry}" width="12" height="12" rx="2" fill="{COLOR_STD_IO}"/>'
            )
            self._parts.append(
                f'<text x="16" y="{ry + 10}" font-size="11" fill="{COLOR_LABEL}">standard I/O</text>'
            )
            self._parts.append(
                f'<rect x="170" y="{ry}" width="12" height="12" rx="2" fill="{COLOR_IO_URING}"/>'
            )
            self._parts.append(
                f'<text x="186" y="{ry + 10}" font-size="11" fill="{COLOR_LABEL}">io_uring</text>'
            )
        if has_ssh_transport:
            row += 1
            ry = row * 18
            if has_ssh_3way:
                self._parts.append(
                    f'<rect x="0" y="{ry}" width="12" height="12" rx="2" '
                    f'fill="{COLOR_SSH_UPSTREAM}"/>'
                )
                self._parts.append(
                    f'<text x="16" y="{ry + 10}" font-size="11" '
                    f'fill="{COLOR_LABEL}">upstream rsync (ssh)</text>'
                )
                self._parts.append(
                    f'<rect x="170" y="{ry}" width="12" height="12" rx="2" '
                    f'fill="{COLOR_SSH_SUBPROCESS}"/>'
                )
                self._parts.append(
                    f'<text x="186" y="{ry + 10}" font-size="11" '
                    f'fill="{COLOR_LABEL}">oc-rsync (ssh subprocess)</text>'
                )
                row += 1
                ry = row * 18
                self._parts.append(
                    f'<rect x="0" y="{ry}" width="12" height="12" rx="2" '
                    f'fill="{COLOR_SSH_RUSSH}"/>'
                )
                self._parts.append(
                    f'<text x="16" y="{ry + 10}" font-size="11" '
                    f'fill="{COLOR_LABEL}">oc-rsync (russh embedded)</text>'
                )
            else:
                self._parts.append(
                    f'<rect x="0" y="{ry}" width="12" height="12" rx="2" '
                    f'fill="{COLOR_SSH_SUBPROCESS}"/>'
                )
                self._parts.append(
                    f'<text x="16" y="{ry + 10}" font-size="11" '
                    f'fill="{COLOR_LABEL}">SSH subprocess</text>'
                )
                self._parts.append(
                    f'<rect x="170" y="{ry}" width="12" height="12" rx="2" '
                    f'fill="{COLOR_SSH_RUSSH}"/>'
                )
                self._parts.append(
                    f'<text x="186" y="{ry + 10}" font-size="11" '
                    f'fill="{COLOR_LABEL}">SSH russh (embedded)</text>'
                )
        self._parts.append("</g>")

    def render(self) -> str:
        self._parts.append("</svg>")
        return "\n".join(self._parts)

    def _add_bar(self, bar: BarSpec, unit: MetricUnit) -> None:
        value_str = fmt_value(bar.value, unit)
        self._parts.append(
            f'<rect x="{LEFT_MARGIN}" y="{bar.y:.1f}" '
            f'width="{bar.width:.1f}" height="{BAR_HEIGHT}" '
            f'rx="3" fill="{bar.color}">'
            f"<title>{escape(bar.series_label)}: "
            f"{fmt_value_exact(bar.value, unit)}</title>"
            f"</rect>"
        )
        text_y = bar.y + BAR_HEIGHT - 4
        if bar.width > TEXT_INSIDE_THRESHOLD:
            tx = LEFT_MARGIN + bar.width - 8
            self._parts.append(
                f'<text x="{tx:.1f}" y="{text_y:.1f}" '
                f'text-anchor="end" font-size="10" fill="{COLOR_TEXT_ON_BAR}">'
                f"{value_str}</text>"
            )
        else:
            tx = LEFT_MARGIN + bar.width + 4
            self._parts.append(
                f'<text x="{tx:.1f}" y="{text_y:.1f}" '
                f'text-anchor="start" font-size="10" fill="{COLOR_TEXT_OFF_BAR}">'
                f"{value_str}</text>"
            )

    def _add_speedup(self, pair: TestPair, unit: MetricUnit) -> None:
        """Stack this row's ratio annotations, centred on the bar group."""
        x = CHART_WIDTH - RIGHT_MARGIN + 10
        count = len(pair.annotations)
        if count == 1:
            prefix, ratio = pair.annotations[0]
            text, color = ratio_text(ratio, unit)
            label = f"{prefix} {text}".strip()
            self._parts.append(
                f'<text x="{x}" y="{pair.center_y + 4:.1f}" '
                f'font-size="11" font-weight="600" fill="{color}">'
                f"{escape(label)}</text>"
            )
            return

        line_height = 16
        top = pair.center_y - (count - 1) * line_height / 2 + 4
        for i, (prefix, ratio) in enumerate(pair.annotations):
            text, color = ratio_text(ratio, unit)
            label = f"{prefix} {text}".strip()
            self._parts.append(
                f'<text x="{x}" y="{top + i * line_height:.1f}" '
                f'font-size="10" font-weight="600" fill="{color}">'
                f"{escape(label)}</text>"
            )


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------


def generate_chart(data: dict) -> str:
    """Generate complete SVG string from benchmark data."""
    tests_by_mode: dict[str, list[dict]] = {}
    for t in data["tests"]:
        tests_by_mode.setdefault(t["mode"], []).append(t)

    has_openssl = any(m in OPENSSL_MODES for m in tests_by_mode)
    has_io_uring = any(m in IO_URING_MODES for m in tests_by_mode)
    has_ssh_transport = any(m in SSH_TRANSPORT_MODES for m in tests_by_mode)
    has_ssh_3way = any(
        m in SSH_TRANSPORT_MODES and any("upstream_ssh" in t for t in ts)
        for m, ts in tests_by_mode.items()
    )
    labels = baseline_labels(data)
    layout = compute_layout(tests_by_mode, labels)

    extra_legend_rows = (
        int(has_openssl) + int(has_io_uring) + int(has_ssh_transport)
        + int(has_ssh_3way) + max(len(labels) - 1, 0)
    )
    extra_legend = extra_legend_rows * 18
    chart_height = layout.chart_height + extra_legend

    builder = ChartBuilder(CHART_WIDTH, chart_height)

    test_data = data.get("test_data", {})
    size_mb = test_data.get("size_mb", "?")
    files = test_data.get("files", "?")
    oc_version = data.get("oc_rsync_version") or ""
    env = data.get("environment") or {}
    kernel = env.get("kernel_release")
    machine = env.get("machine", "x86_64")
    subject = f"oc-rsync {oc_version}".strip()
    builder.add_title(
        f"{subject} vs upstream rsync "
        + " and ".join(labels),
        f"{size_mb} MB, {files} files \u2014 Linux {machine}"
        + (f", kernel {kernel}" if kernel else " CI"),
    )

    # No global x-axis grid: with per-group scaling -- and with modes that do
    # not even share a unit -- a single shared axis would be misleading. Each
    # bar carries its own label in its mode's unit instead.
    for group in layout.groups:
        builder.add_mode_group(group)

    builder.add_legend(
        chart_height - 30 - extra_legend,
        has_openssl,
        has_io_uring,
        has_ssh_transport,
        has_ssh_3way,
        tuple(labels),
    )

    return builder.render()


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Generate SVG benchmark chart from benchmark_results.json"
    )
    parser.add_argument(
        "--input",
        default="benchmark_results.json",
        help="Path to benchmark results JSON (default: benchmark_results.json)",
    )
    parser.add_argument(
        "--output",
        default="docs/assets/benchmark.svg",
        help="Output SVG file path (default: docs/assets/benchmark.svg)",
    )
    args = parser.parse_args()

    with open(args.input) as f:
        data = json.load(f)

    svg = generate_chart(data)

    os.makedirs(os.path.dirname(args.output) or ".", exist_ok=True)
    with open(args.output, "w") as f:
        f.write(svg)

    print(f"Wrote {args.output}", file=sys.stderr)


if __name__ == "__main__":
    main()
