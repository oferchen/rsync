#!/usr/bin/env python3
"""Select and audit the cargo-nextest cells that run against an upstream rsync.

Cross-implementation tests resolve their oracle through one function,
`test_support::require_upstream_rsync`. Without `OC_RSYNC_UPSTREAM_COMPAT=1`
it prints a skip line and the test passes, so a job that never built the
oracle reports green having compared nothing. The interop job builds the
oracles, sets the variable (a missing oracle then fails), and runs exactly the
integration-test binaries this script derives from the tree.

The binary set is derived, never pinned: every top-level `tests/*.rs` file
that calls the resolver, in the root `bin` package or under `crates/*/`. A
new oracle test is picked up on the next run.

Usage:
    python3 tools/ci/upstream_oracle_tests.py args
        Print the nextest selection, one argument per line
        (`-p <pkg>` ... `-E <filterset>`), for `mapfile`.
    python3 tools/ci/upstream_oracle_tests.py check-log <nextest-output>
        Fail unless the run executed at least one test and no oracle cell
        printed a skip line.

Exit status: 0 on success, 1 when check-log rejects the run, 2 when the tree
or the log could not be analysed.
"""
from __future__ import annotations

import argparse
import re
import sys
import tomllib
from pathlib import Path

RESOLVER_CALL = re.compile(r"\brequire_upstream_rsync\(")
SKIP_LINE = "Skipping upstream-compat test:"
TESTS_RUN = re.compile(r"^\s*Summary \[.*?\]\s+(\d+) tests? run", re.MULTILINE)


def package_name(manifest: Path) -> str:
    with manifest.open("rb") as handle:
        return tomllib.load(handle)["package"]["name"]


def oracle_binaries(root: Path) -> list[tuple[str, str]]:
    """Return sorted (package, test binary) pairs that call the resolver.

    Raises ValueError for a call in a nested test module, whose binary
    cannot be named from its path.
    """
    test_dirs = [root / "tests"] + sorted(root.glob("crates/*/tests"))
    cells: set[tuple[str, str]] = set()
    for tests in test_dirs:
        package = package_name(tests.parent / "Cargo.toml")
        for source in sorted(tests.rglob("*.rs")):
            if not RESOLVER_CALL.search(source.read_text(encoding="utf-8")):
                continue
            if source.parent != tests:
                raise ValueError(
                    f"{source.relative_to(root)} calls require_upstream_rsync from a "
                    "nested test module; call it from the top-level test file"
                )
            cells.add((package, source.stem))
    return sorted(cells)


def nextest_args(cells: list[tuple[str, str]]) -> list[str]:
    args: list[str] = []
    for package in sorted({package for package, _ in cells}):
        args += ["-p", package]
    filterset = " | ".join(f"binary_id({package}::{binary})" for package, binary in cells)
    return args + ["-E", filterset]


def check_log(text: str) -> list[str]:
    """Return the reasons the oracle run must be rejected (empty when sound)."""
    problems = []
    skipped = [line.strip() for line in text.splitlines() if SKIP_LINE in line]
    if skipped:
        problems.append(f"{len(skipped)} oracle cell(s) skipped: {skipped[:5]}")
    counts = [int(count) for count in TESTS_RUN.findall(text)]
    if not counts:
        problems.append("no nextest summary line found")
    elif counts[-1] == 0:
        problems.append("nextest ran 0 tests")
    return problems


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("args")
    log = sub.add_parser("check-log")
    log.add_argument("log", type=Path)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    options = parser.parse_args(argv)

    if options.command == "args":
        try:
            cells = oracle_binaries(options.root)
        except ValueError as error:
            print(f"error: {error}", file=sys.stderr)
            return 2
        if not cells:
            print("error: no test calls require_upstream_rsync", file=sys.stderr)
            return 2
        print("\n".join(nextest_args(cells)))
        return 0

    try:
        text = options.log.read_text(encoding="utf-8", errors="replace")
    except OSError as error:
        print(f"error: cannot read {options.log}: {error}", file=sys.stderr)
        return 2
    problems = check_log(text)
    for problem in problems:
        print(f"error: {problem}", file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
