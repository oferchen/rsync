"""The upstream testsuite legs have one workflow, and one README badge.

`upstream-testsuite-3.5.1.yml` is the required gate: its caller jobs publish
contexts such as `upstream-testsuite / upstream testsuite`, which the branch
ruleset lists by NAME. GitHub matches a required context by name alone and a
context carries no workflow name, so the tests compose check names the way
GitHub does (`<caller job name> / <reusable job name>`) and assert that the gate
publishes every required testsuite context, that no other workflow calls the
testsuite at all, and that the gate runs wherever a required check must arrive.
"""

from __future__ import annotations

import itertools
import re
import unittest
from pathlib import Path

import yaml

REPO = Path(__file__).resolve().parents[2]
WORKFLOWS_DIR = REPO / ".github" / "workflows"
GATE = WORKFLOWS_DIR / "upstream-testsuite-3.5.1.yml"
CI_YML = WORKFLOWS_DIR / "ci.yml"
TESTING_MD = REPO / "docs" / "contributing" / "TESTING.md"
TESTSUITE_REUSABLES = ("_upstream-testsuite.yml", "_upstream-testsuite-macos.yml")

# The branch ruleset's required contexts. docs/contributing/TESTING.md lists
# the same ten with the command that re-derives them; the last test keeps the
# two lists from drifting apart.
REQUIRED_CONTEXTS = {
    "fmt + clippy",
    "nextest (stable)",
    "Windows (stable)",
    "macOS (stable)",
    "Linux musl (stable)",
    "interop / interop with upstream rsync",
    "upstream-testsuite / upstream testsuite",
    "upstream-testsuite / upstream testsuite (root)",
    "upstream-testsuite-tcp / upstream testsuite",
    "upstream-testsuite-tcp / upstream testsuite (root)",
}
MATRIX_REF = re.compile(r"\$\{\{\s*matrix\.([A-Za-z0-9_-]+)\s*\}\}")


def _load(path: Path) -> dict:
    return yaml.safe_load(path.read_text())


def _triggers(workflow: dict) -> dict:
    # PyYAML reads the unquoted key `on` as the boolean True.
    return workflow.get("on", workflow.get(True)) or {}


def _matrix_rows(job: dict) -> list[dict]:
    matrix = (job.get("strategy") or {}).get("matrix") or {}
    include = matrix.get("include") or []
    axes = {k: v for k, v in matrix.items() if k not in ("include", "exclude")}
    rows = [dict(zip(axes, combo)) for combo in itertools.product(*axes.values())] if axes else []
    return rows + list(include) if (rows or include) else [{}]


def _expand(template: str, row: dict) -> str:
    return MATRIX_REF.sub(lambda m: str(row[m.group(1)]), template)


def _reusable_job_names(uses: str) -> list[str]:
    called = _load(REPO / uses.removeprefix("./"))
    return [job.get("name", job_id) for job_id, job in called["jobs"].items()]


def _published_contexts(path: Path, only_reusable: bool = False) -> set[str]:
    contexts = set()
    for job_id, job in _load(path)["jobs"].items():
        if only_reusable and not job.get("uses", "").startswith("./"):
            continue
        inner = _reusable_job_names(job["uses"]) if "uses" in job else [None]
        for row in _matrix_rows(job):
            outer = _expand(job.get("name", job_id), row)
            for name in inner:
                contexts.add(outer if name is None else f"{outer} / {name}")
    return contexts


class UtsBadgeWorkflowTests(unittest.TestCase):
    REQUIRED_TESTSUITE = {c for c in REQUIRED_CONTEXTS if c.startswith("upstream-testsuite")}

    def test_the_gate_publishes_every_required_testsuite_context(self) -> None:
        # The gate moved workflow, not name: the ruleset lists these contexts
        # verbatim, so a renamed caller job would leave them permanently pending.
        published = _published_contexts(GATE, only_reusable=True)
        self.assertEqual(self.REQUIRED_TESTSUITE - published, set())

    def test_only_the_gate_runs_the_testsuite(self) -> None:
        # One caller per upstream release keeps one publisher per context and
        # one badge per suite. The 3.5.0 corpus was retired rather than kept
        # alongside: its test names are a subset of 3.5.1's apart from two
        # cells asserting 3.5.0 behaviour that 3.5.1 changed.
        callers = set()
        for path in WORKFLOWS_DIR.glob("*.yml"):
            if path.name.startswith("_"):
                continue
            for job in (_load(path).get("jobs") or {}).values():
                if job.get("uses", "").rsplit("/", 1)[-1] in TESTSUITE_REUSABLES:
                    callers.add(path.name)
        self.assertEqual(callers, {GATE.name})

    def test_ci_publishes_no_required_testsuite_context(self) -> None:
        contexts = _published_contexts(CI_YML)
        self.assertTrue(contexts, "ci.yml expanded to no contexts")
        self.assertEqual(contexts & self.REQUIRED_TESTSUITE, set())

    def test_triggers(self) -> None:
        # The gate must run on every pull request, unfiltered, or a required
        # check never arrives; master pushes feed its badge.
        gate = _triggers(_load(GATE))
        self.assertEqual(set(gate), {"push", "pull_request", "workflow_dispatch"})
        self.assertIsNone(gate["pull_request"])
        self.assertEqual(gate["push"], {"branches": ["master"]})

    def test_the_gate_cancels_like_ci(self) -> None:
        # A newer push to a pull request supersedes the older run, but every
        # master push keeps its own group, so no master run shows as cancelled.
        ci, gate = _load(CI_YML)["concurrency"], _load(GATE)["concurrency"]
        self.assertEqual(gate["cancel-in-progress"], ci["cancel-in-progress"])
        self.assertEqual(gate["group"].removeprefix("upstream-testsuite-3.5.1-"),
                         ci["group"].removeprefix("ci-"))
        self.assertIn("github.sha", gate["group"])

    def test_every_leg_runs_the_gate_release(self) -> None:
        # One cache key across the legs, so they share the built upstream tree.
        gate_jobs = [job for job in _load(GATE)["jobs"].values()
                     if job.get("uses", "").rsplit("/", 1)[-1] in TESTSUITE_REUSABLES]
        self.assertEqual(len({job["with"]["cache_version"] for job in gate_jobs}), 1)
        for job in gate_jobs:
            self.assertEqual(job["with"]["upstream_rsync_version"],
                             "${{ vars.UPSTREAM_TESTSUITE_VERSION || '3.5.1' }}")

    def test_required_contexts_match_the_testing_guide(self) -> None:
        text = TESTING_MD.read_text()
        missing = sorted(c for c in REQUIRED_CONTEXTS if f"`{c}`" not in text)
        self.assertEqual(missing, [], "TESTING.md and this list disagree")


if __name__ == "__main__":
    unittest.main()
