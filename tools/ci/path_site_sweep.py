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

A whole FILE is test code when the `mod` declaration that pulls it in is
cfg(test)-gated. The gate sits in the parent, so the file's own text carries no
attribute and a line-scan inside it sees production code; matching file NAMES
instead only works while every such file is called `tests.rs`. Resolving the
declaration is exact and needs no naming convention. Exclusion propagates
through the submodules such a file declares, since a module unreachable outside
`cfg(test)` cannot make its children reachable.

Crates depended on only from `[dev-dependencies]` are excluded for the same
reason one level up: they are never linked into the shipped binary, so their
call sites are not an attack surface either.

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
import tomllib

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

# A `mod foo;` declaration, with or without a visibility qualifier. Only the
# `;` form matters: an inline `mod foo { .. }` body is already covered by the
# cfg-attribute brace tracking in production_lines().
_MOD_DECL = re.compile(
    r"^\s*(?:pub\s*(?:\([^)]*\)\s*)?)?mod\s+(?P<name>[A-Za-z_][A-Za-z0-9_]*)\s*;"
)
_ATTR_LINE = re.compile(r"^\s*#\[")

SKIP_DIRS = frozenset({"tests", "benches", "examples"})
SKIP_FILES = frozenset({"tests.rs", "build.rs"})


def is_test_cfg(line: str) -> bool:
    """True when `line` carries a cfg attribute gated on `test`."""
    match = _CFG_ATTR.search(line)
    if not match:
        return False
    return bool(_BARE_TEST.search(_QUOTED.sub("", match.group("pred"))))


def _module_targets(path: str, gated_only: bool):
    """Yield the file each `mod NAME;` in `path` resolves to.

    With `gated_only`, only declarations carrying a cfg(test) attribute count -
    either inline or on one of the attribute lines directly above.
    """
    directory = os.path.dirname(path)
    with open(path, errors="replace") as handle:
        lines = handle.read().splitlines()
    for index, line in enumerate(lines):
        match = _MOD_DECL.match(line)
        if not match:
            continue
        if gated_only and not _declared_under_test_cfg(lines, index):
            continue
        stem = match.group("name")
        for candidate in (
            os.path.join(directory, f"{stem}.rs"),
            os.path.join(directory, stem, "mod.rs"),
        ):
            if os.path.isfile(candidate):
                yield candidate


def _declared_under_test_cfg(lines: list[str], index: int) -> bool:
    """True when the `mod` declaration at `index` is gated on `test`."""
    if is_test_cfg(lines[index]):
        return True
    cursor = index - 1
    while cursor >= 0 and _ATTR_LINE.match(lines[cursor]):
        if is_test_cfg(lines[cursor]):
            return True
        cursor -= 1
    return False


def test_only_files(root: str) -> set[str]:
    """Return every file reachable only through a cfg(test)-gated `mod`.

    Seeded from the gated declarations, then closed over the submodules those
    files declare: a module the compiler only builds under `cfg(test)` cannot
    make its children reachable in a production build.
    """
    pending = []
    for dirpath, _dirnames, filenames in os.walk(root):
        for filename in filenames:
            if filename.endswith(".rs"):
                path = os.path.join(dirpath, filename)
                pending.extend(_module_targets(path, gated_only=True))

    seen: set[str] = set()
    while pending:
        path = pending.pop()
        if path in seen:
            continue
        seen.add(path)
        pending.extend(_module_targets(path, gated_only=False))
    return seen


def _dependency_names(data: dict, sections: tuple[str, ...]) -> set[str]:
    """Collect dependency names from `sections`, including per-target tables."""
    names: set[str] = set()
    for section in sections:
        names.update(data.get(section, {}))
    for target in data.get("target", {}).values():
        for section in sections:
            names.update(target.get(section, {}))
    return names


def dev_only_crates(root: str) -> set[str]:
    """Return crate directory names depended on ONLY from dev-dependencies.

    Such a crate is never linked into the shipped binary, so its call sites
    carry no confinement obligation. Derived from the manifests rather than
    from the crate's name, so a renamed helper crate stays excluded.

    Two conditions, and both matter. A crate needs at least one dev edge: a
    crate nothing depends on at all is unreferenced, which is a different
    finding and not this function's to make. And the scan must include the
    WORKSPACE ROOT manifest, which carries real `[target.'cfg(..)'.dependencies]`
    edges - reading only `crates/*/Cargo.toml` misclassifies a platform-gated
    runtime dependency as test-only.
    """
    manifests = {}
    workspace_root = os.path.join(os.path.dirname(root) or ".", "Cargo.toml")
    if os.path.isfile(workspace_root):
        with open(workspace_root, "rb") as handle:
            manifests[None] = tomllib.load(handle)
    for entry in sorted(os.listdir(root)):
        manifest = os.path.join(root, entry, "Cargo.toml")
        if os.path.isfile(manifest):
            with open(manifest, "rb") as handle:
                manifests[entry] = tomllib.load(handle)

    names = {
        data.get("package", {}).get("name", entry): entry
        for entry, data in manifests.items()
        if entry is not None
    }
    non_dev: set[str] = set()
    dev: set[str] = set()
    for data in manifests.values():
        non_dev |= _dependency_names(data, ("dependencies", "build-dependencies"))
        dev |= _dependency_names(data, ("dev-dependencies",))
    return {entry for name, entry in names.items() if name in dev - non_dev}


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
    skip_paths = test_only_files(root)
    skip_crates = dev_only_crates(root)
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
            if path in skip_paths:
                continue
            # Relative to `root`, so the crate is the first component whatever
            # --root was given. Indexing the absolute path positionally only
            # ever worked for the literal default.
            parts = os.path.relpath(path, root).split(os.sep)
            crate = parts[0] if len(parts) > 1 else "?"
            if crate in skip_crates:
                continue
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
