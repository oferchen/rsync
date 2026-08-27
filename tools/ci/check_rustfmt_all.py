#!/usr/bin/env python3
"""Format-check every tracked Rust source file, including the ones `cargo fmt` cannot see.

rustfmt reaches a file only through the module tree - `mod x;` and
`#[path = "..."] mod x;`. It never expands `include!()`, and it never sees a
file that no module declaration references at all. Both kinds are invisible to
`cargo fmt --all -- --check`, which exits 0 no matter how they are formatted.

This script closes that hole without depending on a module walk: it hands
rustfmt every `.rs` file git tracks. Enumerating the reachable set and
subtracting it would make the gate only as correct as the walk; enumerating the
whole tree makes the walk's correctness irrelevant.

Usage:
    python3 tools/ci/check_rustfmt_all.py           # check, exit 1 on diff
    python3 tools/ci/check_rustfmt_all.py --write   # reformat in place
"""

from __future__ import annotations

import argparse
import subprocess
import sys
import tomllib
from pathlib import Path


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
            "fall back to edition 2015 and format every file against the wrong\n"
            "edition"
        )
    return edition


def tracked_rust_sources(root: Path) -> list[str]:
    """Every `.rs` path git tracks, relative to the repository root.

    Paths stay relative and rustfmt runs with `cwd=root`: the absolute forms
    total roughly 400 KB of argv, which approaches the platform limit on macOS.
    """
    out = subprocess.run(
        ["git", "ls-files", "-z", "--", "*.rs"],
        cwd=root,
        check=True,
        capture_output=True,
        text=True,
    )
    return sorted(name for name in out.stdout.split("\0") if name)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--write",
        action="store_true",
        help="reformat the files in place instead of checking them",
    )
    args = parser.parse_args()

    root = repo_root()

    try:
        edition = workspace_edition(root)
    except EditionError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    print(f"rustfmt edition {edition} (from [workspace.package] in Cargo.toml)")

    targets = tracked_rust_sources(root)
    if not targets:
        print(
            "no tracked .rs files found - the enumerator matched nothing",
            file=sys.stderr,
        )
        return 1

    cmd = ["rustfmt", "--edition", edition]
    if not args.write:
        cmd.append("--check")
    cmd += targets

    # One invocation, not one per file. stdout streams the diffs; stderr is
    # captured so a parse failure can be told apart from a formatting diff.
    result = subprocess.run(cmd, cwd=root, stderr=subprocess.PIPE, text=True)
    if result.stderr:
        sys.stderr.write(result.stderr)

    if result.returncode != 0:
        if result.stderr:
            # `include!()` is legal at expression position, so a fragment can be
            # valid where it is included and still not parse as a standalone
            # file. That is reported, never skipped: a skip arm would silently
            # restore the hole this gate exists to close.
            print(
                "\nrustfmt could not process one of the files above. A fragment "
                "that is only\nvalid at its include! site does not parse "
                "standalone; this gate does not skip\nsuch files. Either give "
                "the fragment a form that parses on its own, or make it\na real "
                f"module. {len(targets)} tracked .rs file(s) were submitted.",
                file=sys.stderr,
            )
        elif not args.write:
            print(
                f"\n{len(targets)} tracked .rs file(s) checked. Diffs in a file "
                "that no `mod`\ndeclaration reaches are invisible to `cargo fmt "
                "--all -- --check`.\n"
                "Fix with: python3 tools/ci/check_rustfmt_all.py --write",
                file=sys.stderr,
            )
        return result.returncode

    verb = "reformatted" if args.write else "checked"
    print(f"{verb} {len(targets)} tracked .rs file(s) at edition {edition}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
