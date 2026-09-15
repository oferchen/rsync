"""The 3.5.0 expected-skip oracle: guards, emitter, and the committed ledger.

WHAT IS BEING GUARDED

runtests.py's expected-skip oracle is what stops a test from quietly becoming
a permanent no-op: on a full run the set of skipped tests must be EXACTLY the
expected set (testsuite/skiplist/README.md). But the oracle is gated on
full_run (3.5.0 runtests.py:998), and both --expect-result (:850) and
--daemon-tests-only (:836) clear full_run - so a leg that passes
--expect-skipped alongside either flag gets no error and no oracle: the flag
is accepted and silently never checked. Every expect-result leg in CI had
exactly that shape, which is why the skip-oracle leg exists and why the
harness refuses the dead combinations outright.

Three populations here:

  * the harness guards: EXPECT_SKIPPED composed with EXPECT_RESULT or
    DAEMON_TESTS_ONLY must be refused loudly, never accepted-and-inert;
  * the EMIT_EXPECT_SKIPPED emitter: the ledger is generated from a run's
    log, sorted the way expand_skip_spec demands, never typed;
  * the committed ledger itself: it must agree with the expect-result
    manifest measured on the same leg, so re-baselining one without the
    other cannot drift silently.

A fourth class proves the oracle against upstream's own runner using a
synthetic two-test suite; it self-skips when the pinned tree is not
extracted, so it never reports on a population it did not read.
"""

from __future__ import annotations

import os
import re
import shlex
import subprocess
import tempfile
import textwrap
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
HARNESS = REPO / "tools" / "ci" / "run_upstream_testsuite.sh"
SKIPLIST = REPO / "tools" / "ci" / "upstream-3.5.0-skiplist.nonroot.txt"
EXPECT = REPO / "tools" / "ci" / "upstream-3.5.0-expect.nonroot.txt"
RUNTESTS = (
    REPO / "target" / "interop" / "upstream-src" / "rsync-3.5.0" / "runtests.py"
)

TEST_NAME = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_.+-]*$")


def _scrubbed_env(**overrides: str) -> dict[str, str]:
    env = dict(os.environ)
    for k in ("EXPECT_SKIPPED", "EXPECT_RESULT", "DAEMON_TESTS_ONLY",
              "EMIT_EXPECT_SKIPPED", "USE_TCP"):
        env.pop(k, None)
    env.update(overrides)
    return env


def _source_harness(env: dict[str, str], extra: str = "") -> subprocess.CompletedProcess[str]:
    program = f"source {shlex.quote(str(HARNESS))}\n{extra}"
    return subprocess.run(
        ["bash", "-c", program], capture_output=True, text=True,
        check=False, env=env,
    )


def _names(path: Path) -> list[str]:
    """Non-comment names, upstream's skiplist parse: strip from '#', skip blanks."""
    out = []
    for raw in path.read_text().splitlines():
        line = raw.split("#", 1)[0].strip()
        if line:
            out.append(line)
    return out


class ExpectSkippedGuardTests(unittest.TestCase):
    """The harness refuses the combinations under which the oracle is inert."""

    def test_expect_skipped_with_expect_result_is_refused(self) -> None:
        # runtests.py accepts both flags together and simply never checks the
        # skip spec (full_run cleared at :850, oracle gated at :998). Accepted-
        # and-inert is the vacuity the oracle exists to prevent, so the harness
        # must die at parse time, before any build work.
        result = _source_harness(_scrubbed_env(
            EXPECT_SKIPPED="@x.txt", EXPECT_RESULT="y.txt"))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("mutually exclusive", result.stderr)

    def test_expect_skipped_with_daemon_tests_only_is_refused(self) -> None:
        # Same inertness through the other flag: --daemon-tests-only clears
        # full_run (:836) because the skip list describes a full run.
        result = _source_harness(_scrubbed_env(
            EXPECT_SKIPPED="@x.txt", DAEMON_TESTS_ONLY="yes"))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("DAEMON_TESTS_ONLY", result.stderr)

    def test_expect_skipped_alone_is_accepted(self) -> None:
        # The guard must not refuse the one configuration the oracle fires in.
        result = _source_harness(_scrubbed_env(EXPECT_SKIPPED="@x.txt"))
        self.assertEqual(result.returncode, 0, result.stderr)


class EmitExpectSkippedTests(unittest.TestCase):
    """emit_expect_skipped_manifest(): generated, sorted, never empty."""

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.tmp = Path(self._tmp.name)

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def _emit(self, log_text: str) -> subprocess.CompletedProcess[str]:
        log = self.tmp / "output.log"
        log.write_text(log_text)
        self.manifest = self.tmp / "skiplist.txt"
        extra = textwrap.dedent(
            f"""
            upstream_label="release 3.5.0"
            mode_tag=nonroot
            transport_tag=pipe
            emit_expect_skipped_manifest {shlex.quote(str(log))}
            """
        )
        return _source_harness(
            _scrubbed_env(EMIT_EXPECT_SKIPPED=str(self.manifest)), extra)

    def test_skip_lines_become_a_sorted_deduped_ledger(self) -> None:
        # runtests.py prints `SKIP    name (reason)`; the reason is not part of
        # the name. expand_skip_spec refuses an unsorted or duplicated @FILE
        # (runtests.py:423-425, bytewise comparison), so the emitter must sort
        # the way Python compares.
        result = self._emit(
            "PASS    alpha\n"
            "SKIP    zeta (needs root)\n"
            "SKIP    beta (no tcp)\n"
            "SKIP    beta (no tcp)\n"
            "XFAIL   gamma\n"
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(_names(self.manifest), ["beta", "zeta"])
        self.assertIn("do not hand-edit", self.manifest.read_text())

    def test_a_skipless_log_is_refused_not_written_empty(self) -> None:
        # expand_skip_spec dies on an @FILE with no names; "expect no skips"
        # is spelled as an empty spec. Writing an empty file would hand the
        # consumer a guaranteed rejection later instead of an error now.
        result = self._emit("PASS    alpha\nPASS    beta\n")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.manifest.exists())


class CommittedLedgerTests(unittest.TestCase):
    """The committed skiplist agrees with the leg's expect-result manifest.

    Both files are generated from the same leg configuration (nonroot/pipe,
    ubuntu-latest), so their skip sets must be identical: re-baselining the
    expect manifest without regenerating the skiplist (or vice versa) is
    drift between two ledgers that claim to describe one leg.
    """

    def test_skiplist_matches_expect_manifest_skip_rows(self) -> None:
        skiplist = _names(SKIPLIST)
        expect_rows = _names(EXPECT)
        expect_skips = sorted(
            r.split()[0] for r in expect_rows if r.split()[1] == "skip"
        )
        self.assertEqual(skiplist, expect_skips)

    def test_skiplist_is_strictly_sorted_and_well_formed(self) -> None:
        # expand_skip_spec's own acceptance rules (runtests.py:418-431):
        # sorted, duplicate-free, one plain test name per line.
        names = _names(SKIPLIST)
        self.assertTrue(names, f"{SKIPLIST.name} lists no tests")
        for name in names:
            self.assertRegex(name, TEST_NAME)
        for prev, cur in zip(names, names[1:]):
            self.assertLess(prev, cur, f"{cur!r} follows {prev!r}")


class SkipOracleFiresTests(unittest.TestCase):
    """Upstream's runner enforces the skip set - proven against runtests.py.

    A two-test suite (one pass, one skip) driven by the PINNED 3.5.0
    runtests.py: the right expected set passes, a wrong one fails, and the
    same wrong one under --expect-result passes - the inertness the harness
    guards against. Skipped, never faked, when the tarball is not extracted.
    """

    HELPERS = ("tls", "trimslash", "t_unsafe", "t_chmod_secure",
               "t_secure_relpath", "wildtest", "getgroups", "getfsdev")

    def setUp(self) -> None:
        if not RUNTESTS.is_file():
            self.skipTest(f"{RUNTESTS} is not extracted here")
        self._tmp = tempfile.TemporaryDirectory()
        self.tmp = Path(self._tmp.name)
        tooldir = self.tmp / "tooldir"
        suite = self.tmp / "srcdir" / "testsuite"
        (self.tmp / "scratch").mkdir()
        tooldir.mkdir()
        suite.mkdir(parents=True)
        for helper in self.HELPERS:
            (tooldir / helper).touch()
        (tooldir / "fake-rsync").touch()
        (suite / "alpha_test.py").write_text("import sys\nsys.exit(0)\n")
        (suite / "beta_test.py").write_text("import sys\nsys.exit(77)\n")

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def _run(self, *flags: str) -> subprocess.CompletedProcess[str]:
        env = dict(os.environ)
        env["scratchbase"] = str(self.tmp / "scratch")
        return subprocess.run(
            ["python3", str(RUNTESTS),
             f"--rsync-bin={self.tmp / 'tooldir' / 'fake-rsync'}",
             f"--tooldir={self.tmp / 'tooldir'}",
             f"--srcdir={self.tmp / 'srcdir'}",
             "--timeout=30", *flags],
            capture_output=True, text=True, check=False, env=env,
        )

    def test_the_exact_skip_set_passes(self) -> None:
        result = self._run("--expect-skipped=beta")
        self.assertEqual(result.returncode, 0, result.stdout)

    def test_a_wrong_skip_set_fails_the_run(self) -> None:
        # THE oracle: every test passed or skipped, yet the run fails because
        # the skip set is not the expected one - a green-looking run cannot
        # hide a cell that became a no-op.
        result = self._run("--expect-skipped=alpha")
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertIn("expected: alpha", result.stdout)
        self.assertIn("got:      beta", result.stdout)

    def test_expect_result_silences_the_same_wrong_set(self) -> None:
        # The premise the harness guard rests on: the identical wrong spec is
        # accepted and never checked once --expect-result clears full_run.
        manifest = self.tmp / "expect.txt"
        manifest.write_text("alpha pass\nbeta skip\n")
        result = self._run("--expect-skipped=alpha",
                           f"--expect-result={manifest}")
        self.assertEqual(result.returncode, 0, result.stdout)


if __name__ == "__main__":
    unittest.main()
