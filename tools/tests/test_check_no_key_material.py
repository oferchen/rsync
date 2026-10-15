"""Unit tests for the checked-in key/certificate gate.

The gate exists so a static key or certificate can never land in the tree, and
it must stay quiet on the two legitimate shapes that look similar: PEM encoders
that wrap a generated body, and short malformed inputs that test parse errors.
A gate that flags those gets an allowlist entry for every new test and stops
meaning anything, so both directions are pinned here.

Every PEM-shaped string below is assembled at run time from random bytes, so
this file holds no key material and does not trip the gate it tests.
"""
from __future__ import annotations

import base64
import os
import tempfile
import unittest
from pathlib import Path

from tools.ci.check_no_key_material import findings

BEGIN = "-----BEGIN " + "CERTIFICATE-----"
END = "-----END " + "CERTIFICATE-----"


def body() -> str:
    encoded = base64.b64encode(os.urandom(300)).decode()
    return "\n".join(encoded[i : i + 64] for i in range(0, len(encoded), 64))


class FindingsTests(unittest.TestCase):
    def setUp(self) -> None:
        self._tempdir = tempfile.TemporaryDirectory()
        self.root = Path(self._tempdir.name)

    def tearDown(self) -> None:
        self._tempdir.cleanup()

    def _scan(self, name: str, content: str | bytes) -> list[str]:
        path = self.root / name
        data = content.encode() if isinstance(content, str) else content
        path.write_bytes(data)
        return findings(self.root, [name])

    def test_key_extension_is_flagged_whatever_the_content(self) -> None:
        for name in ("server.pem", "client.KEY", "ca.crt", "leaf.der", "id.p12"):
            with self.subTest(name=name):
                self.assertEqual(len(self._scan(name, b"")), 1)

    def test_pem_file_content_is_flagged_under_any_extension(self) -> None:
        found = self._scan("fixture.txt", f"{BEGIN}\n{body()}\n{END}\n")
        self.assertEqual(found, ["fixture.txt:1: embedded PEM block"])

    def test_pem_literal_with_escaped_newline_is_flagged(self) -> None:
        literal = body().replace("\n", "\\n")
        src = f'const CERT: &str = "{BEGIN}\\n{literal}\\n{END}";\n'
        self.assertEqual(len(self._scan("lib.rs", "// x\n" + src)), 1)

    def test_bare_base64_der_is_flagged(self) -> None:
        blob = "MII" + base64.b64encode(os.urandom(60)).decode()
        self.assertIn("base64 DER blob", self._scan("a.rs", f'"{blob}"')[0])

    def test_pem_encoder_around_a_generated_body_passes(self) -> None:
        src = (
            f'let mut out = format!("-----BEGIN {{label}}-----\\n");\n'
            f'let pem = String::from("{BEGIN}\\n");\n'
            f'format!("{BEGIN}\\n{{body}}{END}\\n")\n'
        )
        self.assertEqual(self._scan("enc.rs", src), [])

    def test_malformed_parse_error_inputs_pass(self) -> None:
        src = (
            f'b"{BEGIN}\\n@@@ not base64 @@@\\n{END}\\n",\n'
            'b"-----BEGIN PRIVATE KEY-----\\nAAAA\\n-----END PRIVATE KEY-----\\n",\n'
        )
        self.assertEqual(self._scan("trust.rs", src), [])


if __name__ == "__main__":
    unittest.main()
