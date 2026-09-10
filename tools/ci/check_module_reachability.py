#!/usr/bin/env python3
"""Fail when a `.rs` file under `crates/` is reachable from no compilation root.

A Rust file only ever compiles if the module tree reaches it: a `mod` item names
it, or an `include!()` splices it into a file that is itself reached. A file that
neither reaches is dead weight the compiler never sees. Its `#[test]` functions
never run, its `#[allow]`s never apply, and nothing about it is checked - it
reads exactly like live code and behaves like a comment.

Nothing in the normal toolchain reports this. `cargo build` compiles the
reachable set and is silent about the rest; `cargo fmt --all` has the same blind
spot, which is why `check_rustfmt_all.py` sits beside this script and formats the
whole tree without a module walk. That script deliberately avoids a reachability
walk so its correctness does not depend on one. This script is the walk, kept
separate for exactly that reason: a bug here weakens only this gate.

The walk starts from every compilation root cargo would build - lib, bins,
integration tests, benches, examples, build scripts - and follows:

  * `mod name;`                    -> DIR/name.rs or DIR/name/mod.rs
  * `#[path = "p"] mod name;`      -> DIR/p
  * `mod name { ... }`             -> nested items resolve under DIR/name
  * `include!("p")`                -> FILE_DIR/p

The `include!` rule is the subtle one. `include!` splices tokens, but rustc
re-bases module resolution on the *included* file's own directory, so a
`mod tests;` inside an `include!`-ed fragment names a sibling of that fragment -
not a sibling of the file that included it. Getting this backwards makes real
files look orphaned and orphans look reachable.

Usage:
    python3 tools/ci/check_module_reachability.py            # exit 1 on orphans
    python3 tools/ci/check_module_reachability.py --list      # print the reached set
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
import tomllib
from pathlib import Path

# Files that are intentionally unreachable from any compilation root. Every entry
# needs a reason: an orphan with no justification is a defect, and an allowlist
# that absorbs them silently is worse than no gate. Paths are repo-relative.
ALLOWLIST: dict[str, str] = {}

BLOCK_COMMENT = re.compile(r"/\*.*?\*/", re.DOTALL)
LINE_COMMENT = re.compile(r"//[^\n]*")
# Two forms, both anchored so an identifier ending in `r` cannot start one. The
# hashless form stops at the first quote; only the hashed form may span lines.
# An unanchored `r(#*)".*?"\1` looks equivalent and is not: it matches inside
# ordinary identifiers and swallows arbitrary spans of real code between two
# unrelated quotes, silently hiding the `mod` items in between.
RAW_STRING = re.compile(r'(?<![A-Za-z0-9_])(?:r"[^"]*"|r(#+)".*?"\1)', re.DOTALL)

MOD_ITEM = re.compile(r"\bmod\s+(?P<name>[A-Za-z_][A-Za-z0-9_]*)\s*(?P<tail>[;{])")
# `#[path]` is matched by looking BACKWARDS from the `mod` keyword, because
# anything may sit between them: further attributes and a visibility modifier.
# The workspace writes `#[cfg(..)] #[path = ".."] pub mod x;`, where a
# forward-only "attribute immediately precedes mod" pattern silently drops the
# path and resolves the module to the wrong file.
MOD_PATH_ATTR = re.compile(
    r"""\#\s*\[\s*path\s*=\s*"(?P<path>[^"]+)"\s*\]\s*"""
    r"""(?:\#\s*\[[^\]]*\]\s*)*"""
    r"""(?:pub\s*(?:\([^)]*\)\s*)?)?$""",
    re.DOTALL,
)
# How far back to look for that attribute. Attributes and visibility only; a
# window this size spans far more than any real prefix.
MOD_PREFIX_WINDOW = 512
INCLUDE_ITEM = re.compile(r'\binclude(?:_str|_bytes)?\s*!\s*[(\[{]\s*"(?P<path>[^"]+)"')


def repo_root() -> Path:
    out = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"],
        check=True,
        capture_output=True,
        text=True,
    )
    return Path(out.stdout.strip())


def tracked_sources(root: Path) -> set[Path]:
    """Every `.rs` path under `crates/`, tracked or merely untracked-and-not-ignored.

    Tracked-only would be a hole: a new file is invisible to the gate until it is
    `git add`ed, so it passes locally and fails in CI, where the checkout has it
    tracked. Mirrors the enumerator in `check_rustfmt_all.py`.
    """
    out = subprocess.run(
        [
            "git", "ls-files", "-z", "--cached", "--others", "--exclude-standard",
            "--", "crates/*.rs",
        ],
        cwd=root,
        check=True,
        capture_output=True,
        text=True,
    )
    return {root / name for name in out.stdout.split("\0") if name}


def strip_noise(text: str) -> str:
    """Remove comments and raw strings so their contents cannot match as items.

    A `mod` or `include!` inside a doc comment or a test fixture string is not a
    module declaration. Plain string literals are left alone: they are short, and
    stripping them costs more in escape handling than it saves.
    """
    text = RAW_STRING.sub('""', text)
    text = BLOCK_COMMENT.sub("", text)
    return LINE_COMMENT.sub("", text)


def module_items(text: str) -> list[tuple[str, str | None, list[str]]]:
    """Yield `(name, explicit_path, enclosing_inline_mods)` for every `mod` item.

    `enclosing_inline_mods` is the chain of `mod name { ... }` blocks the item sits
    inside, because those blocks push the resolution directory down one level each.
    Brace depth is tracked over the noise-stripped text, which is why comments and
    raw strings are removed first.
    """
    items: list[tuple[str, str | None, list[str]]] = []
    # Stack of (depth_at_open, name) for inline `mod name { ... }` blocks.
    inline: list[tuple[int, str]] = []
    depth = 0
    pos = 0
    for match in MOD_ITEM.finditer(text):
        # Advance brace depth up to this item, closing any inline mods we left.
        for char in text[pos : match.start()]:
            if char == "{":
                depth += 1
            elif char == "}":
                depth -= 1
                while inline and inline[-1][0] >= depth:
                    inline.pop()
        pos = match.start()
        window = text[max(0, match.start() - MOD_PREFIX_WINDOW) : match.start()]
        attr = MOD_PATH_ATTR.search(window)
        items.append(
            (match["name"], attr["path"] if attr else None, [name for _, name in inline])
        )
        if match["tail"] == "{":
            # The body opens at the brace this match consumed.
            depth += 1
            inline.append((depth, match["name"]))
            pos = match.end()
    return items


def resolve_mod(
    base: Path,
    file_dir: Path,
    name: str,
    explicit: str | None,
    enclosing: list[str],
) -> list[Path]:
    """Candidate files for one `mod` item.

    `base` is the module directory (a crate root and a `mod.rs` own their own
    directory; any other file owns the subdirectory named after it) and `file_dir`
    is the directory the source file itself sits in. The two differ for a plain
    `foo.rs`, and `#[path]` is the case where the difference bites: a `#[path]`
    that is NOT inside an inline `mod { }` block resolves against the source file's
    own directory, ignoring the module directory entirely. Only inside an inline
    block does the module directory apply. Resolving `#[path]` against `base`
    unconditionally sends every such module one directory too deep - it finds
    nothing, and the real target reads as an orphan.
    """
    if explicit is not None:
        directory = file_dir if not enclosing else base.joinpath(*enclosing)
        return [directory / explicit]
    directory = base.joinpath(*enclosing)
    return [directory / f"{name}.rs", directory / name / "mod.rs"]


def mod_base(path: Path) -> Path:
    """Directory a `mod` item resolves against, for a file reached by a `mod` item.

    A crate root and a `mod.rs` own the directory they sit in; any other file owns
    the subdirectory named after it.
    """
    if path.name in ("mod.rs", "lib.rs", "main.rs"):
        return path.parent
    return path.parent / path.stem


def walk(roots: list[Path]) -> set[Path]:
    """Transitively reach every file the compiler would parse from `roots`.

    Each queue entry carries the directory its `mod` items resolve against, because
    that directory depends on HOW the file was reached, not on the file alone. A
    file pulled in by a `mod` item descends into its own subdirectory; the same
    file pulled in by `include!` resolves against the directory it sits in. Deriving
    the base from the path alone conflates the two and reports live files - the
    daemon's section tests among them - as orphans.
    """
    reached: set[Path] = set()
    seen: set[tuple[Path, Path]] = set()
    queue = [(path.resolve(), path.parent.resolve()) for path in roots if path.is_file()]
    while queue:
        current, base = queue.pop()
        reached.add(current)
        if (current, base) in seen:
            continue
        seen.add((current, base))
        try:
            text = strip_noise(current.read_text(encoding="utf-8", errors="replace"))
        except OSError:
            continue

        for name, explicit, enclosing in module_items(text):
            for candidate in resolve_mod(base, current.parent, name, explicit, enclosing):
                if candidate.is_file():
                    resolved = candidate.resolve()
                    queue.append((resolved, mod_base(resolved)))
                    break

        for match in INCLUDE_ITEM.finditer(text):
            target = (current.parent / match["path"]).resolve()
            if target.is_file() and target.suffix == ".rs":
                queue.append((target, target.parent))
    return reached


def manifest_roots(crate: Path) -> list[Path]:
    """Compilation roots cargo would build for one crate.

    Explicit `path` keys win; otherwise cargo's auto-discovery layout applies.
    Both are honoured because the workspace uses each in places, and missing a
    root would report its whole subtree as orphaned.
    """
    manifest = crate / "Cargo.toml"
    try:
        data = tomllib.loads(manifest.read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError):
        return []

    roots: list[Path] = []

    def add_targets(key: str, default_dir: str) -> None:
        section = data.get(key)
        entries = section if isinstance(section, list) else [section] if isinstance(section, dict) else []
        for entry in entries:
            path = entry.get("path") if isinstance(entry, dict) else None
            if isinstance(path, str):
                roots.append(crate / path)
        directory = crate / default_dir
        if directory.is_dir():
            roots.extend(sorted(directory.glob("*.rs")))
            roots.extend(sorted(directory.glob("*/main.rs")))

    lib = data.get("lib")
    lib_path = lib.get("path") if isinstance(lib, dict) else None
    roots.append(crate / lib_path if isinstance(lib_path, str) else crate / "src/lib.rs")
    roots.append(crate / "src/main.rs")
    roots.append(crate / "build.rs")

    add_targets("bin", "src/bin")
    add_targets("test", "tests")
    add_targets("bench", "benches")
    add_targets("example", "examples")

    return roots


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--list",
        action="store_true",
        help="print every reached file instead of only the orphans",
    )
    args = parser.parse_args()

    root = repo_root()
    # Recursive, not one level deep: the fuzz crates carry their own manifests and
    # declare their targets as `[[bin]]`. Enumerating only `crates/*/Cargo.toml`
    # skips those roots and reports every fuzz target as an orphan.
    crates = sorted(
        p.parent
        for p in (root / "crates").rglob("Cargo.toml")
        if "target" not in p.relative_to(root).parts
    )
    if not crates:
        print("no crates found under crates/ - the enumerator matched nothing", file=sys.stderr)
        return 1

    roots: list[Path] = []
    for crate in crates:
        roots.extend(manifest_roots(crate))

    reached = walk(roots)
    sources = tracked_sources(root)
    if not sources:
        print("no .rs files found under crates/ - the enumerator matched nothing", file=sys.stderr)
        return 1

    orphans = sorted(
        path.relative_to(root).as_posix()
        for path in sources
        if path.resolve() not in reached
    )

    if args.list:
        for path in sorted(p.relative_to(root).as_posix() for p in reached if root in p.parents):
            print(path)

    allowed = [p for p in orphans if p in ALLOWLIST]
    unexplained = [p for p in orphans if p not in ALLOWLIST]

    for path in allowed:
        print(f"allowed orphan: {path} - {ALLOWLIST[path]}")

    stale = sorted(set(ALLOWLIST) - set(orphans))
    if stale:
        for path in stale:
            print(f"error: {path} is on the allowlist but IS reachable", file=sys.stderr)
        print(
            "\nA stale allowlist entry waives a file that no longer needs waiving, and\n"
            "would keep waiving it if it went orphaned again. Remove the entry from\n"
            "ALLOWLIST in tools/ci/check_module_reachability.py.",
            file=sys.stderr,
        )
        return 1

    if unexplained:
        for path in unexplained:
            print(f"error: no `mod` or `include!` reaches {path}", file=sys.stderr)
        print(
            f"\n{len(unexplained)} of {len(sources)} .rs file(s) under crates/ are compiled by\n"
            "nothing. The compiler never parses them: their tests never run and their\n"
            "code is never checked. Either declare them (`mod name;`, `include!(...)`)\n"
            "or delete them. A file that is genuinely meant to stay unreachable goes in\n"
            "ALLOWLIST in tools/ci/check_module_reachability.py with its reason.",
            file=sys.stderr,
        )
        return 1

    print(f"{len(sources)} .rs file(s) under crates/ checked; all are reachable")
    return 0


if __name__ == "__main__":
    sys.exit(main())
