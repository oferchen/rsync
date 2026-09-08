"""Unit tests for the dead-code census and its gate.

Each classification test pins one of the three rules that were measured
defects in an earlier version of this instrument: re-export lines counted as
callers (hid 236 of 827 zero-caller functions), file-granular cfg(test)
masking, and trait impl methods treated as ordinary definitions.  The gate
tests pin the allowlist contract: reason and expiry are mandatory, expiry is
enforced, and a stale row fails as loudly as a missing one.
"""

from __future__ import annotations

import datetime
import io
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path

from tools.ci.dead_code_census import census, mask_source, run_gate


class CensusFixture(unittest.TestCase):
    def setUp(self) -> None:
        self._tempdir = tempfile.TemporaryDirectory()
        self.root = Path(self._tempdir.name)

    def tearDown(self) -> None:
        self._tempdir.cleanup()

    def write(self, relative: str, text: str) -> Path:
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")
        return path

    def tier_names(self) -> set[str]:
        return {row["name"] for row in census(self.root)}


class ReexportTests(CensusFixture):
    def test_a_pub_use_line_is_not_a_call_site(self) -> None:
        self.write("crates/demo/src/lib.rs", "pub fn orphan() {}\n")
        self.write("crates/demo/src/api.rs", "pub use crate::orphan;\n")
        self.assertIn("orphan", self.tier_names())

    def test_a_plain_use_import_is_not_a_call_site_either(self) -> None:
        self.write("crates/demo/src/lib.rs", "pub fn orphan() {}\n")
        self.write("crates/demo/src/api.rs", "use crate::orphan;\n")
        self.assertIn("orphan", self.tier_names())

    def test_a_multi_line_use_group_is_not_a_call_site(self) -> None:
        self.write("crates/demo/src/lib.rs", "pub fn orphan() {}\n")
        self.write(
            "crates/demo/src/api.rs",
            "pub use crate::{\n    orphan,\n    other,\n};\n",
        )
        self.assertIn("orphan", self.tier_names())

    def test_a_real_call_site_keeps_a_function_out(self) -> None:
        self.write("crates/demo/src/lib.rs", "pub fn wired() {}\n")
        self.write(
            "crates/demo/src/api.rs",
            "pub fn caller() {\n    crate::wired();\n}\n",
        )
        self.assertNotIn("wired", self.tier_names())

    def test_reexports_are_counted_separately(self) -> None:
        self.write("crates/demo/src/lib.rs", "pub fn orphan() {}\n")
        self.write("crates/demo/src/api.rs", "pub use crate::orphan;\n")
        rows = {row["name"]: row for row in census(self.root)}
        self.assertEqual(rows["orphan"]["reexport"], 1)


class CfgTestGranularityTests(CensusFixture):
    def test_a_test_only_reference_does_not_wire_a_function(self) -> None:
        # zero-caller-but-tested is OUTSIDE the gated tier: the tier is
        # zero-caller AND zero-test.
        self.write(
            "crates/demo/src/lib.rs",
            "pub fn tested_only() {}\n"
            "#[cfg(test)]\n"
            "mod tests {\n"
            "    #[test]\n"
            "    fn calls() {\n"
            "        crate::tested_only();\n"
            "    }\n"
            "}\n",
        )
        self.assertNotIn("tested_only", self.tier_names())

    def test_the_mask_ends_with_the_item_not_the_file(self) -> None:
        # A definition AFTER the cfg(test) module is still production, and a
        # reference after it is still a production reference.  File-granular
        # masking would hide both.
        self.write(
            "crates/demo/src/lib.rs",
            "#[cfg(test)]\n"
            "mod tests {\n"
            "    fn helper() {}\n"
            "}\n"
            "pub fn after_the_tests() {}\n"
            "pub fn wired_after_the_tests() {}\n"
            "pub fn caller() {\n"
            "    wired_after_the_tests();\n"
            "}\n",
        )
        names = self.tier_names()
        self.assertIn("after_the_tests", names)
        self.assertNotIn("wired_after_the_tests", names)

    def test_a_definition_inside_cfg_test_is_not_a_census_row(self) -> None:
        self.write(
            "crates/demo/src/lib.rs",
            "#[cfg(test)]\n"
            "mod tests {\n"
            "    pub fn test_helper_fn() {}\n"
            "}\n",
        )
        self.assertNotIn("test_helper_fn", self.tier_names())

    def test_a_braceless_cfg_test_item_masks_one_line(self) -> None:
        self.write(
            "crates/demo/src/lib.rs",
            "pub fn orphan() {}\n"
            "#[cfg(test)]\n"
            "use crate::orphan;\n"
            "pub fn still_production() {}\n",
        )
        names = self.tier_names()
        self.assertIn("orphan", names)
        self.assertIn("still_production", names)

    def test_a_whole_test_file_is_test_side(self) -> None:
        self.write("crates/demo/src/lib.rs", "pub fn tested_only() {}\n")
        self.write(
            "crates/demo/tests/integration.rs",
            "fn t() {\n    demo::tested_only();\n}\n",
        )
        self.assertNotIn("tested_only", self.tier_names())


class TraitImplTests(CensusFixture):
    def test_a_trait_impl_method_is_excluded(self) -> None:
        # Reached through dispatch, so textual counting cannot see callers.
        self.write(
            "crates/demo/src/lib.rs",
            "impl Frobber for Widget {\n"
            "    pub fn frobnicate(&self) {}\n"
            "}\n",
        )
        self.assertNotIn("frobnicate", self.tier_names())

    def test_an_inherent_impl_method_is_included(self) -> None:
        self.write(
            "crates/demo/src/lib.rs",
            "impl Widget {\n"
            "    pub fn dead_helper(&self) {}\n"
            "}\n",
        )
        self.assertIn("dead_helper", self.tier_names())


class AttributionGuardTests(CensusFixture):
    def test_a_name_defined_twice_is_ambiguous_and_excluded(self) -> None:
        self.write("crates/a/src/lib.rs", "pub fn twin() {}\n")
        self.write("crates/b/src/lib.rs", "pub fn twin() {}\n")
        self.assertNotIn("twin", self.tier_names())

    def test_a_mention_in_a_comment_or_string_is_not_a_reference(self) -> None:
        self.write("crates/demo/src/lib.rs", "pub fn orphan() {}\n")
        self.write(
            "crates/demo/src/api.rs",
            '// orphan is documented here\n'
            'pub fn caller() {\n'
            '    let _ = "orphan";\n'
            '}\n',
        )
        self.assertIn("orphan", self.tier_names())

    def test_mask_source_blanks_strings_and_comments(self) -> None:
        masked = mask_source('let x = "call()"; // call()\n/* call() */ real()')
        self.assertNotIn('"call()"', masked)
        self.assertEqual(masked.count("call()"), 0)
        self.assertIn("real()", masked)


class GateTests(CensusFixture):
    TODAY = datetime.date(2026, 9, 8)

    def gate(self, allowlist_text: str | None) -> tuple[int, str]:
        allowlist = self.root / "allowlist.tsv"
        if allowlist_text is not None:
            allowlist.write_text(allowlist_text, encoding="utf-8")
        out = io.StringIO()
        with redirect_stdout(out), redirect_stderr(out):
            code = run_gate(self.root, allowlist, self.TODAY)
        return code, out.getvalue()

    def dead_fn(self) -> None:
        self.write("crates/demo/src/lib.rs", "pub fn planted_dead_fn() {}\n")

    def test_an_unallowlisted_dead_fn_fails_the_gate(self) -> None:
        self.dead_fn()
        code, output = self.gate("# empty\n")
        self.assertEqual(code, 1)
        self.assertIn("planted_dead_fn", output)

    def test_an_allowlisted_row_with_future_expiry_passes(self) -> None:
        self.dead_fn()
        code, output = self.gate(
            "planted_dead_fn\tcrates/demo/src/lib.rs\t2026-11-07\tstaged\n"
        )
        self.assertEqual(code, 0, output)

    def test_an_expired_row_fails_loudly(self) -> None:
        self.dead_fn()
        code, output = self.gate(
            "planted_dead_fn\tcrates/demo/src/lib.rs\t2026-09-01\tstaged\n"
        )
        self.assertEqual(code, 1)
        self.assertIn("EXPIRED", output)

    def test_a_stale_row_fails(self) -> None:
        # The function is wired, so its allowlist row must be removed.
        self.write(
            "crates/demo/src/lib.rs",
            "pub fn planted_dead_fn() {}\n"
            "fn caller() {\n    planted_dead_fn();\n}\n",
        )
        code, output = self.gate(
            "planted_dead_fn\tcrates/demo/src/lib.rs\t2026-11-07\tstaged\n"
        )
        self.assertEqual(code, 1)
        self.assertIn("stale allowlist row", output)

    def test_a_row_without_a_reason_fails(self) -> None:
        self.dead_fn()
        code, output = self.gate(
            "planted_dead_fn\tcrates/demo/src/lib.rs\t2026-11-07\t\n"
        )
        self.assertEqual(code, 1)
        self.assertIn("empty reason", output)

    def test_a_malformed_expiry_fails(self) -> None:
        self.dead_fn()
        code, output = self.gate(
            "planted_dead_fn\tcrates/demo/src/lib.rs\tsoon\tstaged\n"
        )
        self.assertEqual(code, 1)
        self.assertIn("not YYYY-MM-DD", output)

    def test_a_missing_allowlist_fails(self) -> None:
        self.dead_fn()
        code, output = self.gate(None)
        self.assertEqual(code, 1)
        self.assertIn("allowlist not found", output)

    def test_a_clean_tree_with_an_empty_allowlist_passes(self) -> None:
        self.write(
            "crates/demo/src/lib.rs",
            "pub fn wired() {}\nfn caller() {\n    wired();\n}\n",
        )
        code, output = self.gate("# nothing staged\n")
        self.assertEqual(code, 0, output)


if __name__ == "__main__":
    unittest.main()
