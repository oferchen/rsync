"""Unit tests for the upstream-oracle cell selector and its log gate.

The gate exists because an oracle cell that cannot find upstream rsync passes
with a skip line, so CI counted those cells green while comparing nothing.
The selector must therefore find every resolver caller (a missed binary is a
cell CI never runs), and the log check must reject both a skipped cell and a
run that executed nothing (an empty filterset also exits 0 in nextest).
"""
from __future__ import annotations

import contextlib
import io
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from tools.ci import upstream_oracle_tests
from tools.ci.upstream_oracle_tests import (
    check_log,
    job_block,
    nextest_args,
    oracle_binaries,
    stale_exclusions,
)

CALL = "fn t() { let _ = test_support::require_upstream_rsync(V); }\n"
WORKFLOW = (
    "name: ci\n"
    "jobs:\n"
    "  perf:\n"
    "    steps:\n"
    "      - run: cargo nextest run -E 'test(=bench)'\n"
    "  other:\n"
    "    steps:\n"
    "      - run: cargo nextest run -E 'test(=unrelated)'\n"
)


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

    def test_an_excluded_cell_is_left_to_its_own_job(self) -> None:
        self._write("tests/oracle.rs", CALL)
        self._write("tests/bench.rs", CALL)
        excluded = {"tests/bench.rs": (".github/workflows/ci.yml", "perf")}
        self.assertEqual(oracle_binaries(self.root, excluded), [("bin", "oracle")])

    def test_args_select_each_package_once_and_every_binary(self) -> None:
        args = nextest_args([("bin", "a"), ("bin", "b"), ("cli", "c")])
        self.assertEqual(
            args,
            ["-p", "bin", "-p", "cli",
             "--test", "a", "--test", "b", "--test", "c", "-E",
             "binary_id(bin::a) | binary_id(bin::b) | binary_id(cli::c)"],
        )

    def test_args_build_only_the_selected_test_targets(self) -> None:
        # Without --test, cargo compiles every integration test of each
        # package before the filterset skips them, which is what pushed the
        # interop job past its time limit.
        args = nextest_args([("bin", "a"), ("cli", "a"), ("cli", "c")])
        targets = [args[i + 1] for i, arg in enumerate(args) if arg == "--test"]
        self.assertEqual(targets, ["a", "c"])


class StaleExclusionTests(unittest.TestCase):
    """An exclusion moves a cell out of the oracle step, so it is only sound
    while another job still runs that cell: each way an entry can rot must
    be reported, or the cell silently stops running anywhere."""

    ENTRY = {"tests/bench.rs": (".github/workflows/ci.yml", "perf")}

    def setUp(self) -> None:
        self._tempdir = tempfile.TemporaryDirectory()
        self.root = Path(self._tempdir.name)
        self._write(".github/workflows/ci.yml", WORKFLOW)
        self._write("tests/bench.rs", CALL)

    def tearDown(self) -> None:
        self._tempdir.cleanup()

    def _write(self, rel: str, text: str) -> None:
        path = self.root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)

    def test_an_entry_its_job_runs_holds(self) -> None:
        self.assertEqual(stale_exclusions(self.root, self.ENTRY), [])

    def test_a_removed_file_is_stale(self) -> None:
        (self.root / "tests/bench.rs").unlink()
        self.assertEqual(
            stale_exclusions(self.root, self.ENTRY),
            ["excluded tests/bench.rs no longer exists"],
        )

    def test_a_file_that_stopped_calling_the_resolver_is_stale(self) -> None:
        self._write("tests/bench.rs", "fn t() {}\n")
        self.assertEqual(
            stale_exclusions(self.root, self.ENTRY),
            ["excluded tests/bench.rs no longer calls require_upstream_rsync"],
        )

    def test_a_missing_workflow_is_stale(self) -> None:
        excluded = {"tests/bench.rs": (".github/workflows/gone.yml", "perf")}
        self.assertEqual(
            stale_exclusions(self.root, excluded),
            ["excluded tests/bench.rs: workflow .github/workflows/gone.yml does not exist"],
        )

    def test_a_missing_job_is_stale(self) -> None:
        excluded = {"tests/bench.rs": (".github/workflows/ci.yml", "nightly")}
        self.assertEqual(
            stale_exclusions(self.root, excluded),
            ["excluded tests/bench.rs: .github/workflows/ci.yml has no job nightly"],
        )

    def test_a_job_that_no_longer_names_the_cell_is_stale(self) -> None:
        # The test name appears in the workflow, but under a different job:
        # the named job must be the one that runs it.
        excluded = {"tests/bench.rs": (".github/workflows/ci.yml", "other")}
        self.assertEqual(
            stale_exclusions(self.root, excluded),
            ["excluded tests/bench.rs: job other in .github/workflows/ci.yml does not run it"],
        )

    def test_a_job_block_ends_at_the_next_job(self) -> None:
        self.assertNotIn("unrelated", job_block(WORKFLOW, "perf"))
        self.assertIn("unrelated", job_block(WORKFLOW, "other"))

    def test_args_refuses_to_select_while_an_exclusion_is_stale(self) -> None:
        self._write("Cargo.toml", '[package]\nname = "bin"\n')
        stale = {"tests/gone.rs": (".github/workflows/ci.yml", "perf")}
        with mock.patch.object(upstream_oracle_tests, "EXCLUDED", stale), \
                contextlib.redirect_stderr(io.StringIO()) as err, \
                contextlib.redirect_stdout(io.StringIO()):
            status = upstream_oracle_tests.main(["--root", str(self.root), "args"])
        self.assertEqual(status, 2)
        self.assertIn("excluded tests/gone.rs no longer exists", err.getvalue())

    def test_the_tree_has_no_stale_exclusion(self) -> None:
        root = Path(__file__).resolve().parents[2]
        self.assertEqual(stale_exclusions(root), [])


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


    def test_check_log_strips_ansi_escapes(self) -> None:
        log = "\x1b[32m     Summary\x1b[0m [  33.777s] \x1b[1m205\x1b[0m tests run: 205 passed, 0 skipped\n"
        self.assertEqual(check_log(log), [])


if __name__ == "__main__":
    unittest.main()
