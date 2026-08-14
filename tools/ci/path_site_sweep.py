#!/usr/bin/env python3
"""Enumerate every path-mutating / path-opening call site in crates/.

Why this exists: upstream rsync 3.5.0 routes every such operation through a
confinement resolver (see docs/design/upstream-3.5.0-path-confinement-model.md).
To mirror that, oc-rsync first needs to know the complete set of sites. A list
maintained by hand is a list that rots, so this derives it mechanically and can
be re-run to prove no site was missed after the resolver is wired in.

What is excluded, and why: test code creates and removes files constantly and
is not an attack surface, so counting it buries the real sites. That means
`#[cfg(test)]` *and* its `#[cfg(all(test, ...))]` spellings, `crates/*/tests/`,
`benches/`, `examples/`, and `build.rs`. Comments are excluded too - a doc
comment naming `File::open` is documentation, not a call site.

What is deliberately NOT counted, so the number is not read as more than it is:
bare `.metadata()`, because it is `File::metadata` (fd-based, already anchored)
as often as `Path::metadata` (path-resolving), and the two are not
distinguishable by regex. The `fs::` and `symlink_metadata` spellings that are
unambiguously path-resolving are counted.

Exit status is 0 unless `--check-patterns` is passed and some pattern matched
nothing: a pattern that matches nothing is the same defect as a filter that
matches too much, and it fails silently, so it is checkable on demand.
"""

from __future__ import annotations

import argparse
import collections
import os
import re
import sys

# One pattern per operation class. The class matters because the confinement
# obligation differs: a create/rename/link/unlink mutates the filesystem, an
# open exposes content, and a stat only resolves a path (still TOCTOU-relevant,
# but a different fix).
OPERATION_PATTERNS = [
    ("MUTATE_CREATE", r"\bFile::create\b"),
    ("MUTATE_RENAME", r"\bfs::rename\b"),
    ("MUTATE_LINK", r"\bfs::hard_link\b|\bsymlink(?:_file|_dir)?\("),
    ("MUTATE_UNLINK", r"\bfs::remove_(?:file|dir|dir_all)\b"),
    ("MUTATE_MKDIR", r"\bfs::create_dir(?:_all)?\b"),
    ("MUTATE_PERM", r"\bfs::set_permissions\b"),
    ("OPEN_READ", r"\bFile::open\b"),
    # How the receiver actually opens destination files. Absent from the
    # original pattern set, which is why 61 of the most security-relevant
    # sites in the tree were invisible to it.
    ("OPEN_OPTS", r"\bOpenOptions::new\b"),
    # The docstring always named stat as a class; the patterns never had it.
    # Only the unambiguously path-resolving spellings - see the module docstring
    # on why bare `.metadata()` is left out.
    (
        "STAT",
        r"\bfs::(?:metadata|symlink_metadata|read_link|canonicalize)\b"
        r"|\.symlink_metadata\(",
    ),
]

_MATCHER = re.compile(
    "|".join(f"(?P<{name}>{pattern})" for name, pattern in OPERATION_PATTERNS)
)

# `#[cfg(test)]` is only the simplest spelling. `#[cfg(all(test, unix))]` and
# `#[cfg(all(unix, test))]` are equally common and the literal pattern missed
# every one of them. Match any `cfg` attribute carrying a bare `test` token,
# after quoted strings are removed so `feature = "test-utils"` does not count.
_CFG_ATTR = re.compile(r"#\[cfg(?:_attr)?\((?P<pred>.*)\)\]")
_QUOTED = re.compile(r'"[^"]*"')
_BARE_TEST = re.compile(r"\btest\b")

SKIP_DIRS = frozenset({"tests", "benches", "examples"})
SKIP_FILES = frozenset({"tests.rs", "build.rs"})


def is_test_cfg(line: str) -> bool:
    """True when `line` carries a cfg attribute gated on `test`."""
    match = _CFG_ATTR.search(line)
    if not match:
        return False
    return bool(_BARE_TEST.search(_QUOTED.sub("", match.group("pred"))))


def code_before_comment(line: str) -> str:
    """Return the part of `line` outside a `//` comment.

    A doc comment naming an API is not a call site. Quote-parity is tracked so
    a `//` inside a string literal - a path like `"rsync://host/mod"` - is not
    mistaken for a comment. Block comments are not handled; they are rare in
    this tree and would need multi-line state.
    """
    quoted = False
    index = 0
    while index < len(line) - 1:
        char = line[index]
        if char == "\\":
            index += 2
            continue
        if char == '"':
            quoted = not quoted
        elif char == "/" and line[index + 1] == "/" and not quoted:
            return line[:index]
        index += 1
    return line


def production_lines(path: str):
    """Yield (lineno, code) for code outside any test-gated block or comment.

    Brace depth is tracked from the attribute onward so an entire test module
    is skipped, not just the attribute line.
    """
    in_test = False
    depth = 0
    with open(path, errors="replace") as handle:
        for lineno, line in enumerate(handle, 1):
            if is_test_cfg(line):
                in_test = True
                depth = 0
                continue
            if in_test:
                depth += line.count("{") - line.count("}")
                if depth <= 0 and "}" in line:
                    in_test = False
                continue
            code = code_before_comment(line)
            if code.strip():
                yield lineno, code


def sweep(root: str):
    """Return a list of (operation, crate, path, lineno, text) for every site."""
    sites = []
    for dirpath, dirnames, filenames in os.walk(root):
        # Prune in place so os.walk does not descend at all. The original
        # substring check on dirpath only caught "tests", so benches/ and
        # examples/ were swept as if they were production.
        dirnames[:] = [d for d in dirnames if d not in SKIP_DIRS]
        if any(part in SKIP_DIRS for part in dirpath.split(os.sep)):
            continue
        for filename in filenames:
            if not filename.endswith(".rs") or filename in SKIP_FILES:
                continue
            path = os.path.join(dirpath, filename)
            parts = path.split(os.sep)
            crate = parts[1] if len(parts) > 1 else "?"
            for lineno, code in production_lines(path):
                match = _MATCHER.search(code)
                if match:
                    sites.append((match.lastgroup, crate, path, lineno, code.strip()))
    return sites


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", default="crates")
    parser.add_argument(
        "--list", action="store_true", help="print one line per site, not a summary"
    )
    parser.add_argument(
        "--check-patterns",
        action="store_true",
        help="exit 1 if any declared pattern matched nothing (a dead pattern "
        "is a silent hole, not a passing check)",
    )
    args = parser.parse_args()

    sites = sweep(args.root)
    if args.list:
        for operation, crate, path, lineno, text in sorted(sites):
            print(f"{operation}\t{crate}\t{path}:{lineno}\t{text}")
        return 0

    by_operation = collections.Counter(site[0] for site in sites)
    by_crate = collections.Counter(site[1] for site in sites)

    print("path-mutating / path-opening sites in production code")
    print(
        f"(root={args.root}; test-gated cfg blocks, {'/, '.join(sorted(SKIP_DIRS))}/, "
        f"{', '.join(sorted(SKIP_FILES))} and comments excluded)\n"
    )
    print("by operation:")
    for operation, _ in OPERATION_PATTERNS:
        print(f"  {operation:16} {by_operation[operation]:5}")

    print("\nby crate:")
    for crate, count in by_crate.most_common():
        print(f"  {crate:16} {count:5}")

    print(f"\nTOTAL {len(sites)}")

    dead = [name for name, _ in OPERATION_PATTERNS if not by_operation[name]]
    if dead:
        print(f"\nWARNING: patterns matching nothing: {', '.join(dead)}")
        print("A pattern that matches nothing hides sites exactly as a")
        print("too-broad filter invents them. Fix the pattern or drop it.")
        if args.check_patterns:
            return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
