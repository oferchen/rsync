"""Unit tests for tools/ci/ensure_apt_packages.sh.

The script exists because `awalsh128/cache-apt-pkgs-action` reports package
presence from a manifest the restored cache carries, not from the machine, so a
payload-less cache entry announces every package installed and installs none.
The repair path is therefore the whole point of the script - and it is the path
a CI run cannot be relied on to exercise, because whether the cache is poisoned
on any given run is not something the run controls. A green workflow proves the
step is wired; only these tests prove it repairs.

`dpkg-query` and `sudo` are stubbed on PATH so the tests are hermetic: they
install nothing, need no root, and run identically on a developer machine and on
a CI runner. The stubs share a state file, which is what lets a test decide
whether the simulated install succeeds or silently does nothing.
"""

from __future__ import annotations

import os
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[2] / "tools" / "ci" / "ensure_apt_packages.sh"

# Reports "installed" for any package listed in $STATE_FILE, and exits non-zero
# otherwise - dpkg-query's own behaviour for an unknown package, which is one of
# the two ways a package can be missing.
DPKG_STUB = """#!/bin/sh
# Invoked as `dpkg-query -W -f=${Status} <pkg>`, so the package name is $3.
pkg=$3
if grep -qx "$pkg" "$STATE_FILE" 2>/dev/null; then
    printf 'install ok installed'
    exit 0
fi
printf 'unknown'
exit 1
"""

# `apt-get install` appends to the state file, so a later dpkg-query sees the
# package. `apt-get update` is a no-op.
SUDO_INSTALL_STUB = """#!/bin/sh
echo "STUB $*" >> "$LOG_FILE"
if [ "$2" = "install" ]; then
    for a in "$@"; do
        case "$a" in
            sudo|apt-get|install|-y|--no-install-recommends) ;;
            *) echo "$a" >> "$STATE_FILE" ;;
        esac
    done
fi
exit 0
"""

# Exits 0 while installing NOTHING. This is the shape that matters: `apt-get`
# can exit 0 having skipped a package it could not resolve, so a script that
# trusted its exit code would report success here.
SUDO_NOOP_STUB = """#!/bin/sh
echo "STUB $*" >> "$LOG_FILE"
exit 0
"""


class EnsureAptPackagesTests(unittest.TestCase):
    def setUp(self) -> None:
        self._tempdir = tempfile.TemporaryDirectory()
        self.tmp = Path(self._tempdir.name)
        self.state = self.tmp / "installed.txt"
        self.log = self.tmp / "calls.log"
        self.state.write_text("")
        self.log.write_text("")
        self.bin = self.tmp / "bin"
        self.bin.mkdir()
        self._write_stub("dpkg-query", DPKG_STUB)

    def tearDown(self) -> None:
        self._tempdir.cleanup()

    def _write_stub(self, name: str, body: str) -> None:
        path = self.bin / name
        path.write_text(body)
        path.chmod(0o755)

    def _installed(self, *packages: str) -> None:
        self.state.write_text("".join(f"{p}\n" for p in packages))

    def _run(self, *args: str) -> subprocess.CompletedProcess[str]:
        env = dict(os.environ)
        env["PATH"] = f"{self.bin}{os.pathsep}{env['PATH']}"
        env["STATE_FILE"] = str(self.state)
        env["LOG_FILE"] = str(self.log)
        return subprocess.run(
            ["bash", str(SCRIPT), *args],
            capture_output=True,
            text=True,
            env=env,
            check=False,
        )

    def _calls(self) -> str:
        return self.log.read_text()

    def test_all_installed_is_a_no_op(self) -> None:
        self._write_stub("sudo", SUDO_NOOP_STUB)
        self._installed("libacl1-dev", "libxxhash-dev")

        result = self._run("libacl1-dev", "libxxhash-dev")

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("All 2 requested APT packages are installed", result.stdout)
        self.assertEqual(self._calls(), "", "a healthy cache must not invoke apt-get")

    def test_quoted_and_unquoted_package_lists_are_equivalent(self) -> None:
        # Callers pass `$APT_PACKAGES`; whether it word-splits depends on a pair
        # of quotes someone will eventually "correct" in either direction. Both
        # spellings must mean the same thing rather than one of them failing
        # with dpkg complaining about a package named after the whole list.
        self._write_stub("sudo", SUDO_NOOP_STUB)
        self._installed("libacl1-dev", "libxxhash-dev")

        split = self._run("libacl1-dev", "libxxhash-dev")
        joined = self._run("libacl1-dev libxxhash-dev")

        self.assertEqual(split.returncode, 0, split.stderr)
        self.assertEqual(joined.returncode, 0, joined.stderr)
        self.assertEqual(split.stdout, joined.stdout)

    def test_missing_package_is_installed_and_only_the_missing_one(self) -> None:
        self._write_stub("sudo", SUDO_INSTALL_STUB)
        self._installed("libacl1-dev")

        result = self._run("libacl1-dev", "libxxhash-dev")

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("::warning::", result.stdout)
        self.assertIn("libxxhash-dev", result.stdout)
        calls = self._calls()
        self.assertIn("apt-get install", calls)
        self.assertIn("libxxhash-dev", calls)
        self.assertNotIn(
            "libacl1-dev",
            calls.split("install", 1)[1],
            "an already-installed package must not be reinstalled",
        )

    def test_install_that_exits_zero_without_installing_still_fails(self) -> None:
        # The decisive case. The stub exits 0 and installs nothing, so a script
        # that inherited apt-get's verdict would report success. Re-asking dpkg
        # is what makes the guard fail closed.
        self._write_stub("sudo", SUDO_NOOP_STUB)
        self._installed("libacl1-dev")

        result = self._run("libacl1-dev", "libxxhash-dev")

        self.assertEqual(result.returncode, 1)
        self.assertIn("::error::", result.stdout)
        self.assertIn("still missing after install", result.stdout)
        self.assertIn("libxxhash-dev", result.stdout)

    def test_no_arguments_is_a_usage_error(self) -> None:
        self._write_stub("sudo", SUDO_NOOP_STUB)

        result = self._run()

        self.assertEqual(result.returncode, 2)
        self.assertIn("usage:", result.stderr)


if __name__ == "__main__":
    unittest.main()
