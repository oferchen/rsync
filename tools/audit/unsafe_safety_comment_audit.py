#!/usr/bin/env python3
"""Audit `unsafe { ... }` blocks across `crates/` for SAFETY comments.

The project coding standards require every `unsafe { ... }` expression block to be preceded by a
SAFETY comment explaining the invariants the caller upholds. This script
enumerates every block under `crates/` and reports either:

- `missing`: no `SAFETY:` (or lower-case `Safety:`/`safety:`) comment found in
  the 15 lines preceding the block (scan stops at `fn`/`impl`/`mod` boundaries
  but tolerates intermediate code so a single SAFETY note can cover several
  `match` arms or `if`/`else` branches).
- `placeholder`: the comment body is empty or matches `todo`/`fixme`/`tbd`/`n/a`.

The script also tags each crate as `permitted` or `NOT PERMITTED` based on the
project's unsafe-code policy.

Two modes:

- `report` (default): print the per-scope block counts and every violation.
  Always exits 0; this is the human-readable inventory.
- `ratchet`: compare the per-scope violation counts against the frozen
  baseline (`unsafe_safety_comment_baseline.tsv`) and exit NON-ZERO when any
  scope EXCEEDS its baseline. This is the CI gate: it enforces "no new
  unsafe-without-SAFETY" without demanding the outstanding violations be driven
  to zero first. A DECREASE never fails; it prints a nudge to lower the
  baseline and lock in the improvement.

Usage:

    python3 tools/audit/unsafe_safety_comment_audit.py            # report
    python3 tools/audit/unsafe_safety_comment_audit.py ratchet    # CI gate
"""

from __future__ import annotations

import argparse
import re
import sys
from collections import defaultdict
from pathlib import Path

PERMITTED = {"fast_io", "metadata", "checksums", "engine", "protocol"}
# The coding standards name an explicit never-contain-unsafe set. A crate that is
# on neither list is UNLISTED, not forbidden - reporting those two the same way
# turns ordinary dev-only crates into apparent policy violations.
FORBIDDEN = {
    "daemon", "cli", "core", "transfer", "batch", "filters", "signature",
    "bandwidth", "logging", "logging-sink", "branding", "rsync_io", "compress",
}

# Repository root, inferred from this file's location so the gate works from any
# cwd. The per-scope baseline lives next to this script.
REPO_ROOT = Path(__file__).resolve().parents[2]
BASELINE = Path(__file__).resolve().parent / "unsafe_safety_comment_baseline.tsv"


def classify(crate: str) -> str:
    if crate in PERMITTED:
        return "permitted"
    if crate in FORBIDDEN:
        return "FORBIDDEN"
    return "unlisted"


def scope_of(rel: Path) -> str:
    """Crate name, qualified by cargo target.

    `rel` is a path relative to the repository root, i.e. `crates/<c>/...`.

    This audit answers two questions with different populations, and one walk
    can serve both only while they stay distinct:

      - *which crates may contain unsafe* is about the LIBRARY;
      - *does every unsafe block carry a SAFETY comment* is about ALL compiled
        unsafe, because a test can invoke UB just as well as a library can.

    `crates/<c>/tests/*.rs` and `crates/<c>/benches/*.rs` are their own crate
    roots, so the library's `#![deny(unsafe_code)]` does not reach them. Unsafe
    found there is real, and it is not unsafe *in the library*. Do not re-merge
    these keys: collapsing them makes a test binary look like a policy breach.
    """
    crate = rel.parts[1]
    if len(rel.parts) > 2 and rel.parts[2] in ("tests", "benches"):
        return f"{crate} ({rel.parts[2]})"
    return crate

UNSAFE_BLOCK_RE = re.compile(r"\bunsafe\s*\{")
UNSAFE_FN_RE = re.compile(r"\bunsafe\s+fn\b")
UNSAFE_TRAIT_RE = re.compile(r"\bunsafe\s+trait\b")
UNSAFE_IMPL_RE = re.compile(r"\bunsafe\s+impl\b")
SAFETY_RE = re.compile(r"\b(?:SAFETY|Safety|safety)\s*:")
SAFETY_BODY_RE = re.compile(r"(?:SAFETY|Safety|safety)\s*:\s*(.*)$")
SCOPE_BOUNDARY_RE = re.compile(r"^\s*(fn |pub\s+fn |pub\s*\(.*\)\s*fn |impl\b|mod\b)")


def is_unsafe_block(line: str) -> bool:
    if not UNSAFE_BLOCK_RE.search(line):
        return False
    if UNSAFE_FN_RE.search(line) or UNSAFE_TRAIT_RE.search(line) or UNSAFE_IMPL_RE.search(line):
        return False
    stripped = line.lstrip()
    if stripped.startswith("//!") or stripped.startswith("///"):
        return False
    return True


def safety_state(lines: list[str], block_idx: int) -> tuple[bool, bool]:
    """Returns (has_safety, is_placeholder)."""
    checked = 0
    j = block_idx - 1
    while j >= 0 and checked < 15:
        prev = lines[j].strip()
        if prev == "":
            j -= 1
            continue
        checked += 1
        if SAFETY_RE.search(prev):
            body = ""
            body_match = SAFETY_BODY_RE.search(prev)
            if body_match:
                body = body_match.group(1).strip()
                # Walk up: collect continuation `//` comment lines above.
                k = j - 1
                while k >= 0:
                    pl = lines[k].strip()
                    if pl.startswith("//") and not SAFETY_RE.search(pl) and pl != "//":
                        body = pl.lstrip("/").lstrip() + " " + body
                        k -= 1
                    else:
                        break
                # Walk down: collect continuation `//` lines below.
                k = j + 1
                while k < block_idx:
                    pl = lines[k].strip()
                    if pl.startswith("//") and not SAFETY_RE.search(pl):
                        body = body + " " + pl.lstrip("/").lstrip()
                        k += 1
                    else:
                        break
            body = body.strip()
            placeholder = not body or body.lower() in {"todo", "fixme", "tbd", "n/a"} or len(body) < 8
            return True, placeholder
        if SCOPE_BOUNDARY_RE.match(prev):
            break
        j -= 1
    return False, False


def collect(root: Path) -> tuple[dict[str, int], dict[str, int], list[tuple[str, int, str, str]]]:
    """Walk `root/crates` and return (files-per-scope, blocks-per-scope, violations).

    Each violation is (relative-path, 1-based line, kind, snippet). Paths are
    relative to `root` so the report reads `crates/...` regardless of cwd.
    """
    crates = root / "crates"
    per_crate_files: dict[str, int] = defaultdict(int)
    per_crate_blocks: dict[str, int] = defaultdict(int)
    violations: list[tuple[str, int, str, str]] = []

    for path in sorted(crates.rglob("*.rs")):
        try:
            text = path.read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue
        if "unsafe {" not in text:
            continue
        rel = path.relative_to(root)
        scope = scope_of(rel)
        per_crate_files[scope] += 1
        lines = text.split("\n")
        for i, line in enumerate(lines):
            if not is_unsafe_block(line):
                continue
            per_crate_blocks[scope] += 1
            has_safety, placeholder = safety_state(lines, i)
            if not has_safety:
                violations.append((str(rel), i + 1, "missing", line.strip()))
            elif placeholder:
                violations.append((str(rel), i + 1, "placeholder", line.strip()))

    return per_crate_files, per_crate_blocks, violations


def violations_by_scope(violations: list[tuple[str, int, str, str]]) -> dict[str, int]:
    """Count violations per crate-qualified scope, keyed exactly as the baseline."""
    counts: dict[str, int] = defaultdict(int)
    for path, _line, _kind, _snippet in violations:
        counts[scope_of(Path(path))] += 1
    return dict(counts)


def parse_baseline(path: Path) -> tuple[dict[str, int], list[str]]:
    """Parse `scope<TAB>count` rows; return (counts, errors).

    Blank lines and `#` comments are ignored. Each data row must be exactly two
    tab-separated fields: a scope key (as emitted by `scope_of`, so it may
    contain spaces, e.g. `fast_io (tests)`) and a non-negative integer.
    """
    entries: dict[str, int] = {}
    errors: list[str] = []
    if not path.exists():
        return entries, [f"baseline not found: {path}"]
    for lineno, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        parts = raw.split("\t")
        if len(parts) != 2:
            errors.append(f"{path.name}:{lineno}: expected 2 tab-separated fields (scope<TAB>count)")
            continue
        scope, count = (p.strip() for p in parts)
        try:
            n = int(count)
        except ValueError:
            errors.append(f"{path.name}:{lineno}: {scope}: count {count!r} is not an integer")
            continue
        if n < 0:
            errors.append(f"{path.name}:{lineno}: {scope}: count {n} is negative")
            continue
        if scope in entries:
            errors.append(f"{path.name}:{lineno}: duplicate entry for {scope}")
            continue
        entries[scope] = n
    return entries, errors


def run_report(root: Path) -> int:
    if not (root / "crates").is_dir():
        print(f"error: expected {root / 'crates'}/ to exist", file=sys.stderr)
        return 2

    per_crate_files, per_crate_blocks, violations = collect(root)

    print("=== Per-crate unsafe block counts ===")
    for crate in sorted(per_crate_blocks, key=lambda c: -per_crate_blocks[c]):
        base = crate.split(" (")[0]
        permit = (
            f"{classify(base)}; test/bench target, library policy N/A"
            if "(" in crate
            else classify(base)
        )
        print(
            f"  {crate}: {per_crate_blocks[crate]} blocks across {per_crate_files[crate]} files ({permit})"
        )

    print(f"\nTotal blocks: {sum(per_crate_blocks.values())}")
    print(f"Total violations: {len(violations)}")

    # An UNCLASSIFIED crate is an outcome in its own right, not a pass and not a
    # failure: the coding standards list crates that may hold unsafe and crates
    # that may not, and say nothing about the rest. Reporting the count and the
    # names keeps a newly-added crate carrying unsafe visible instead of
    # silently benign - the state to watch is this number growing.
    unclassified = sorted(
        {c.split(" (")[0] for c in per_crate_blocks if classify(c.split(" (")[0]) == "unlisted"}
    )
    print(f"Unclassified crates carrying unsafe: {len(unclassified)}")
    for crate in unclassified:
        print(f"  {crate} - on neither the permitted nor the forbidden list")
    print()

    print("=== Violations (missing or placeholder SAFETY) ===")
    by_crate: dict[str, list[tuple[str, int, str, str]]] = defaultdict(list)
    for v in violations:
        by_crate[scope_of(Path(v[0]))].append(v)
    for crate in sorted(by_crate):
        print(f"\n--- crate: {crate} ({len(by_crate[crate])} violations) ---")
        for path, line, kind, snippet in by_crate[crate]:
            print(f"  {path}:{line} [{kind}] {snippet[:80]}")

    return 0


def run_ratchet(root: Path, baseline_path: Path) -> int:
    """Fail when any scope's violation count EXCEEDS its baseline.

    This is a ratchet, not a cleanup: outstanding violations are tolerated at
    their baseline level, but a NEW unsafe block without a SAFETY comment - or a
    new crate that carries one - pushes a scope above its baseline and fails the
    gate, naming the scope and the delta. A decrease never fails; it prints a
    nudge to lower the baseline.
    """
    if not (root / "crates").is_dir():
        print(f"error: expected {root / 'crates'}/ to exist", file=sys.stderr)
        return 2

    _files, _blocks, violations = collect(root)
    current = violations_by_scope(violations)
    baseline, problems = parse_baseline(baseline_path)
    nudges: list[str] = []

    for scope in sorted(current):
        cur = current[scope]
        base = baseline.get(scope)
        if base is None:
            problems.append(
                f"{scope}: {cur} unsafe block(s) without a SAFETY comment, but "
                f"no baseline entry. Add the SAFETY comment(s), or record a "
                f"baseline row deliberately."
            )
        elif cur > base:
            problems.append(
                f"{scope}: {cur} violations exceeds baseline {base} "
                f"(+{cur - base}). New unsafe block(s) missing a SAFETY comment "
                f"- add the comment rather than raising the baseline."
            )
        elif cur < base:
            nudges.append(
                f"{scope}: {cur} < baseline {base}; lower the baseline to {cur} "
                f"to lock in the improvement."
            )

    for scope in sorted(baseline):
        if scope not in current and baseline[scope] != 0:
            nudges.append(
                f"{scope}: 0 violations now (baseline {baseline[scope]}); set the "
                f"baseline row to 0 or remove it."
            )

    if problems:
        print(f"unsafe SAFETY ratchet: {len(problems)} regression(s)", file=sys.stderr)
        for p in problems:
            print(f"error: {p}", file=sys.stderr)
        return 1

    total = sum(current.values())
    print(
        f"unsafe SAFETY ratchet: OK ({total} violation(s) across "
        f"{len(current)} scope(s), none above baseline)"
    )
    for n in nudges:
        print(f"nudge: {n}")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "mode",
        choices=["report", "ratchet"],
        nargs="?",
        default="report",
        help="report: print the inventory (default); ratchet: enforce the baseline (CI)",
    )
    parser.add_argument("--root", type=Path, default=REPO_ROOT, help="repository root to scan")
    parser.add_argument("--baseline", type=Path, default=BASELINE)
    args = parser.parse_args(argv)

    if args.mode == "ratchet":
        return run_ratchet(args.root, args.baseline)
    return run_report(args.root)


if __name__ == "__main__":
    sys.exit(main())
