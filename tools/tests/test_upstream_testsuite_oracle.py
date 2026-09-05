"""Unit tests for the legacy-oracle machinery in tools/ci/.

WHAT IS BEING GUARDED

The rsync 3.5.0 testsuite has tests that assert against a REAL old rsync when
one sits in the tree's old_versions/ directory and degrade when one does not.
The release tarball ships no such binaries, so on a stock runner the degraded
path is the only path ever taken - and for two of the three consumers the
degradation is invisible:

  * daemon-symlink-escape-matrix swaps a live 3.2.7 daemon for
    static_followed(), a hand-written prediction of 3.2.7, across 100 of its
    200 cells, and reports PASS;
  * daemon-auth-digest-floor drops its md5-downgrade case with
    `raise SystemExit(0)`, and reports PASS;
  * daemon-max-alloc-zero calls test_skipped(), which is counted.

runtests.py prints a passing test's stdout only under --always-log, so the
lines those tests print about which oracle they used never reach a green log.

So the machinery under test has two jobs, and a green CI run demonstrates
neither of them: put the binary on disk when a leg asks for it, and make a
failure to do so loud instead of letting the suite quietly assert less. Both
are exercised here against synthetic trees and a stub builder - hermetic, no
network, no root, identical on a laptop and on a runner. The one cell that
does invoke a compiler is OracleCflagsEraTests, and it self-skips with a
reason when no usable one is present.
"""

from __future__ import annotations

import os
import shlex
import shutil
import subprocess
import tarfile
import tempfile
import textwrap
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
HARNESS = REPO / "tools" / "ci" / "run_upstream_testsuite.sh"
BUILDER = REPO / "tools" / "ci" / "build_old_rsync_oracle.sh"

# A test that really consults the archive: it evaluates the string literals, so
# the AST scan must find it. Shaped like daemon-symlink-escape-matrix.
CONSUMER = '''\
"""A consuming test."""

from pathlib import Path

ORACLE = Path(__file__).resolve().parents[1] / 'old_versions' / 'rsync_3.2.7'
print(ORACLE)
'''

# The false-positive shape: 7 of the 10 files a text grep matches in the real
# 3.5.0 tree look exactly like this - the archive is named in a DOCSTRING as a
# cross-version note and never stat'd.
PROSE_ONLY = '''\
"""Pure local client behaviour: no daemon/root/tcp.  Cross-version: expected
identical against --rsync-bin=old_versions/rsync_3.2.7.
"""

print('nothing to do with the archive')
'''

# Evaluates 'old_versions' but names no rsync_<version> literal, so which
# binary it wants cannot be derived.
UNDERIVABLE = '''\
"""A consumer whose oracle name is computed."""

from pathlib import Path

VER = '3.2.7'
ORACLE = Path(__file__).resolve().parents[1] / 'old_versions' / ('rsync_' + VER)
print(ORACLE)
'''


def consumer_needing(tcp: bool, root: bool) -> str:
    """A consuming test carrying the preconditions the harness reads."""
    body = CONSUMER
    if tcp:
        body += "require_tcp('needs a real TCP peer')\n"
    if root:
        body += "import os\nif os.geteuid() != 0:\n    raise SystemExit(0)\n"
    return body


class LegacyOracleDiscoveryTests(unittest.TestCase):
    """legacy_oracle_requirements(): what the tree actually asks for."""

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.tmp = Path(self._tmp.name)
        self.tree = self.tmp / "rsync-3.5.0"
        (self.tree / "testsuite").mkdir(parents=True)

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def _test(self, name: str, body: str) -> None:
        (self.tree / "testsuite" / f"{name}_test.py").write_text(body)

    def _run(self) -> subprocess.CompletedProcess[str]:
        program = textwrap.dedent(
            f"""
            source {shlex.quote(str(HARNESS))}
            upstream_src_dir={shlex.quote(str(self.tree))}
            legacy_oracle_requirements
            """
        )
        return subprocess.run(
            ["bash", "-c", program], capture_output=True, text=True, check=False
        )

    def test_code_reference_is_discovered_with_its_preconditions(self) -> None:
        self._test("matrix", consumer_needing(tcp=True, root=True))
        result = self._run()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "3.2.7\tmatrix\tyes\tyes")

    def test_preconditions_are_read_from_the_test_not_assumed(self) -> None:
        self._test("plain", consumer_needing(tcp=False, root=False))
        result = self._run()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "3.2.7\tplain\tno\tno")

    def test_docstring_mention_is_not_a_consumer(self) -> None:
        # The whole reason the scan is over the AST. A text scan reports this
        # test as needing an oracle, which both builds a binary nothing reads
        # and - worse - names a healthy test as degraded.
        self._test("prose", PROSE_ONLY)
        result = self._run()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "")

    def test_underivable_oracle_name_is_refused_not_ignored(self) -> None:
        # Silently skipping it would leave the test asserting its fallback,
        # which is exactly the condition this machinery exists to end.
        self._test("computed", UNDERIVABLE)
        result = self._run()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("cannot be derived", result.stderr)

    def test_the_pinned_upstream_tree_names_its_consumers(self) -> None:
        # A shape check against the real thing when it happens to be extracted:
        # every row must be version/test/yes-or-no/yes-or-no. Skipped rather
        # than faked when the tree is absent, so it never reports on a
        # population it did not read.
        tree = REPO / "target" / "interop" / "upstream-src" / "rsync-3.5.0"
        if not (tree / "testsuite").is_dir():
            self.skipTest(f"{tree} is not extracted here")
        self.tree = tree
        result = self._run()
        self.assertEqual(result.returncode, 0, result.stderr)
        for line in result.stdout.splitlines():
            version, name, needs_tcp, needs_root = line.split("\t")
            self.assertRegex(version, r"^\d+\.\d+(\.\d+)?$")
            self.assertTrue(name)
            self.assertIn(needs_tcp, ("yes", "no"))
            self.assertIn(needs_root, ("yes", "no"))


class EnsureLegacyOraclesTests(unittest.TestCase):
    """ensure_legacy_oracles(): builds, refuses, or names the degradation."""

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.tmp = Path(self._tmp.name)
        self.workspace = self.tmp / "workspace"
        (self.workspace / "tools" / "ci").mkdir(parents=True)
        self.tree = self.tmp / "rsync-3.5.0"
        (self.tree / "testsuite").mkdir(parents=True)
        (self.tree / "testsuite" / "matrix_test.py").write_text(
            consumer_needing(tcp=False, root=False)
        )
        self.calls = self.tmp / "builder-calls"

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def _builder(self, exit_code: int) -> None:
        """Stand in for build_old_rsync_oracle.sh at the path the harness uses."""
        stub = self.workspace / "tools" / "ci" / "build_old_rsync_oracle.sh"
        stub.write_text(
            "#!/usr/bin/env bash\n"
            f'printf "%s\\n" "$*" >> {shlex.quote(str(self.calls))}\n'
            f"exit {exit_code}\n"
        )
        stub.chmod(0o755)

    def _run(self, env_overrides: dict, extra: str = "") -> subprocess.CompletedProcess[str]:
        env = dict(os.environ)
        env.pop("LEGACY_ORACLES", None)
        env.pop("USE_TCP", None)
        env.pop("EXPECT_RESULT", None)
        env.update(env_overrides)
        program = textwrap.dedent(
            f"""
            source {shlex.quote(str(HARNESS))}
            workspace_root={shlex.quote(str(self.workspace))}
            upstream_src_dir={shlex.quote(str(self.tree))}
            {extra}
            ensure_legacy_oracles
            """
        )
        return subprocess.run(
            ["bash", "-c", program], capture_output=True, text=True,
            check=False, env=env,
        )

    def test_on_builds_the_oracle_the_consumer_names(self) -> None:
        self._builder(0)
        result = self._run({"LEGACY_ORACLES": "on"})
        self.assertEqual(result.returncode, 0, result.stderr)
        call = self.calls.read_text().split()
        self.assertEqual(call[0], "3.2.7")
        self.assertEqual(call[1], str(self.tree / "old_versions"))
        self.assertIn("1 on disk", result.stderr)

    def test_a_failed_build_fails_the_leg(self) -> None:
        # THE point of the module. Without this the leg keeps running and the
        # consumer asserts its fallback while reporting PASS, so the run is
        # green over a contract it never checked.
        self._builder(1)
        result = self._run({"LEGACY_ORACLES": "on"})
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("matrix", result.stderr)
        self.assertIn("3.2.7", result.stderr)
        self.assertIn("still reports PASS", result.stderr)

    def test_off_names_every_consumer_that_will_run_degraded(self) -> None:
        self._builder(0)
        result = self._run({"LEGACY_ORACLES": "off"})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(self.calls.exists(), "off must not build anything")
        self.assertIn("NOT BUILT", result.stderr)
        self.assertIn("matrix", result.stderr)
        self.assertIn("1 consumer(s) running degraded", result.stderr)

    def test_off_annotates_the_degradation_on_github(self) -> None:
        self._builder(0)
        result = self._run(
            {"LEGACY_ORACLES": "off", "GITHUB_ACTIONS": "true"}
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("::warning ", result.stdout + result.stderr)

    def test_a_consumer_that_cannot_run_here_is_not_built_and_not_degraded(self) -> None:
        (self.tree / "testsuite" / "matrix_test.py").write_text(
            consumer_needing(tcp=True, root=False)
        )
        self._builder(0)
        result = self._run({"LEGACY_ORACLES": "on", "USE_TCP": "no"})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(self.calls.exists())
        self.assertIn("not needed", result.stderr)
        self.assertIn("0 consumer(s) running degraded", result.stderr)

    def test_a_consumer_the_manifest_omits_is_not_built(self) -> None:
        manifest = self.tmp / "expect.txt"
        manifest.write_text("# ledger\nsomething-else pass\n")
        self._builder(0)
        result = self._run(
            {"LEGACY_ORACLES": "on", "EXPECT_RESULT": str(manifest)}
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(self.calls.exists())
        self.assertIn("does not name it", result.stderr)

    def test_an_unknown_mode_is_refused(self) -> None:
        self._builder(0)
        result = self._run({"LEGACY_ORACLES": "yes"})
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("must be 'on' or 'off'", result.stderr)


class BuildOldRsyncOracleTests(unittest.TestCase):
    """build_old_rsync_oracle.sh: what it installs and what it refuses.

    Hermetic: a pre-placed source tarball means curl is never reached, and the
    tarball's ./configure writes a Makefile whose `all` emits a shell script
    standing in for the built rsync. Real tar and real make, no compiler.
    """

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.tmp = Path(self._tmp.name)
        self.dest = self.tmp / "old_versions"
        self.workdir = self.tmp / "build"
        self.workdir.mkdir(parents=True)

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def _plant_source(self, version: str, reports: str) -> None:
        """A fake rsync-<version>.tar.gz whose build reports `reports`."""
        src = self.tmp / "src" / f"rsync-{version}"
        src.mkdir(parents=True)
        (src / "configure").write_text(
            "#!/bin/sh\n"
            "printf 'all:\\n\\tprintf \"#!/bin/sh\\\\necho \\\\\"rsync  version "
            f"{reports}  protocol version 31\\\\\"\\\\n\" > rsync\\n"
            "\\tchmod +x rsync\\n' > Makefile\n"
            "touch configured.marker\n"
        )
        (src / "configure").chmod(0o755)
        with tarfile.open(self.workdir / f"rsync-{version}.tar.gz", "w:gz") as tar:
            tar.add(src, arcname=f"rsync-{version}")

    def _run(self, version: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["bash", str(BUILDER), version, str(self.dest), str(self.workdir)],
            capture_output=True, text=True, check=False,
        )

    def test_a_build_reporting_the_requested_version_is_installed(self) -> None:
        self._plant_source("3.2.7", "3.2.7")
        result = self._run("3.2.7")
        self.assertEqual(result.returncode, 0, result.stderr)
        installed = self.dest / "rsync_3.2.7"
        self.assertTrue(installed.is_file())
        self.assertIn("3.2.7", subprocess.run(
            [str(installed), "--version"], capture_output=True, text=True).stdout)

    def test_a_build_reporting_another_version_is_refused_and_removed(self) -> None:
        # A binary that is present and executable but is not the release asked
        # for is the worst outcome available: the consuming test would accept
        # it as its oracle and pin the wrong behaviour. Leaving it on disk
        # would also make the next run's idempotence check step over it.
        self._plant_source("3.2.7", "3.4.1")
        result = self._run("3.2.7")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("does not report version 3.2.7", result.stderr)
        self.assertFalse((self.dest / "rsync_3.2.7").exists())

    def test_an_unfetchable_version_fails(self) -> None:
        # No tarball planted and no network reachable for a version that does
        # not exist: the script must fail rather than install nothing quietly.
        env = dict(os.environ)
        env["RSYNC_TARBALL_BASE_URL"] = "file:///nonexistent-oracle-source"
        result = subprocess.run(
            ["bash", str(BUILDER), "0.0.1", str(self.dest), str(self.workdir)],
            capture_output=True, text=True, check=False, env=env,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.dest / "rsync_0.0.1").exists())

    def test_an_already_good_binary_is_not_rebuilt(self) -> None:
        self._plant_source("3.2.7", "3.2.7")
        self.assertEqual(self._run("3.2.7").returncode, 0)
        marker = self.workdir / "rsync-3.2.7" / "configured.marker"
        self.assertTrue(marker.is_file())
        marker.unlink()
        result = self._run("3.2.7")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(marker.exists(), "a second run reconfigured the tree")
        self.assertIn("already present", result.stderr)



# The construct both 3.1.3 and 3.2.7 carry in syscall.c: an empty-parameter-list
# forward declaration followed by a real definition and a three-argument call.
# Under C17 `()` means "unspecified arguments" and this compiles; under C23 it
# means `(void)`, and the same three lines are two hard errors. Reduced from
# rsync-3.2.7/syscall.c:392-396 (`extern OFF_T lseek64();` then
# `return lseek64(fd, offset, whence);`) with the glibc-only names removed, so
# the fixture reproduces the CLASS rather than one platform's spelling.
_ERA_PROBE_C = """\
extern long probe_fn();
long probe_fn(int a, long b, int c) { return a + b + c; }
long call_probe(void) { return probe_fn(1, 2, 3); }
"""


class OracleCflagsEraTests(unittest.TestCase):
    """The oracle's CFLAGS must compile a pre-C23 release on a C23 compiler.

    A C23-default compiler is SIMULATED rather than required: a leading
    `-std=gnu23` is prepended to the recorded CFLAGS, and both gcc and clang
    take the LAST `-std` on the command line, so the leading one stands in for
    the compiler's own default and an explicit pin in CFLAGS overrides it
    exactly as it would on gcc 15. Without that, the cell would be vacuous on
    every host whose `cc` still defaults to gnu17 - measured: it is, on macOS,
    where dropping the pin from the builder killed nothing.

    Not a text assertion on the flag string either: the flags are handed to a
    real compiler along with the construct that breaks, and the same simulation
    WITHOUT the recorded flags is the negative control.
    """

    def setUp(self) -> None:
        self.cc = os.environ.get("CC") or shutil.which("cc") or shutil.which("gcc")
        if not self.cc:
            self.skipTest("no C compiler on PATH; the era pin cannot be exercised")
        self._tmp = tempfile.TemporaryDirectory()
        self.tmp = Path(self._tmp.name)
        self.probe = self.tmp / "era_probe.c"
        self.probe.write_text(_ERA_PROBE_C)
        if self._compile(["-std=gnu23"]) == 0:
            self.skipTest(
                f"{self.cc} does not treat `()` as (void) even at -std=gnu23; "
                "this compiler cannot exhibit the failure being guarded"
            )

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def _compile(self, extra: list[str]) -> int:
        return subprocess.run(
            [self.cc, *extra, "-c", str(self.probe), "-o", os.devnull],
            capture_output=True, text=True, check=False,
        ).returncode

    def _recorded_cflags(self) -> list[str]:
        """Run the builder against a tarball whose configure records CFLAGS."""
        workdir = self.tmp / "build"
        workdir.mkdir()
        record = self.tmp / "cflags.txt"
        src = self.tmp / "src" / "rsync-3.2.7"
        src.mkdir(parents=True)
        (src / "configure").write_text(
            "#!/bin/sh\n"
            f'printf %s "$CFLAGS" > {shlex.quote(str(record))}\n'
            "printf 'all:\\n\\tprintf \"#!/bin/sh\\\\necho \\\\\"rsync  version "
            "3.2.7  protocol version 31\\\\\"\\\\n\" > rsync\\n"
            "\\tchmod +x rsync\\n' > Makefile\n"
        )
        (src / "configure").chmod(0o755)
        with tarfile.open(workdir / "rsync-3.2.7.tar.gz", "w:gz") as tar:
            tar.add(src, arcname="rsync-3.2.7")
        result = subprocess.run(
            ["bash", str(BUILDER), "3.2.7", str(self.tmp / "old_versions"), str(workdir)],
            capture_output=True, text=True, check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        return shlex.split(record.read_text())

    def test_the_oracle_cflags_compile_a_pre_c23_declaration(self) -> None:
        cflags = self._recorded_cflags()
        self.assertEqual(
            self._compile(["-std=gnu23", *cflags]), 0,
            "the CFLAGS the builder passes to ./configure do not override a "
            "C23 default, so they cannot compile the empty-parameter-list "
            "declaration rsync 3.1.3 and 3.2.7 both carry; on gcc 15 every "
            "legacy oracle build fails and every oracle-backed testsuite cell "
            "degrades to its fallback",
        )

    def test_the_simulated_c23_default_really_bites(self) -> None:
        # The negative control for the simulation. Without the recorded flags
        # the leading -std=gnu23 must break the probe; if it stops doing so,
        # the assertion above is passing for the wrong reason.
        self.assertNotEqual(
            self._compile(["-std=gnu23"]), 0,
            "the probe compiled under a bare -std=gnu23, so the simulated "
            "C23 default no longer reproduces the conflict",
        )


if __name__ == "__main__":
    unittest.main()
