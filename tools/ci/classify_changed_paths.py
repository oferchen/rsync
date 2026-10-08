#!/usr/bin/env python3
"""Classify a pull request's changed paths as docs-only or not.

Reads one repository-relative path per line on stdin and prints a single
`docs_only=true|false` line in GitHub Actions output syntax, so a workflow can
append it to `$GITHUB_OUTPUT` directly.

`docs_only=true` lets the CI workflows skip their build and test work while
still publishing every required check, so the answer must fail toward `false`:
an empty change set, or any path outside the allowlist, is not docs-only.

The allowlist is deliberately narrow:
- `docs/**`
- top-level `*.md` and `LICENSE*`
- `.github/*.md` (PR and release templates; not `.github/**`)

Markdown anywhere else is code-adjacent. Six crates embed their README as crate
docs via `#![doc = include_str!("../README.md")]`, so editing one changes
rustdoc and doctest input, and files under `tools/` or `tests/` may be fixtures.
"""

from __future__ import annotations

import sys


def is_docs_path(path: str) -> bool:
    """Report whether one changed path is documentation that no build or test reads."""
    if path.startswith("docs/"):
        return True
    parent, _, name = path.rpartition("/")
    if parent == "":
        return name.endswith(".md") or name.startswith("LICENSE")
    if parent == ".github":
        return name.endswith(".md")
    return False


def is_docs_only(paths: list[str]) -> bool:
    """Report whether a non-empty change set consists solely of docs paths."""
    return bool(paths) and all(is_docs_path(p) for p in paths)


def main() -> int:
    paths = [line.strip() for line in sys.stdin if line.strip()]
    print(f"docs_only={'true' if is_docs_only(paths) else 'false'}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
