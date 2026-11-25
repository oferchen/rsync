"""Every CI job that builds debug test binaries uses line-tables-only debuginfo.

ci.yml sets `CARGO_PROFILE_{DEV,TEST}_DEBUG=line-tables-only` at workflow
level. A called (`workflow_call`) workflow does not inherit its caller's
workflow-level env, so the feature matrix, the interop and testsuite legs and
the Windows nightlies built with full debuginfo. The `--workspace` test build
of the default-features row then ran the runner out of disk (ENOSPC while
linking). Each job that compiles in a debug profile has to see the setting
through its own workflow or job env.
"""

from __future__ import annotations

import re
import unittest
from pathlib import Path

import yaml

REPO = Path(__file__).resolve().parents[2]
WORKFLOWS = REPO / ".github" / "workflows"
DEBUG_VARS = ("CARGO_PROFILE_DEV_DEBUG", "CARGO_PROFILE_TEST_DEBUG")

# A cargo invocation that compiles: build, test, run, or nextest run/archive.
# clippy, check and doc emit no linked binaries and are not counted.
CARGO = re.compile(r"\bcargo\s+(?:\+\S+\s+)?(?:build|test|run|nextest\s+(?:run|archive))\b([^\n]*)")
NON_DEBUG_PROFILE = re.compile(r"--release\b|--(?:cargo-)?profile[ =](?!dev\b|test\b|ci\b)\S+")


def workflows() -> list[tuple[str, dict]]:
    return [(p.name, yaml.safe_load(p.read_text(encoding="utf-8"))) for p in sorted(WORKFLOWS.glob("*.yml"))]


def is_called(workflow: dict) -> bool:
    # PyYAML reads the bare `on:` key as boolean True.
    on = workflow.get(True, workflow.get("on"))
    return isinstance(on, dict) and "workflow_call" in on


def debug_build_jobs(workflow: dict):
    for job_id, job in (workflow.get("jobs") or {}).items():
        runs = "\n".join(str(step.get("run", "")) for step in job.get("steps") or [])
        if any(not NON_DEBUG_PROFILE.search(m.group(1)) for m in CARGO.finditer(runs)):
            yield job_id, job


def expected() -> dict:
    env = yaml.safe_load((WORKFLOWS / "ci.yml").read_text(encoding="utf-8")).get("env") or {}
    return {var: env.get(var) for var in DEBUG_VARS}


class TestDebugBuildDebuginfo(unittest.TestCase):
    def test_ci_sets_the_reference_level(self) -> None:
        self.assertEqual(expected(), dict.fromkeys(DEBUG_VARS, "line-tables-only"))

    def test_called_workflows_with_debug_builds_are_found(self) -> None:
        called = [name for name, wf in workflows() if is_called(wf) and any(debug_build_jobs(wf))]
        self.assertIn("_test-features.yml", called, "the scan no longer recognises the feature matrix")

    def test_every_debug_build_job_sets_it(self) -> None:
        want = expected()
        for name, wf in workflows():
            for job_id, job in debug_build_jobs(wf):
                env = {**(wf.get("env") or {}), **(job.get("env") or {})}
                for var in DEBUG_VARS:
                    with self.subTest(workflow=name, job=job_id, var=var):
                        self.assertEqual(
                            env.get(var),
                            want[var],
                            f"{name} job {job_id} builds in a debug profile without {var}={want[var]}"
                            + ("; a called workflow does not inherit its caller's env" if is_called(wf) else ""),
                        )


if __name__ == "__main__":
    unittest.main()
