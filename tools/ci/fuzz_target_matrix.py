#!/usr/bin/env python3
"""Emit the daily fuzz sweep matrix: every cargo-fuzz target, in shards.

The daily sweep used to be a hand-written 22-row matrix. 14 of the 36 declared
targets were missing from it, and nothing noticed, because a list copied from
the Cargo.toml files drifts the moment a target is added. This script reads
the `[[bin]]` tables of the three fuzz workspaces instead, so a new target
joins the sweep without a workflow edit.

Targets that an overnight workflow already fuzzes for 30-60 minutes are left
out: two minutes more of the same target adds nothing. The overnight set is
read from those workflows' matrix `target:` rows, so it cannot drift either.

Targets are grouped per workspace into shards of at most SHARD_SIZE. A shard
builds its workspace once and runs its targets one after another, so the
sanitizer build is paid per shard, not per target.

Usage: fuzz_target_matrix.py
Prints a JSON list of {"workspace", "shard", "targets"} objects on one line;
"targets" is space-separated.
"""

from __future__ import annotations

import json
import re
import sys
import tomllib
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]

WORKSPACES = ("fuzz", "crates/protocol/fuzz", "crates/filters/fuzz")

OVERNIGHT_WORKFLOWS = (
    ".github/workflows/differential-fuzzer-overnight.yml",
    ".github/workflows/filter-fuzzer-overnight.yml",
)

# 8 targets x 120 s keeps a shard's fuzzing at 16 minutes, well inside the
# job timeout next to the sanitizer and coverage builds.
SHARD_SIZE = 8

_TARGET_ROW = re.compile(r"^\s*-\s*target:\s*([A-Za-z0-9_]+)\s*$", re.MULTILINE)


def fuzz_bins(workspace: str, repo: Path = REPO) -> list[str]:
    """Return the `[[bin]]` names declared by `<workspace>/Cargo.toml`."""
    manifest = tomllib.loads((repo / workspace / "Cargo.toml").read_text())
    if not manifest.get("package", {}).get("metadata", {}).get("cargo-fuzz"):
        raise ValueError(f"{workspace} is not a cargo-fuzz workspace")
    names = [b["name"] for b in manifest.get("bin", [])]
    if not names:
        raise ValueError(f"{workspace} declares no [[bin]] targets")
    return names


def overnight_targets(repo: Path = REPO) -> set[str]:
    """Return the targets the overnight fuzz workflows run."""
    found: set[str] = set()
    for workflow in OVERNIGHT_WORKFLOWS:
        rows = _TARGET_ROW.findall((repo / workflow).read_text())
        if not rows:
            raise ValueError(f"{workflow} has no matrix target rows")
        found.update(rows)
    return found


def build_matrix(repo: Path = REPO) -> list[dict[str, str]]:
    """Build the shard cells, failing on an overnight target no workspace declares."""
    skip = overnight_targets(repo)
    declared: set[str] = set()
    cells = []
    for workspace in WORKSPACES:
        bins = fuzz_bins(workspace, repo)
        declared.update(bins)
        daily = [name for name in bins if name not in skip]
        for start in range(0, len(daily), SHARD_SIZE):
            cells.append(
                {
                    "workspace": workspace,
                    "shard": f"{workspace.replace('/', '-')}-{start // SHARD_SIZE + 1}",
                    "targets": " ".join(daily[start : start + SHARD_SIZE]),
                }
            )
    unknown = sorted(skip - declared)
    if unknown:
        raise ValueError(f"overnight workflows name undeclared targets: {', '.join(unknown)}")
    return cells


def main(argv: list[str]) -> int:
    if argv:
        print("usage: fuzz_target_matrix.py", file=sys.stderr)
        return 2
    try:
        cells = build_matrix()
    except (OSError, KeyError, ValueError) as exc:
        print(f"fuzz_target_matrix: {exc}", file=sys.stderr)
        return 1
    print(json.dumps(cells, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
