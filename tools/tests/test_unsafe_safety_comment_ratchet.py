"""Unit tests for the unsafe SAFETY-comment audit and its ratchet gate.

CI runs the gate on every push, but a green run only proves the gate is wired,
never that it fires on the right input. These tests are what pin the ratchet
contract: a NEW unsafe block without a SAFETY comment pushes a scope above its
baseline and fails; an annotated block does not count; a decrease never fails
but nudges; and the malformed-baseline paths fail loudly rather than passing
vacuously.

The synthetic crate trees stand in for `crates/` so the tests need no Rust
sources and run identically on a developer machine and a runner.
"""

from __future__ import annotations

import io
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path

from tools.audit.unsafe_safety_comment_audit import (
    run_ratchet,
    violations_by_scope,
    collect,
)

MISSING = "pub fn f() {\n    let _ = unsafe { g() };\n}\n"
ANNOTATED = (
    "pub fn f() {\n"
    "    // SAFETY: g() has no preconditions and is called on the main thread.\n"
    "    let _ = unsafe { g() };\n"
    "}\n"
)
PLACEHOLDER = (
    "pub fn f() {\n"
    "    // SAFETY: TODO\n"
    "    let _ = unsafe { g() };\n"
    "}\n"
)


class RatchetFixture(unittest.TestCase):
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

    def ratchet(self, baseline_text: str | None) -> tuple[int, str]:
        baseline = self.root / "baseline.tsv"
        if baseline_text is not None:
            baseline.write_text(baseline_text, encoding="utf-8")
        out = io.StringIO()
        with redirect_stdout(out), redirect_stderr(out):
            code = run_ratchet(self.root, baseline)
        return code, out.getvalue()


class ScopeCountingTests(RatchetFixture):
    def test_a_missing_safety_block_is_a_violation(self) -> None:
        self.write("crates/demo/src/lib.rs", MISSING)
        counts = violations_by_scope(collect(self.root)[2])
        self.assertEqual(counts.get("demo"), 1)

    def test_an_annotated_block_is_not_a_violation(self) -> None:
        self.write("crates/demo/src/lib.rs", ANNOTATED)
        counts = violations_by_scope(collect(self.root)[2])
        self.assertNotIn("demo", counts)

    def test_a_placeholder_safety_is_a_violation(self) -> None:
        self.write("crates/demo/src/lib.rs", PLACEHOLDER)
        counts = violations_by_scope(collect(self.root)[2])
        self.assertEqual(counts.get("demo"), 1)

    def test_a_test_target_is_keyed_separately_from_the_library(self) -> None:
        self.write("crates/demo/src/lib.rs", MISSING)
        self.write("crates/demo/tests/it.rs", MISSING)
        counts = violations_by_scope(collect(self.root)[2])
        self.assertEqual(counts.get("demo"), 1)
        self.assertEqual(counts.get("demo (tests)"), 1)


class RatchetGateTests(RatchetFixture):
    def test_at_baseline_passes(self) -> None:
        self.write("crates/demo/src/lib.rs", MISSING)
        code, output = self.ratchet("demo\t1\n")
        self.assertEqual(code, 0, output)
        self.assertIn("none above baseline", output)

    def test_an_increase_above_baseline_fails_naming_the_scope(self) -> None:
        self.write("crates/demo/src/a.rs", MISSING)
        self.write("crates/demo/src/b.rs", MISSING)
        code, output = self.ratchet("demo\t1\n")
        self.assertEqual(code, 1)
        self.assertIn("demo", output)
        self.assertIn("exceeds baseline 1", output)

    def test_a_new_scope_without_a_baseline_row_fails(self) -> None:
        self.write("crates/demo/src/lib.rs", MISSING)
        code, output = self.ratchet("# empty\n")
        self.assertEqual(code, 1)
        self.assertIn("no baseline entry", output)

    def test_annotating_the_block_makes_the_gate_green(self) -> None:
        # The discriminating pair: same file, SAFETY comment added.
        self.write("crates/demo/src/lib.rs", ANNOTATED)
        code, output = self.ratchet("# empty\n")
        self.assertEqual(code, 0, output)

    def test_a_decrease_passes_and_nudges(self) -> None:
        self.write("crates/demo/src/lib.rs", MISSING)
        code, output = self.ratchet("demo\t5\n")
        self.assertEqual(code, 0, output)
        self.assertIn("nudge", output)
        self.assertIn("lower the baseline to 1", output)

    def test_a_fully_fixed_scope_nudges_to_zero(self) -> None:
        # No unsafe at all, but the baseline still carries the row.
        self.write("crates/demo/src/lib.rs", "pub fn f() {}\n")
        code, output = self.ratchet("demo\t3\n")
        self.assertEqual(code, 0, output)
        self.assertIn("0 violations now", output)

    def test_a_malformed_row_fails(self) -> None:
        self.write("crates/demo/src/lib.rs", MISSING)
        code, output = self.ratchet("demo\t1\textra\n")
        self.assertEqual(code, 1)
        self.assertIn("expected 2 tab-separated fields", output)

    def test_a_non_integer_count_fails(self) -> None:
        self.write("crates/demo/src/lib.rs", MISSING)
        code, output = self.ratchet("demo\tmany\n")
        self.assertEqual(code, 1)
        self.assertIn("not an integer", output)

    def test_a_missing_baseline_fails(self) -> None:
        self.write("crates/demo/src/lib.rs", MISSING)
        code, output = self.ratchet(None)
        self.assertEqual(code, 1)
        self.assertIn("baseline not found", output)

    def test_a_clean_tree_at_zero_baseline_passes(self) -> None:
        self.write("crates/demo/src/lib.rs", ANNOTATED)
        code, output = self.ratchet("# nothing outstanding\n")
        self.assertEqual(code, 0, output)


if __name__ == "__main__":
    unittest.main()
