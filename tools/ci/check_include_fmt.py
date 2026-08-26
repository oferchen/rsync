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
from pathlib import Path

# `include!("path")`, allowing the newlines rustfmt itself inserts when the
# path literal is too long to fit on one line. Non-literal forms such as
# `include!(concat!(env!("OUT_DIR"), ...))` refer to generated files outside
# the source tree and are deliberately not matched.
INCLUDE_RE = re.compile(r"""include!\s*\(\s*"([^"]+)"\s*,?\s*\)""")

EDITION = "2024"


def repo_root() -> Path:
    out = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"],
        check=True,
        capture_output=True,
        text=True,
    )
    return Path(out.stdout.strip())


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
    targets, missing = included_files(root)

    if missing:
        print("include!() targets that do not resolve to a file:", file=sys.stderr)
        for entry in missing:
            print(f"  {entry}", file=sys.stderr)
        return 1

    if not targets:
        print("no include!() targets found - the enumerator matched nothing", file=sys.stderr)
        return 1

    cmd = ["rustfmt", "--edition", EDITION]
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
    print(f"{verb} {len(targets)} include!()d file(s)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
