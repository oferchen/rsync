"""Tests for tools/ci/classify_changed_paths.py.

A `true` verdict lets every required check report without building or testing
anything, so each false positive is a code change merged untested. The cases
below pin the direction of every ambiguity to `false`.
"""

from __future__ import annotations

import re
import subprocess
import sys
import unittest
from pathlib import Path

import yaml

from tools.ci.classify_changed_paths import is_docs_only, is_docs_path

REPO = Path(__file__).resolve().parents[2]
SCRIPT = REPO / "tools" / "ci" / "classify_changed_paths.py"
WORKFLOWS = REPO / ".github" / "workflows"
GATE = "needs.changes.outputs.docs_only != 'true'"


class ClassifyChangedPathsTests(unittest.TestCase):
    def test_a_readme_and_changelog_pr_is_docs_only(self) -> None:
        # The change set of PR #8144, which ran the full matrix.
        self.assertTrue(is_docs_only(["README.md", "CHANGELOG.md"]))

    def test_docs_tree_and_templates_are_docs(self) -> None:
        for path in ("docs/design/x.md", "docs/parity-options.yml", "LICENSE",
                     ".github/PULL_REQUEST_TEMPLATE.md", "SECURITY.md"):
            self.assertTrue(is_docs_path(path), path)

    def test_one_code_file_makes_the_whole_set_code(self) -> None:
        # The code+docs shape that once let a stand-in green a required check.
        self.assertFalse(is_docs_only(["docs/a.md", "crates/core/src/lib.rs"]))

    def test_an_empty_change_set_is_not_docs_only(self) -> None:
        self.assertFalse(is_docs_only([]))

    def test_markdown_that_feeds_a_build_or_test_is_code(self) -> None:
        # Crate READMEs are include_str!'d as crate docs; nested markdown under
        # tools/ or tests/ may be a fixture; workflows are CI code.
        for path in ("crates/core/README.md", "tools/ci/notes.md",
                     "tests/fixtures/readme.md", ".github/workflows/notes.md",
                     ".github/ISSUE_TEMPLATE/bug.md"):
            self.assertFalse(is_docs_path(path), path)

    def test_lookalike_top_level_names_are_code(self) -> None:
        for path in ("Cargo.toml", "README.md.rs", "docs", "mydocs/a.md"):
            self.assertFalse(is_docs_path(path), path)

    def test_the_cli_prints_a_github_output_line(self) -> None:
        # The workflow appends stdout to $GITHUB_OUTPUT verbatim.
        def run(stdin: str) -> str:
            return subprocess.run([sys.executable, str(SCRIPT)], input=stdin,
                                  capture_output=True, text=True, check=True).stdout
        self.assertEqual(run("README.md\n\ndocs/a.md\n"), "docs_only=true\n")
        self.assertEqual(run("README.md\nCargo.lock\n"), "docs_only=false\n")
        self.assertEqual(run(""), "docs_only=false\n")


class DocsOnlyGateWiringTests(unittest.TestCase):
    """The workflows must consume the verdict so that a fault runs MORE, not less."""

    def _gated_workflows(self) -> dict[str, dict]:
        out = {}
        for path in sorted(WORKFLOWS.glob("*.yml")):
            if "docs_only" in path.read_text() and not path.name.startswith("_"):
                out[path.name] = yaml.safe_load(path.read_text())
        return out

    def test_the_verdict_is_only_ever_compared_to_true(self) -> None:
        # An empty output (classifier failed, or not a pull request) must read
        # as "code changed". Comparing to 'false' would read it as docs-only.
        for path in WORKFLOWS.glob("*.yml"):
            for use in re.findall(r"docs_only\s*[!=]=\s*'(\w+)'", path.read_text()):
                self.assertEqual(use, "true", path.name)

    def test_every_gated_job_survives_a_failed_classifier(self) -> None:
        # Without !cancelled(), a failed `changes` job skips its dependents, and
        # a skipped literal-named job satisfies a required check unrun.
        workflows = self._gated_workflows()
        self.assertGreaterEqual(len(workflows), 2)
        for name, wf in workflows.items():
            for job_id, job in wf["jobs"].items():
                needs = job.get("needs") or []
                needs = [needs] if isinstance(needs, str) else needs
                if "changes" in needs:
                    self.assertIn("!cancelled()", job.get("if", ""), f"{name}:{job_id}")

    def test_matrix_jobs_after_changes_gate_every_step(self) -> None:
        # A matrix job cannot be skipped at job level without publishing its
        # unexpanded name, so on docs-only it RUNS and each step must skip
        # itself - including on the ubuntu runner a Windows/macOS job swaps to.
        jobs = yaml.safe_load((WORKFLOWS / "ci.yml").read_text())["jobs"]
        gated = {j for j, job in jobs.items()
                 if "matrix." in job.get("name", "") and "changes" in (job.get("needs") or [])}
        self.assertEqual(gated, {"test", "windows-test", "macos-test", "linux-musl"})
        for job_id in gated:
            for step in jobs[job_id]["steps"]:
                self.assertIn(GATE, step.get("if", ""), f"{job_id}: {step.get('name')}")


if __name__ == "__main__":
    unittest.main()
