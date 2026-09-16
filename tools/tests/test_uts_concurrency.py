"""Unit tests for the shared-resource isolation in tools/ci/run_upstream_testsuite.sh.

WHAT IS BEING GUARDED

The harness copies its binary and hosts its scratch tree under host-global
paths (/usr/local/bin, /tmp) so two harness runs on ONE host share those
namespaces. A run that names such a resource only by its LEG
(mode_tag-transport_tag) collides with a second run of the same leg: the later
run's `rm -rf` wipes the earlier run's live per-test trees and runtests.py then
fails link_stat with ENOENT - a silent, run-clobbering-run failure. The binary
was made per-run in an earlier change (publish_oc_rsync_bin); its sibling, the
Python-suite scratch tree, is made per-run by uts_scratch_home_path(), proved
here.

The tests are hermetic: they SOURCE the harness (they never launch the 45-minute
suite) and call the real functions, so a hand copy of the naming cannot pass in
place of the shipped one. No network, no root, no build - identical on a laptop
and on a runner.
"""

from __future__ import annotations

import shlex
import subprocess
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
HARNESS = REPO / "tools" / "ci" / "run_upstream_testsuite.sh"


def _source_and_eval(snippet: str, env_prefix: str = "") -> subprocess.CompletedProcess:
    """Source the harness in a fresh shell and run `snippet`. `env_prefix` is
    prepended verbatim (e.g. "UTS_JOBS=abc "), so a value that trips the
    prologue's validation exits before the snippet ever runs."""
    program = f"{env_prefix}source {shlex.quote(str(HARNESS))}\n{snippet}\n"
    return subprocess.run(
        ["bash", "-c", program], capture_output=True, text=True, check=False
    )


class ScratchHomeIsolationTests(unittest.TestCase):
    """uts_scratch_home_path(): distinct per run, stable within a run."""

    def test_distinct_run_ids_yield_distinct_trees(self) -> None:
        # The load-bearing, deterministic proof: with the run id pinned to two
        # different values (as two concurrent processes would have), the SAME
        # leg resolves to two DIFFERENT scratch trees, so neither run's rm -rf
        # can reach the other's tree.
        a = _source_and_eval(
            'uts_run_id=RUNA; uts_scratch_home_path /tmp nonroot pipe'
        )
        b = _source_and_eval(
            'uts_run_id=RUNB; uts_scratch_home_path /tmp nonroot pipe'
        )
        self.assertEqual(a.returncode, 0, a.stderr)
        self.assertEqual(b.returncode, 0, b.stderr)
        self.assertEqual(a.stdout.strip(), "/tmp/oc-rsync-uts-scratch-nonroot-pipe-RUNA")
        self.assertEqual(b.stdout.strip(), "/tmp/oc-rsync-uts-scratch-nonroot-pipe-RUNB")
        self.assertNotEqual(a.stdout.strip(), b.stdout.strip())

    def test_path_is_stable_within_one_run(self) -> None:
        # Same run id, called twice -> identical, so the in-run rm -rf and the
        # EXIT-trap cleanup address the very tree the run created.
        result = _source_and_eval(
            'uts_run_id=RUNX\n'
            'a=$(uts_scratch_home_path /tmp root tcp)\n'
            'b=$(uts_scratch_home_path /tmp root tcp)\n'
            '[[ "$a" == "$b" ]] && echo SAME || echo DIFF'
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "SAME")

    def test_leg_tags_survive_in_the_name(self) -> None:
        # The per-run suffix does not erase the leg identity: a preserved tree
        # is still attributable to its leg.
        result = _source_and_eval(
            'uts_run_id=R; uts_scratch_home_path /tmp nonroot tcp'
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "/tmp/oc-rsync-uts-scratch-nonroot-tcp-R")

    def test_auto_generated_run_id_differs_across_processes(self) -> None:
        # Documents WHERE the per-run identity comes from: $$ differs between two
        # separate processes, so two real concurrent invocations auto-derive
        # distinct ids (and thus distinct trees) with no coordination.
        a = _source_and_eval('printf %s "$uts_run_id"')
        b = _source_and_eval('printf %s "$uts_run_id"')
        self.assertEqual(a.returncode, 0, a.stderr)
        self.assertEqual(b.returncode, 0, b.stderr)
        self.assertTrue(a.stdout.strip())
        self.assertTrue(b.stdout.strip())
        self.assertNotEqual(a.stdout.strip(), b.stdout.strip())


class UtsJobsValidationTests(unittest.TestCase):
    """UTS_JOBS: the runtests.py -j lever, validated at load."""

    def test_default_is_one(self) -> None:
        # Default 1 keeps the historical serial argv, so no required context
        # changes timing or flakiness unless a caller opts in.
        result = _source_and_eval('printf %s "$uts_jobs"')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "1")

    def test_accepts_a_positive_integer(self) -> None:
        result = _source_and_eval('printf %s "$uts_jobs"', env_prefix="UTS_JOBS=4 ")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "4")

    def test_rejects_non_integer(self) -> None:
        result = _source_and_eval('echo unreached', env_prefix="UTS_JOBS=abc ")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("positive integer", result.stderr)
        self.assertNotIn("unreached", result.stdout)

    def test_rejects_zero(self) -> None:
        result = _source_and_eval('echo unreached', env_prefix="UTS_JOBS=0 ")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("positive integer", result.stderr)


if __name__ == "__main__":
    unittest.main()
