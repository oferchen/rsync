#!/usr/bin/env python3
"""Test-seam gate: fast_io's `test-seams` feature stays out of shipped code.

`fast_io::test_seams` lets a test make the owner walk judge a planted symlink
as owned by another uid.  It must never reach a shipped build.  The compiler
enforces part of that already: the module exists only under `cfg(test)` or
the `test-seams` feature, and with resolver 2 a `[dev-dependencies]` feature
is not enabled for lib or bin targets.  Two holes remain, and this gate closes
them:

  * MANIFESTS.  A `[dependencies]`, `[target.*.dependencies]` or
    `[build-dependencies]` table that enables `test-seams`, or any
    `[features]` entry that forwards to it, would compile the seam into
    production.  Only fast_io's own `[features]` table may define it, and only
    `[dev-dependencies]` tables (plain or per-target) may enable it.

  * SOURCE.  A workspace-wide `--all-features` build turns the feature on for
    fast_io itself, so a non-test caller in another crate would compile there.
    Every `.rs` file that names `test_seams` or `test-seams` must match one of
    ALLOWED_SOURCE_PATTERNS - test directories and test modules, plus the
    three fast_io files that define and read the seam.

The gate is enforced by tools/tests/test_test_seam_gate.py, which runs it over
the repository in the tools-tests CI job; `python3 tools/ci/test_seam_gate.py`
runs the same check by hand.

The allow-list is an explicit, ordered tuple of glob patterns so that widening
it is a visible diff.  Patterns use `fnmatch` semantics on the POSIX path
relative to the repository root, where `*` also matches `/`.
"""

from __future__ import annotations

import argparse
import fnmatch
import re
import sys
import tomllib
from pathlib import Path

FEATURE = "test-seams"
OWNER_CRATE = "fast_io"

# Where a reference to the seam may appear.  Keep this list short and
# commented; every entry is a place a test, never production code, lives.
ALLOWED_SOURCE_PATTERNS: tuple[str, ...] = (
    # The module declaration in fast_io's crate root.
    "crates/fast_io/src/lib.rs",
    # The seam itself.
    "crates/fast_io/src/test_seams.rs",
    # The one place the walk reads it (`judged_symlink_owner`).
    "crates/fast_io/src/owner_walk.rs",
    # Integration tests: the root package and every crate.
    "tests/*",
    "crates/*/tests/*",
    # In-crate test modules: `tests.rs` files and `tests/` directories.
    "crates/*/src/tests.rs",
    "crates/*/src/*/tests.rs",
    "crates/*/src/tests/*",
    "crates/*/src/*/tests/*",
)

SOURCE_TOKEN = re.compile(r"\btest_seams\b|\btest-seams\b")

SKIPPED_DIRS = frozenset({"target", ".git"})

PRODUCTION_DEP_TABLES = ("dependencies", "build-dependencies")


def source_is_allowed(rel: str) -> bool:
    """True when the repository-relative POSIX path may reference the seam."""
    return any(fnmatch.fnmatchcase(rel, pattern) for pattern in ALLOWED_SOURCE_PATTERNS)


def manifests(root: Path) -> list[Path]:
    """The root manifest, every crates/<name>/Cargo.toml, and xtask's."""
    found = []
    top = root / "Cargo.toml"
    if top.is_file():
        found.append(top)
    crates_dir = root / "crates"
    if crates_dir.is_dir():
        for child in sorted(crates_dir.iterdir()):
            manifest = child / "Cargo.toml"
            if manifest.is_file():
                found.append(manifest)
    xtask = root / "xtask" / "Cargo.toml"
    if xtask.is_file():
        found.append(xtask)
    return found


def _enables_feature(spec: object) -> bool:
    return isinstance(spec, dict) and FEATURE in (spec.get("features") or [])


def _dep_table_findings(table: object, where: str) -> list[str]:
    if not isinstance(table, dict):
        return []
    return [
        f"{where}: dependency `{name}` enables `{FEATURE}`"
        for name, spec in table.items()
        if _enables_feature(spec)
    ]


def manifest_findings(manifest: Path, root: Path) -> list[str]:
    """Every way this manifest lets `test-seams` reach a production build."""
    data = tomllib.loads(manifest.read_text(encoding="utf-8"))
    rel = manifest.relative_to(root).as_posix()
    crate = data.get("package", {}).get("name", "")
    findings: list[str] = []

    for table in PRODUCTION_DEP_TABLES:
        findings += _dep_table_findings(data.get(table), f"{rel} [{table}]")
    target = data.get("target")
    if isinstance(target, dict):
        for cfg, spec in target.items():
            if not isinstance(spec, dict):
                continue
            for table in PRODUCTION_DEP_TABLES:
                findings += _dep_table_findings(
                    spec.get(table), f"{rel} [target.{cfg}.{table}]"
                )
    workspace_deps = data.get("workspace", {}).get("dependencies")
    findings += _dep_table_findings(workspace_deps, f"{rel} [workspace.dependencies]")

    features = data.get("features")
    if isinstance(features, dict):
        for name, members in features.items():
            if name == FEATURE and crate == OWNER_CRATE:
                continue
            if name == FEATURE:
                findings.append(f"{rel} [features]: defines `{FEATURE}` outside {OWNER_CRATE}")
            for member in members or []:
                if isinstance(member, str) and member.split("/")[-1] == FEATURE:
                    findings.append(
                        f"{rel} [features]: `{name}` forwards to `{member}`"
                    )
    return findings


def source_files(root: Path) -> list[Path]:
    """Every .rs file under the root, skipping build output and VCS data."""
    files = []
    for path in root.rglob("*.rs"):
        parts = path.relative_to(root).parts
        if any(part in SKIPPED_DIRS for part in parts):
            continue
        files.append(path)
    return sorted(files)


def source_findings(root: Path) -> list[str]:
    """Every .rs file outside the allow-list that names the seam."""
    findings = []
    for path in source_files(root):
        rel = path.relative_to(root).as_posix()
        if source_is_allowed(rel):
            continue
        text = path.read_text(encoding="utf-8", errors="replace")
        for lineno, line in enumerate(text.splitlines(), 1):
            if SOURCE_TOKEN.search(line):
                findings.append(f"{rel}:{lineno}: names the test seam outside a test path")
    return findings


def run_gate(root: Path) -> int:
    findings: list[str] = []
    for manifest in manifests(root):
        findings += manifest_findings(manifest, root)
    findings += source_findings(root)
    if findings:
        for finding in findings:
            print(f"::error::test-seam gate: {finding}", file=sys.stderr)
        print(
            f"test-seam gate: FAILED ({len(findings)} finding(s)); `{FEATURE}` may be "
            f"enabled only from [dev-dependencies] and named only on a test path",
            file=sys.stderr,
        )
        return 1
    print(
        f"test-seam gate: OK (`{FEATURE}` reaches no production table and no "
        f"non-test source)"
    )
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--root",
        type=Path,
        default=Path(__file__).resolve().parents[2],
        help="repository root to scan",
    )
    args = parser.parse_args(argv)
    return run_gate(args.root)


if __name__ == "__main__":
    sys.exit(main())
