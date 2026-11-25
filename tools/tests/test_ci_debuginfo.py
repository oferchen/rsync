"""Tests for tools/ci/ci_debuginfo.py, the CI debuginfo gate.

A called workflow does not inherit its caller's workflow-level env, so the
line-tables-only debuginfo ci.yml sets never reached the reusable workflows.
The feature matrix's `--workspace` test build then ran the runner out of disk
(#8804). These tests pin that the gate catches exactly that shape, that it does
not flag release or clippy-only jobs, and that the real tree passes it.
"""

from __future__ import annotations

import io
import tempfile
import textwrap
import unittest
from contextlib import redirect_stdout
from pathlib import Path

from tools.ci import ci_debuginfo as gate

REPO = Path(__file__).resolve().parents[2]
REAL = REPO / ".github" / "workflows"

REFERENCE = """\
on: [push]
env:
  CARGO_PROFILE_DEV_DEBUG: line-tables-only
  CARGO_PROFILE_TEST_DEBUG: line-tables-only
jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - run: cargo nextest run --workspace
  feature-matrix:
    uses: ./.github/workflows/_called.yml
"""

CALLED_WITHOUT_ENV = """\
on:
  workflow_call:
jobs:
  default-features:
    runs-on: ubuntu-latest
    steps:
      - run: |
          cargo nextest run --locked --profile ci --workspace 2>&1 | tee log
"""


def tree(files: dict[str, str]) -> tempfile.TemporaryDirectory:
    tmp = tempfile.TemporaryDirectory()
    for name, body in files.items():
        (Path(tmp.name) / name).write_text(textwrap.dedent(body), encoding="utf-8")
    return tmp


def run_gate(workflows: Path) -> tuple[int, str]:
    out = io.StringIO()
    with redirect_stdout(out):
        rc = gate.main(["gate", "--workflows", str(workflows)])
    return rc, out.getvalue()


class TestGate(unittest.TestCase):
    def test_the_repository_passes(self) -> None:
        rc, out = run_gate(REAL)
        self.assertEqual(rc, 0, out)

    def test_the_scan_sees_the_feature_matrix(self) -> None:
        jobs = dict(gate.debug_build_jobs(gate.load(REAL / "_test-features.yml")))
        self.assertIn("feature-flags-linux", jobs, "the gate no longer recognises the job that hit ENOSPC")

    def test_a_called_workflow_without_the_level_fails(self) -> None:
        with tree({"ci.yml": REFERENCE, "_called.yml": CALLED_WITHOUT_ENV}) as tmp:
            rc, out = run_gate(Path(tmp))
        self.assertEqual(rc, 1)
        for var in gate.DEBUG_VARS:
            self.assertIn(f"_called.yml job default-features builds in a debug profile with {var}=None", out)
        self.assertIn("a called workflow does not inherit its caller's env", out)

    def test_a_called_workflow_with_its_own_level_passes(self) -> None:
        fixed = CALLED_WITHOUT_ENV.replace(
            "jobs:\n",
            "env:\n  CARGO_PROFILE_DEV_DEBUG: line-tables-only\n  CARGO_PROFILE_TEST_DEBUG: line-tables-only\njobs:\n",
        )
        with tree({"ci.yml": REFERENCE, "_called.yml": fixed}) as tmp:
            self.assertEqual(run_gate(Path(tmp))[0], 0)

    def test_a_job_level_env_counts(self) -> None:
        fixed = CALLED_WITHOUT_ENV.replace(
            "    runs-on: ubuntu-latest\n",
            "    runs-on: ubuntu-latest\n    env:\n      CARGO_PROFILE_DEV_DEBUG: line-tables-only\n"
            "      CARGO_PROFILE_TEST_DEBUG: line-tables-only\n",
        )
        with tree({"ci.yml": REFERENCE, "_called.yml": fixed}) as tmp:
            self.assertEqual(run_gate(Path(tmp))[0], 0)

    def test_a_different_level_fails(self) -> None:
        wrong = CALLED_WITHOUT_ENV.replace(
            "jobs:\n", "env:\n  CARGO_PROFILE_DEV_DEBUG: full\n  CARGO_PROFILE_TEST_DEBUG: line-tables-only\njobs:\n"
        )
        with tree({"ci.yml": REFERENCE, "_called.yml": wrong}) as tmp:
            rc, out = run_gate(Path(tmp))
        self.assertEqual(rc, 1)
        self.assertIn("CARGO_PROFILE_DEV_DEBUG='full'", out)
        self.assertNotIn("CARGO_PROFILE_TEST_DEBUG", out)

    def test_release_profile_and_lint_only_jobs_are_not_debug_builds(self) -> None:
        others = """\
        on:
          workflow_call:
        jobs:
          release:
            runs-on: ubuntu-latest
            steps:
              - run: cargo build --locked --release --bin oc-rsync
          dist:
            runs-on: ubuntu-latest
            steps:
              - run: cargo build --locked --profile dist --target x86_64-unknown-linux-gnu
          lint:
            runs-on: ubuntu-latest
            steps:
              - run: cargo clippy --workspace -- -D warnings
        """
        with tree({"ci.yml": REFERENCE, "_others.yml": others}) as tmp:
            self.assertEqual(run_gate(Path(tmp))[0], 0)

    def test_a_reference_without_the_level_fails(self) -> None:
        bare = REFERENCE.replace("  CARGO_PROFILE_DEV_DEBUG: line-tables-only\n", "")
        with tree({"ci.yml": bare}) as tmp:
            rc, out = run_gate(Path(tmp))
        self.assertEqual(rc, 1)
        self.assertIn("ci.yml does not set CARGO_PROFILE_DEV_DEBUG at workflow level", out)


if __name__ == "__main__":
    unittest.main()
