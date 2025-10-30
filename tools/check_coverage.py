#!/usr/bin/env python3
"""Validate coverage thresholds while printing detailed metrics."""

from __future__ import annotations

import argparse
import json
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable


@dataclass(frozen=True)
class CoverageMetric:
    name: str
    covered: float
    total: float
    percent: float

    def format_line(self) -> str:
        if self.total:
            return f"{self.name.title():<10}: {self.percent:.2f}% ({int(self.covered)}/{int(self.total)})"
        return f"{self.name.title():<10}: {self.percent:.2f}% (no tracked items)"


def load_metrics(summary_path: Path) -> dict[str, CoverageMetric]:
    document = json.loads(summary_path.read_text())
    data = document.get("data")
    if not data:
        raise ValueError("coverage summary is missing `data`")
    totals = data[0].get("totals")
    if not totals:
        raise ValueError("coverage summary is missing `totals`")

    metrics: dict[str, CoverageMetric] = {}
    for key in ("lines", "functions", "regions", "branches"):
        entry = totals.get(key)
        if not entry:
            continue
        covered = float(entry.get("covered", 0))
        total = float(entry.get("count", 0))
        percent = float(entry.get("percent", (covered / total * 100) if total else 100.0))
        metrics[key] = CoverageMetric(key, covered, total, percent)
    return metrics


def enforce_thresholds(metrics: dict[str, CoverageMetric], requirements: dict[str, float]) -> int:
    failures: list[str] = []
    for name, minimum in requirements.items():
        metric = metrics.get(name)
        if metric is None:
            failures.append(f"missing metric: {name}")
            continue
        if metric.percent + 1e-9 < minimum:
            failures.append(
                f"{name} coverage {metric.percent:.2f}% is below the required {minimum:.2f}%"
            )
    if failures:
        for message in failures:
            print(f"::error ::{message}")
        return 1
    return 0


def print_metrics(metrics: Iterable[CoverageMetric]) -> None:
    print("Coverage summary:")
    for metric in metrics:
        print(f"  {metric.format_line()}")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("summary", type=Path, help="Path to cargo-llvm-cov JSON summary")
    parser.add_argument("--min-lines", type=float, default=95.0, help="Minimum line coverage percentage")
    parser.add_argument(
        "--min-functions", type=float, default=95.0, help="Minimum function coverage percentage"
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    metrics = load_metrics(args.summary)
    print_metrics(metrics.values())
    requirements = {"lines": args.min_lines, "functions": args.min_functions}
    return enforce_thresholds(metrics, requirements)


if __name__ == "__main__":
    sys.exit(main())
