"""Behavioural tests for the PR labeller in .github/workflows/labeler.yml.

The script under test is EXTRACTED from the committed YAML and executed, never
retyped here. A hand copy would keep passing after a YAML-level quoting change
broke the real workflow, which is the failure mode these tests exist to catch.

The labeller runs on `edited`, so it sees a PR whose title has been corrected.
It used to only ever add, leaving the previous category label in place; GitHub
files a PR under the FIRST matching category of .github/release.yml, so a
corrected `feat:` -> `fix:` was still released under Features.
"""

import json
import os
import shutil
import subprocess
import tempfile
import unittest

import yaml

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
WORKFLOW = os.path.join(REPO_ROOT, ".github", "workflows", "labeler.yml")

HARNESS = """
const calls = { added: [], removed: [] };
const github = {
  rest: {
    issues: {
      addLabels: async (a) => { calls.added.push(...a.labels); },
      removeLabel: async (a) => { calls.removed.push(a.name); },
    },
  },
};
const context = {
  repo: { owner: 'oferchen', repo: 'rsync' },
  payload: { pull_request: INPUT },
};
(async () => {
SCRIPT
})().then(() => console.log(JSON.stringify(calls)));
"""


def extract_script():
    with open(WORKFLOW, encoding="utf-8") as handle:
        workflow = yaml.safe_load(handle)
    steps = workflow["jobs"]["label"]["steps"]
    scripts = [s["with"]["script"] for s in steps if "with" in s and "script" in s["with"]]
    if len(scripts) != 1:
        raise AssertionError(f"expected exactly one github-script step, found {len(scripts)}")
    return scripts[0]


def run_labeler(title, labels):
    """Run the extracted script against one PR payload; return the API calls."""
    payload = {"title": title, "number": 1, "labels": [{"name": n} for n in labels]}
    program = HARNESS.replace("INPUT", json.dumps(payload)).replace("SCRIPT", extract_script())
    with tempfile.TemporaryDirectory() as tmp:
        path = os.path.join(tmp, "harness.mjs")
        with open(path, "w", encoding="utf-8") as handle:
            handle.write(program)
        out = subprocess.run(
            ["node", path], capture_output=True, text=True, timeout=30, check=True
        )
    return json.loads(out.stdout)


@unittest.skipUnless(shutil.which("node"), "node is required to execute the workflow script")
class LabelerTests(unittest.TestCase):
    def test_a_corrected_prefix_drops_the_previous_category_label(self):
        # The defect: `enhancement` used to survive and win the release category.
        calls = run_labeler("fix(transfer): confine the rename", ["enhancement"])
        self.assertEqual(calls["added"], ["bug"])
        self.assertEqual(calls["removed"], ["enhancement"])

    def test_an_already_correct_label_set_is_left_alone(self):
        calls = run_labeler("fix: something", ["bug"])
        self.assertEqual(calls["added"], [])
        self.assertEqual(calls["removed"], [])

    def test_labels_applied_by_hand_survive_the_reconcile(self):
        # Only the prefix table's own labels are managed; `security` is not one.
        calls = run_labeler("fix: something", ["enhancement", "security"])
        self.assertEqual(calls["removed"], ["enhancement"])
        self.assertNotIn("security", calls["removed"])

    def test_an_unrecognised_prefix_touches_nothing(self):
        # A title without a conventional prefix is a policy problem, not a
        # licence to strip whatever a human put there.
        calls = run_labeler("wip: still thinking", ["enhancement"])
        self.assertEqual(calls["added"], [])
        self.assertEqual(calls["removed"], [])

    def test_a_scoped_prefix_is_recognised(self):
        calls = run_labeler("feat(engine): add a thing", [])
        self.assertEqual(calls["added"], ["enhancement"])

    def test_every_documented_prefix_maps_to_a_label(self):
        expected = {
            "feat": "enhancement",
            "perf": "performance",
            "fix": "bug",
            "docs": "documentation",
            "ci": "ci",
            "test": "test",
            "refactor": "refactor",
            "chore": "chore",
            "style": "style",
        }
        for prefix, label in expected.items():
            with self.subTest(prefix=prefix):
                calls = run_labeler(f"{prefix}: subject", [])
                self.assertEqual(calls["added"], [label])


if __name__ == "__main__":
    unittest.main()
