"""Tests for tools/ci/interop_oracle.py, the upstream-judged interop cells.

The oracle exists because the interop harness compared destination trees
only, and every defect the cross-implementation audit found (a wrong exit
code, a dropped deletion count, a directory itemized as a file, a silently
lost non-UTF-8 name) left the tree check green. These tests pin the parts of
the comparison that make those defects visible, the masking that keeps two
honest runs equal, and the expectation contract that stops a fixed defect
from hiding behind a stale row.
"""

from __future__ import annotations

import os
import sys
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "tools" / "ci"))

import interop_oracle as oracle  # noqa: E402
import interop_oracle_cells as cells  # noqa: E402

FIXTURE_NS = cells.SRC_MTIME * 10**9
WALL_NS = oracle.WALL_CLOCK_NS + 10**9


def outcome(rc=0, items=(), stats=None, tree=None):
    return oracle.Outcome(rc, sorted(items), stats or {}, tree or {}, "", "cmd")


class TreeDiffTest(unittest.TestCase):
    def test_wall_clock_mtimes_on_both_sides_are_not_compared(self):
        # A directory the transfer creates without preserving its time gets
        # "now" in both runs; the two nows never agree.
        a = {"d": {"t": 4, "mtime": WALL_NS}}
        b = {"d": {"t": 4, "mtime": WALL_NS + 5}}
        self.assertEqual(oracle.tree_diff(a, b), [])

    def test_preserved_fixture_mtime_must_match(self):
        # One side preserved the source time and the other stamped "now":
        # that is a real divergence and must not be masked.
        a = {"f": {"t": 8, "mtime": FIXTURE_NS}}
        b = {"f": {"t": 8, "mtime": WALL_NS}}
        self.assertEqual(len(oracle.tree_diff(a, b)), 1)

    def test_missing_and_extra_entries_are_reported(self):
        self.assertEqual(oracle.tree_diff({"a": {}}, {"b": {}}), ["-a", "+b"])

    def test_ignored_fields_are_skipped(self):
        a = {"f": {"uid": 1}}
        b = {"f": {"uid": 2}}
        self.assertEqual(oracle.tree_diff(a, b, ("uid",)), [])
        self.assertEqual(len(oracle.tree_diff(a, b)), 1)


class ParseOutputTest(unittest.TestCase):
    def test_counts_are_read_from_both_output_styles(self):
        # rsync >= 3.1 groups digits and adds a type breakdown; older
        # releases print the bare count. Only the count is compared.
        new = "Number of files: 1,234 (reg: 1,200, dir: 34)\nNumber of deleted files: 2 (reg: 2)\n"
        old = "Number of files: 1234\nNumber of deleted files: 2\n"
        self.assertEqual(oracle.parse_output(new)[1], oracle.parse_output(old)[1])
        self.assertEqual(oracle.parse_output(new)[1]["Number of deleted files"], 2)

    def test_itemize_and_deleting_lines_are_collected_sorted(self):
        out = ">f+++++++++ b\ncd+++++++++ a/\n*deleting   x\ndeleting y\nsent 1 bytes\n"
        items, _ = oracle.parse_output(out)
        self.assertEqual(items, sorted([">f+++++++++ b", "cd+++++++++ a/",
                                        "*deleting   x", "deleting y"]))

    def test_legacy_normalization_ignores_column_width(self):
        # A 2.6.9 client pads the change string to 9 columns, 3.1+ to 11.
        old = oracle.normalize_items(["*deleting gone"], legacy=True)
        new = oracle.normalize_items(["*deleting   gone"], legacy=True)
        self.assertEqual(old, new)
        self.assertEqual(oracle.normalize_items(["x"], legacy=False), ["x"])


class CompareTest(unittest.TestCase):
    def test_identical_runs_pass(self):
        base = outcome(0, [">f+++++++++ a"], {"Number of files": 2}, {"a": {"t": 8}})
        self.assertEqual(oracle.compare(base, base, False, True), {})

    def test_exit_code_divergence_fails_with_an_identical_tree(self):
        # --max-delete: the tree is the same, only the exit code shows the
        # limit was reported (25) or silently swallowed (0).
        probs = oracle.compare(outcome(25), outcome(0), False, True)
        self.assertEqual(probs, {"rc": "25 -> 0"})

    def test_stats_divergence_fails_with_an_identical_tree(self):
        # The server sender that never echoes NDX_DEL_STATS leaves the tree
        # right and the deletion count at zero.
        base = outcome(stats={"Number of deleted files": 3})
        got = outcome(stats={"Number of deleted files": 0})
        self.assertIn("stats", oracle.compare(base, got, False, True))

    def test_itemize_divergence_fails_with_an_identical_tree(self):
        base = outcome(items=["cd+++++++++ b/"])
        got = outcome(items=["cf+++++++++ "])
        self.assertIn("itemize", oracle.compare(base, got, False, True))


class FreePortTest(unittest.TestCase):
    def test_ports_are_unique_and_below_the_ephemeral_range(self):
        # Cells run in parallel; two daemons sharing a port make one cell's
        # start probe reach the other's daemon, and an ephemeral port can be
        # taken by a concurrent client socket before the daemon binds it.
        ports = [oracle.free_port() for _ in range(20)]
        self.assertEqual(len(set(ports)), len(ports))
        self.assertTrue(all(p < 32768 for p in ports), ports)


class ExpectationTest(unittest.TestCase):
    def write(self, text: str) -> Path:
        fd, path = tempfile.mkstemp()
        os.write(fd, text.encode())
        os.close(fd)
        self.addCleanup(os.unlink, path)
        return Path(path)

    def test_rows_need_an_owner_task(self):
        with self.assertRaises(ValueError):
            oracle.load_expectations(self.write("basic/3.5.1/rsh/push/oc-client\n"))
        with self.assertRaises(ValueError):
            oracle.load_expectations(self.write("basic/3.5.1/rsh/push/oc-client someone\n"))

    def test_duplicate_rows_are_rejected(self):
        row = "basic/3.5.1/rsh/push/oc-client task-1 x\n"
        with self.assertRaises(ValueError):
            oracle.load_expectations(self.write(row + row))

    def test_classification_contract(self):
        results = [oracle.Result("pass"), oracle.Result("xfail", {"rc": "1 -> 2"}),
                   oracle.Result("fail", {"rc": "1 -> 2"}), oracle.Result("xpass")]
        expected = {"xfail": "task-1", "xpass": "task-2", "gone": "task-3"}
        v = oracle.classify(results, expected, {"pass", "xfail", "fail", "xpass"})
        self.assertEqual(v["PASS"], ["pass"])
        self.assertEqual(v["XFAIL"], ["xfail"])
        self.assertEqual(v["FAIL"], ["fail"])
        # A fixed defect must delete its row, and a row for a cell that no
        # longer exists is dead weight that would hide a future regression.
        self.assertEqual(v["XPASS"], ["xpass"])
        self.assertEqual(v["STALE"], ["gone"])


class CatalogueTest(unittest.TestCase):
    def test_every_expectation_row_names_a_catalogue_cell(self):
        rows = oracle.load_expectations(REPO / "tools" / "ci" / "interop_oracle_expect.txt")
        known = {c.cell_id for c in cells.catalogue(cells.ALL_VERSIONS)}
        self.assertEqual(sorted(set(rows) - known), [])

    def test_every_expectation_row_names_an_open_owner(self):
        for cell_id, owner in oracle.load_expectations(
                REPO / "tools" / "ci" / "interop_oracle_expect.txt").items():
            self.assertRegex(owner, r"^task-\d+$", cell_id)

    def test_cells_are_limited_to_available_releases(self):
        ids = [c.cell_id for c in cells.catalogue({"3.5.1"})]
        self.assertTrue(ids)
        self.assertTrue(all("/3.5.1/" in i for i in ids))
        # A 3.0.9-only cell must not appear without a 3.0.9 binary.
        self.assertNotIn("files-from/3.0.9/rsh/pull/oc-server", ids)
        self.assertIn("files-from/3.0.9/rsh/pull/oc-server",
                      [c.cell_id for c in cells.catalogue({"3.0.9"})])
        # 3.0.9's own daemon ignores octal outgoing chmod, so no such cell.
        self.assertNotIn("outgoing-chmod/3.0.9/daemon/pull/oc-server",
                         [c.cell_id for c in cells.catalogue({"3.0.9"})])

    def test_core_oc_client_push_cells_avoid_the_inc_recurse_race(self):
        # task-2520 makes these cells pass or fail by timing; the defect is
        # carried by the dedicated inc-recurse-dirs cells instead.
        for c in cells.catalogue({"2.6.9", "3.5.1"}):
            racy = c.role == "oc-client" and c.direction == "push" and c.version != "2.6.9"
            if c.case in {case for case, _, _ in cells.CORE_CASES}:
                self.assertEqual("--no-inc-recursive" in c.opts, racy, c.cell_id)

    def test_the_audit_defects_each_have_a_cell(self):
        ids = {c.cell_id for c in cells.catalogue(cells.ALL_VERSIONS)}
        for wanted in ("non-utf8/3.5.1/rsh/push/oc-server",
                       "non-utf8/3.5.1/rsh/pull/oc-client",
                       "copy-unsafe-links/3.5.1/rsh/pull/oc-server",
                       "append-longer-dest/3.5.1/rsh/push/oc-server",
                       "outgoing-chmod/3.5.1/daemon/pull/oc-server",
                       "files-from/3.0.9/rsh/pull/oc-server",
                       "prune-empty-dirs/3.5.1/rsh/push/oc-client",
                       "batch-local/3.4.4/local/push/oc-client",
                       "max-delete/3.5.1/rsh/push/oc-server",
                       "delete/3.5.1/rsh/pull/oc-server",
                       "inc-recurse-dirs/3.5.1/rsh/push/oc-client"):
            self.assertIn(wanted, ids)


@unittest.skipUnless(sys.platform.startswith("linux"),
                     "fixtures need non-UTF-8 file names, which APFS refuses")
class FixtureTest(unittest.TestCase):
    def test_fixtures_are_deterministic_and_keep_non_utf8_names_distinct(self):
        with tempfile.TemporaryDirectory() as a, tempfile.TemporaryDirectory() as b:
            cells.build_fixtures(Path(a))
            cells.build_fixtures(Path(b))
            for name in ("basic", "delta", "nonutf8", "symlinks", "fifo", "incdirs"):
                sa = oracle.snapshot(Path(a) / name / "src")
                sb = oracle.snapshot(Path(b) / name / "src")
                # Absolute link targets embed the fixture root.
                for snap in (sa, sb):
                    snap.pop("abs", None)
                self.assertEqual(sa, sb, name)
            names = set(oracle.snapshot(Path(a) / "nonutf8" / "src"))
            self.assertIn("f\\xef", names)
            self.assertIn("g\\xef", names)
            self.assertIn("na\\xefve/a.txt", names)

    def test_fixture_times_lie_below_the_wall_clock_mask(self):
        with tempfile.TemporaryDirectory() as a:
            cells.build_fixtures(Path(a))
            for rec in oracle.snapshot(Path(a) / "basic" / "src").values():
                if "mtime" in rec:
                    self.assertLess(rec["mtime"], oracle.WALL_CLOCK_NS)


class RunInteropWiringTest(unittest.TestCase):
    SCRIPT = (REPO / "tools" / "ci" / "run_interop.sh").read_text()

    def test_update_cell_stamps_the_future_time_in_utc(self):
        # touch -t reads local time; the check compares a UTC epoch. East of
        # UTC the stamp lands before the threshold and the cell fails.
        self.assertIn("env TZ=UTC0 touch -t 203001010000", self.SCRIPT)

    def test_oracle_failure_fails_the_harness(self):
        self.assertIn("interop_oracle.py", self.SCRIPT)
        self.assertIn('failed+=("oracle")', self.SCRIPT)


if __name__ == "__main__":
    unittest.main()
