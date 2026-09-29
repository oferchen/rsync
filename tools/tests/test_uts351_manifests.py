"""The rsync 3.5.1 testsuite legs: manifests, owners and scheduling.

`.github/workflows/upstream-testsuite-3.5.1-<platform>-<privilege>-<transport>.yml`
run upstream's 3.5.1 corpus on eight contexts ({pipe,tcp} x {nonroot,root} x
{Linux,macOS}), one workflow per context so each has its own status badge, each
against its own --expect-result manifest. These tests hold that ledger to three rules:

1. Every expected failure names its cause and owner. The manifests accept
   known divergences, and an unexplained `fail` row is a silent waiver: nobody
   is on the hook to remove it, so it outlives the defect it records. The
   emitter writes bare rows, so a re-baseline that drops the annotations fails
   here instead of quietly discarding them.
2. The workflow and the files agree. A manifest nothing reads is dead, and a
   path the workflow names without a file fails the leg only at run time.
3. The legs are not a pull-request gate yet. CI runs about one workflow at a
   time; eight more jobs per PR would gridlock the merge queue. Making 3.5.1
   the required gate is a separate, deliberate step (the pin flip), so the
   3.5.0 callers in ci.yml must not pick up these manifests by accident.
"""

from __future__ import annotations

import re
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
CI_DIR = REPO / "tools" / "ci"
WORKFLOWS = sorted((REPO / ".github" / "workflows").glob("upstream-testsuite-3.5.1-*.yml"))
CI_YML = REPO / ".github" / "workflows" / "ci.yml"
SUITE = REPO / "target" / "interop" / "upstream-src" / "rsync-3.5.1" / "testsuite"

CONTEXTS = [
    f"upstream-3.5.1-expect.{plat}{priv}{tcp}.txt"
    for plat in ("", "macos.")
    for priv in ("nonroot", "root")
    for tcp in ("", ".tcp")
]
OWNER = re.compile(r"owner: task \d+")
VALID_OUTCOMES = {"pass", "fail", "skip", "xfail"}


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

    def test_workflows_read_exactly_the_eight_manifests(self) -> None:
        self.assertEqual(len(WORKFLOWS), len(CONTEXTS), "one workflow per context")
        named = []
        for workflow in WORKFLOWS:
            found = re.findall(r"tools/ci/(upstream-3\.5\.1-expect\.[\w.]+\.txt)",
                               workflow.read_text())
            self.assertEqual(len(found), 1, f"{workflow.name}: exactly one manifest")
            named += found
        self.assertEqual(sorted(named), sorted(CONTEXTS))

    def test_legs_are_not_a_pull_request_gate(self) -> None:
        for workflow in WORKFLOWS:
            on_block = workflow.read_text().split("\non:", 1)[1].split("\njobs:", 1)[0]
            self.assertNotIn("pull_request", on_block, workflow.name)
            for trigger in ("push:", "schedule:", "workflow_dispatch:"):
                self.assertIn(trigger, on_block, workflow.name)
        self.assertNotIn("upstream-3.5.1-", CI_YML.read_text())


if __name__ == "__main__":
    unittest.main()
