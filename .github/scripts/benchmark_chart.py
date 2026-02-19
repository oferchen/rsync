#!/usr/bin/env python3
"""Generate an SVG benchmark chart from benchmark_results.json.

Pure Python -- no external dependencies.  Reads CI benchmark timing data
and produces a grouped horizontal bar chart comparing oc-rsync against
upstream rsync across all transfer modes.

Design patterns:
  - Builder (ChartBuilder) for incremental SVG construction
  - Data classes for typed, immutable layout geometry
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
from html import escape

# ---------------------------------------------------------------------------
# Layout constants
# ---------------------------------------------------------------------------

CHART_WIDTH = 800
LEFT_MARGIN = 180
RIGHT_MARGIN = 120
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

# ---------------------------------------------------------------------------
# Colors
# ---------------------------------------------------------------------------

COLOR_UPSTREAM = "#6e7681"
COLOR_OC_RSYNC = "#58a6ff"
COLOR_PURE_RUST = "#58a6ff"
COLOR_OPENSSL = "#d2a8ff"
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

MODE_ORDER = ["local", "ssh_pull", "ssh_push", "daemon_pull", "daemon_push", "checksum_openssl"]
MODE_LABELS = {
    "local": "Local Copy",
    "ssh_pull": "SSH Pull",
    "ssh_push": "SSH Push",
    "daemon_pull": "Daemon Pull",
    "daemon_push": "Daemon Push",
    "checksum_openssl": "Checksum: OpenSSL vs Pure Rust",
}
MODE_CLI_HINTS = {
    "local": "rsync -av src/ dst/",
    "ssh_pull": "rsync -av host:src/ dst/",
    "ssh_push": "rsync -av src/ host:dst/",
    "daemon_pull": "rsync -av rsync://host/mod/ dst/",
    "daemon_push": "rsync -av src/ rsync://host/mod/",
    "checksum_openssl": "rsync -avc src/ dst/",
}

# Modes where bars represent pure-Rust vs OpenSSL instead of upstream vs oc-rsync
OPENSSL_MODES = {"checksum_openssl"}

CLI_HINT_HEIGHT = 16

# ---------------------------------------------------------------------------
# Data classes
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class BarSpec:
    """One horizontal bar in the chart."""

    y: float
    width: float
    timing: float
    color: str
    series_label: str


@dataclass(frozen=True)
class TestPair:
    """A pair of bars (upstream + oc-rsync) for one test scenario."""

    name: str
    upstream: BarSpec
    oc_rsync: BarSpec
    ratio: float
    center_y: float


@dataclass(frozen=True)
class ModeGroup:
    """A group of test pairs under one transfer mode header."""

    label: str
    header_y: float
    cli_hint: str
    cli_hint_y: float
    tests: list[TestPair]


@dataclass(frozen=True)
class ChartLayout:
    """Complete layout geometry for the chart."""

    groups: list[ModeGroup]
    chart_height: float
    content_bottom: float
    max_time: float
    scale: float


# ---------------------------------------------------------------------------
# Layout computation
# ---------------------------------------------------------------------------


def compute_layout(tests_by_mode: dict[str, list[dict]]) -> ChartLayout:
    """Compute y-positions for every element and overall chart dimensions."""
    all_times = []
    for mode_tests in tests_by_mode.values():
        for t in mode_tests:
            all_times.append(t["upstream"]["mean"])
            all_times.append(t["oc_rsync"]["mean"])

    max_time = max(all_times) if all_times else 1.0
    scale = BAR_AREA_WIDTH / (max_time * 1.1)

    y = TOP_MARGIN
    groups: list[ModeGroup] = []

    for i, mode in enumerate(MODE_ORDER):
        mode_tests = tests_by_mode.get(mode, [])
        if not mode_tests:
            continue

        if groups:
            y += MODE_GAP

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

            up_y = y
            oc_y = y + BAR_HEIGHT + BAR_GAP
            center_y = y + BAR_HEIGHT + BAR_GAP / 2

            up_w = max(t["upstream"]["mean"] * scale, MIN_BAR_WIDTH)
            oc_w = max(t["oc_rsync"]["mean"] * scale, MIN_BAR_WIDTH)

            if mode in OPENSSL_MODES:
                bar1_color, bar1_label = COLOR_PURE_RUST, "pure Rust"
                bar2_color, bar2_label = COLOR_OPENSSL, "OpenSSL"
            else:
                bar1_color, bar1_label = COLOR_UPSTREAM, "upstream"
                bar2_color, bar2_label = COLOR_OC_RSYNC, "oc-rsync"

            pairs.append(
                TestPair(
                    name=t["name"],
                    upstream=BarSpec(up_y, up_w, t["upstream"]["mean"], bar1_color, bar1_label),
                    oc_rsync=BarSpec(oc_y, oc_w, t["oc_rsync"]["mean"], bar2_color, bar2_label),
                    ratio=t["ratio"],
                    center_y=center_y,
                )
            )
            y += 2 * BAR_HEIGHT + BAR_GAP

        groups.append(ModeGroup(
            label=MODE_LABELS[mode],
            header_y=header_y,
            cli_hint=cli_hint,
            cli_hint_y=cli_hint_y,
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


def ratio_text(ratio: float) -> tuple[str, str]:
    """Return (display_text, color) for a speedup annotation."""
    if ratio < 0.95:
        speedup = 1.0 / ratio
        return (f"{speedup:.1f}x faster", COLOR_FASTER)
    if ratio <= 1.05:
        return ("~same", COLOR_SAME)
    return (f"{ratio:.1f}x slower", COLOR_SLOWER)


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
    ) -> None:
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
                f"{fmt_time(val)}</text>"
            )
            val += step
        self._parts.append("</g>")

    def add_mode_group(self, group: ModeGroup, scale: float) -> None:
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
            self._add_bar(pair.upstream, scale)
            self._add_bar(pair.oc_rsync, scale)
            self._add_speedup(pair)

    def add_legend(self, y: float, has_openssl: bool = False) -> None:
        cx = CHART_WIDTH / 2
        self._parts.append(f'<g transform="translate({cx - 160}, {y:.0f})">')
        self._parts.append(
            f'<rect x="0" y="0" width="12" height="12" rx="2" fill="{COLOR_UPSTREAM}"/>'
        )
        self._parts.append(
            f'<text x="16" y="10" font-size="11" fill="{COLOR_LABEL}">upstream rsync 3.4.1</text>'
        )
        self._parts.append(
            f'<rect x="170" y="0" width="12" height="12" rx="2" fill="{COLOR_OC_RSYNC}"/>'
        )
        self._parts.append(
            f'<text x="186" y="10" font-size="11" fill="{COLOR_LABEL}">oc-rsync</text>'
        )
        if has_openssl:
            self._parts.append(
                f'<rect x="0" y="18" width="12" height="12" rx="2" fill="{COLOR_PURE_RUST}"/>'
            )
            self._parts.append(
                f'<text x="16" y="28" font-size="11" fill="{COLOR_LABEL}">oc-rsync (pure Rust)</text>'
            )
            self._parts.append(
                f'<rect x="210" y="18" width="12" height="12" rx="2" fill="{COLOR_OPENSSL}"/>'
            )
            self._parts.append(
                f'<text x="226" y="28" font-size="11" fill="{COLOR_LABEL}">oc-rsync (OpenSSL)</text>'
            )
        self._parts.append("</g>")

    def render(self) -> str:
        self._parts.append("</svg>")
        return "\n".join(self._parts)

    def _add_bar(self, bar: BarSpec, scale: float) -> None:
        self._parts.append(
            f'<rect x="{LEFT_MARGIN}" y="{bar.y:.1f}" '
            f'width="{bar.width:.1f}" height="{BAR_HEIGHT}" '
            f'rx="3" fill="{bar.color}">'
            f"<title>{escape(bar.series_label)}: {bar.timing:.3f}s</title>"
            f"</rect>"
        )
        time_str = fmt_time(bar.timing)
        text_y = bar.y + BAR_HEIGHT - 4
        if bar.width > TEXT_INSIDE_THRESHOLD:
            tx = LEFT_MARGIN + bar.width - 8
            self._parts.append(
                f'<text x="{tx:.1f}" y="{text_y:.1f}" '
                f'text-anchor="end" font-size="10" fill="{COLOR_TEXT_ON_BAR}">'
                f"{time_str}</text>"
            )
        else:
            tx = LEFT_MARGIN + bar.width + 4
            self._parts.append(
                f'<text x="{tx:.1f}" y="{text_y:.1f}" '
                f'text-anchor="start" font-size="10" fill="{COLOR_TEXT_OFF_BAR}">'
                f"{time_str}</text>"
            )

    def _add_speedup(self, pair: TestPair) -> None:
        text, color = ratio_text(pair.ratio)
        x = CHART_WIDTH - RIGHT_MARGIN + 10
        y = pair.center_y + 4
        self._parts.append(
            f'<text x="{x}" y="{y:.1f}" '
            f'font-size="11" font-weight="600" fill="{color}">'
            f"{escape(text)}</text>"
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
    layout = compute_layout(tests_by_mode)

    # Extra height for second legend row when OpenSSL tests are present
    extra_legend = 18 if has_openssl else 0
    chart_height = layout.chart_height + extra_legend

    builder = ChartBuilder(CHART_WIDTH, chart_height)

    test_data = data.get("test_data", {})
    size_mb = test_data.get("size_mb", "?")
    files = test_data.get("files", "?")
    builder.add_title(
        "oc-rsync vs upstream rsync 3.4.1",
        f"{size_mb} MB, {files} files \u2014 Linux x86_64 CI",
    )

    builder.add_grid(layout.max_time, layout.scale, TOP_MARGIN, layout.content_bottom)

    for group in layout.groups:
        builder.add_mode_group(group, layout.scale)

    builder.add_legend(chart_height - 30 - extra_legend, has_openssl)

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
