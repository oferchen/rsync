"""Tests for tools/ci/fetch_upstream_rsync.sh and its pin manifest.

The fetcher exists because CI piped unverified downloads from samba.org into
tar, and a transient partial transfer (curl exit 18) failed jobs with a gzip
EOF. These tests pin the properties that make that failure class impossible:
nothing unverified is ever extracted or cached, a bad cached file is replaced
rather than trusted, and no CI site bypasses the fetcher.
"""

from __future__ import annotations

import hashlib
import os
import re
import subprocess
import tarfile
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
FETCH = REPO / "tools" / "ci" / "fetch_upstream_rsync.sh"
MANIFEST = REPO / "tools" / "ci" / "upstream-tarballs.sha256"


def _pinned_versions() -> set[str]:
    return set(re.findall(r"^[0-9a-f]{64}  rsync-(\S+)\.tar\.gz$",
                          MANIFEST.read_text(), re.M))


class FetchTests(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.tmp = Path(self._tmp.name)
        self.mirror = self.tmp / "mirror"
        self.cache = self.tmp / "cache"
        self.out = self.tmp / "out"
        self.mirror.mkdir()
        src = self.tmp / "src" / "rsync-9.9.9"
        src.mkdir(parents=True)
        (src / "configure").write_text("#!/bin/sh\n")
        self.good = self.mirror / "rsync-9.9.9.tar.gz"
        with tarfile.open(self.good, "w:gz") as tar:
            tar.add(src, arcname="rsync-9.9.9")
        self.good_sha = hashlib.sha256(self.good.read_bytes()).hexdigest()
        self.manifest = self.tmp / "pins.sha256"
        self._pin(self.good_sha)

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def _pin(self, sha: str) -> None:
        self.manifest.write_text(f"# test pins\n{sha}  rsync-9.9.9.tar.gz\n")

    def _run(self, *args: str, mirror: str | None = None) -> subprocess.CompletedProcess[str]:
        env = dict(os.environ)
        env["UPSTREAM_TARBALL_CACHE"] = str(self.cache)
        env["UPSTREAM_TARBALL_MANIFEST"] = str(self.manifest)
        env["RSYNC_TARBALL_BASE_URL"] = mirror or self.mirror.as_uri()
        return subprocess.run(["bash", str(FETCH), *args],
                              capture_output=True, text=True, check=False, env=env)

    def test_a_miss_downloads_verifies_caches_and_extracts(self) -> None:
        result = self._run("9.9.9", str(self.out))
        self.assertEqual(result.returncode, 0, result.stderr)
        cached = self.cache / "rsync-9.9.9.tar.gz"
        self.assertEqual(result.stdout.strip(), str(cached))
        self.assertEqual(cached.read_bytes(), self.good.read_bytes())
        self.assertTrue((self.out / "rsync-9.9.9" / "configure").is_file())

    def test_a_hit_needs_no_network(self) -> None:
        # The point of the cache: once verified, a run must not touch the
        # mirror at all, so an unreachable mirror cannot fail it.
        self.assertEqual(self._run("9.9.9").returncode, 0)
        result = self._run("9.9.9", str(self.out), mirror="file:///nonexistent-mirror")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("downloading", result.stderr)
        self.assertTrue((self.out / "rsync-9.9.9").is_dir())

    def test_a_download_not_matching_the_pin_fails_loudly_and_leaves_nothing(self) -> None:
        self._pin("0" * 64)
        result = self._run("9.9.9", str(self.out))
        self.assertEqual(result.returncode, 3)
        self.assertIn("sha256 mismatch", result.stderr)
        self.assertIn(self.good_sha, result.stderr)
        self.assertFalse(self.out.exists(), "an unverified tarball was extracted")
        self.assertEqual(list(self.cache.iterdir()), [], "an unverified tarball was cached")

    def test_a_truncated_cached_tarball_is_replaced_not_trusted(self) -> None:
        # The curl-18 shape: a partial file sitting where a good one should be.
        self.cache.mkdir()
        (self.cache / "rsync-9.9.9.tar.gz").write_bytes(self.good.read_bytes()[:100])
        result = self._run("9.9.9", str(self.out))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("discarding", result.stderr)
        self.assertEqual((self.cache / "rsync-9.9.9.tar.gz").read_bytes(),
                         self.good.read_bytes())

    def test_a_failed_download_falls_over_to_the_next_source(self) -> None:
        # A single mirror was the single point of failure: one partial
        # transfer failed the job. The next source is tried, and its bytes
        # must still match the same pin.
        result = self._run("9.9.9", str(self.out),
                           mirror=f"file:///nonexistent-mirror {self.mirror.as_uri()}")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("nonexistent-mirror/rsync-9.9.9.tar.gz failed", result.stderr)
        self.assertTrue((self.out / "rsync-9.9.9").is_dir())

    def test_a_version_placeholder_in_a_source_is_expanded(self) -> None:
        # GitHub release assets live under a per-version tag directory
        # (releases/download/v<version>/), so a source must be able to name
        # the version, and a version-less source must still work after it.
        tagged = self.tmp / "gh" / "v9.9.9"
        tagged.mkdir(parents=True)
        (tagged / "rsync-9.9.9.tar.gz").write_bytes(self.good.read_bytes())
        template = (self.tmp / "gh").as_uri() + "/v%v"
        result = self._run("9.9.9", str(self.out), mirror=template)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("gh/v9.9.9/rsync-9.9.9.tar.gz", result.stderr)
        self.assertNotIn("%v", result.stderr)
        self.assertTrue((self.out / "rsync-9.9.9").is_dir())

    def test_the_default_sources_try_github_releases_first(self) -> None:
        text = FETCH.read_text()
        default = re.search(r'RSYNC_TARBALL_BASE_URL:-([^"}]+)', text)
        self.assertIsNotNone(default)
        self.assertTrue(default.group(1).startswith(
            "https://github.com/RsyncProject/rsync/releases/download/v%v "))

    def test_a_mismatch_is_fatal_and_does_not_fall_over(self) -> None:
        # Wrong bytes are not an availability problem; trying another source
        # would hide a tampered or corrupt mirror behind a green run.
        other = self.tmp / "other"
        other.mkdir()
        (other / "rsync-9.9.9.tar.gz").write_bytes(self.good.read_bytes())
        (self.mirror / "rsync-9.9.9.tar.gz").write_bytes(b"not the release")
        result = self._run("9.9.9", mirror=f"{self.mirror.as_uri()} {other.as_uri()}")
        self.assertEqual(result.returncode, 3)
        self.assertNotIn(other.as_uri(), result.stderr)

    def test_every_source_failing_is_reported(self) -> None:
        result = self._run("9.9.9", mirror="file:///nope-a file:///nope-b")
        self.assertEqual(result.returncode, 1)
        self.assertIn("could not be downloaded from any source", result.stderr)

    def test_an_unpinned_version_is_refused(self) -> None:
        result = self._run("1.2.3")
        self.assertEqual(result.returncode, 2)
        self.assertIn("no pinned sha256", result.stderr)

    def test_all_fetches_every_pinned_version(self) -> None:
        result = self._run("--all")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((self.cache / "rsync-9.9.9.tar.gz").is_file())


class ManifestCoverageTests(unittest.TestCase):
    """Every release CI builds must be pinned, and nothing may bypass the pin."""

    def test_the_manifest_lines_are_well_formed(self) -> None:
        for line in MANIFEST.read_text().splitlines():
            if line and not line.startswith("#"):
                self.assertRegex(line, r"^[0-9a-f]{64}  rsync-[0-9.]+\.tar\.gz$")

    def test_every_version_run_interop_builds_is_pinned(self) -> None:
        text = (REPO / "tools" / "ci" / "run_interop.sh").read_text()
        wanted: set[str] = set()
        for name in ("versions", "extra_build_versions", "source_only_versions"):
            m = re.search(rf"^{name}=\(([^)]*)\)", text, re.M)
            self.assertIsNotNone(m, name)
            wanted.update(m.group(1).split())
        self.assertLessEqual(wanted, _pinned_versions())

    def test_every_version_a_workflow_extracts_is_pinned(self) -> None:
        wanted = set()
        for wf in (REPO / ".github").rglob("*.yml"):
            text = wf.read_text()
            for block in re.findall(r"uses: \./\.github/actions/fetch-upstream-rsync\n"
                                    r"(?:\s+with:\n(?:\s+\S+:.*\n)+)?", text):
                for m in re.finditer(r"versions?: '?([0-9. ]+)'?$", block, re.M):
                    wanted.update(m.group(1).split())
        self.assertIn("3.5.0", wanted, "the scan found no literal versions")
        self.assertLessEqual(wanted, _pinned_versions())

    def test_no_ci_site_downloads_a_tarball_itself(self) -> None:
        # The regression this guards: a new job or script reintroducing
        # `curl .../rsync-X.tar.gz | tar xz`, i.e. an unverified per-run fetch.
        offenders = []
        roots = [REPO / ".github", REPO / "tools" / "ci", REPO / "scripts"]
        for root in roots:
            for path in root.rglob("*"):
                if not path.is_file() or path == FETCH:
                    continue
                if path.suffix not in {".yml", ".yaml", ".sh", ".py"}:
                    continue
                for n, line in enumerate(path.read_text(errors="replace").splitlines(), 1):
                    if re.search(r"(curl|wget)\b.*rsync-.*\.tar\.gz", line) or \
                            re.search(r"samba\.org/(pub|ftp)/rsync/src", line):
                        offenders.append(f"{path.relative_to(REPO)}:{n}: {line.strip()}")
        self.assertEqual(offenders, [], "route these through tools/ci/fetch_upstream_rsync.sh")


if __name__ == "__main__":
    unittest.main()
