#!/usr/bin/env python3
"""Format-check the source files that `cargo fmt` cannot see.

rustfmt walks the module tree through `mod` declarations only; it never
expands `include!()`. Every file pulled in with `include!("...")` is therefore
invisible to `cargo fmt --all -- --check`, which exits 0 no matter how the
included file is formatted. This script closes that hole: it collects the
literal paths named by `include!()` across the workspace and runs
`rustfmt --check` on each one directly.

Usage:
    python3 tools/ci/check_include_fmt.py           # check, exit 1 on diff
    python3 tools/ci/check_include_fmt.py --write   # reformat in place
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
import tomllib
from pathlib import Path

# `include!("path")`, allowing the newlines rustfmt itself inserts when the
# path literal is too long to fit on one line. Non-literal forms such as
# `include!(concat!(env!("OUT_DIR"), ...))` refer to generated files outside
# the source tree and are deliberately not matched.
INCLUDE_RE = re.compile(r"""include!\s*\(\s*"([^"]+)"\s*,?\s*\)""")


class EditionError(Exception):
    """The workspace Rust edition could not be resolved."""


def repo_root() -> Path:
    out = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"],
        check=True,
        capture_output=True,
        text=True,
    )
    return Path(out.stdout.strip())


def workspace_edition(root: Path) -> str:
    """Read `edition` from `[workspace.package]` in the workspace manifest.

    rustfmt has no notion of a workspace and defaults to edition 2015, so the
    edition has to be handed to it explicitly. Restating the value here would
    let it drift from the manifest unnoticed - formatting against a stale
    edition either reports spurious diffs or masks real ones - so it is read at
    runtime and a missing value is a hard error, never a silent default.
    """
    manifest = root / "Cargo.toml"
    try:
        data = tomllib.loads(manifest.read_text(encoding="utf-8"))
    except OSError as error:
        raise EditionError(f"cannot read {manifest}: {error}") from error
    except tomllib.TOMLDecodeError as error:
        raise EditionError(f"cannot parse {manifest}: {error}") from error

    section = data.get("workspace")
    package = section.get("package") if isinstance(section, dict) else None
    edition = package.get("edition") if isinstance(package, dict) else None
    if not isinstance(edition, str) or not edition:
        raise EditionError(
            f"{manifest} declares no [workspace.package] edition; rustfmt would\n"
            "fall back to edition 2015 and format every include!()d file against\n"
            "the wrong edition"
        )
    return edition


def tracked_rust_sources(root: Path) -> list[Path]:
    out = subprocess.run(
        ["git", "ls-files", "-z", "--", "*.rs"],
        cwd=root,
        check=True,
        capture_output=True,
        text=True,
    )
    return [root / name for name in out.stdout.split("\0") if name]


def included_files(root: Path) -> tuple[list[Path], list[str]]:
    """Return the `include!()` targets and any unresolvable references."""
    found: set[Path] = set()
    missing: list[str] = []
    for source in tracked_rust_sources(root):
        text = source.read_text(encoding="utf-8", errors="replace")
        if "include!" not in text:
            continue
        for rel in INCLUDE_RE.findall(text):
            target = (source.parent / rel).resolve()
            if target.is_file():
                found.add(target)
            else:
                missing.append(f"{source.relative_to(root)}: include!(\"{rel}\")")
    return sorted(found), missing


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--write",
        action="store_true",
        help="reformat the included files in place instead of checking them",
    )
    args = parser.parse_args()

    root = repo_root()

    try:
        edition = workspace_edition(root)
    except EditionError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    print(f"rustfmt edition {edition} (from [workspace.package] in Cargo.toml)")

    targets, missing = included_files(root)

    if missing:
        print("include!() targets that do not resolve to a file:", file=sys.stderr)
        for entry in missing:
            print(f"  {entry}", file=sys.stderr)
        return 1

    if not targets:
        print("no include!() targets found - the enumerator matched nothing", file=sys.stderr)
        return 1

    cmd = ["rustfmt", "--edition", edition]
    if not args.write:
        cmd.append("--check")
    cmd += [str(path) for path in targets]

    result = subprocess.run(cmd, cwd=root)
    if result.returncode != 0:
        if not args.write:
            print(
                f"\n{len(targets)} include!()d file(s) checked; the diffs above are "
                "invisible to `cargo fmt --all -- --check`.\n"
                "Fix with: python3 tools/ci/check_include_fmt.py --write",
                file=sys.stderr,
            )
        return result.returncode

    verb = "reformatted" if args.write else "checked"
    print(f"{verb} {len(targets)} include!()d file(s) at edition {edition}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
