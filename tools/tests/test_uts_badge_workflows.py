"""The two nightly testsuite badge workflows must never impersonate a required check.

`upstream-testsuite-3.5.0.yml` and `upstream-testsuite-3.5.1.yml` each run eight
legs from a matrix and feed one README badge. They call the same reusable
workflows as ci.yml, whose callers publish required contexts such as
`upstream-testsuite / upstream testsuite`. GitHub matches a required context
by NAME alone, so if a badge job ever expanded to one of those names - and
someone later added a trigger that reaches pull requests - the ruleset could be
satisfied by the wrong run. These tests expand every matrix, compose the check
names the way GitHub does (`<caller job name> / <reusable job name>`), and assert
none of them is required. They also pin the triggers: nightly and on demand,
never on pull requests, pushes or merge groups.
"""

from __future__ import annotations

import itertools
import re
import unittest
from pathlib import Path

import yaml

REPO = Path(__file__).resolve().parents[2]
WORKFLOWS_DIR = REPO / ".github" / "workflows"
BADGE_WORKFLOWS = {
    "3.5.0": WORKFLOWS_DIR / "upstream-testsuite-3.5.0.yml",
    "3.5.1": WORKFLOWS_DIR / "upstream-testsuite-3.5.1.yml",
}
TESTING_MD = REPO / "docs" / "contributing" / "TESTING.md"

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


def _published_contexts(path: Path) -> set[str]:
    contexts = set()
    for job_id, job in _load(path)["jobs"].items():
        inner = _reusable_job_names(job["uses"]) if "uses" in job else [None]
        for row in _matrix_rows(job):
            outer = _expand(job.get("name", job_id), row)
            for name in inner:
                contexts.add(outer if name is None else f"{outer} / {name}")
    return contexts


class UtsBadgeWorkflowTests(unittest.TestCase):
    def test_no_badge_job_publishes_a_required_context(self) -> None:
        for version, path in BADGE_WORKFLOWS.items():
            contexts = _published_contexts(path)
            self.assertTrue(contexts, f"{path.name}: expanded to no contexts")
            self.assertEqual(contexts & REQUIRED_CONTEXTS, set(), path.name)

    def test_each_workflow_runs_all_eight_legs(self) -> None:
        for version, path in BADGE_WORKFLOWS.items():
            legs = set()
            for job_id, job in _load(path)["jobs"].items():
                for row in _matrix_rows(job):
                    legs.add((job_id, row["variant"], row["transport"]))
                    self.assertIn(f"upstream-{version}-expect.", row["manifest"], path.name)
            self.assertEqual(
                legs,
                {(p, v, t) for p in ("linux", "macos")
                 for v in ("nonroot", "root") for t in ("pipe", "tcp")},
                path.name,
            )

    def test_badge_workflows_run_nightly_and_on_demand_only(self) -> None:
        crons = []
        for path in BADGE_WORKFLOWS.values():
            triggers = _triggers(_load(path))
            self.assertEqual(set(triggers), {"schedule", "workflow_dispatch"}, path.name)
            crons += [entry["cron"] for entry in triggers["schedule"]]
        self.assertEqual(len(crons), len(set(crons)), "two badge workflows share a cron minute")

    def test_3_5_0_badge_passes_the_same_inputs_as_the_ci_gate(self) -> None:
        # The badge reports the gate's result only while both run the same
        # upstream version with the same cache key.
        ci_inputs = [
            job["with"] for job in _load(WORKFLOWS_DIR / "ci.yml")["jobs"].values()
            if job.get("uses", "").endswith("/_upstream-testsuite.yml")
        ]
        self.assertTrue(ci_inputs, "ci.yml calls no testsuite reusable")
        for job in _load(BADGE_WORKFLOWS["3.5.0"])["jobs"].values():
            for key in ("upstream_rsync_version", "cache_version"):
                self.assertEqual({job["with"][key]}, {w[key] for w in ci_inputs}, key)

    def test_required_contexts_match_the_testing_guide(self) -> None:
        text = TESTING_MD.read_text()
        missing = sorted(c for c in REQUIRED_CONTEXTS if f"`{c}`" not in text)
        self.assertEqual(missing, [], "TESTING.md and this list disagree")


if __name__ == "__main__":
    unittest.main()
