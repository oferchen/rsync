#!/usr/bin/env python3
"""Debuginfo gate: every CI job that compiles in a debug profile uses line-tables-only.

ci.yml sets `CARGO_PROFILE_DEV_DEBUG` and `CARGO_PROFILE_TEST_DEBUG` to
`line-tables-only` at workflow level. A called (`workflow_call`) workflow does
not inherit its caller's workflow-level env, so a reusable workflow that
compiles without setting them itself builds with full debuginfo. The
`--workspace` test build of the feature matrix's default-features row did
exactly that and ran the runner out of disk (ENOSPC while linking, #8804).

A job "compiles in a debug profile" when one of its `run:` steps calls
`cargo build`, `cargo test`, `cargo run`, `cargo nextest run` or
`cargo nextest archive` without `--release` or a `--profile` other than
dev, test or ci. clippy, check and doc link no test binaries and are not
counted. Such a job must see both variables, through its workflow-level or
job-level env, at the value ci.yml sets.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path
from typing import Iterator

import yaml

DEBUG_VARS = ("CARGO_PROFILE_DEV_DEBUG", "CARGO_PROFILE_TEST_DEBUG")
REFERENCE = "ci.yml"

CARGO = re.compile(r"\bcargo\s+(?:\+\S+\s+)?(?:build|test|run|nextest\s+(?:run|archive))\b([^\n]*)")
NON_DEBUG_PROFILE = re.compile(r"--release\b|--(?:cargo-)?profile[ =](?!dev\b|test\b|ci\b)\S+")


def load(path: Path) -> dict:
    return yaml.safe_load(path.read_text(encoding="utf-8")) or {}


def is_called(workflow: dict) -> bool:
    # PyYAML reads the bare `on:` key as boolean True.
    on = workflow.get(True, workflow.get("on"))
    return isinstance(on, dict) and "workflow_call" in on


def debug_build_jobs(workflow: dict) -> Iterator[tuple[str, dict]]:
    for job_id, job in (workflow.get("jobs") or {}).items():
        runs = "\n".join(str(step.get("run", "")) for step in job.get("steps") or [])
        if any(not NON_DEBUG_PROFILE.search(m.group(1)) for m in CARGO.finditer(runs)):
            yield job_id, job


def reference_levels(workflows: Path) -> dict[str, str | None]:
    env = load(workflows / REFERENCE).get("env") or {}
    return {var: env.get(var) for var in DEBUG_VARS}


def violations(workflows: Path) -> list[str]:
    want = reference_levels(workflows)
    found = [
        f"{REFERENCE} does not set {var} at workflow level; it is the reference level"
        for var, value in want.items()
        if not value
    ]
    for path in sorted(workflows.glob("*.yml")):
        workflow = load(path)
        for job_id, job in debug_build_jobs(workflow):
            env = {**(workflow.get("env") or {}), **(job.get("env") or {})}
            for var in DEBUG_VARS:
                if want[var] and env.get(var) != want[var]:
                    hint = "; a called workflow does not inherit its caller's env" if is_called(workflow) else ""
                    found.append(
                        f"{path.name} job {job_id} builds in a debug profile with {var}={env.get(var)!r}, "
                        f"expected {want[var]!r}{hint}"
                    )
    return found


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="command", required=True)
    gate = sub.add_parser("gate", help="fail when a debug-profile job lacks the debuginfo level")
    gate.add_argument("--workflows", type=Path, default=Path(".github/workflows"))
    args = parser.parse_args(argv)

    found = violations(args.workflows)
    for line in found:
        print(f"::error::{line}")
    if found:
        return 1
    print(f"debuginfo gate: every debug-profile job in {args.workflows} uses {REFERENCE}'s level")
    return 0


if __name__ == "__main__":
    sys.exit(main())
