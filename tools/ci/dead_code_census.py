#!/usr/bin/env python3
"""Census of unwired public functions, and the blocking gate over its strictest tier.

The census finds every `pub fn` whose name has exactly one production
definition and classifies each reference to that name.  The gate fails when a
function has ZERO production callers AND ZERO test references and is not
carried by the allowlist (tools/ci/dead_code_allowlist.tsv), where every entry
needs a reason and an expiry date.  Expired entries fail the gate loudly, and
so do entries whose function has since been wired, tested, or deleted - a
stale row is removed in the same change that made it stale.

The gate deliberately covers ONLY the zero-caller-zero-test tier.  The wider
zero-caller tier (hundreds of rows) is not ratcheted: a frozen baseline that
large hides the one new dead function that matters inside it.

Three classification rules are load-bearing, each one a measured defect in an
earlier version of this instrument:

  1. A `use` / `pub use` line is NOT a call site.  Counting re-export lines as
     callers previously hid 236 of 827 zero-caller functions.
  2. `#[cfg(test)]` is honoured at ITEM granularity: the attribute masks the
     one item that follows it (a `mod tests { .. }` block, a single fn, or a
     `use` line), never the whole file, and never less than the item.
  3. Trait impl methods are EXCLUDED from the census: they are reached through
     dynamic or generic dispatch, so textual reference counting cannot see
     their callers.

References are counted textually (identifier tokens outside strings and
comments), so a name with more than one production definition is ambiguous and
is excluded, as are a few generic method names that appear on many types.
That makes the census a strict UNDER-approximation of dead code - every row it
reports is a name whose only textual occurrences are its own definition.
"""

from __future__ import annotations

import argparse
import datetime as _dt
import os
import re
import sys
from collections import defaultdict
from pathlib import Path

ALLOWLIST = Path(__file__).resolve().parent / "dead_code_allowlist.tsv"

def skip_dir(name: str) -> bool:
    """Build output and hidden directories are never scanned - a hidden
    directory can nest a whole second checkout under the repository root."""
    return name == "target" or name.startswith(".")

# Method names so generic that a single textual token cannot be attributed to
# one definition even when only one production `pub fn` spells it (trait
# methods and inherent methods on foreign types share these spellings).  Part
# of the measured tier definition - widening or narrowing this set changes the
# population, so change it deliberately or not at all.
GENERIC_NAMES = frozenset(
    """new default fmt from into try_from try_into clone drop next len is_empty iter
    as_ref as_mut deref eq hash cmp partial_cmp main build get set push pop insert remove
    contains extend name value kind path id code size mode flags inner""".split()
)

CFG_TEST_RE = re.compile(r"#\[cfg\((?:all\()?\s*test\b")
PUB_FN_RE = re.compile(
    r"^\s*pub(?:\s*\([^)]*\))?\s+"
    r"(?:default\s+)?(?:const\s+)?(?:async\s+)?(?:unsafe\s+)?"
    r'(?:extern\s+"[^"]*"\s+)?fn\s+(?P<name>[A-Za-z_][A-Za-z0-9_]*)'
)
IMPL_RE = re.compile(r"^\s*(?:unsafe\s+)?impl\b(?P<rest>.*)$")
USE_RE = re.compile(r"^\s*(?:pub(?:\s*\([^)]*\))?\s+)?use\s")
IDENT_RE = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")
CHAR_LIT_RE = re.compile(r"'(?:[^'\\\n]|\\(?:.|u\{[0-9a-fA-F_]+\}))'")


def mask_source(text: str) -> str:
    """Blank strings and comments, preserving line structure.

    Handles line comments (and thus doc comments), nested block comments, and
    string / raw-string / char literals so that brace counting and identifier
    extraction never read quoted or commented text.  Blanked bytes become
    spaces; newlines survive so line numbers stay aligned.
    """
    out = list(text)
    i = 0
    n = len(text)

    def blank(start: int, end: int) -> None:
        for k in range(start, end):
            if out[k] != "\n":
                out[k] = " "

    while i < n:
        c = text[i]
        if c == "/" and i + 1 < n and text[i + 1] == "/":
            j = text.find("\n", i)
            j = n if j == -1 else j
            blank(i, j)
            i = j
        elif c == "/" and i + 1 < n and text[i + 1] == "*":
            depth = 1
            j = i + 2
            while j < n and depth:
                if text.startswith("/*", j):
                    depth += 1
                    j += 2
                elif text.startswith("*/", j):
                    depth -= 1
                    j += 2
                else:
                    j += 1
            blank(i, j)
            i = j
        elif c == "r" and i + 1 < n and text[i + 1] in '#"':
            m = re.match(r'r(#*)"', text[i:])
            if not m:
                i += 1
                continue
            closer = '"' + m.group(1)
            j = text.find(closer, i + m.end())
            j = n if j == -1 else j + len(closer)
            blank(i, j)
            i = j
        elif c == '"':
            j = i + 1
            while j < n:
                if text[j] == "\\":
                    j += 2
                elif text[j] == '"':
                    j += 1
                    break
                else:
                    j += 1
            blank(i, j)
            i = j
        elif c == "'":
            m = CHAR_LIT_RE.match(text, i)
            if m:
                blank(i, m.end())
                i = m.end()
            else:
                i += 1  # a lifetime, not a literal
        else:
            i += 1
    return "".join(out)


def is_test_path(rel: str) -> bool:
    """Whole files that are test-side by location or naming convention."""
    parts = rel.split("/")
    if parts[0] in ("tests", "fuzz"):
        return True
    if "benches" in parts or "tests" in parts[:-1]:
        return True
    base = parts[-1]
    if base == "tests.rs" or base.startswith("test_") or base.endswith("_tests.rs"):
        return True
    if rel.startswith("crates/test-support/"):
        return True
    return False


def cfg_test_mask(lines: list[str]) -> list[bool]:
    """Per-line mask of item-granular `#[cfg(test)]` regions.

    From an attribute line, the mask extends over the one item that follows:
    to the close of its brace block, or to the terminating `;` for a braceless
    item.  Lines before the attribute and after the item stay unmasked.
    """
    mask = [False] * len(lines)
    i = 0
    n = len(lines)
    while i < n:
        if not CFG_TEST_RE.search(lines[i]):
            i += 1
            continue
        depth = 0
        started = False
        j = i
        while j < n:
            mask[j] = True
            depth += lines[j].count("{") - lines[j].count("}")
            if depth > 0:
                started = True
            if started and depth <= 0:
                break
            if not started and j > i and ";" in lines[j]:
                break
            j += 1
        i = j + 1
    return mask


def use_mask(lines: list[str]) -> list[bool]:
    """Per-line mask of `use` items, including multi-line braced groups."""
    mask = [False] * len(lines)
    i = 0
    n = len(lines)
    while i < n:
        if not USE_RE.match(lines[i]):
            i += 1
            continue
        depth = 0
        j = i
        while j < n:
            mask[j] = True
            depth += lines[j].count("{") - lines[j].count("}")
            if depth <= 0 and lines[j].rstrip().endswith(";"):
                break
            j += 1
        i = j + 1
    return mask


class SourceFile:
    def __init__(self, rel: str, text: str) -> None:
        self.rel = rel
        self.is_test = is_test_path(rel)
        self.lines = mask_source(text).split("\n")
        self.test_mask = (
            [True] * len(self.lines) if self.is_test else cfg_test_mask(self.lines)
        )
        self.use_mask = use_mask(self.lines)


def collect_sources(root: Path) -> list[SourceFile]:
    sources = []
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = sorted(d for d in dirnames if not skip_dir(d))
        for fname in sorted(filenames):
            if not fname.endswith(".rs"):
                continue
            path = Path(dirpath) / fname
            rel = path.relative_to(root).as_posix()
            try:
                text = path.read_text(encoding="utf-8", errors="replace")
            except OSError:
                continue
            sources.append(SourceFile(rel, text))
    return sources


def scan_defs(sources: list[SourceFile]) -> dict[str, list[dict]]:
    """All production `pub fn` definitions, flagged when inside a trait impl."""
    defs: dict[str, list[dict]] = defaultdict(list)
    for src in sources:
        if src.is_test:
            continue
        impl_stack: list[tuple[int, bool]] = []  # (entry depth, is_trait_impl)
        depth = 0
        for idx, line in enumerate(src.lines):
            pending_impl = None
            m = IMPL_RE.match(line)
            if m and "{" in line:
                head = m.group("rest").split("{")[0]
                pending_impl = " for " in head
            d = PUB_FN_RE.match(line)
            if d and not src.test_mask[idx]:
                defs[d.group("name")].append(
                    {
                        "file": src.rel,
                        "line": idx + 1,
                        "trait_impl": any(t[1] for t in impl_stack),
                    }
                )
            opens = line.count("{")
            closes = line.count("}")
            if pending_impl is not None and opens:
                impl_stack.append((depth, pending_impl))
            depth += opens - closes
            while impl_stack and depth <= impl_stack[-1][0]:
                impl_stack.pop()
    return defs


def scan_refs(
    sources: list[SourceFile], defs: dict[str, list[dict]]
) -> dict[str, dict[str, int]]:
    """Count references per name: production call sites, test refs, re-exports.

    A reference is an identifier token outside strings/comments that is not on
    the definition's own line and not inside a `use` item.  `use` lines are
    tallied separately as re-exports - they are never call sites.
    """
    wanted = set(defs)
    def_sites = defaultdict(set)
    for name, dlist in defs.items():
        for d in dlist:
            def_sites[name].add((d["file"], d["line"]))
    refs = {name: {"prod": 0, "test": 0, "reexport": 0} for name in wanted}
    for src in sources:
        for idx, line in enumerate(src.lines):
            hit = set(IDENT_RE.findall(line)) & wanted
            if not hit:
                continue
            for name in hit:
                if (src.rel, idx + 1) in def_sites[name]:
                    continue
                # A use line is never a call site, wherever it sits - a
                # cfg(test) import without a call is not test evidence either.
                if src.use_mask[idx]:
                    refs[name]["reexport"] += 1
                elif src.test_mask[idx]:
                    refs[name]["test"] += 1
                else:
                    refs[name]["prod"] += 1
    return refs


def census(root: Path) -> list[dict]:
    """The strictest tier: single-definition pub fns with zero production
    callers and zero test references."""
    sources = collect_sources(root)
    defs = scan_defs(sources)
    refs = scan_refs(sources, defs)
    rows = []
    for name, dlist in defs.items():
        if len(dlist) != 1:
            continue
        d = dlist[0]
        if d["trait_impl"] or name in GENERIC_NAMES or name.startswith("_"):
            continue
        r = refs[name]
        if r["prod"] == 0 and r["test"] == 0:
            rows.append(
                {
                    "name": name,
                    "file": d["file"],
                    "line": d["line"],
                    "reexport": r["reexport"],
                }
            )
    rows.sort(key=lambda r: (r["file"], r["line"]))
    return rows


def parse_allowlist(path: Path) -> tuple[dict[str, dict], list[str]]:
    """Parse `name<TAB>file<TAB>expiry<TAB>reason` rows; return entries and errors."""
    entries: dict[str, dict] = {}
    errors: list[str] = []
    if not path.exists():
        return entries, [f"allowlist not found: {path}"]
    for lineno, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        parts = raw.split("\t")
        if len(parts) != 4:
            errors.append(f"{path.name}:{lineno}: expected 4 tab-separated fields")
            continue
        name, file, expiry, reason = (p.strip() for p in parts)
        if not reason:
            errors.append(f"{path.name}:{lineno}: {name}: empty reason")
            continue
        try:
            expiry_date = _dt.date.fromisoformat(expiry)
        except ValueError:
            errors.append(
                f"{path.name}:{lineno}: {name}: expiry {expiry!r} is not YYYY-MM-DD"
            )
            continue
        if name in entries:
            errors.append(f"{path.name}:{lineno}: duplicate entry for {name}")
            continue
        entries[name] = {
            "file": file,
            "expiry": expiry_date,
            "reason": reason,
            "lineno": lineno,
        }
    return entries, errors


def run_gate(root: Path, allowlist_path: Path, today: _dt.date) -> int:
    rows = census(root)
    entries, problems = parse_allowlist(allowlist_path)
    tier_names = {r["name"] for r in rows}

    for row in rows:
        entry = entries.get(row["name"])
        where = f"{row['file']}:{row['line']}"
        if entry is None:
            problems.append(
                f"{row['name']} ({where}) has zero production callers and zero "
                f"test references. Wire it, test it, delete it, or add an "
                f"allowlist row with a reason and an expiry date."
            )
        elif entry["expiry"] < today:
            problems.append(
                f"{row['name']} ({where}) allowlist entry EXPIRED on "
                f"{entry['expiry']} (reason was: {entry['reason']}). Wire it, "
                f"test it, delete it, or renew the expiry deliberately."
            )
    for name, entry in sorted(entries.items()):
        if name not in tier_names:
            problems.append(
                f"stale allowlist row: {name} ({allowlist_path.name}:"
                f"{entry['lineno']}) is no longer zero-caller-zero-test - "
                f"remove the row."
            )

    if problems:
        print(f"dead-code census gate: {len(problems)} problem(s)", file=sys.stderr)
        for p in problems:
            print(f"error: {p}", file=sys.stderr)
        return 1
    print(
        f"dead-code census gate: OK "
        f"({len(rows)} zero-caller-zero-test fns, all allowlisted and unexpired)"
    )
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "mode",
        choices=["gate", "census"],
        nargs="?",
        default="gate",
        help="gate: enforce the allowlist (CI); census: print the tier rows",
    )
    parser.add_argument(
        "--root",
        type=Path,
        default=Path(__file__).resolve().parents[2],
        help="repository root to scan",
    )
    parser.add_argument("--allowlist", type=Path, default=ALLOWLIST)
    parser.add_argument(
        "--today",
        type=_dt.date.fromisoformat,
        default=_dt.date.today(),
        help="override the expiry reference date (tests)",
    )
    args = parser.parse_args(argv)

    if args.mode == "census":
        for row in census(args.root):
            print(
                f"{row['name']}\t{row['file']}:{row['line']}\t"
                f"reexports={row['reexport']}"
            )
        return 0
    return run_gate(args.root, args.allowlist, args.today)


if __name__ == "__main__":
    sys.exit(main())
