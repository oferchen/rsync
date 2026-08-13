#!/usr/bin/env python3
"""Enumerate every path-mutating / path-opening call site in crates/.

Why this exists: upstream rsync 3.5.0 routes every such operation through a
confinement resolver (see docs/design/upstream-3.5.0-path-confinement-model.md).
To mirror that, oc-rsync first needs to know the complete set of sites. A list
maintained by hand is a list that rots, so this derives it mechanically and can
be re-run to prove no site was missed after the resolver is wired in.

`#[cfg(test)]` blocks are excluded: test code creates and removes files
constantly and is not an attack surface, so counting it buries the real
sites. Integration tests under crates/*/tests/ are skipped for the same
reason. The exclusion is brace-depth tracked rather than line-matched, so a
multi-line test module is dropped whole.

Exit status is always 0; this reports, it does not gate. Task 607 turns the
comparison into a ratchet once every site is classified.
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
]

_MATCHER = re.compile(
    "|".join(f"(?P<{name}>{pattern})" for name, pattern in OPERATION_PATTERNS)
)
_CFG_TEST = re.compile(r"#\[cfg\(test\)\]")


def production_lines(path: str):
    """Yield (lineno, text) for lines outside any #[cfg(test)] block.

    Brace depth is tracked from the attribute onward so an entire test module
    is skipped, not just the attribute line.
    """
    in_test = False
    depth = 0
    with open(path, errors="replace") as handle:
        for lineno, line in enumerate(handle, 1):
            if _CFG_TEST.search(line):
                in_test = True
                depth = 0
                continue
            if in_test:
                depth += line.count("{") - line.count("}")
                if depth <= 0 and "}" in line:
                    in_test = False
                continue
            yield lineno, line


def sweep(root: str):
    """Return a list of (operation, crate, path, lineno, text) for every site."""
    sites = []
    for dirpath, _, filenames in os.walk(root):
        # crates/<name>/tests/ is integration-test scaffolding, not a surface.
        if f"{os.sep}tests" in dirpath:
            continue
        for filename in filenames:
            if not filename.endswith(".rs") or filename == "tests.rs":
                continue
            path = os.path.join(dirpath, filename)
            parts = path.split(os.sep)
            crate = parts[1] if len(parts) > 1 else "?"
            for lineno, line in production_lines(path):
                match = _MATCHER.search(line)
                if match:
                    sites.append(
                        (match.lastgroup, crate, path, lineno, line.strip())
                    )
    return sites


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", default="crates")
    parser.add_argument(
        "--list", action="store_true", help="print one line per site, not a summary"
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
    print(f"(root={args.root}, #[cfg(test)] blocks and */tests/ excluded)\n")
    print("by operation:")
    for operation in sorted(by_operation):
        print(f"  {operation:16} {by_operation[operation]:5}")
    print("\nby crate:")
    for crate, count in by_crate.most_common():
        print(f"  {crate:16} {count:5}")
    print(f"\nTOTAL {len(sites)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
