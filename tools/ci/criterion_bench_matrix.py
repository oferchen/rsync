#!/usr/bin/env python3
"""Emit the Criterion release matrix: one cell per declared bench target.

benchmark-release.yml used to run one cell per crate, and each cell ran every
bench binary of its crate back to back. The engine crate declares 24 of them;
on the v0.6.3 and v0.6.4 release runs its cell reached only the ninth
(`delta_transfer_benchmark`) before the 90-minute job timeout, and a timed-out
cell cancels the publish job, so no release got a Criterion summary.

One cell per `[[bench]]` target gives every binary its own timeout budget and
keeps a newly added bench from pushing a whole crate over the limit. The
targets are read from each crate's Cargo.toml so the matrix cannot drift from
the benches that exist.

Usage: criterion_bench_matrix.py CRATE [CRATE...]
Prints a JSON list of {"crate": ..., "bench": ...} objects on one line.
"""

from __future__ import annotations

import json
import sys
import tomllib
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]

# GitHub rejects a job matrix with more than 256 cells.
MAX_CELLS = 256


def bench_targets(crate: str, repo: Path = REPO) -> list[str]:
    """Return the `[[bench]]` target names declared by `crates/<crate>/Cargo.toml`."""
    manifest = tomllib.loads((repo / "crates" / crate / "Cargo.toml").read_text())
    package = manifest["package"]["name"]
    if package != crate:
        raise ValueError(f"crates/{crate} is package {package!r}; pass the package name")
    return [bench["name"] for bench in manifest.get("bench", [])]


def build_matrix(crates: list[str], repo: Path = REPO) -> list[dict[str, str]]:
    """Build the matrix cells, failing on an empty crate or an oversized matrix."""
    cells = []
    for crate in crates:
        names = bench_targets(crate, repo)
        if not names:
            raise ValueError(f"crates/{crate} declares no [[bench]] targets")
        cells.extend({"crate": crate, "bench": name} for name in names)
    if len(cells) > MAX_CELLS:
        raise ValueError(f"{len(cells)} cells exceeds the {MAX_CELLS}-cell matrix limit")
    return cells


def main(argv: list[str]) -> int:
    if not argv:
        print("usage: criterion_bench_matrix.py CRATE [CRATE...]", file=sys.stderr)
        return 2
    try:
        cells = build_matrix(argv)
    except (OSError, KeyError, ValueError) as exc:
        print(f"criterion_bench_matrix: {exc}", file=sys.stderr)
        return 1
    print(json.dumps(cells, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
