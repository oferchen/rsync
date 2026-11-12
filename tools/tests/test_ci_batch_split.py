"""Pull requests run the light checks; merge-train batches run the full set.

The merge train merges a batch of pull requests onto an integration/ branch,
dispatches ci.yml there, and on success posts the batch's required contexts on
each pull request's head as commit statuses. That only gates anything if a
pull request's own run can never publish a build-and-test required context:
GitHub reports a job skipped by its `if:` as a successful check under the
job's name, so a literal-named required job skipped on pull requests would
satisfy the ruleset without testing anything. A skipped matrix job reports its
unexpanded name and a skipped reusable-workflow caller reports the caller name
alone, so neither can match a required context.
"""

from __future__ import annotations

import unittest

from test_uts_badge_workflows import (
    CI_YML,
    WORKFLOWS_DIR,
    _expand,
    _load,
    _matrix_rows,
    _reusable_job_names,
    _triggers,
)

# The branch ruleset's required contexts (ruleset 12911634).
LIGHT_REQUIRED = {"fmt + clippy", "rustdoc links", "unsafe-safety-comment-audit"}
BATCH_REQUIRED = {
    "nextest (stable)",
    "Windows (stable)",
    "macOS (stable)",
    "Linux musl (stable)",
    "interop / interop with upstream rsync",
    "upstream-testsuite / upstream testsuite",
    "upstream-testsuite / upstream testsuite (root)",
    "upstream-testsuite-tcp / upstream testsuite",
    "upstream-testsuite-tcp / upstream testsuite (root)",
    "upstream-testsuite-macos / upstream testsuite (macOS)",
    "upstream-testsuite-macos / upstream testsuite (macOS, root)",
    "upstream-testsuite-macos-tcp / upstream testsuite (macOS)",
    "upstream-testsuite-macos-tcp / upstream testsuite (macOS, root)",
}
# Workflows whose whole run is batch-only: no pull_request trigger, dispatched
# by the train.
BATCH_ONLY_WORKFLOWS = ("interop-validation.yml", "iouring-kernel-compat.yml")


def _runs_on(job: dict, event: str) -> bool:
    """Whether a job's `if:` admits `event`, for the event tests ci.yml uses."""
    cond = str(job.get("if", ""))
    if f"github.event_name != '{event}'" in cond:
        return False
    named = [e for e in ("pull_request", "workflow_dispatch", "push")
             if f"github.event_name == '{e}'" in cond]
    return not named or event in named


def _contexts(job_id: str, job: dict) -> set[str]:
    inner = _reusable_job_names(job["uses"]) if "uses" in job else [None]
    return {outer if name is None else f"{outer} / {name}"
            for row in _matrix_rows(job)
            for outer in [_expand(job.get("name", job_id), row)]
            for name in inner}


class CiBatchSplitTests(unittest.TestCase):
    def setUp(self) -> None:
        self.jobs = _load(CI_YML)["jobs"]

    def _published_on(self, event: str) -> set[str]:
        return {c for job_id, job in self.jobs.items() if _runs_on(job, event)
                for c in _contexts(job_id, job)}

    def test_pull_requests_publish_every_required_context(self) -> None:
        published = self._published_on("pull_request")
        self.assertEqual((LIGHT_REQUIRED | BATCH_REQUIRED) - published, set())

    def test_a_batch_dispatch_publishes_every_required_context(self) -> None:
        # The train relays only what the batch run published; a context missing
        # here would leave every pull request permanently pending.
        published = self._published_on("workflow_dispatch")
        self.assertEqual((LIGHT_REQUIRED | BATCH_REQUIRED) - published, set())

    def test_master_pushes_still_run_the_required_stable_cells(self) -> None:
        published = self._published_on("push")
        for context in ("fmt + clippy", "nextest (stable)", "Windows (stable)",
                        "macOS (stable)", "Linux musl (stable)",
                        "interop / interop with upstream rsync"):
            self.assertIn(context, published)

    def test_heavy_side_workflows_run_on_pull_requests_and_dispatch(self) -> None:
        for name in BATCH_ONLY_WORKFLOWS:
            with self.subTest(workflow=name):
                triggers = _triggers(_load(WORKFLOWS_DIR / name))
                self.assertIn("pull_request", triggers)
                self.assertIn("workflow_dispatch", triggers)


if __name__ == "__main__":
    unittest.main()
