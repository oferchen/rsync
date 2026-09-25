"""Unit tests for the content-based citation retarget.

A pin move rewrites thousands of line numbers at once, so the rewrite is only
as trustworthy as its refusals. Each test pins one reason a number may or may
not move:

  * a construct that moved is followed to its new line, both ends of a range;
  * a construct that vanished is NOT repointed - the comment beside it may now
    describe behaviour the new release no longer has;
  * a release-prefixed citation changes release only when its numbers moved;
  * a note under docs/ written against an earlier release keeps its numbers;
  * a hand resolution replaces a whole citation, and `to: null` leaves it be.

The tests build throwaway upstream trees and a throwaway git workspace, so they
need neither the upstream tarballs nor the real tree.
"""

from __future__ import annotations

import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace

REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT))

from tools.ci import citation_retarget as rt  # noqa: E402

OLD_C = """\
static int helper(int x)
{
\tint y = x;
\treturn y;
}

int keep_me(int f)
{
\tif (f < 0)
\t\treturn -1;
\twrite_int(f, 42);
\tdoomed_call(f);
\treturn 0;
}
"""

# Two lines inserted at the top of keep_me(), and doomed_call() deleted.
NEW_C = """\
static int helper(int x)
{
\tint y = x;
\treturn y;
}

int keep_me(int f)
{
\tint extra = 1;
\t(void)extra;
\tif (f < 0)
\t\treturn -1;
\twrite_int(f, 42);
\treturn 0;
}
"""

WRITE_INT_OLD, WRITE_INT_NEW = 11, 13
DOOMED_OLD = 12


def rewrite(workspace: dict[str, str], resolutions: dict | None = None) -> tuple[dict, dict]:
    """Run the tool over `workspace` and return (rewritten files, report)."""
    with tempfile.TemporaryDirectory() as tmp:
        t = Path(tmp)
        for ver, text in (("1.0.0", OLD_C), ("1.0.1", NEW_C), ("0.9.0", OLD_C)):
            (t / f"rsync-{ver}").mkdir()
            (t / f"rsync-{ver}" / "io.c").write_text(text)
        root = t / "ws"
        root.mkdir()
        for rel, text in workspace.items():
            (root / rel).parent.mkdir(parents=True, exist_ok=True)
            (root / rel).write_text(text)
        subprocess.run(["git", "init", "-q"], cwd=root, check=True)
        subprocess.run(["git", "add", "-A"], cwd=root, check=True)
        args = SimpleNamespace(
            from_ver="1.0.0", to_ver="1.0.1", root=str(root),
            from_dir=str(t / "rsync-1.0.0"), to_dir=str(t / "rsync-1.0.1"),
            prior_dir=str(t / "rsync-0.9.0"), resolutions=resolutions or {},
        )
        hits, rewrites, _ = rt.process(args)
        return rewrites, rt.summarise(hits)


class RetargetTests(unittest.TestCase):
    def test_a_moved_construct_is_followed_at_both_ends_of_a_range(self):
        src = f"// upstream: io.c:{WRITE_INT_OLD}-{WRITE_INT_OLD + 2} writes the int\n"
        out, report = rewrite({"a.rs": src})
        self.assertIn(f"io.c:{WRITE_INT_NEW}-{WRITE_INT_NEW + 1} ", out["a.rs"])
        self.assertEqual(report["per_class"].get("vanished"), None)

    def test_an_unmoved_construct_is_left_alone(self):
        out, report = rewrite({"a.rs": "// upstream: io.c:3 int y = x;\n"})
        self.assertNotIn("a.rs", out)
        self.assertEqual(report["per_class"], {"unchanged": 1})

    def test_a_vanished_construct_is_reported_and_not_repointed(self):
        # The comment claims doomed_call() happens; the new release deleted it,
        # so any new number would attach that claim to unrelated code.
        src = f"// upstream: io.c:{DOOMED_OLD} doomed_call()\n"
        out, report = rewrite({"a.rs": src})
        self.assertNotIn("a.rs", out)
        self.assertEqual(report["per_class"], {"vanished": 1})
        self.assertEqual(report["items"][0]["anchor"], "doomed_call(f);")

    def test_a_release_prefix_moves_only_with_its_numbers(self):
        moved = f"// upstream: rsync-1.0.0/io.c:{WRITE_INT_OLD}\n"
        gone = f"// upstream: rsync-1.0.0/io.c:{DOOMED_OLD}\n"
        out, _ = rewrite({"a.rs": moved, "b.rs": gone})
        self.assertEqual(out["a.rs"], f"// upstream: rsync-1.0.1/io.c:{WRITE_INT_NEW}\n")
        self.assertNotIn("b.rs", out)

    def test_a_record_written_against_an_earlier_release_keeps_its_numbers(self):
        # The old pin and the release before it hold the same text, so the
        # note's anchors fit both equally: nothing says it was retargeted at
        # the old pin, and its numbers must stay as history.
        note = (f"- `io.c:{WRITE_INT_OLD}` `write_int(f, 42);`\n"
                f"- `io.c:9` `if (f < 0)`\n")
        out, _ = rewrite({"docs/audit.md": note, "a.rs": note})
        self.assertNotIn("docs/audit.md", out)
        self.assertIn("a.rs", out)

    def test_a_hand_resolution_replaces_the_whole_citation(self):
        src = f"// upstream: io.c:{DOOMED_OLD} and io.c:{DOOMED_OLD}-{DOOMED_OLD + 1}\n"
        res = {
            f"io.c:{DOOMED_OLD}": {"to": "14", "why": "re-located by reading 1.0.1"},
            f"io.c:{DOOMED_OLD}-{DOOMED_OLD + 1}": {"to": None, "why": "gone"},
        }
        out, report = rewrite({"a.rs": src}, res)
        self.assertEqual(
            out["a.rs"],
            f"// upstream: io.c:14 and io.c:{DOOMED_OLD}-{DOOMED_OLD + 1}\n",
        )
        self.assertEqual(report["per_class"], {"resolved": 1, "vanished": 2})


if __name__ == "__main__":
    unittest.main()
