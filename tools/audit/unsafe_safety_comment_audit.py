#!/usr/bin/env python3
"""Audit `unsafe { ... }` blocks across `crates/` for SAFETY comments.

CLAUDE.md requires every `unsafe { ... }` expression block to be preceded by a
SAFETY comment explaining the invariants the caller upholds. This script
enumerates every block under `crates/` and reports either:

- `missing`: no `SAFETY:` (or lower-case `Safety:`/`safety:`) comment found in
  the 15 lines preceding the block (scan stops at `fn`/`impl`/`mod` boundaries
  but tolerates intermediate code so a single SAFETY note can cover several
  `match` arms or `if`/`else` branches).
- `placeholder`: the comment body is empty or matches `todo`/`fixme`/`tbd`/`n/a`.

The script also tags each crate as `permitted` or `NOT PERMITTED` based on the
CLAUDE.md unsafe-code policy.

Usage:

    python3 tools/audit/unsafe_safety_comment_audit.py
"""

from __future__ import annotations

import re
import sys
from collections import defaultdict
from pathlib import Path

ROOT = Path("crates")
PERMITTED = {"fast_io", "metadata", "checksums", "engine", "protocol"}

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


def main() -> int:
    if not ROOT.is_dir():
        print(f"error: run from workspace root (expected {ROOT}/)", file=sys.stderr)
        return 2

    per_crate_files: dict[str, int] = defaultdict(int)
    per_crate_blocks: dict[str, int] = defaultdict(int)
    violations: list[tuple[str, int, str, str]] = []

    for path in sorted(ROOT.rglob("*.rs")):
        try:
            text = path.read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue
        if "unsafe {" not in text:
            continue
        crate = path.parts[1]
        per_crate_files[crate] += 1
        lines = text.split("\n")
        for i, line in enumerate(lines):
            if not is_unsafe_block(line):
                continue
            per_crate_blocks[crate] += 1
            has_safety, placeholder = safety_state(lines, i)
            if not has_safety:
                violations.append((str(path), i + 1, "missing", line.strip()))
            elif placeholder:
                violations.append((str(path), i + 1, "placeholder", line.strip()))

    print("=== Per-crate unsafe block counts ===")
    for crate in sorted(per_crate_blocks, key=lambda c: -per_crate_blocks[c]):
        permit = "permitted" if crate in PERMITTED else "NOT PERMITTED"
        print(
            f"  {crate}: {per_crate_blocks[crate]} blocks across {per_crate_files[crate]} files ({permit})"
        )

    print(f"\nTotal blocks: {sum(per_crate_blocks.values())}")
    print(f"Total violations: {len(violations)}")
    print()

    print("=== Violations (missing or placeholder SAFETY) ===")
    by_crate: dict[str, list[tuple[str, int, str, str]]] = defaultdict(list)
    for v in violations:
        by_crate[Path(v[0]).parts[1]].append(v)
    for crate in sorted(by_crate):
        print(f"\n--- crate: {crate} ({len(by_crate[crate])} violations) ---")
        for path, line, kind, snippet in by_crate[crate]:
            print(f"  {path}:{line} [{kind}] {snippet[:80]}")

    return 0


if __name__ == "__main__":
    sys.exit(main())
