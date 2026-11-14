"""The upstream testsuite legs have one workflow, and one README badge.

The testsuite legs publish contexts such as `upstream-testsuite / upstream
testsuite`, which the branch ruleset lists by NAME. GitHub matches a required
context by name alone and a context carries no workflow name, so the tests
compose check names the way GitHub does (`<caller job name> / <reusable job
name>`).

The legs run inside ci.yml, once per merge-train batch (a workflow_dispatch
on the batch's integration/ branch); the train relays their contexts to each
pull request in the batch.
`upstream-testsuite-3.5.1.yml` keeps the same legs for the nightly badge and
`bootstrap` dispatches. The tests assert that ci.yml publishes every required
testsuite context, that the two files call the legs identically, and that no
other workflow publishes them on a pull request.
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
README_MD = REPO / "README.md"
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
# The gate's `${{ !inputs.bootstrap && '<manifest>' || '' }}`; ci.yml passes the
# manifest itself.
BOOTSTRAP_BRANCH = re.compile(r"\$\{\{ !inputs\.bootstrap && '([^']+)' \|\| '' \}\}")
# Non-blocking Windows legs that run on pull requests only through ci.yml.
BAKE_LEGS = ("windows-nightly-long-path.yml", "windows-nightly-case-insensitive.yml")


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


def _testsuite_callers(path: Path) -> dict[str, dict]:
    return {job_id: job for job_id, job in (_load(path).get("jobs") or {}).items()
            if job.get("uses", "").rsplit("/", 1)[-1] in TESTSUITE_REUSABLES}


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

    def test_ci_publishes_every_required_testsuite_context(self) -> None:
        # The ruleset lists these contexts verbatim, so a renamed caller job
        # would leave them permanently pending.
        published = _published_contexts(CI_YML, only_reusable=True)
        self.assertEqual(self.REQUIRED_TESTSUITE - published, set())

    def test_ci_and_the_gate_call_the_testsuite_identically(self) -> None:
        # The nightly badge must measure what pull requests are gated on. ci.yml
        # has no `bootstrap` input, so its manifest paths are the gate's
        # non-bootstrap branch, and merge-train batch dispatches only.
        ci, gate = _testsuite_callers(CI_YML), _testsuite_callers(GATE)
        self.assertEqual(set(ci), set(gate))
        for job_id, job in gate.items():
            self.assertEqual(ci[job_id]["uses"], job["uses"], job_id)
            expected = {key: BOOTSTRAP_BRANCH.sub(r"\1", str(value))
                        for key, value in job["with"].items()}
            self.assertEqual({k: str(v) for k, v in ci[job_id]["with"].items()},
                             expected, job_id)
            self.assertIn("github.event_name == 'pull_request'", ci[job_id]["if"], job_id)
            self.assertIn("github.event_name == 'workflow_dispatch'", ci[job_id]["if"], job_id)

    def test_only_ci_and_the_gate_run_the_testsuite(self) -> None:
        # The 3.5.0 corpus was retired rather than kept alongside: its test
        # names are a subset of 3.5.1's apart from two cells asserting 3.5.0
        # behaviour that 3.5.1 changed.
        callers = {path.name for path in WORKFLOWS_DIR.glob("*.yml")
                   if not path.name.startswith("_") and _testsuite_callers(path)}
        self.assertEqual(callers, {CI_YML.name, "ci-master.yml", GATE.name})

    def test_triggers(self) -> None:
        # ci.yml runs on every pull request, unfiltered, or a required check
        # never arrives. The gate no longer triggers on pull requests: a second
        # publisher of the same context would let the ruleset resolve it from
        # either run.
        ci, gate = _triggers(_load(CI_YML)), _triggers(_load(GATE))
        self.assertIsNone(ci["pull_request"])
        self.assertEqual(set(gate), {"schedule", "workflow_dispatch"})
        self.assertTrue(gate["schedule"], "the gate has no nightly cron")

    def test_every_pull_request_check_runs_in_one_run_per_pull_request(self) -> None:
        # One group per pull request: a newer push cancels the superseded
        # run, and pull requests do not queue behind each other. Cancelling
        # is limited to pull requests, so every master push keeps its own
        # per-commit group and a completed run for bisection.
        ci = _load(CI_YML)["concurrency"]
        self.assertIn("format('pr-{0}', github.event.pull_request.number)", ci["group"])
        self.assertIn("github.sha", ci["group"])
        self.assertEqual(ci["cancel-in-progress"], "${{ github.event_name == 'pull_request' }}")
        self.assertNotIn("queue", ci)
        called = {job["uses"].removeprefix("./.github/workflows/")
                  for job in _load(CI_YML)["jobs"].values() if "uses" in job}
        for name in BAKE_LEGS:
            triggers = _triggers(_load(WORKFLOWS_DIR / name))
            self.assertIn(name, called)
            self.assertIn("workflow_call", triggers, name)
            self.assertNotIn("pull_request", triggers, name)
            self.assertNotIn("queue", _load(WORKFLOWS_DIR / name)["concurrency"], name)

    def test_the_badge_reads_the_scheduled_runs(self) -> None:
        # A badge filtered on an event the workflow never runs on shows no
        # status at all, so the badge's event must be one of the gate's.
        badge = re.search(r"upstream-testsuite-3\.5\.1\.yml\?[^)]*", README_MD.read_text())
        self.assertIsNotNone(badge, "README carries no 3.5.1 testsuite badge")
        self.assertIn("event=schedule", badge.group(0))

    def test_every_leg_runs_the_gate_release(self) -> None:
        # One cache key across the legs, so they share the built upstream tree.
        gate_jobs = [*_testsuite_callers(GATE).values(), *_testsuite_callers(CI_YML).values()]
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
