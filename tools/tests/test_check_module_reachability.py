"""Unit tests for the module-reachability gate's file walk.

The gate reports `.rs` files the compiler never parses. Its whole value is the
walk being right, and every rule below is one that was measured WRONG in a
first draft against the live tree: each mis-rule turned live, compiled files
into reported orphans, and a gate that cries wolf on hundreds of real files gets
switched off rather than fixed.

The rules are the ones rustc applies, not approximations of them:

  * `include!()` re-bases module resolution on the INCLUDED file's directory,
    so a `mod tests;` inside an included fragment names a sibling of the
    fragment. The daemon's 96 section tests reach the compiler exactly this way.
  * `#[path]` outside an inline `mod { }` block resolves against the source
    file's OWN directory, never the module directory - so a non-`mod.rs` file
    does not push it one level down.
  * `#[path]` is separated from the `mod` keyword by whatever the author wrote
    between them: further attributes, and a visibility modifier.

The test count assertions are deliberate: a walk that reaches nothing passes any
"no orphans" assertion vacuously.
"""

from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from tools.ci.check_module_reachability import module_items, strip_noise, walk


class WalkTests(unittest.TestCase):
    def setUp(self) -> None:
        self._tempdir = tempfile.TemporaryDirectory()
        self.root = Path(self._tempdir.name)

    def tearDown(self) -> None:
        self._tempdir.cleanup()

    def _write(self, name: str, body: str) -> Path:
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(body)
        return path

    def _reached(self, root_file: str) -> set[str]:
        found = walk([self.root / root_file])
        return {p.relative_to(self.root.resolve()).as_posix() for p in found}

    def test_plain_mod_descends_into_the_named_directory(self) -> None:
        self._write("src/lib.rs", "mod outer;\n")
        self._write("src/outer.rs", "mod inner;\n")
        self._write("src/outer/inner.rs", "fn probe() {}\n")

        self.assertEqual(
            self._reached("src/lib.rs"),
            {"src/lib.rs", "src/outer.rs", "src/outer/inner.rs"},
        )

    def test_mod_rs_owns_its_own_directory(self) -> None:
        self._write("src/lib.rs", "mod outer;\n")
        self._write("src/outer/mod.rs", "mod inner;\n")
        self._write("src/outer/inner.rs", "fn probe() {}\n")

        self.assertEqual(
            self._reached("src/lib.rs"),
            {"src/lib.rs", "src/outer/mod.rs", "src/outer/inner.rs"},
        )

    def test_include_rebases_mod_resolution_on_the_fragment(self) -> None:
        """The rule that reaches the daemon's section tests.

        `sections/chunk.rs` is spliced into `lib.rs`, and its `mod tests;` names
        `sections/tests.rs` - a sibling of the FRAGMENT, not of `lib.rs`. Read
        the other way round the walk looks for `src/tests.rs`, finds nothing,
        and calls a file with 96 live tests an orphan.
        """
        self._write("src/lib.rs", 'include!("sections/chunk.rs");\n')
        self._write("src/sections/chunk.rs", "#[cfg(test)]\nmod tests;\n")
        self._write("src/sections/tests.rs", "#[test]\nfn probe() {}\n")

        self.assertEqual(
            self._reached("src/lib.rs"),
            {"src/lib.rs", "src/sections/chunk.rs", "src/sections/tests.rs"},
        )

    def test_include_path_is_relative_to_the_including_file(self) -> None:
        self._write("src/lib.rs", 'include!("sections/chunk.rs");\n')
        self._write("src/sections/chunk.rs", 'include!("chunk/part.rs");\n')
        self._write("src/sections/chunk/part.rs", "fn probe() {}\n")

        self.assertIn("src/sections/chunk/part.rs", self._reached("src/lib.rs"))

    def test_path_attribute_resolves_against_the_files_own_directory(self) -> None:
        """`#[path]` in a non-`mod.rs` file does NOT descend into `name/`.

        Resolving it against the module directory instead sends it one level too
        deep, where it finds nothing - which is how the logging crate's wired-in
        bridge tests read as unreachable.
        """
        self._write("src/lib.rs", "mod bridge;\n")
        self._write(
            "src/bridge.rs",
            '#[cfg(test)]\n#[path = "bridge_tests.rs"]\nmod integration_tests;\n',
        )
        self._write("src/bridge_tests.rs", "#[test]\nfn probe() {}\n")

        self.assertIn("src/bridge_tests.rs", self._reached("src/lib.rs"))

    def test_path_attribute_survives_intervening_visibility_and_attributes(self) -> None:
        """`#[cfg(..)] #[path = ".."] pub mod x;` - the workspace's own spelling.

        A forward-only "attribute immediately precedes `mod`" match is defeated
        by the `pub`, silently drops the path, and resolves the module to the
        wrong file. That mis-rule reported all 36 of fast_io's platform stubs as
        orphans.
        """
        self._write(
            "src/lib.rs",
            '#[cfg(not(unix))]\n#[path = "reader_stub.rs"]\npub mod reader;\n',
        )
        self._write("src/reader_stub.rs", "fn probe() {}\n")

        self.assertEqual(
            self._reached("src/lib.rs"), {"src/lib.rs", "src/reader_stub.rs"}
        )

    def test_inline_mod_block_pushes_resolution_one_level_down(self) -> None:
        self._write("src/lib.rs", "mod outer {\n    mod inner;\n}\n")
        self._write("src/outer/inner.rs", "fn probe() {}\n")

        self.assertEqual(
            self._reached("src/lib.rs"), {"src/lib.rs", "src/outer/inner.rs"}
        )

    def test_cfg_gated_modules_are_reached_on_every_platform(self) -> None:
        """The walk ignores `cfg`, deliberately.

        A Windows-only module is unreachable on a Linux build and still must not
        be reported: the gate asks whether the module tree names a file at all,
        not whether today's target compiles it.
        """
        self._write(
            "src/lib.rs",
            "#[cfg(windows)]\nmod win;\n#[cfg(unix)]\nmod nix;\n",
        )
        self._write("src/win.rs", "fn probe() {}\n")
        self._write("src/nix.rs", "fn probe() {}\n")

        self.assertEqual(
            self._reached("src/lib.rs"), {"src/lib.rs", "src/win.rs", "src/nix.rs"}
        )

    def test_orphan_is_not_reached(self) -> None:
        """The gate's whole point: a declared file is reached, a stray one is not."""
        self._write("src/lib.rs", "mod live;\n")
        self._write("src/live.rs", "fn probe() {}\n")
        self._write("src/stray.rs", "#[test]\nfn never_runs() {}\n")

        reached = self._reached("src/lib.rs")

        self.assertIn("src/live.rs", reached)
        self.assertNotIn("src/stray.rs", reached)

    def test_module_cycle_terminates(self) -> None:
        """Two files that `include!` each other must not spin the walk forever."""
        self._write("src/lib.rs", 'include!("a.rs");\n')
        self._write("src/a.rs", 'include!("b.rs");\n')
        self._write("src/b.rs", 'include!("a.rs");\n')

        self.assertEqual(
            self._reached("src/lib.rs"), {"src/lib.rs", "src/a.rs", "src/b.rs"}
        )


class StripNoiseTests(unittest.TestCase):
    def test_commented_out_declarations_do_not_count(self) -> None:
        text = strip_noise("// mod ghost;\n/* mod other; */\nmod real;\n")

        self.assertEqual([name for name, _, _ in module_items(text)], ["real"])

    def test_raw_string_stripping_does_not_swallow_real_declarations(self) -> None:
        """An unanchored raw-string pattern ate the code between two quotes.

        `for "x" ... "y"` has no raw string in it, but a pattern matching a bare
        `r"..."` anywhere finds one inside `for`, spans to the next quote, and
        deletes every `mod` item in between. That is how a working metadata
        crate reported four orphaned modules.
        """
        text = strip_noise(
            'fn f() { for c in "a" { let _ = "b"; } }\nmod real;\nlet s = r"raw";\n'
        )

        self.assertIn("real", [name for name, _, _ in module_items(text)])


if __name__ == "__main__":
    unittest.main()
