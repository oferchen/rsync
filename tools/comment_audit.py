#!/usr/bin/env python3
"""Report comments that violate the repo's comment policy.

The policy this encodes:

  KEEP   - anything carrying information, and every reference to the upstream
           rsync C source.
  DELETE - restatement comments that echo the code, outdated comments, debug
           checkpoint comments, decorative banner/divider comments, and
           commented-out code.

The unit of judgement is a **block**, not a line. A comment is one thought
wrapped across however many lines it needs, and judging its lines
independently is what makes a line-based audit useless:

  * the continuation of "...uses a TEMPlate" reads as a `TEMP` debug marker;
  * the continuation "receiver config." reads as a bare restatement;
  * quoted upstream C source loses its protection, because the `// upstream:`
    attribution sits on a *different line* from the `if (getpeername(...))` it
    introduces.

Protection is therefore block-scoped: one upstream/SAFETY/CVE reference
anywhere in a block protects the whole block.

Two further rules keep the report honest, both learned by measuring:

  * The debris detectors are LINE-comment concerns only. `///` and `//!`
    legitimately carry fenced examples and markdown tables; running the
    banner and commented-out-code detectors over rustdoc mistakes
    documentation for debris.
  * Every keyword pattern is word-bounded. Without `\\b`, `format_number`
    matches `for`, `use_chroot` matches `use`, and `Temp directory` matches
    `TEMP`.

This tool only reports, and deliberately does not gate CI: at the time of
writing the workspace has ~250 findings of which the large majority are
defensible, and a ratchet at that baseline would freeze the false positives in
while hiding the one addition that matters.

Usage:
    comment_audit.py [--root DIR] [--category NAME] [--crate NAME]
                     [--list] [--limit N]
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path

# A block naming the upstream source, a safety invariant, a CVE or an explicit
# reviewer REASON is load bearing by definition.
PROTECTED = re.compile(
    r"upstream|SAFETY|CVE-\d|rsync-3\.|\b\w+\.[ch]:\d|REASON:", re.IGNORECASE
)

BANNER_LINE = re.compile(r"^[=\-*#_/~+<>! ]{5,}$")
# Word-bounded; see the module docs for why.
CODEISH = re.compile(
    r"^\s*(?:(?:let|if|else|for|while|loop|match|fn|use|impl|struct|enum|trait|"
    r"mod|return|pub|const|static|assert|assert_eq|debug_assert|println|eprintln|"
    r"dbg|panic|unsafe)\b|self\.|drop\(|\}|\{)"
)
# Only ever tested against a block's FIRST line, and deliberately narrow: the
# marker must carry a colon, or be shouted in caps. `\b` alone is not enough -
# it matches between `G` and `.`, so `debug.log` read as a `DEBUG` marker, and
# a case-insensitive bare word made `Temp directory` read as `TEMP`.
#
# `STEP n` is not here on purpose: measured against the tree it matched only
# protocol-phase labels ("Step 3: Send module name") that structure a long
# handshake test rather than marking a debug checkpoint.
DEBUG_MARKER = re.compile(
    r"^(?:(?i:DEBUG|TEMP|HACK|WIP|CHECKPOINT|NOTE TO SELF)\s*:"
    r"|(?:DEBUG|TEMP|HACK|WIP|CHECKPOINT)\b)"
)
# A real placeholder is the marker used AS a marker: followed by a colon or a
# parenthesis, or standing as the whole first token. Prose *about* the
# upstream `XSTATE_TODO` wire state is not one.
PLACEHOLDER = re.compile(r"\b(?:TODO|FIXME|XXX)\b\s*[:(]|^\s*(?:TODO|FIXME|XXX)\b")

STOPWORDS = frozenset(
    {
        "a", "an", "the", "of", "to", "for", "in", "on", "is", "are", "this",
        "that", "we", "it", "its", "and", "or", "be", "as", "with", "from",
        "into", "by",
    }
)

ITEM = re.compile(
    r"^\s*(?:#\[[^\]]*\]\s*)?(?:pub(?:\([^)]*\))?\s+)?"
    r"(?:default\s+|const\s+|async\s+|unsafe\s+|extern\s+\"[^\"]*\"\s+)*"
    r"(?:fn|struct|enum|trait|type|mod|union)\s+([A-Za-z_][A-Za-z0-9_]*)"
)

MARKERS = ("///", "//!", "//")


def words(text: str) -> set[str]:
    """Content words of `text`: lowercased, snake/camel split, stopwords dropped."""
    out: set[str] = set()
    for token in re.split(r"[^A-Za-z0-9]+", text):
        for part in re.findall(r"[A-Z]+(?![a-z])|[A-Z][a-z0-9]*|[a-z0-9]+", token):
            lowered = part.lower()
            if lowered and lowered not in STOPWORDS:
                out.add(lowered.rstrip("s") or lowered)
    return out


@dataclass
class Block:
    """One comment, however many lines it wraps across."""

    marker: str
    start: int
    bodies: list[str]

    @property
    def text(self) -> str:
        return " ".join(self.bodies).strip()

    def prose(self) -> list[str]:
        """The block's non-empty prose lines, with fenced examples removed."""
        out: list[str] = []
        fenced = False
        for body in self.bodies:
            if body.startswith("```"):
                fenced = not fenced
                continue
            if not fenced and body:
                out.append(body)
        return out


@dataclass(frozen=True)
class Finding:
    """One block the policy would have something to say about."""

    path: str
    line: int
    category: str
    text: str


@dataclass
class Totals:
    """Per-crate tallies."""

    comment_lines: int = 0
    blocks: int = 0
    protected: int = 0
    by_category: dict[str, int] = field(default_factory=dict)


def strip_marker(line: str) -> tuple[str, str] | None:
    """Split a comment line into `(marker, body)`, or `None` when it is not one."""
    stripped = line.strip()
    for marker in MARKERS:
        if stripped.startswith(marker):
            return marker, stripped[len(marker):].strip()
    return None


def blocks_of(lines: list[str]) -> list[Block]:
    """Group consecutive same-marker comment lines into blocks."""
    out: list[Block] = []
    current: Block | None = None
    for index, line in enumerate(lines):
        split = strip_marker(line)
        if not split:
            current = None
            continue
        marker, body = split
        if current is not None and current.marker == marker:
            current.bodies.append(body)
        else:
            current = Block(marker, index, [body])
            out.append(current)
    return out


def next_code_line(lines: list[str], after: int) -> str | None:
    """The first non-comment, non-attribute, non-blank line at or after `after`."""
    for candidate in lines[after:]:
        text = candidate.strip()
        if not text:
            return None
        if strip_marker(candidate) or text.startswith("#["):
            continue
        return text
    return None


def classify(block: Block, lines: list[str]) -> str | None:
    """The policy category a block falls in, or `None` when it is fine."""
    prose = block.prose()
    if not prose:
        return None

    if PLACEHOLDER.search(" ".join(prose)):
        return "placeholder"

    if block.marker == "//":
        if all(BANNER_LINE.match(body) for body in prose):
            return "banner"
        if sum(1 for body in prose if CODEISH.match(body)) * 2 > len(prose):
            return "commented-out-code"
        if DEBUG_MARKER.match(prose[0]):
            return "debug-marker"

    # A restatement echoes the very line it sits above, adding no word the code
    # does not already carry. Single-line and short by construction: a wrapped
    # sentence is explaining something, not repeating a signature.
    if len(prose) != 1:
        return None
    body_words = words(prose[0])
    if not body_words:
        return None
    after = block.start + len(block.bodies)
    if block.marker == "//" and len(body_words) <= 6:
        code = next_code_line(lines, after)
        if code and body_words <= words(code):
            return "restatement"
    if block.marker == "///" and len(body_words) <= 5:
        match = ITEM.match(next_code_line(lines, after) or "")
        if match and body_words <= words(match.group(1)):
            return "restatement-doc"
    return None


def crate_of(path: Path, root: Path) -> str:
    """The crate a tracked file belongs to, for grouping."""
    parts = path.relative_to(root).parts
    if parts[0] == "crates" and len(parts) > 1:
        return parts[1]
    return parts[0]


def audit(paths: list[Path], root: Path) -> tuple[list[Finding], dict[str, Totals]]:
    """Classify every comment block in `paths`."""
    findings: list[Finding] = []
    per_crate: dict[str, Totals] = {}
    for path in paths:
        totals = per_crate.setdefault(crate_of(path, root), Totals())
        try:
            lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
        except OSError:
            continue
        for block in blocks_of(lines):
            totals.comment_lines += len(block.bodies)
            totals.blocks += 1
            if PROTECTED.search(block.text):
                totals.protected += 1
                continue
            category = classify(block, lines)
            if category:
                totals.by_category[category] = totals.by_category.get(category, 0) + 1
                findings.append(
                    Finding(
                        str(path.relative_to(root)),
                        block.start + 1,
                        category,
                        block.text[:110],
                    )
                )
    return findings, per_crate


def tracked_rust_files(root: Path) -> list[Path]:
    """Every tracked `.rs` file, so untracked scratch work is never audited."""
    out = subprocess.run(
        ["git", "ls-files", "-z", "--cached", "--", "*.rs"],
        cwd=root, capture_output=True, check=True,
    ).stdout
    return [root / name for name in out.decode().split("\0") if name]


def render_table(per_crate: dict[str, Totals]) -> str:
    """The per-crate summary, one row per crate plus a total."""
    categories = sorted({c for t in per_crate.values() for c in t.by_category})
    header = ["crate", "lines", "blocks", "upstream", *categories, "flagged"]
    rows: list[list[object]] = []
    for crate in sorted(per_crate):
        totals = per_crate[crate]
        counts = [totals.by_category.get(c, 0) for c in categories]
        rows.append(
            [crate, totals.comment_lines, totals.blocks, totals.protected,
             *counts, sum(counts)]
        )
    grand: list[object] = [
        "TOTAL",
        sum(t.comment_lines for t in per_crate.values()),
        sum(t.blocks for t in per_crate.values()),
        sum(t.protected for t in per_crate.values()),
    ]
    grand += [sum(int(r[4 + i]) for r in rows) for i in range(len(categories))]
    grand.append(sum(int(r[-1]) for r in rows))
    rows.append(grand)

    widths = [max(len(str(r[i])) for r in [header, *rows]) for i in range(len(header))]
    return "\n".join(
        "  ".join(str(cell).rjust(widths[i]) for i, cell in enumerate(row))
        for row in [header, *rows]
    )


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", default=".", type=Path)
    parser.add_argument("--category")
    parser.add_argument("--crate")
    parser.add_argument("--list", action="store_true")
    parser.add_argument("--limit", type=int, default=40)
    args = parser.parse_args(argv)

    root = args.root.resolve()
    findings, per_crate = audit(tracked_rust_files(root), root)

    if args.list:
        selected = [
            f for f in findings
            if (not args.category or f.category == args.category)
            and (not args.crate or crate_of(root / f.path, root) == args.crate)
        ]
        for finding in selected[: args.limit]:
            print(f"{finding.path}:{finding.line}\t{finding.category}\t{finding.text}")
        print(f"--- {len(selected)} selected of {len(findings)} total findings")
    else:
        print(render_table(per_crate))
    return 0


if __name__ == "__main__":
    sys.exit(main())
