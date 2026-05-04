"""Unit tests for the SHA-256 helper script used by packaging automation."""

from __future__ import annotations

import hashlib
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from tools.compute_sha256 import compute_sha256


class ComputeSha256Tests(unittest.TestCase):
    def setUp(self) -> None:
        self._tempdir = tempfile.TemporaryDirectory()
        self.temp_path = Path(self._tempdir.name)

    def tearDown(self) -> None:
        self._tempdir.cleanup()

    def _write_file(self, name: str, data: bytes) -> Path:
        path = self.temp_path / name
        path.write_bytes(data)
        return path

    def test_compute_sha256_matches_hashlib_for_random_payload(self) -> None:
        payload = os.urandom(4096)
        path = self._write_file("payload.bin", payload)
        expected = hashlib.sha256(payload).hexdigest()
        self.assertEqual(expected, compute_sha256(path))

    def test_compute_sha256_supports_empty_files(self) -> None:
        path = self._write_file("empty.bin", b"")
        self.assertEqual(hashlib.sha256(b"").hexdigest(), compute_sha256(path))

    def test_compute_sha256_rejects_missing_file(self) -> None:
        missing = self.temp_path / "missing.bin"
        with self.assertRaises(FileNotFoundError):
            compute_sha256(missing)

    def test_compute_sha256_rejects_invalid_chunk_size(self) -> None:
        path = self._write_file("small.bin", b"test-data")
        with self.assertRaises(ValueError):
            compute_sha256(path, chunk_size=0)

    def test_cli_outputs_digest(self) -> None:
        payload = b"cli-digest-check"
        path = self._write_file("cli.bin", payload)
        expected = hashlib.sha256(payload).hexdigest()

        result = subprocess.run(
            [sys.executable, str(Path("tools/compute_sha256.py").resolve()), str(path)],
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )

        self.assertEqual("", result.stderr)
        self.assertEqual(f"{expected}\n", result.stdout)


if __name__ == "__main__":
    unittest.main()
