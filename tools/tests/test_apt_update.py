"""Unit tests for tools/ci/apt_update.sh.

The script reclassifies exactly one `apt-get update` failure - index files that
apt itself reports as ignored - as non-fatal, and leaves every other failure
fatal. Both halves have to be tested, and the second half is the one that keeps
the first half honest: a script that simply swallowed exit 100 would pass every
test about the Chrome hash-sum case and quietly hide a lock conflict.

CI cannot exercise either path on demand. Whether a third-party repository
republishes its Release mid-fetch on a given run is not something the run
controls, so a green workflow proves the script is wired, never that it
classifies correctly. These tests are the only thing that does.

`sudo` is stubbed on PATH: `sudo apt-get` replays a canned transcript and any
other command runs unprivileged against a temporary APT_ETC, so the tests
install nothing, need no root, and run identically on a developer machine and a
runner.
"""

from __future__ import annotations

import os
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[2] / "tools" / "ci" / "apt_update.sh"

CLEAN = """\
Hit:1 http://azure.archive.ubuntu.com/ubuntu noble InRelease
Get:2 http://azure.archive.ubuntu.com/ubuntu noble-updates InRelease [126 kB]
Fetched 126 kB in 1s (126 kB/s)
Reading package lists...
"""

# Transcript captured verbatim from job 102552809625 of run 34376473178
# (2026-09-09), the failure this script exists for. The Ubuntu archive indexes
# all fetched; Google's Chrome repository republished its Release mid-fetch.
CHROME_HASH_MISMATCH = """\
Get:23 http://azure.archive.ubuntu.com/ubuntu noble-security/restricted Translation-en [334 kB]
Get:24 https://dl.google.com/linux/chrome-stable/deb stable/main amd64 Packages [1405 B]
Err:24 https://dl.google.com/linux/chrome-stable/deb stable/main amd64 Packages
  Hash Sum mismatch
  Hashes of expected file:
   - Filesize:1405 [weak]
   - SHA256:233e56de019b57db89238fa7bcc3647718dbbea3a40c2dc1c633a8c8952aa9e9
  Hashes of received file:
   - SHA256:bc1428ab27c6d76ee9bb76de07f1ded0ddb4aaabd958fc72855634ef5894a4b3
  Last modification reported: Wed, 09 Sep 2026 09:41:12 +0000
  Release file created at: Wed, 09 Sep 2026 17:16:59 +0000
Fetched 9436 kB in 1s (9511 kB/s)
Reading package lists...
E: Failed to fetch https://dl.google.com/linux/chrome-stable/deb/dists/stable/main/binary-amd64/Packages.gz  Hash Sum mismatch
   Hashes of expected file:
    - Filesize:1405 [weak]
E: Some index files failed to download. They have been ignored, or old ones used instead.
"""

LOCK_CONFLICT = """\
Reading package lists...
E: Could not get lock /var/lib/dpkg/lock-frontend. It is held by process 4242
E: Unable to acquire the dpkg frontend lock (/var/lib/dpkg/lock-frontend), is another process using it?
"""

# The decisive mixed case: a real error arriving alongside the ignorable
# summary. Reclassifying on the summary alone would swallow the lock too.
IGNORED_PLUS_LOCK = """\
Reading package lists...
E: Failed to fetch https://dl.google.com/linux/chrome-stable/deb/dists/stable/main/binary-amd64/Packages.gz  Hash Sum mismatch
E: Could not get lock /var/lib/dpkg/lock-frontend. It is held by process 4242
E: Some index files failed to download. They have been ignored, or old ones used instead.
"""

NO_DIAGNOSTIC = """\
Reading package lists...
"""


class AptUpdateTests(unittest.TestCase):
    def setUp(self) -> None:
        self._tempdir = tempfile.TemporaryDirectory()
        self.tmp = Path(self._tempdir.name)
        self.bin = self.tmp / "bin"
        self.bin.mkdir()
        self.apt_etc = self.tmp / "etc-apt"
        (self.apt_etc / "sources.list.d").mkdir(parents=True)

    def tearDown(self) -> None:
        self._tempdir.cleanup()

    def _stub_sudo(self, transcript: str, exit_code: int) -> None:
        canned = self.tmp / "apt-output.txt"
        canned.write_text(transcript)
        stub = self.bin / "sudo"
        stub.write_text(
            "#!/bin/sh\n"
            'if [ "$1" != apt-get ]; then exec "$@"; fi\n'
            f'cat "{canned}"\n'
            f"exit {exit_code}\n"
        )
        stub.chmod(0o755)

    def _run(self) -> subprocess.CompletedProcess[str]:
        env = dict(os.environ)
        env["PATH"] = f"{self.bin}{os.pathsep}{env['PATH']}"
        env["APT_ETC"] = str(self.apt_etc)
        return subprocess.run(
            ["bash", str(SCRIPT)],
            capture_output=True,
            text=True,
            env=env,
            check=False,
        )

    def test_clean_update_succeeds_silently(self) -> None:
        self._stub_sudo(CLEAN, 0)

        result = self._run()

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("::warning::", result.stdout)
        self.assertNotIn("::error::", result.stdout)

    def test_ignored_index_download_is_not_fatal_and_is_announced(self) -> None:
        # The measured case. apt exits 100 having declared the failed indexes
        # ignored; the caller's `apt-get install` is what decides whether the
        # package it wants is resolvable.
        self._stub_sudo(CHROME_HASH_MISMATCH, 100)

        result = self._run()

        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertIn("::warning::", result.stdout)
        self.assertIn("ignored 1 unreachable index file(s)", result.stdout)
        self.assertIn("dl.google.com", result.stdout)
        self.assertNotIn("::error::", result.stdout)

    def test_lock_conflict_stays_fatal(self) -> None:
        self._stub_sudo(LOCK_CONFLICT, 100)

        result = self._run()

        self.assertEqual(result.returncode, 100)
        self.assertIn("::error::", result.stdout)
        self.assertIn("did not report the failure as ignorable", result.stdout)

    def test_a_real_error_alongside_the_ignorable_summary_stays_fatal(self) -> None:
        # Without this, the script would be a blanket swallow of exit 100
        # wearing a hash-sum-shaped excuse.
        self._stub_sudo(IGNORED_PLUS_LOCK, 100)

        result = self._run()

        self.assertEqual(result.returncode, 100)
        self.assertIn("::error::", result.stdout)
        self.assertIn("beyond the ignored index downloads", result.stdout)
        self.assertIn("Could not get lock", result.stdout)

    def test_failure_without_any_diagnostic_stays_fatal(self) -> None:
        self._stub_sudo(NO_DIAGNOSTIC, 100)

        result = self._run()

        self.assertEqual(result.returncode, 100)
        self.assertIn("::error::", result.stdout)

    def test_apt_output_is_always_echoed(self) -> None:
        # The transcript is the diagnostic. Reclassifying the exit status must
        # not also hide what apt said.
        self._stub_sudo(CHROME_HASH_MISMATCH, 100)

        result = self._run()

        self.assertIn("Hash Sum mismatch", result.stdout)
        self.assertIn("Fetched 9436 kB", result.stdout)


    # Runner image sources, as shipped on ubuntu-24.04 (deb822 via the mirror
    # list) and on older images (one-line, geographic host named directly).
    def _write_source(self, name: str, text: str) -> Path:
        path = self.apt_etc / "sources.list.d" / name
        path.write_text(text)
        return path

    def test_mirror_list_source_is_pointed_at_the_global_archive(self) -> None:
        source = self._write_source(
            "ubuntu.sources",
            "Types: deb\n"
            "URIs: mirror+file:/etc/apt/apt-mirrors.txt\n"
            "Suites: noble noble-updates noble-backports\n",
        )
        self._stub_sudo(CLEAN, 0)

        result = self._run()

        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("URIs: http://archive.ubuntu.com/ubuntu/\n", source.read_text())
        self.assertNotIn("apt-mirrors", source.read_text())

    def test_geographic_mirror_host_is_pointed_at_the_global_archive(self) -> None:
        listed = self.apt_etc / "sources.list"
        listed.write_text(
            "deb http://azure.archive.ubuntu.com/ubuntu/ jammy main\n"
            "deb https://us.archive.ubuntu.com/ubuntu jammy-updates main\n"
        )
        self._stub_sudo(CLEAN, 0)

        result = self._run()

        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(
            listed.read_text(),
            "deb http://archive.ubuntu.com/ubuntu/ jammy main\n"
            "deb http://archive.ubuntu.com/ubuntu/ jammy-updates main\n",
        )

    def test_global_and_third_party_sources_are_left_alone(self) -> None:
        # Only a geographic Ubuntu mirror is rewritten; the global archive,
        # the security archive and unrelated repositories keep their URIs.
        text = (
            "deb http://archive.ubuntu.com/ubuntu/ noble main\n"
            "deb http://security.ubuntu.com/ubuntu noble-security main\n"
            "deb http://ports.ubuntu.com/ubuntu-ports noble main\n"
            "deb https://dl.google.com/linux/chrome/deb/ stable main\n"
        )
        source = self._write_source("mixed.list", text)
        self._stub_sudo(CLEAN, 0)

        result = self._run()

        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(source.read_text(), text)

if __name__ == "__main__":
    unittest.main()
