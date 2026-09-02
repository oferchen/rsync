"""Unit tests for the citation drift audit's two-population scan.

The audit's line filter used to be `if "upstream" not in ln.lower(): continue`.
Citations written as bullets under a `/// # Upstream Reference` heading carry
that word on the heading line and never on the bullets, so 42% of the tree's
`file.c:NNN` citations - 5,240 of 12,596, across 748 files - were never once
opened by the tool. Widening the filter has to satisfy two claims at the same
time, and each test below pins one of them:

  * the widened scan SEES a bullet-style citation, and
  * the population that already gated is untouched by the widening.

Nine of the ten tests fail against the pre-widening script, which is the point:
a test that passes both ways proves nothing about the change. The tenth,
`test_the_bullet_is_not_counted_in_the_gating_population`, passes both ways BY
DESIGN and is labelled as such - it is the regression pin on the half that must
not move, and a pin that only starts holding after the change is not a pin.

The tests build their own throwaway workspace and their own throwaway upstream
C source, so they do not need the pinned tarball the CI job fetches.
"""

from __future__ import annotations

import contextlib
import io
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
SCRIPT = REPO_ROOT / "tools" / "ci" / "citation_drift_audit.py"

sys.path.insert(0, str(REPO_ROOT))

from tools.ci import citation_drift_audit as audit_mod  # noqa: E402

# Anchors long enough for `anchors()` (>= 8 chars, containing a space) and
# distinctive enough to resolve to exactly one line of the fake source below.
BLOCKING_ANCHOR = "if (protocol_version < 30)"
BULLET_ANCHOR = "while (S_ISLNK(file->mode))"

# `flist` is in the tool's HIGH set, so citations naming it are audited.
BLOCKING_ANCHOR_LINE = 12
BULLET_ANCHOR_LINE = 99
BULLET_CITED_LINE = 40  # 59 lines away from where the anchor really lives


def fake_upstream() -> list[str]:
    lines = ["static int pad;"] * 200
    lines[BLOCKING_ANCHOR_LINE - 1] = f"\t{BLOCKING_ANCHOR}"
    lines[BULLET_ANCHOR_LINE - 1] = f"\t{BULLET_ANCHOR}"
    return lines


@contextlib.contextmanager
def workspace(sources: dict[str, str]):
    """A temporary tree of `crates/<crate>/src/lib.rs`, cwd'd into."""
    prev = os.getcwd()
    with tempfile.TemporaryDirectory() as tmp:
        for crate, text in sources.items():
            src = Path(tmp) / "crates" / crate / "src"
            src.mkdir(parents=True)
            (src / "lib.rs").write_text(text)
        os.chdir(tmp)
        try:
            yield Path(tmp)
        finally:
            os.chdir(prev)


def run_audit(crate: str):
    """Call `audit(crate)` against the fake upstream source, capturing stdout.

    Returns `(stdout, result)`. The result is whatever `audit` returns, which
    differs between the pre- and post-widening scripts; assertions that must run
    against BOTH read the stdout instead, because its blocking half is
    byte-identical across the change.
    """
    audit_mod._cache.clear()
    audit_mod._cache["flist"] = fake_upstream()
    buf = io.StringIO()
    with contextlib.redirect_stdout(buf):
        result = audit_mod.audit(crate)
    return buf.getvalue(), result


class WidenedScanTests(unittest.TestCase):
    """The whole point: a bullet-style citation is no longer invisible."""

    BULLET_ONLY = f"""
/// # Upstream Reference
/// - `{BULLET_ANCHOR}` at flist.c:{BULLET_CITED_LINE}
pub fn f() {{}}
"""

    def test_a_bullet_under_an_upstream_reference_heading_is_scanned(self) -> None:
        with workspace({"demo": self.BULLET_ONLY}):
            out, _ = run_audit("demo")
        self.assertIn("[non-blocking]", out)
        self.assertIn("extended-scan citations=1", out)
        self.assertIn("string-anchored=1 suspected-drift=1", out)

    def test_the_bullet_is_not_counted_in_the_gating_population(self) -> None:
        # PASSES BOTH BEFORE AND AFTER THE WIDENING, deliberately. This is the
        # regression pin on the population that must not move; a pin that only
        # begins to hold after the change would not have detected the change
        # doing damage. The claims that discriminate live in the tests around it.
        with workspace({"demo": self.BULLET_ONLY}):
            out, _ = run_audit("demo")
        # The crate's own line - the one the ratchet reads - must still report a
        # zero it can carry, not the finding from the widened half.
        self.assertIn("demo: string-anchored=0 suspected-drift=0 (0%) unresolved=0", out)


class PopulationSplitTests(unittest.TestCase):
    """Widening must move nothing INTO the gating population."""

    BOTH_STYLES = f"""
// upstream: flist.c:{BLOCKING_ANCHOR_LINE} "{BLOCKING_ANCHOR}"
pub fn g() {{}}

/// # Upstream Reference
/// - `{BULLET_ANCHOR}` at flist.c:{BULLET_CITED_LINE}
pub fn h() {{}}
"""

    def test_each_style_lands_in_exactly_one_population(self) -> None:
        with workspace({"demo": self.BOTH_STYLES}):
            out, result = run_audit("demo")
        # Blocking: the one `// upstream:` citation, anchored and on target.
        self.assertIn("demo: string-anchored=1 suspected-drift=0 (0%) unresolved=0", out)
        # Non-blocking: the one bullet, anchored and drifted.
        self.assertIn("[non-blocking] extended-scan citations=1", out)
        self.assertIn("string-anchored=1 suspected-drift=1", out)

        blocking, extended, read = result
        self.assertEqual((blocking.cites, blocking.checked, blocking.miss), (1, 1, 0))
        self.assertEqual((extended.cites, extended.checked, extended.miss), (1, 1, 1))
        self.assertEqual(read, 1)

    def test_a_backwards_range_found_only_by_the_widened_scan_does_not_hard_fail(self) -> None:
        # A backwards range in the gating population is a hard failure and stays
        # one. Widening the filter must not turn a tree that was green into a red
        # one, so an inverted range the tool could not previously see is reported
        # in the non-blocking half instead.
        source = """
// upstream: flist.c:12-20 in-order range
/// # Upstream Reference
/// - flist.c:80-40
pub fn f() {}
"""
        with workspace({"demo": source}):
            _, result = run_audit("demo")
        blocking, extended, _ = result
        self.assertEqual(blocking.backwards, [])
        self.assertEqual(len(extended.backwards), 1)
        self.assertIn("flist.c:80-40 runs backwards", extended.backwards[0])


class ReportTests(unittest.TestCase):
    """The non-blocking half is a counted, reason-carrying report."""

    def build(self) -> tuple[dict, dict, str]:
        with workspace({"demo": PopulationSplitTests.BOTH_STYLES}):
            _, (blocking, extended, read) = run_audit("demo")
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            text = audit_mod.extended_report(
                {"demo": blocking}, {"demo": extended}, read, ["demo"]
            )
        return text, buf.getvalue()

    def test_a_warning_annotation_carries_the_count(self) -> None:
        _, stdout = self.build()
        warning = [l for l in stdout.splitlines() if l.startswith("::warning")]
        self.assertEqual(len(warning), 1, stdout)
        self.assertIn("non-blocking", warning[0])
        self.assertIn("1 suspected-drift finding(s)", warning[0])
        # `%` opens an escape in a workflow command, so the annotation must not
        # carry the percentage the human-readable lines print.
        self.assertNotIn("%", warning[0])

    def test_the_summary_separates_the_two_populations_by_name(self) -> None:
        text, _ = self.build()
        self.assertIn("### 1. BLOCKING", text)
        self.assertIn("### 2. NON-BLOCKING", text)
        self.assertLess(text.index("### 1. BLOCKING"), text.index("### 2. NON-BLOCKING"))
        self.assertIn("Ratcheted per crate", text)
        self.assertIn("No baseline is written for these", text)

    def test_the_summary_states_the_size_of_both_populations(self) -> None:
        # A report that only ever prints findings reads as clean when it scanned
        # nothing at all. Stating both population sizes is what separates "no
        # drift found" from "nothing examined".
        text, _ = self.build()
        self.assertIn("Scanned 1 Rust file(s) across 1 crate(s)", text)
        self.assertIn("1 citation(s); 1 string-anchored; 0 suspected drift", text)
        self.assertIn("1 citation(s); 1 string-anchored; **1 suspected drift**", text)

    def test_the_summary_goes_to_the_step_summary_file_when_one_is_set(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "summary.md")
            prev = os.environ.get("GITHUB_STEP_SUMMARY")
            os.environ["GITHUB_STEP_SUMMARY"] = path
            try:
                text, _ = self.build()
            finally:
                if prev is None:
                    os.environ.pop("GITHUB_STEP_SUMMARY", None)
                else:
                    os.environ["GITHUB_STEP_SUMMARY"] = prev
            self.assertEqual(Path(path).read_text(), text)


class EndToEndTests(unittest.TestCase):
    """Drive the script the way CI does, in a self-contained workspace."""

    def run_script(self, sources: dict[str, str], env_extra: dict | None = None):
        with workspace(sources) as tmp:
            pinned = tmp / "target" / "interop" / "upstream-src" / f"rsync-{audit_mod.VER}"
            pinned.mkdir(parents=True)
            (pinned / "flist.c").write_text("\n".join(fake_upstream()) + "\n")
            env = dict(os.environ)
            env.pop("GITHUB_STEP_SUMMARY", None)
            env.update(env_extra or {})
            proc = subprocess.run(
                [sys.executable, str(SCRIPT)],
                capture_output=True, text=True, cwd=tmp, env=env,
            )
        return proc

    def test_a_run_emits_the_non_blocking_count(self) -> None:
        proc = self.run_script({"demo": PopulationSplitTests.BOTH_STYLES})
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        self.assertIn("::warning title=Citation drift (non-blocking)::", proc.stdout)
        self.assertIn("1 suspected-drift finding(s)", proc.stdout)

    def test_a_whole_tree_run_that_reaches_nothing_new_refuses(self) -> None:
        # If the line filter ever narrows back, the non-blocking section would
        # print an empty table - indistinguishable from a clean one. The tool
        # refuses instead of reporting. Thousands of bullet-style citations exist
        # in the real tree, so this cannot fire on a healthy run.
        only_blocking = f"""
// upstream: flist.c:{BLOCKING_ANCHOR_LINE} "{BLOCKING_ANCHOR}"
pub fn g() {{}}
"""
        proc = self.run_script({"demo": only_blocking})
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("widened scan saw ZERO citations", proc.stdout + proc.stderr)


if __name__ == "__main__":
    unittest.main()
