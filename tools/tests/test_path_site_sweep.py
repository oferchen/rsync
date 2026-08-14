"""Unit tests for the production path-site sweep.

The sweep's output feeds the confinement-resolver work (docs/design/
upstream-3.5.0-path-confinement-model.md), so a site it wrongly counts becomes
work that gets scoped and never needed. Each test below pins one reason a file
is or is not production code; the negative cases matter as much as the
positive ones, because a rule that excludes too much shrinks the number just as
convincingly as a correct one.
"""

from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from tools.ci.path_site_sweep import dev_only_crates, sweep, test_only_files


class SweepFixture(unittest.TestCase):
    def setUp(self) -> None:
        self._tempdir = tempfile.TemporaryDirectory()
        self.root = Path(self._tempdir.name)
        self.crates = self.root / "crates"

    def tearDown(self) -> None:
        self._tempdir.cleanup()

    def write(self, relative: str, text: str) -> Path:
        path = self.crates / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)
        return path

    def site_paths(self) -> set[str]:
        return {site[2] for site in sweep(str(self.crates))}


class TestOnlyModuleTests(SweepFixture):
    """A file is test code when its `mod` declaration is cfg(test)-gated."""

    def test_file_behind_a_gated_mod_is_not_a_production_site(self) -> None:
        self.write("demo/src/lib.rs", "#[cfg(test)]\nmod helper;\n")
        self.write("demo/src/helper.rs", "fn f() { File::create(p); }\n")
        self.assertEqual(set(), self.site_paths())

    def test_file_behind_an_ungated_mod_is_still_counted(self) -> None:
        """The control. Without it, a rule that excluded everything would pass."""
        self.write("demo/src/lib.rs", "mod helper;\n")
        self.write("demo/src/helper.rs", "fn f() { File::create(p); }\n")
        self.assertEqual(1, len(self.site_paths()))

    def test_the_all_test_unix_spelling_is_recognised(self) -> None:
        """`#[cfg(test)]` is only the simplest spelling; the tree uses others."""
        self.write("demo/src/lib.rs", "#[cfg(all(unix, test))]\nmod helper;\n")
        self.write("demo/src/helper.rs", "fn f() { File::create(p); }\n")
        self.assertEqual(set(), self.site_paths())

    def test_an_intervening_attribute_does_not_hide_the_gate(self) -> None:
        self.write(
            "demo/src/lib.rs",
            "#[cfg(test)]\n#[allow(clippy::pedantic)]\nmod helper;\n",
        )
        self.write("demo/src/helper.rs", "fn f() { File::create(p); }\n")
        self.assertEqual(set(), self.site_paths())

    def test_a_mod_rs_directory_module_resolves(self) -> None:
        self.write("demo/src/lib.rs", "#[cfg(test)]\nmod helper;\n")
        self.write("demo/src/helper/mod.rs", "fn f() { File::create(p); }\n")
        self.assertEqual(set(), self.site_paths())

    def test_exclusion_reaches_submodules_of_a_gated_module(self) -> None:
        """A module the compiler only builds under cfg(test) cannot make its
        children reachable, so the gate has to propagate."""
        self.write("demo/src/lib.rs", "#[cfg(test)]\nmod helper;\n")
        self.write("demo/src/helper/mod.rs", "mod deeper;\n")
        self.write("demo/src/helper/deeper.rs", "fn f() { File::create(p); }\n")
        self.assertEqual(set(), self.site_paths())

    def test_a_visibility_qualifier_does_not_hide_the_declaration(self) -> None:
        self.write("demo/src/lib.rs", "#[cfg(test)]\npub(crate) mod helper;\n")
        self.write("demo/src/helper.rs", "fn f() { File::create(p); }\n")
        self.assertEqual(set(), self.site_paths())

    def test_a_quoted_test_feature_name_is_not_a_test_gate(self) -> None:
        """`feature = "test-utils"` is a production feature, not cfg(test)."""
        self.write("demo/src/lib.rs", '#[cfg(feature = "test-utils")]\nmod helper;\n')
        self.write("demo/src/helper.rs", "fn f() { File::create(p); }\n")
        self.assertEqual(1, len(self.site_paths()))

    def test_only_the_declared_file_is_excluded(self) -> None:
        self.write("demo/src/lib.rs", "#[cfg(test)]\nmod helper;\nmod real;\n")
        self.write("demo/src/helper.rs", "fn f() { File::create(p); }\n")
        self.write("demo/src/real.rs", "fn g() { fs::rename(a, b); }\n")
        excluded = test_only_files(str(self.crates))
        self.assertIn(str(self.crates / "demo/src/helper.rs"), excluded)
        self.assertNotIn(str(self.crates / "demo/src/real.rs"), excluded)


class DevOnlyCrateTests(SweepFixture):
    """A crate reached only from dev-dependencies is never shipped."""

    def manifest(self, relative: str, text: str) -> None:
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)

    def test_a_crate_used_only_as_a_dev_dependency_is_excluded(self) -> None:
        self.manifest("Cargo.toml", "[workspace]\n")
        self.manifest("crates/helper/Cargo.toml", '[package]\nname = "helper"\n')
        self.manifest(
            "crates/app/Cargo.toml",
            '[package]\nname = "app"\n\n[dev-dependencies]\nhelper = { path = "../helper" }\n',
        )
        self.assertEqual({"helper"}, dev_only_crates(str(self.crates)))

    def test_a_crate_also_used_as_a_real_dependency_is_kept(self) -> None:
        self.manifest("Cargo.toml", "[workspace]\n")
        self.manifest("crates/helper/Cargo.toml", '[package]\nname = "helper"\n')
        self.manifest(
            "crates/app/Cargo.toml",
            '[package]\nname = "app"\n\n[dependencies]\nhelper = { path = "../helper" }\n'
            '\n[dev-dependencies]\nhelper = { path = "../helper" }\n',
        )
        self.assertEqual(set(), dev_only_crates(str(self.crates)))

    def test_a_root_manifest_target_dependency_keeps_the_crate(self) -> None:
        """The measured false positive: a platform-gated runtime dependency is
        declared in the WORKSPACE ROOT manifest and in no crate manifest.
        Reading only crates/*/Cargo.toml classified it as test-only - and
        because the crate happened to hold no path sites, the total did not
        move, so the misclassification was invisible in the output.

        The dev edge is deliberately placed in a CRATE manifest and the runtime
        edge in the ROOT one. Putting both in the root makes the test pass even
        when the root scan is removed - the crate then has no dev edge either,
        so the dev-edge requirement alone suppresses it and the assertion holds
        for the wrong reason.
        """
        self.manifest(
            "Cargo.toml",
            "[workspace]\n\n[target.'cfg(windows)'.dependencies]\n"
            'shim = { path = "crates/shim" }\n',
        )
        self.manifest("crates/shim/Cargo.toml", '[package]\nname = "shim"\n')
        self.manifest(
            "crates/app/Cargo.toml",
            '[package]\nname = "app"\n\n[dev-dependencies]\nshim = { path = "../shim" }\n',
        )
        self.assertEqual(set(), dev_only_crates(str(self.crates)))

    def test_a_crate_nothing_depends_on_is_not_called_dev_only(self) -> None:
        """Unreferenced is a different finding from test-only, and claiming the
        latter would quietly drop a shipped crate that simply has no in-tree
        dependents."""
        self.manifest("Cargo.toml", "[workspace]\n")
        self.manifest("crates/lonely/Cargo.toml", '[package]\nname = "lonely"\n')
        self.assertEqual(set(), dev_only_crates(str(self.crates)))

    def test_sites_in_a_dev_only_crate_are_not_swept(self) -> None:
        self.manifest("Cargo.toml", "[workspace]\n")
        self.manifest("crates/helper/Cargo.toml", '[package]\nname = "helper"\n')
        self.manifest(
            "crates/app/Cargo.toml",
            '[package]\nname = "app"\n\n[dev-dependencies]\nhelper = { path = "../helper" }\n',
        )
        self.write("helper/src/lib.rs", "fn f() { File::create(p); }\n")
        self.write("app/src/lib.rs", "fn g() { fs::rename(a, b); }\n")
        self.assertEqual({str(self.crates / "app/src/lib.rs")}, self.site_paths())


if __name__ == "__main__":
    unittest.main()
