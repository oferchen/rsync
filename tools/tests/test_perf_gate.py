#!/usr/bin/env python3
"""Tests for scripts/perf_gate.py, the performance-gate harness.

The harness produces the numbers a release decision rests on, so these guard
the parts where a silent mistake would change a reported figure or destroy
data: which rows enter a median, how a ratio is formed, how strace and --stats
output is read, which binary serves each side, and what may be removed.
"""

from __future__ import annotations

import importlib.util
import io
import sys
import tempfile
import unittest
from contextlib import redirect_stderr
from pathlib import Path

_SCRIPT = Path(__file__).resolve().parents[2] / "scripts" / "perf_gate.py"
_spec = importlib.util.spec_from_file_location("perf_gate", _SCRIPT)
perf_gate = importlib.util.module_from_spec(_spec)
sys.modules["perf_gate"] = perf_gate
# Loading by path would otherwise leave scripts/__pycache__ in the checkout.
_write_bytecode, sys.dont_write_bytecode = sys.dont_write_bytecode, True
try:
    _spec.loader.exec_module(perf_gate)
finally:
    sys.dont_write_bytecode = _write_bytecode


def _row(side: str, wall: float, kind: str = "run", rss: int = 1000) -> dict:
    return {"cell": "c", "transport": "local", "side": side, "kind": kind, "iter": 1,
            "exit": 0, "wall_s": wall, "user_s": 0.5, "sys_s": 0.25, "maxrss_kb": rss,
            "srv_maxrss_kb": ""}


class WorkspaceRemovalTests(unittest.TestCase):
    """Only directories the run created may be removed: a benchmark cleanup
    once wiped a bind-mounted checkout, so every refusal here is deliberate."""

    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.data = Path(self.tmp.name)

    def test_recorded_directory_is_removed_and_foreign_one_is_refused(self) -> None:
        work = perf_gate.Workspace(self.data, "t")
        own = work.make_dir("dst")
        foreign = work.root / "foreign"
        foreign.mkdir()
        work.remove_dir(own)
        self.assertFalse(own.exists())
        with self.assertRaises(perf_gate.HarnessError):
            work.remove_dir(foreign)
        with self.assertRaises(perf_gate.HarnessError):
            work.remove_dir(self.data)
        self.assertTrue(foreign.exists())

    def test_nothing_is_removed_once_the_marker_is_gone(self) -> None:
        work = perf_gate.Workspace(self.data, "t")
        own = work.make_dir("dst")
        (work.root / perf_gate.MARKER).unlink()
        with self.assertRaises(perf_gate.HarnessError):
            work.remove_dir(own)
        with self.assertRaises(perf_gate.HarnessError):
            work.destroy()
        self.assertTrue(own.exists())

    def test_an_existing_directory_is_never_adopted(self) -> None:
        first = perf_gate.Workspace(self.data, "t")
        with self.assertRaises(FileExistsError):
            perf_gate.Workspace(self.data, "t")
        self.assertTrue(first.root.exists())


class RequiredArgumentTests(unittest.TestCase):
    def test_missing_data_dir_is_refused_before_anything_runs(self) -> None:
        with redirect_stderr(io.StringIO()) as err, self.assertRaises(SystemExit) as caught:
            perf_gate.main(["--cell", "small-100k", "--oc", "/bin/true"])
        self.assertEqual(caught.exception.code, 2)
        self.assertIn("--data-dir", err.getvalue())

    def test_missing_oc_binary_is_refused(self) -> None:
        with redirect_stderr(io.StringIO()) as err, self.assertRaises(SystemExit):
            perf_gate.main(["--cell", "small-100k", "--data-dir", "/nonexistent"])
        self.assertIn("--oc", err.getvalue())


class UpstreamBinaryTests(unittest.TestCase):
    """The reference side must come from somewhere every checkout can name; a
    path that exists on one machine only made the harness unusable elsewhere."""

    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.repo = Path(self.tmp.name)

    def _executable(self, relative: str) -> Path:
        path = self.repo / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("#!/bin/sh\n")
        path.chmod(0o755)
        return path.resolve()

    def test_argument_beats_the_variable_which_beats_the_interop_build(self) -> None:
        given = self._executable("given/rsync")
        named = self._executable("named/rsync")
        built = self._executable(perf_gate.UPSTREAM_INTEROP_PATH)
        env = {perf_gate.UPSTREAM_ENV: str(named)}
        resolve = perf_gate.resolve_upstream
        self.assertEqual(resolve(str(given), env, self.repo), str(given))
        self.assertEqual(resolve(None, env, self.repo), str(named))
        self.assertEqual(Path(resolve(None, {}, self.repo)).resolve(), built)

    def test_nothing_to_resolve_names_all_three_ways_out(self) -> None:
        with self.assertRaises(perf_gate.HarnessError) as caught:
            perf_gate.resolve_upstream(None, {}, self.repo)
        message = str(caught.exception)
        for hint in ("--upstream", perf_gate.UPSTREAM_ENV, "tools/ci/run_interop.sh"):
            self.assertIn(hint, message)

    def test_a_variable_naming_a_missing_file_is_an_error_not_a_fallthrough(self) -> None:
        # Falling through to another binary would silently measure the wrong
        # reference.
        self._executable(perf_gate.UPSTREAM_INTEROP_PATH)
        with self.assertRaises(perf_gate.HarnessError):
            perf_gate.resolve_upstream(None, {perf_gate.UPSTREAM_ENV: "/nonexistent/rsync"}, self.repo)

    def test_the_default_matches_the_interop_harness_install_path(self) -> None:
        script = (perf_gate.REPO_ROOT / "tools" / "ci" / "run_interop.sh").read_text()
        self.assertIn('upstream_install_root="${workspace_root}/target/interop/upstream-install"', script)
        self.assertEqual(perf_gate.UPSTREAM_INTEROP_PATH,
                         "target/interop/upstream-install/3.5.2/bin/rsync")


class ParserTests(unittest.TestCase):
    STRACE = (
        "% time     seconds  usecs/call     calls    errors syscall\n"
        "------ ----------- ----------- --------- --------- ----------------\n"
        " 60.00    0.000600           6       100           write\n"
        " 20.00    0.000200           4        50         3 openat\n"
        " 10.00    0.000100           2        40           newfstatat\n"
        "  5.00    0.000050           1        30           read\n"
        "  5.00    0.000050           1         7           pwrite64\n"
        "  0.00    0.000000           0         9         1 futex\n"
        "------ ----------- ----------- --------- --------- ----------------\n"
        "100.00    0.001000           4       236         4 total\n"
    )

    def test_strace_counts_use_the_calls_column_not_errors_or_total(self) -> None:
        counts = perf_gate.parse_strace_summary(self.STRACE)
        self.assertEqual(counts["openat"], 50)
        self.assertNotIn("total", counts)
        families = perf_gate.syscall_families(counts)
        self.assertEqual(families, {"total": 236, "write": 107, "read": 30, "open": 50, "stat": 40})

    def test_stats_bytes_are_read_with_thousands_separators(self) -> None:
        text = "Total bytes sent: 1,234,567\nTotal bytes received: 89\n"
        self.assertEqual(perf_gate.parse_stats_bytes(text), (1234567, 89))
        self.assertEqual(perf_gate.parse_stats_bytes("no stats"), (None, None))

    def test_filesystem_is_that_of_the_deepest_containing_mount(self) -> None:
        mounts = "rootfs / ext4 rw 0 0\ntmpfs /dev/shm tmpfs rw 0 0\ndevtmpfs /dev devtmpfs rw 0 0\n"
        self.assertEqual(perf_gate.filesystem_of(Path("/dev/shm/x/src"), mounts), "tmpfs")
        self.assertEqual(perf_gate.filesystem_of(Path("/dev/shmother"), mounts), "devtmpfs")
        self.assertEqual(perf_gate.filesystem_of(Path("/home/u/x"), mounts), "ext4")


class QuietHostTests(unittest.TestCase):
    """Numbers taken on a busy host were reported once and had to be thrown
    away, so the refusal and the reading it rests on are pinned here."""

    PSI = ("some avg10=93.14 avg60=92.88 avg300=91.90 total=9422320974\n"
           "full avg10=85.50 avg60=84.09 avg300=80.32 total=6455090143\n")

    def test_pressure_is_the_some_line_avg10_not_full_or_avg60(self) -> None:
        self.assertEqual(perf_gate.parse_pressure_some_avg10(self.PSI), 93.14)
        self.assertIsNone(perf_gate.parse_pressure_some_avg10(""))

    def test_either_a_high_load_or_high_io_pressure_refuses(self) -> None:
        self.assertEqual(perf_gate.busy_reason(0.5, 2.0, 1.0, 10.0), "")
        self.assertIn("--max-load", perf_gate.busy_reason(1.5, 0.0, 1.0, 10.0))
        self.assertIn("--max-io-pressure", perf_gate.busy_reason(0.5, 10.5, 1.0, 10.0))
        # A kernel without PSI reports blank; that must not read as busy.
        self.assertEqual(perf_gate.busy_reason(0.5, "", 1.0, 10.0), "")

    def test_a_busy_host_stops_the_run_before_any_fixture_is_built(self) -> None:
        with tempfile.TemporaryDirectory() as data, redirect_stderr(io.StringIO()) as err:
            code = perf_gate.main(["--cell", "sparse-64m", "--data-dir", data, "--oc", "/bin/true",
                                   "--upstream", "/bin/true", "--no-strace", "--max-load", "-1"])
            self.assertEqual(code, 2)
            self.assertIn("--max-load", err.getvalue())
            self.assertEqual(list(Path(data).iterdir()), [])


class SummaryTests(unittest.TestCase):
    def test_median_ignores_warmup_and_traced_runs(self) -> None:
        # A warm-up or a strace run is far slower than a measured one; were
        # either counted, the median below would not be 2.0.
        rows = [_row("oc", 1.0), _row("oc", 2.0), _row("oc", 3.0),
                _row("oc", 50.0, kind="warmup"), _row("oc", 90.0, kind="strace")]
        (entry,) = perf_gate.summarize(rows)
        self.assertEqual(entry["runs"], 3)
        self.assertEqual(entry["wall_med_s"], 2.0)
        self.assertAlmostEqual(entry["wall_spread_pct"], 100.0)
        self.assertEqual(entry["cpu_med_s"], 0.75)

    def test_ratios_are_side_over_reference(self) -> None:
        rows = [_row("oc", 1.0, rss=3000), _row("upstream", 2.0, rss=1500),
                _row("off", 1.5, rss=3000)]
        by_side = {e["side"]: e for e in perf_gate.summarize(rows)}
        self.assertEqual(by_side["oc"]["wall_vs_upstream"], 0.5)
        self.assertEqual(by_side["oc"]["rss_vs_upstream"], 2.0)
        self.assertEqual(by_side["oc"]["wall_vs_oc"], "")
        self.assertEqual(by_side["upstream"]["wall_vs_upstream"], "")
        self.assertEqual(by_side["off"]["wall_vs_oc"], 1.5)
        self.assertEqual(by_side["off"]["wall_vs_upstream"], 0.75)


class CommandTests(unittest.TestCase):
    """A side must talk to itself; a row that mixed oc with an upstream server
    would be attributed to the wrong implementation."""

    def setUp(self) -> None:
        self.cell = perf_gate.CELLS["hardlink-50k"]
        self.side = perf_gate.Side("off", "/opt/oc", ("--no-parallel-delta",),
                                   (("K", "V"),), ("parallel delta scan = no",))
        self.src, self.dst, self.rsh = Path("/w/src"), Path("/w/dst"), Path("/w/state/rsh")

    def _cmd(self, transport: str) -> list[str]:
        return perf_gate.client_command(self.side, self.cell, transport, self.src, self.dst,
                                        873, self.rsh)

    def test_every_transport_names_the_right_endpoints(self) -> None:
        head = ["/opt/oc", "-aH", "--stats", "--no-parallel-delta"]
        rsh = ["-e", "/w/state/rsh", "--rsync-path=/opt/oc"]
        self.assertEqual(self._cmd("local"), head + ["/w/src/", "/w/dst/"])
        self.assertEqual(self._cmd("daemon-pull"), head + ["rsync://127.0.0.1:873/src/", "/w/dst/"])
        self.assertEqual(self._cmd("daemon-push"), head + ["/w/src/", "rsync://127.0.0.1:873/dst/"])
        self.assertEqual(self._cmd("rsh-pull"), head + rsh + ["localhost:/w/src/", "/w/dst/"])
        self.assertEqual(self._cmd("rsh-push"), head + rsh + ["/w/src/", "localhost:/w/dst/"])

    def test_a_peer_binary_serves_only_the_far_end(self) -> None:
        # Mixed-end cells (new client against old server) depend on the
        # client argv keeping its own binary while the server path changes.
        mixed = perf_gate.Side("mix", "/opt/new", peer="/opt/old")
        cmd = perf_gate.client_command(mixed, self.cell, "rsh-pull", self.src, self.dst,
                                       None, self.rsh)
        self.assertEqual(cmd[0], "/opt/new")
        self.assertIn("--rsync-path=/opt/old", cmd)
        self.assertEqual(self.side.server, "/opt/oc")

    def test_daemon_parameters_reach_both_modules(self) -> None:
        conf = perf_gate.daemon_config(self.side, self.src, self.dst, 873, Path("/w/state"))
        self.assertEqual(conf.count("parallel delta scan = no"), 2)
        self.assertIn("[src]\n    path = /w/src\n    read only = yes", conf)
        self.assertIn("[dst]\n    path = /w/dst\n    read only = no", conf)


class FixtureTests(unittest.TestCase):
    def test_mutation_touches_about_five_percent_and_is_repeatable(self) -> None:
        # Re-arming a destination in place relies on the same blocks being
        # hit every run; a different pick would leave earlier damage behind.
        size = 256 * perf_gate.MIB
        chunk, picks = perf_gate._mutation_plan(size, 7)
        self.assertEqual(chunk, 64 * perf_gate.KIB)
        self.assertAlmostEqual(len(picks) * chunk / size, 0.05, places=2)
        self.assertEqual(perf_gate._mutation_plan(size, 7), (chunk, picks))
        self.assertEqual(len(perf_gate._mutation_plan(100, 0)[1]), 1)

    def test_cells_named_by_the_release_gate_exist(self) -> None:
        for name in ("small-100k", "large-4g", "nochange-10k", "delete-20k",
                     "hardlink-50k", "sparse-64m"):
            self.assertIn(name, perf_gate.CELLS)


if __name__ == "__main__":
    unittest.main()
