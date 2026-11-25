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

A cell that needs a job of its own (a release-profile benchmark, a long fuzz
run) is listed in EXCLUDED with the workflow job that runs it instead. `args`
refuses to run while an entry is stale, so an exclusion cannot quietly turn
into a cell nothing runs: the file must exist, still call the resolver, and
be named by its job.

Usage:
    python3 tools/ci/upstream_oracle_tests.py args
        Print the nextest selection, one argument per line
        (`-p <pkg>` ... `--test <binary>` ... `-E <filterset>`), for `mapfile`.
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
ANSI_ESCAPE = re.compile(r"\x1b\[[0-9;]*[A-Za-z]")
JOB_KEY = re.compile(r"^  ([A-Za-z0-9_-]+):\s*$")

# Oracle cells kept out of the interop oracle step, each mapped to the
# (workflow file, job id) that runs it.
EXCLUDED: dict[str, tuple[str, str]] = {
    "tests/live_interop_fuzz_1196.rs": (".github/workflows/ci-nightly.yml", "live-interop-fuzz"),
    "tests/ssh_push_throughput_regression_ssr1.rs": (".github/workflows/ci.yml", "ssh-throughput"),
}


def package_name(manifest: Path) -> str:
    with manifest.open("rb") as handle:
        return tomllib.load(handle)["package"]["name"]


def oracle_binaries(
    root: Path, excluded: dict[str, tuple[str, str]] | None = None
) -> list[tuple[str, str]]:
    """Return sorted (package, test binary) pairs that call the resolver.

    Sources listed in `excluded` (paths relative to `root`) are left out.
    Raises ValueError for a call in a nested test module, whose binary
    cannot be named from its path.
    """
    skip = set(EXCLUDED if excluded is None else excluded)
    test_dirs = [root / "tests"] + sorted(root.glob("crates/*/tests"))
    cells: set[tuple[str, str]] = set()
    for tests in test_dirs:
        package = package_name(tests.parent / "Cargo.toml")
        for source in sorted(tests.rglob("*.rs")):
            if source.relative_to(root).as_posix() in skip:
                continue
            if not RESOLVER_CALL.search(source.read_text(encoding="utf-8")):
                continue
            if source.parent != tests:
                raise ValueError(
                    f"{source.relative_to(root)} calls require_upstream_rsync from a "
                    "nested test module; call it from the top-level test file"
                )
            cells.add((package, source.stem))
    return sorted(cells)


def job_block(workflow: str, job: str) -> str | None:
    """Return the text of `job` under the workflow's top-level `jobs:` map."""
    lines = workflow.splitlines()
    try:
        start = lines.index("jobs:") + 1
    except ValueError:
        return None
    block: list[str] | None = None
    for line in lines[start:]:
        if line and not line.startswith(" "):
            break
        key = JOB_KEY.match(line)
        if key:
            if block is not None:
                break
            if key.group(1) == job:
                block = []
            continue
        if block is not None:
            block.append(line)
    return None if block is None else "\n".join(block)


def stale_exclusions(
    root: Path, excluded: dict[str, tuple[str, str]] | None = None
) -> list[str]:
    """Return why each exclusion is stale (empty when every entry holds).

    An entry holds when its source exists, still calls the resolver, and the
    named workflow job mentions the source's test binary.
    """
    problems = []
    for source, (workflow, job) in sorted((EXCLUDED if excluded is None else excluded).items()):
        path = root / source
        if not path.is_file():
            problems.append(f"excluded {source} no longer exists")
            continue
        if not RESOLVER_CALL.search(path.read_text(encoding="utf-8")):
            problems.append(f"excluded {source} no longer calls require_upstream_rsync")
            continue
        workflow_path = root / workflow
        if not workflow_path.is_file():
            problems.append(f"excluded {source}: workflow {workflow} does not exist")
            continue
        block = job_block(workflow_path.read_text(encoding="utf-8"), job)
        if block is None:
            problems.append(f"excluded {source}: {workflow} has no job {job}")
        elif not re.search(rf"\b{re.escape(path.stem)}\b", block):
            problems.append(f"excluded {source}: job {job} in {workflow} does not run it")
    return problems


def nextest_args(cells: list[tuple[str, str]]) -> list[str]:
    """Return the nextest selection: packages, test targets, then the filterset.

    `--test` limits what cargo builds. `-p` alone compiles every integration
    test of each package, several hundred binaries the filterset then skips.
    The filterset stays because `--test` matches a name in any selected
    package.
    """
    args: list[str] = []
    for package in sorted({package for package, _ in cells}):
        args += ["-p", package]
    for binary in sorted({binary for _, binary in cells}):
        args += ["--test", binary]
    filterset = " | ".join(f"binary_id({package}::{binary})" for package, binary in cells)
    return args + ["-E", filterset]


def check_log(text: str) -> list[str]:
    """Return the reasons the oracle run must be rejected (empty when sound)."""
    text = ANSI_ESCAPE.sub("", text)
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
        stale = stale_exclusions(options.root)
        for problem in stale:
            print(f"error: {problem}", file=sys.stderr)
        if stale:
            return 2
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
