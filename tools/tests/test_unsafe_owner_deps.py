"""Unit tests for the two-owner FFI dependency gate.

The scan tests pin what counts as a direct FFI dependency: production tables
only (dev/build excluded), `package =` renames resolved to the real crate, the
two owner crates exempt.  The gate tests pin the allowlist contract - reason
and expiry mandatory, expiry enforced, a stale row as loud as a missing one -
and one behavioural non-vacuity check: a planted FFI dep in a clean crate reds
the gate, and removing it greens it again.
"""

from __future__ import annotations

import datetime
import io
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path

from tools.ci.unsafe_owner_deps import parse_allowlist, run_gate, scan


class Fixture(unittest.TestCase):
    def setUp(self) -> None:
        self._tempdir = tempfile.TemporaryDirectory()
        self.root = Path(self._tempdir.name)

    def tearDown(self) -> None:
        self._tempdir.cleanup()

    def crate(self, name: str, manifest: str) -> None:
        path = self.root / "crates" / name / "Cargo.toml"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(manifest, encoding="utf-8")

    def violations(self) -> set[tuple[str, str]]:
        return {(c, d) for c, d, _ in scan(self.root)}


class ScanTests(Fixture):
    def test_a_direct_libc_dep_is_a_violation(self) -> None:
        self.crate("demo", '[package]\nname = "demo"\n[dependencies]\nlibc = "0.2"\n')
        self.assertIn(("demo", "libc"), self.violations())

    def test_the_two_owner_crates_are_exempt(self) -> None:
        self.crate(
            "platform", '[package]\nname = "platform"\n[dependencies]\nlibc = "0.2"\n'
        )
        self.crate(
            "fast_io", '[package]\nname = "fast_io"\n[dependencies]\nrustix = "1"\n'
        )
        self.assertEqual(self.violations(), set())

    def test_dev_and_build_dependencies_are_out_of_scope(self) -> None:
        self.crate(
            "demo",
            '[package]\nname = "demo"\n'
            '[dev-dependencies]\nlibc = "0.2"\n'
            '[build-dependencies]\nnix = "0.31"\n',
        )
        self.assertEqual(self.violations(), set())

    def test_a_target_specific_dependency_is_in_scope(self) -> None:
        self.crate(
            "demo",
            '[package]\nname = "demo"\n'
            '[target."cfg(windows)".dependencies]\nwindows = "0.65"\n',
        )
        self.assertIn(("demo", "windows"), self.violations())

    def test_a_package_rename_is_resolved_to_the_real_crate(self) -> None:
        self.crate(
            "demo",
            '[package]\nname = "demo"\n'
            '[dependencies]\nnotlibc = { package = "libc", version = "0.2" }\n',
        )
        self.assertIn(("demo", "libc"), self.violations())

    def test_a_non_owner_dependency_is_ignored(self) -> None:
        self.crate(
            "demo", '[package]\nname = "demo"\n[dependencies]\nthiserror = "1"\n'
        )
        self.assertEqual(self.violations(), set())

    def test_all_five_owner_deps_are_matched(self) -> None:
        self.crate(
            "demo",
            '[package]\nname = "demo"\n[dependencies]\n'
            'libc = "0.2"\nnix = "0.31"\nrustix = "1"\n'
            'windows = "0.65"\nwindows-sys = "0.65"\n',
        )
        deps = {d for _, d in self.violations()}
        self.assertEqual(deps, {"libc", "nix", "rustix", "windows", "windows-sys"})


class GateTests(Fixture):
    TODAY = datetime.date(2026, 9, 23)

    def gate(self, allowlist_text: str | None) -> tuple[int, str]:
        allowlist = self.root / "allowlist.tsv"
        if allowlist_text is not None:
            allowlist.write_text(allowlist_text, encoding="utf-8")
        out = io.StringIO()
        with redirect_stdout(out), redirect_stderr(out):
            code = run_gate(self.root, allowlist, self.TODAY)
        return code, out.getvalue()

    def violator(self) -> None:
        self.crate("demo", '[package]\nname = "demo"\n[dependencies]\nlibc = "0.2"\n')

    def test_an_unallowlisted_violator_fails(self) -> None:
        self.violator()
        code, output = self.gate("# empty\n")
        self.assertEqual(code, 1)
        self.assertIn("demo", output)
        self.assertIn("libc", output)

    def test_an_allowlisted_row_with_future_expiry_passes(self) -> None:
        self.violator()
        code, output = self.gate("demo\tlibc\t2026-11-07\terrno constants\n")
        self.assertEqual(code, 0, output)

    def test_an_expired_row_fails_loudly(self) -> None:
        self.violator()
        code, output = self.gate("demo\tlibc\t2026-09-01\terrno constants\n")
        self.assertEqual(code, 1)
        self.assertIn("EXPIRED", output)

    def test_a_stale_row_fails(self) -> None:
        # No crate declares libc, so the allowlist row must be removed.
        self.crate("demo", '[package]\nname = "demo"\n[dependencies]\nthiserror = "1"\n')
        code, output = self.gate("demo\tlibc\t2026-11-07\terrno constants\n")
        self.assertEqual(code, 1)
        self.assertIn("stale allowlist row", output)

    def test_a_row_without_a_reason_fails(self) -> None:
        self.violator()
        code, output = self.gate("demo\tlibc\t2026-11-07\t\n")
        self.assertEqual(code, 1)
        self.assertIn("empty reason", output)

    def test_a_malformed_field_count_fails(self) -> None:
        self.violator()
        code, output = self.gate("demo\tlibc\t2026-11-07\n")
        self.assertEqual(code, 1)
        self.assertIn("4 tab-separated fields", output)

    def test_a_malformed_expiry_fails(self) -> None:
        self.violator()
        code, output = self.gate("demo\tlibc\tsoon\terrno constants\n")
        self.assertEqual(code, 1)
        self.assertIn("not YYYY-MM-DD", output)

    def test_an_owner_crate_in_the_allowlist_is_an_error(self) -> None:
        # platform is exempt, so a row for it can only be a mistake.
        self.crate(
            "platform", '[package]\nname = "platform"\n[dependencies]\nlibc = "0.2"\n'
        )
        code, output = self.gate("platform\tlibc\t2026-11-07\treason\n")
        self.assertEqual(code, 1)
        self.assertIn("owner crate", output)

    def test_a_non_owner_dep_in_the_allowlist_is_an_error(self) -> None:
        self.violator()
        code, output = self.gate(
            "demo\tlibc\t2026-11-07\terrno constants\n"
            "demo\tthiserror\t2026-11-07\tnot an FFI dep\n"
        )
        self.assertEqual(code, 1)
        self.assertIn("not an owner dep", output)

    def test_a_missing_allowlist_fails(self) -> None:
        self.violator()
        code, output = self.gate(None)
        self.assertEqual(code, 1)
        self.assertIn("allowlist not found", output)

    def test_a_clean_tree_with_an_empty_allowlist_passes(self) -> None:
        self.crate("demo", '[package]\nname = "demo"\n[dependencies]\nthiserror = "1"\n')
        code, output = self.gate("# nothing staged\n")
        self.assertEqual(code, 0, output)

    def test_planted_dep_reds_and_removal_greens(self) -> None:
        # Non-vacuity: a clean crate is green; planting an owner dep reds the
        # gate; removing it greens it again.
        clean = '[package]\nname = "core"\n[dependencies]\nthiserror = "1"\n'
        planted = '[package]\nname = "core"\n[dependencies]\nlibc = "0.2"\n'
        self.crate("core", clean)
        self.assertEqual(self.gate("# empty\n")[0], 0)
        self.crate("core", planted)
        code, output = self.gate("# empty\n")
        self.assertEqual(code, 1)
        self.assertIn("core", output)
        self.crate("core", clean)
        self.assertEqual(self.gate("# empty\n")[0], 0)


class ParseTests(Fixture):
    def test_a_duplicate_row_is_reported(self) -> None:
        path = self.root / "allowlist.tsv"
        path.write_text(
            "demo\tlibc\t2026-11-07\tone\ndemo\tlibc\t2026-11-07\ttwo\n",
            encoding="utf-8",
        )
        _, errors = parse_allowlist(path)
        self.assertTrue(any("duplicate entry" in e for e in errors))


if __name__ == "__main__":
    unittest.main()
