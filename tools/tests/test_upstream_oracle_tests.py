"""Unit tests for the upstream-oracle cell selector and its log gate.

The gate exists because an oracle cell that cannot find upstream rsync passes
with a skip line, so CI counted those cells green while comparing nothing.
The selector must therefore find every resolver caller (a missed binary is a
cell CI never runs), and the log check must reject both a skipped cell and a
run that executed nothing (an empty filterset also exits 0 in nextest).
"""
from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from tools.ci.upstream_oracle_tests import check_log, nextest_args, oracle_binaries

CALL = "fn t() { let _ = test_support::require_upstream_rsync(V); }\n"


class OracleBinariesTests(unittest.TestCase):
    def setUp(self) -> None:
        self._tempdir = tempfile.TemporaryDirectory()
        self.root = Path(self._tempdir.name)
        self._write("Cargo.toml", '[package]\nname = "bin"\n')
        self._write("crates/cli/Cargo.toml", '[package]\nname = "cli"\n')

    def tearDown(self) -> None:
        self._tempdir.cleanup()

    def _write(self, rel: str, text: str) -> None:
        path = self.root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)

    def test_selects_resolver_callers_in_root_and_crate_tests(self) -> None:
        self._write("tests/oracle.rs", CALL)
        self._write("tests/plain.rs", "fn t() {}\n")
        self._write("crates/cli/tests/parity.rs", CALL)
        self.assertEqual(
            oracle_binaries(self.root), [("bin", "oracle"), ("cli", "parity")]
        )

    def test_a_nested_module_call_is_an_error_not_a_silent_omission(self) -> None:
        # A call from tests/integration/x.rs belongs to whichever binary
        # declares the module; guessing would drop the cell from CI unnoticed.
        self._write("tests/integration/helper.rs", CALL)
        with self.assertRaises(ValueError):
            oracle_binaries(self.root)

    def test_args_select_each_package_once_and_every_binary(self) -> None:
        args = nextest_args([("bin", "a"), ("bin", "b"), ("cli", "c")])
        self.assertEqual(
            args,
            ["-p", "bin", "-p", "cli", "-E",
             "binary_id(bin::a) | binary_id(bin::b) | binary_id(cli::c)"],
        )


class CheckLogTests(unittest.TestCase):
    SUMMARY = "     Summary [  12.3s] 7 tests run: 7 passed, 0 skipped\n"

    def test_a_clean_run_passes(self) -> None:
        self.assertEqual(check_log(self.SUMMARY), [])

    def test_a_skipped_oracle_cell_rejects_the_run(self) -> None:
        log = "Skipping upstream-compat test: upstream rsync 3.5.1 not installed\n"
        self.assertEqual(len(check_log(log + self.SUMMARY)), 1)

    def test_zero_tests_rejects_the_run(self) -> None:
        log = "     Summary [   0.0s] 0 tests run: 0 passed, 0 skipped\n"
        self.assertEqual(check_log(log), ["nextest ran 0 tests"])

    def test_a_missing_summary_rejects_the_run(self) -> None:
        self.assertEqual(check_log("error: build failed\n"), ["no nextest summary line found"])


if __name__ == "__main__":
    unittest.main()
