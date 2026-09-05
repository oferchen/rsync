"""Unit tests for the whole-tree rustfmt gate's file enumeration.

The gate exists because `cargo fmt --all -- --check` cannot see a file that no
`mod` declaration reaches. Enumerating only *tracked* files reopened a second
hole of the same shape: a brand-new `.rs` file is invisible until it is
`git add`ed, so the gate reports clean locally and CI - which checks out a tree
where that file IS tracked - fails on it. These tests pin both halves.
"""

from __future__ import annotations

import subprocess
import tempfile
import unittest
from pathlib import Path

from tools.ci.check_rustfmt_all import rust_sources


class RustSourcesTests(unittest.TestCase):
    def setUp(self) -> None:
        self._tempdir = tempfile.TemporaryDirectory()
        self.root = Path(self._tempdir.name)
        self._git("init", "--quiet")
        # `git ls-files` needs an identity only for commits, but keeping the
        # repo self-contained stops a developer's global config from steering
        # the fixture.
        self._git("config", "user.email", "gate@example.invalid")
        self._git("config", "user.name", "gate")

    def tearDown(self) -> None:
        self._tempdir.cleanup()

    def _git(self, *args: str) -> None:
        subprocess.run(["git", *args], cwd=self.root, check=True, capture_output=True)

    def _write(self, name: str, body: str = "fn probe() {}\n") -> None:
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(body)

    def test_tracked_sources_are_enumerated(self) -> None:
        self._write("src/tracked.rs")
        self._git("add", "src/tracked.rs")

        self.assertEqual(rust_sources(self.root), ["src/tracked.rs"])

    def test_untracked_sources_are_enumerated(self) -> None:
        """The hole this gate had: an un-added file was invisible to it.

        Without `--others` the gate reports clean on a tree whose newest file
        is misformatted, and CI - where the file is tracked - then fails.
        """
        self._write("src/untracked.rs")

        self.assertEqual(rust_sources(self.root), ["src/untracked.rs"])

    def test_ignored_sources_are_excluded(self) -> None:
        """`--exclude-standard` keeps build output out of the argv.

        Without it, every `.rs` under `target/` would be submitted to rustfmt.
        """
        (self.root / ".gitignore").write_text("target/\n")
        self._write("target/generated.rs")
        self._write("src/kept.rs")

        self.assertEqual(rust_sources(self.root), ["src/kept.rs"])

    def test_non_rust_files_are_excluded(self) -> None:
        """The `*.rs` pathspec, isolated from the tracked/untracked axis."""
        self._write("src/kept.rs")
        (self.root / "notes.txt").write_text("not rust\n")
        (self.root / "build.rs.bak").write_text("not rust either\n")
        self._git("add", "-A")

        self.assertEqual(rust_sources(self.root), ["src/kept.rs"])

    def test_paths_are_relative_and_sorted(self) -> None:
        """Relative paths keep the argv under the platform limit on macOS."""
        for name in ("src/b.rs", "src/a.rs", "z.rs"):
            self._write(name)
        self._git("add", "-A")

        found = rust_sources(self.root)

        self.assertEqual(found, ["src/a.rs", "src/b.rs", "z.rs"])
        self.assertTrue(all(not Path(name).is_absolute() for name in found))


if __name__ == "__main__":
    unittest.main()
