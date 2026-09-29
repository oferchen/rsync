"""The rsync 3.5.1 testsuite legs: manifests, owners and the gate.

`.github/workflows/upstream-testsuite-3.5.1.yml` runs upstream's 3.5.1 corpus
on eight contexts ({pipe,tcp} x {nonroot,root} x {Linux,macOS}) as the required
testsuite gate, each leg against its own --expect-result manifest. These tests
hold that ledger to three rules:

1. Every expected failure names its cause and owner. The manifests accept
   known divergences, and an unexplained `fail` row is a silent waiver: nobody
   is on the hook to remove it, so it outlives the defect it records. The
   emitter writes bare rows, so a re-baseline that drops the annotations fails
   here instead of quietly discarding them.
2. The workflows and the files agree. A manifest nothing reads is dead, and a
   path a workflow names without a file fails the leg only at run time.
3. The gate runs 3.5.1 and nothing else. Every gate caller defaults to 3.5.1
   and reads only 3.5.1 ledgers; the 3.5.0 corpus is retired, so no ledger
   for another release may remain. A bootstrap dispatch drops every ledger, since a manifest limits
   the run to the tests it lists and a re-baseline must measure all of them.
"""

from __future__ import annotations

import re
import unittest
from pathlib import Path

import yaml

REPO = Path(__file__).resolve().parents[2]
CI_DIR = REPO / "tools" / "ci"
WORKFLOW = REPO / ".github" / "workflows" / "upstream-testsuite-3.5.1.yml"
TESTSUITE_REUSABLES = ("_upstream-testsuite.yml", "_upstream-testsuite-macos.yml")
GATE_VERSION = "${{ vars.UPSTREAM_TESTSUITE_VERSION || '3.5.1' }}"
SUITE = REPO / "target" / "interop" / "upstream-src" / "rsync-3.5.1" / "testsuite"

CONTEXTS = [
    f"upstream-3.5.1-expect.{plat}{priv}{tcp}.txt"
    for plat in ("", "macos.")
    for priv in ("nonroot", "root")
    for tcp in ("", ".tcp")
]
OWNER = re.compile(r"owner: task \d+")
VALID_OUTCOMES = {"pass", "fail", "skip", "xfail"}
MANIFEST_PATH = re.compile(r"tools/ci/upstream-[\w.-]+\.txt")


def _rows(path: Path) -> list[tuple[str, str, str]]:
    """(name, outcome, trailing comment) per row, in runtests.py's parse order."""
    rows = []
    for raw in path.read_text().splitlines():
        body, _, comment = raw.partition("#")
        fields = body.split()
        if not fields:
            continue
        assert len(fields) == 2, f"{path.name}: malformed row {raw!r}"
        rows.append((fields[0], fields[1], comment.strip()))
    return rows


def _gate_callers() -> dict:
    jobs = yaml.safe_load(WORKFLOW.read_text())["jobs"]
    return {job_id: job for job_id, job in jobs.items()
            if job.get("uses", "").rsplit("/", 1)[-1] in TESTSUITE_REUSABLES}


def _workflow_call_inputs(path: Path) -> dict:
    workflow = yaml.safe_load(path.read_text())
    # PyYAML reads the unquoted key `on` as the boolean True.
    triggers = workflow.get("on", workflow.get(True))
    return triggers["workflow_call"]["inputs"]


class Uts351ManifestTests(unittest.TestCase):
    def test_all_eight_contexts_have_a_manifest(self) -> None:
        missing = [n for n in CONTEXTS if not (CI_DIR / n).is_file()]
        self.assertEqual(missing, [], "3.5.1 contexts without a manifest")

    def test_every_expected_failure_names_its_owner(self) -> None:
        unowned = [
            f"{name}: {name_} {outcome}"
            for name in CONTEXTS
            for name_, outcome, comment in _rows(CI_DIR / name)
            if outcome in ("fail", "xfail") and not OWNER.search(comment)
        ]
        self.assertEqual(
            unowned,
            [],
            "expected failures must carry '# <cause> - owner: task N' inline",
        )

    def test_rows_are_well_formed_and_unique(self) -> None:
        for name in CONTEXTS:
            rows = _rows(CI_DIR / name)
            self.assertTrue(rows, f"{name} lists no tests")
            names = [r[0] for r in rows]
            self.assertEqual(len(names), len(set(names)), f"{name}: duplicate rows")
            bad = {r[1] for r in rows} - VALID_OUTCOMES
            self.assertFalse(bad, f"{name}: unknown outcomes {bad}")

    def test_tcp_legs_are_a_subset_of_their_pipe_legs(self) -> None:
        # --daemon-tests-only selects a subset of the corpus; a tcp row naming
        # a test the full pipe run never saw is a stale or foreign row.
        for name in CONTEXTS:
            if not name.endswith(".tcp.txt"):
                continue
            tcp = {r[0] for r in _rows(CI_DIR / name)}
            pipe = {r[0] for r in _rows(CI_DIR / name.replace(".tcp.txt", ".txt"))}
            self.assertLessEqual(tcp, pipe, f"{name}: rows absent from the pipe leg")

    def test_every_row_names_a_real_3_5_1_test(self) -> None:
        if not SUITE.is_dir():
            self.skipTest(f"upstream 3.5.1 tree not extracted at {SUITE}")
        corpus = {p.name[: -len("_test.py")] for p in SUITE.glob("*_test.py")}
        for name in CONTEXTS:
            unknown = {r[0] for r in _rows(CI_DIR / name)} - corpus
            self.assertFalse(unknown, f"{name}: not in the 3.5.1 corpus: {unknown}")

    def test_the_gate_reads_exactly_the_eight_manifests(self) -> None:
        # The skip-oracle leg also takes the nonroot pipe manifest as its
        # accepted-failure list, so the gate names that one file twice.
        named = re.findall(r"tools/ci/(upstream-3\.5\.1-expect\.[\w.]+\.txt)",
                           WORKFLOW.read_text())
        self.assertEqual(sorted(named),
                         sorted(CONTEXTS + ["upstream-3.5.1-expect.nonroot.txt"]))

    def test_every_committed_manifest_is_a_gate_manifest(self) -> None:
        # A ledger for a retired release, or for a leg nothing runs, is dead
        # weight that still reads as coverage.
        committed = sorted(p.name for p in CI_DIR.glob("upstream-*-expect.*.txt"))
        self.assertEqual(committed, sorted(CONTEXTS))

    def test_the_gate_runs_3_5_1_only(self) -> None:
        # A caller left on 3.5.0 would run the 360-test corpus against a
        # 345-test manifest, or the old corpus against the new one; either way
        # the required check would be judging the wrong release.
        callers = _gate_callers()
        self.assertTrue(callers, f"{WORKFLOW.name} calls no testsuite reusable")
        for job_id, job in callers.items():
            inputs = job["with"]
            self.assertEqual(inputs.get("upstream_rsync_version"), GATE_VERSION, job_id)
            for key, value in inputs.items():
                for path in MANIFEST_PATH.findall(str(value)):
                    self.assertIn("upstream-3.5.1-", path, f"{job_id}.{key}")
        self.assertNotIn("upstream-3.5.0-", WORKFLOW.read_text())

    def test_reusable_defaults_match_the_gate(self) -> None:
        # A caller that omits the version or a manifest inherits these, so a
        # stale default would silently run a different release.
        for name in TESTSUITE_REUSABLES:
            inputs = _workflow_call_inputs(REPO / ".github" / "workflows" / name)
            self.assertEqual(inputs["upstream_rsync_version"]["default"], "3.5.1", name)
            for key, spec in inputs.items():
                default = str(spec.get("default", ""))
                if default.startswith("tools/ci/upstream-"):
                    self.assertIn("upstream-3.5.1-", default, f"{name}.{key}")

    def test_a_bootstrap_run_drops_every_manifest(self) -> None:
        # --expect-result runs ONLY the tests its manifest lists, so a
        # bootstrap leg that kept one would never measure a test the ledger
        # lacks - exactly the tests a new release adds. A leg either blanks
        # each ledger under bootstrap or does not run at all.
        for job_id, job in _gate_callers().items():
            if job.get("if") == "${{ !inputs.bootstrap }}":
                continue
            ledgers = {k: v for k, v in job["with"].items()
                       if MANIFEST_PATH.search(str(v))}
            self.assertTrue(ledgers, f"{job_id}: reads no manifest")
            for key, value in ledgers.items():
                path = MANIFEST_PATH.search(value).group(0)
                self.assertEqual(value, f"${{{{ !inputs.bootstrap && '{path}' || '' }}}}",
                                 f"{job_id}.{key}")


if __name__ == "__main__":
    unittest.main()
