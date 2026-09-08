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


def consumer_needing(tcp: bool, root: bool, peer_slot: bool = False) -> str:
    """A consuming test carrying the preconditions the harness reads."""
    body = CONSUMER
    if tcp:
        body += "require_tcp('needs a real TCP peer')\n"
    if root:
        body += "import os\nif os.geteuid() != 0:\n    raise SystemExit(0)\n"
    if peer_slot:
        # daemon-symlink-escape-matrix_test.py:75-79 verbatim in shape: the
        # peer WINS over the archive binary, so a wrong peer is not merely an
        # extra oracle, it is a substitute one.
        body += (
            "from rsyncfns import RSYNC, RSYNC_PEER\n"
            "if RSYNC_PEER != RSYNC:\n"
            "    ORACLE = RSYNC_PEER\n"
        )
    return body


# A consumer that only IMPORTS RSYNC_PEER without evaluating it. The import
# alias is not an ast.Name, so the scan must not read this as a hijackable
# slot - every test in the 3.5.0 suite imports from rsyncfns.
PEER_IMPORTED_UNUSED = CONSUMER + "from rsyncfns import RSYNC, RSYNC_PEER\n"


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
        self.assertEqual(result.stdout.strip(), "3.2.7\tmatrix\tyes\tyes\tno")

    def test_preconditions_are_read_from_the_test_not_assumed(self) -> None:
        self._test("plain", consumer_needing(tcp=False, root=False))
        result = self._run()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "3.2.7\tplain\tno\tno\tno")

    def test_a_peer_readable_oracle_slot_is_discovered(self) -> None:
        # The hijackable slot. A test that evaluates RSYNC_PEER takes the
        # --rsync-bin2 binary as its oracle IN PREFERENCE to old_versions/,
        # and labels the column with the version it asked for either way.
        self._test("matrix", consumer_needing(tcp=False, root=False, peer_slot=True))
        result = self._run()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "3.2.7\tmatrix\tno\tno\tyes")

    def test_importing_the_peer_name_is_not_using_it(self) -> None:
        # Every test in the suite imports from rsyncfns. Counting the import
        # would mark all 345 as hijackable and make the gate below fire on
        # legs where no oracle slot exists at all.
        self._test("plain", PEER_IMPORTED_UNUSED)
        result = self._run()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "3.2.7\tplain\tno\tno\tno")

    def test_the_real_matrix_test_is_classified_as_peer_readable(self) -> None:
        # The population this gate exists for, read from the real tree rather
        # than from a fixture shaped like it. Skipped, never faked, when the
        # tarball is not extracted here.
        tree = REPO / "target" / "interop" / "upstream-src" / "rsync-3.5.0"
        consumer = tree / "testsuite" / "daemon-symlink-escape-matrix_test.py"
        if not consumer.is_file():
            self.skipTest(f"{consumer} is not extracted here")
        self.tree = tree
        result = self._run()
        self.assertEqual(result.returncode, 0, result.stderr)
        rows = [line.split("\t") for line in result.stdout.splitlines()]
        matrix = [r for r in rows if r[1] == "daemon-symlink-escape-matrix"]
        self.assertEqual(len(matrix), 1, result.stdout)
        self.assertEqual(matrix[0][0], "3.2.7")
        self.assertEqual(matrix[0][4], "yes", result.stdout)

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
            version, name, needs_tcp, needs_root, peer_slot = line.split("\t")
            self.assertRegex(version, r"^\d+\.\d+(\.\d+)?$")
            self.assertTrue(name)
            self.assertIn(needs_tcp, ("yes", "no"))
            self.assertIn(needs_root, ("yes", "no"))
            self.assertIn(peer_slot, ("yes", "no"))


class LegacyOracleHarnessFixture:
    """A synthetic upstream tree, a stub builder, and a sourced-harness runner.

    A mixin rather than a TestCase base so the classes below do not inherit one
    another's cases: shared fixture, disjoint populations.
    """

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

    def _peer(self, banner: str | None) -> str:
        """A stand-in --rsync-bin2 binary. `banner` None => --version fails."""
        peer = self.tmp / "peer-rsync"
        if banner is None:
            peer.write_text("#!/usr/bin/env bash\nexit 1\n")
        else:
            # Upstream's banner shape, "version" twice, release token first.
            peer.write_text(
                "#!/usr/bin/env bash\n"
                f'printf "%s\\n" {shlex.quote(banner)}\n'
                'printf "%s\\n" "Copyright (C) 1996-2024 by Andrew Tridgell"\n'
            )
        peer.chmod(0o755)
        return str(peer)

    def _run(self, env_overrides: dict, extra: str = "") -> subprocess.CompletedProcess[str]:
        env = dict(os.environ)
        env.pop("LEGACY_ORACLES", None)
        env.pop("USE_TCP", None)
        env.pop("EXPECT_RESULT", None)
        env.pop("UPSTREAM_PEER_BIN", None)
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


class EnsureLegacyOraclesTests(LegacyOracleHarnessFixture, unittest.TestCase):
    """ensure_legacy_oracles(): builds, refuses, or names the degradation."""

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


class OracleSlotProvenanceTests(LegacyOracleHarnessFixture, unittest.TestCase):
    """The --rsync-bin2 peer may only fill an oracle slot it matches.

    MEASURED on the real 3.5.0 tree before this gate existed: running
    daemon-symlink-escape-matrix_test.py with RSYNC_PEER pointed at the 3.5.0
    build started the oracle daemon FROM 3.5.0 and printed all 100
    `insecure links = yes` cells as `want=N(327)`. The label is a hardcoded
    string in upstream's test (:256-257) and the slot's only acceptance test
    is that `--version` exits zero (:87-94), so nothing downstream can tell a
    3.2.7 answer from any other binary's.
    """

    def setUp(self) -> None:
        super().setUp()
        (self.tree / "testsuite" / "matrix_test.py").write_text(
            consumer_needing(tcp=False, root=False, peer_slot=True)
        )

    def test_a_peer_that_is_not_the_named_version_is_refused(self) -> None:
        self._builder(0)
        peer = self._peer("rsync  version 3.5.0  protocol version 32")
        result = self._run({"LEGACY_ORACLES": "on", "UPSTREAM_PEER_BIN": peer})
        self.assertNotEqual(result.returncode, 0, result.stderr)
        self.assertIn("is not rsync 3.2.7", result.stderr)
        self.assertIn("3.5.0", result.stderr)
        self.assertIn("matrix", result.stderr)

    def test_the_refusal_survives_a_built_oracle(self) -> None:
        # The peer wins the `elif`, so building the real 3.2.7 does not rescue
        # the slot - refusing is the only truthful outcome, and it must not be
        # softened into "we built one too".
        self._builder(0)
        peer = self._peer("rsync  version 3.4.1  protocol version 32")
        result = self._run({"LEGACY_ORACLES": "on", "UPSTREAM_PEER_BIN": peer})
        self.assertNotEqual(result.returncode, 0, result.stderr)
        self.assertIn("the peer wins the tie", result.stderr)

    def test_the_refusal_is_annotated_on_github(self) -> None:
        self._builder(0)
        peer = self._peer("rsync  version 3.5.0  protocol version 32")
        result = self._run(
            {"LEGACY_ORACLES": "on", "UPSTREAM_PEER_BIN": peer,
             "GITHUB_ACTIONS": "true"}
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("::error ", result.stdout + result.stderr)

    def test_a_peer_that_will_not_say_its_version_is_refused(self) -> None:
        # Unreadable provenance is not "probably fine". The pre-existing probe
        # in the test only checks the exit status, so a binary that answers
        # nothing intelligible still fills the slot.
        self._builder(0)
        peer = self._peer(None)
        result = self._run({"LEGACY_ORACLES": "on", "UPSTREAM_PEER_BIN": peer})
        self.assertNotEqual(result.returncode, 0, result.stderr)
        self.assertIn("is not rsync 3.2.7", result.stderr)

    def test_a_matching_peer_supplies_the_oracle(self) -> None:
        self._builder(0)
        peer = self._peer("rsync  version 3.2.7  protocol version 31")
        result = self._run({"LEGACY_ORACLES": "on", "UPSTREAM_PEER_BIN": peer})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("supplied by the --rsync-bin2 peer", result.stderr)
        self.assertIn("1 from the --rsync-bin2 peer", result.stderr)
        # It is the oracle, so it is neither built nor degraded - reporting it
        # as degraded would be the same lie pointed the other way.
        self.assertFalse(self.calls.exists())
        self.assertIn("0 consumer(s) running degraded", result.stderr)

    def test_a_matching_peer_is_the_oracle_even_with_oracles_off(self) -> None:
        self._builder(0)
        peer = self._peer("rsync  version 3.2.7  protocol version 31")
        result = self._run({"LEGACY_ORACLES": "off", "UPSTREAM_PEER_BIN": peer})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("supplied by the --rsync-bin2 peer", result.stderr)
        self.assertIn("0 consumer(s) running degraded", result.stderr)

    def test_a_consumer_that_cannot_run_here_does_not_gate_the_peer(self) -> None:
        # The narrow scope is the whole point: UPSTREAM_PEER_BIN's job is
        # version MIXING, and on a leg where no test can adopt it as an oracle
        # a 3.5.0 peer is exactly what it should be. Refusing there would break
        # the knob's documented purpose to fix a problem that is not present.
        (self.tree / "testsuite" / "matrix_test.py").write_text(
            consumer_needing(tcp=True, root=False, peer_slot=True)
        )
        self._builder(0)
        peer = self._peer("rsync  version 3.5.0  protocol version 32")
        result = self._run(
            {"LEGACY_ORACLES": "on", "USE_TCP": "no", "UPSTREAM_PEER_BIN": peer}
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("not needed", result.stderr)

    def test_a_consumer_that_ignores_the_peer_does_not_gate_it(self) -> None:
        (self.tree / "testsuite" / "matrix_test.py").write_text(
            consumer_needing(tcp=False, root=False, peer_slot=False)
        )
        self._builder(0)
        peer = self._peer("rsync  version 3.5.0  protocol version 32")
        result = self._run({"LEGACY_ORACLES": "on", "UPSTREAM_PEER_BIN": peer})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("1 on disk", result.stderr)


class RsyncReportedVersionTests(unittest.TestCase):
    """rsync_reported_version(): the binary is the authority, not the path."""

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.tmp = Path(self._tmp.name)

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def _probe(self, script: str) -> subprocess.CompletedProcess[str]:
        # Named for a version it is NOT, so a reading that trusts the filename
        # rather than the banner cannot pass.
        binary = self.tmp / "rsync_3.2.7"
        binary.write_text("#!/usr/bin/env bash\n" + script)
        binary.chmod(0o755)
        program = textwrap.dedent(
            f"""
            source {shlex.quote(str(HARNESS))}
            rsync_reported_version {shlex.quote(str(binary))}
            """
        )
        return subprocess.run(
            ["bash", "-c", program], capture_output=True, text=True, check=False
        )

    def test_the_release_token_wins_over_the_protocol_one(self) -> None:
        # "rsync  version 3.5.0  protocol version 32" says "version" twice.
        result = self._probe(
            'printf "%s\\n" "rsync  version 3.5.0  protocol version 32"\n'
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "3.5.0")

    def test_a_banner_still_being_written_is_read_whole(self) -> None:
        # rsync writes its banner in ~20 unbuffered write(2)s - MEASURED, 793
        # bytes on 3.2.7 - so a reader that closes the pipe after line 1 kills
        # the producer with SIGPIPE and, under `set -o pipefail`, the whole
        # pipeline reports 141 for a binary that answered perfectly.
        #
        # The sleep is what makes that DETERMINISTIC. Without it the outcome is
        # a race the producer usually wins: 20 small writes fit in the pipe
        # buffer and complete before the reader exits, so a `| head -1` spelling
        # passes this cell most of the time. MEASURED: mutating
        # rsync_reported_version() to pipe through `head -n1` killed nothing
        # until the producer was made to still be writing when the reader left.
        script = 'printf "%s\\n" "rsync  version 3.2.7  protocol version 31"\n'
        script += "sleep 0.5\n"
        script += 'printf "%s\\n" "Copyright (C) 1996-2024 by Andrew Tridgell"\n'
        result = self._probe(script)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "3.2.7")

    def test_a_binary_that_fails_reports_failure(self) -> None:
        result = self._probe("exit 1\n")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "")

    def test_an_unparseable_banner_reports_failure(self) -> None:
        result = self._probe('printf "%s\\n" "some other program"\n')
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "")


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

    def _plant_source(self, version: str, reports: str,
                      banner: str | None = None) -> None:
        """A fake rsync-<version>.tar.gz whose build reports `reports`.

        `banner` overrides the whole shell body of the fake binary, for cases
        that need the banner emitted in more than one write.
        """
        src = self.tmp / "src" / f"rsync-{version}"
        src.mkdir(parents=True)
        if banner is None:
            banner = f'echo "rsync  version {reports}  protocol version 31"\n'
        (src / "rsync.template").write_text("#!/bin/sh\n" + banner)
        (src / "configure").write_text(
            "#!/bin/sh\n"
            "printf 'all:\\n\\tcp rsync.template rsync\\n"
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

    def test_a_binary_that_writes_its_banner_in_stages_is_still_accepted(self) -> None:
        # REGRESSION. The usability probe used to read
        #     "$TARGET_BIN" --version 2>/dev/null | head -1 | grep -q ...
        # inside a script running under `set -o pipefail`. Real rsync does not
        # buffer its banner - MEASURED on a 3.2.7 build, `--version` is 20
        # separate write(2) calls - so `head -1` closes the read end while rsync
        # is still writing, rsync dies of SIGPIPE, and the PIPELINE yields 141
        # even though the version text matched. The script then treats the
        # oracle it has just built as unusable, `rm -f`s it and exits 1, which
        # ensure_legacy_oracles escalates to failing the whole leg.
        #
        # MEASURED 10/10 against a real 3.2.7 build on an 8-core Linux host, and
        # MEASURED not to fire on the GitHub runner (nightly 34031044215
        # installed all three oracles) - a host-dependent failure, so CI green
        # was no evidence.
        #
        # The single-line stub the other cases use CANNOT show this: its writer
        # is finished before `head -1` exits, so no close is ever early. Two
        # writes with a pause between them reproduce it deterministically.
        self._plant_source("3.2.7", "3.2.7", banner=(
            'echo "rsync  version 3.2.7  protocol version 31"\n'
            "sleep 1\n"
            'echo "Copyright (C) 1996-2022 by Andrew Tridgell and others"\n'
        ))
        result = self._run("3.2.7")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((self.dest / "rsync_3.2.7").is_file())

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

    A C23-default compiler is SIMULATED rather than required: the C23 flag is
    prepended to the recorded CFLAGS, and both gcc and clang take the LAST
    `-std` on the command line, so the leading one stands in for the compiler's
    own default and an explicit pin in CFLAGS overrides it exactly as it would
    on gcc 15. Without that, the cell would be vacuous on every host whose `cc`
    still defaults to gnu17 - measured: it is, on macOS, where dropping the pin
    from the builder killed nothing.

    Not a text assertion on the flag string either: the flags are handed to a
    real compiler along with the construct that breaks, and the same simulation
    WITHOUT the recorded flags is the negative control.

    ⚠ The C23 flag is DISCOVERED, not assumed. `-std=gnu23` is a gcc-14 spelling;
    gcc 13 knows only `-std=gnu2x` and rejects the newer name outright. Assuming
    one made both cells report on the wrong thing on a gcc-13 runner: a failed
    compile meant "unrecognized option", so the control passed for the wrong
    reason while the pin failed for the wrong reason. Acceptance is probed on an
    EMPTY translation unit, which cannot fail for any reason but the flag.
    """

    def setUp(self) -> None:
        self.cc = os.environ.get("CC") or shutil.which("cc") or shutil.which("gcc")
        if not self.cc:
            self.skipTest("no C compiler on PATH; the era pin cannot be exercised")
        self._tmp = tempfile.TemporaryDirectory()
        self.tmp = Path(self._tmp.name)
        self.probe = self.tmp / "era_probe.c"
        self.probe.write_text(_ERA_PROBE_C)
        self.empty = self.tmp / "empty.c"
        self.empty.write_text("")
        self.c23 = next(
            (f for f in ("-std=gnu23", "-std=gnu2x")
             if self._compile(self.empty, [f]) == 0),
            None,
        )
        if self.c23 is None:
            self.skipTest(
                f"{self.cc} accepts neither -std=gnu23 nor -std=gnu2x; a C23 "
                "default cannot be simulated here"
            )
        if self._compile(self.probe, [self.c23]) == 0:
            self.skipTest(
                f"{self.cc} does not treat `()` as (void) even at {self.c23}; "
                "this compiler cannot exhibit the failure being guarded"
            )

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def _compile(self, source: Path, extra: list[str]) -> int:
        return subprocess.run(
            [self.cc, *extra, "-c", str(source), "-o", os.devnull],
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
            self._compile(self.probe, [self.c23, *cflags]), 0,
            "the CFLAGS the builder passes to ./configure do not override a "
            "C23 default, so they cannot compile the empty-parameter-list "
            "declaration rsync 3.1.3 and 3.2.7 both carry; on gcc 15 every "
            "legacy oracle build fails and every oracle-backed testsuite cell "
            "degrades to its fallback",
        )

    def test_the_simulated_c23_default_really_bites(self) -> None:
        # The negative control for the simulation. Without the recorded flags
        # the C23 flag must break the probe; if it stops doing so, the
        # assertion above is passing for the wrong reason. setUp has already
        # established that the flag itself is accepted, so a failure here can
        # only be the construct.
        self.assertNotEqual(
            self._compile(self.probe, [self.c23]), 0,
            f"the probe compiled under a bare {self.c23}, so the simulated "
            "C23 default no longer reproduces the conflict",
        )


if __name__ == "__main__":
    unittest.main()
