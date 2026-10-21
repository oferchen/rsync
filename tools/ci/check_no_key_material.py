#!/usr/bin/env python3
"""Fail when a private key or certificate is checked into the repository.

Every key and certificate the tests use is generated at run time, fresh per
test, into that test's tempdir. Static key material in the tree goes stale,
leaks a key anyone can reuse, and lets a validity-window test pass or fail on
the calendar instead of on the code. This gate rejects three shapes:

  * a tracked file whose extension names key or certificate material;
  * a PEM block with a body - a `-----BEGIN ...-----` header followed by
    base64, either on the next line or after an escaped `\\n` in a string
    literal;
  * a header-less base64 DER blob (`MII...`), the encoding of every ASN.1
    SEQUENCE long enough to be a key or certificate.

PEM encoders that build the header around a generated body put a format
placeholder or a closing quote after the header, never base64, so they are not
matched. Deliberately malformed inputs that test parse errors (a short `AAAA`
body) are allowed only in the files named in MALFORMED_INPUT_FILES, and even
there a body of BODY_MIN or more characters is still reported.

Usage:
    python3 tools/ci/check_no_key_material.py      # exit 1 on any finding
"""
from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

KEY_EXTENSIONS = frozenset(
    {".pem", ".key", ".crt", ".cer", ".der", ".csr", ".p7b", ".p8", ".p12", ".pfx", ".jks"}
)
# Exact repo-relative paths whose tests feed malformed PEM to a parser. Every
# entry needs a reason; nothing else may hold a PEM body of any length.
MALFORMED_INPUT_FILES: dict[str, str] = {
    "crates/rsync_io/src/quic/trust.rs": "garbage-key parse-error test (`AAAA` body)",
}
# Real PEM bodies wrap at 64 characters; 40 leaves room for a short last line
# of a long block while staying far above any malformed-input fixture.
BODY_MIN = 40
PEM_HEADER = rb"-----BEGIN [A-Z0-9 ]+-----(?:\r?\n|\\n)\s*"
PEM_BLOCK = re.compile(PEM_HEADER + rb"[A-Za-z0-9+/]{4,}")
REAL_PEM_BLOCK = re.compile(PEM_HEADER + rb"[A-Za-z0-9+/]{%d,}" % BODY_MIN)
BARE_DER = re.compile(rb"MII[A-Za-z0-9+/]{%d,}" % BODY_MIN)


def tracked_files(root: Path) -> list[str]:
    out = subprocess.run(
        ["git", "ls-files", "-z"], cwd=root, check=True, capture_output=True
    ).stdout
    return [p for p in out.decode().split("\0") if p]


def findings(root: Path, paths: list[str]) -> list[str]:
    found = []
    for rel in paths:
        if Path(rel).suffix.lower() in KEY_EXTENSIONS:
            found.append(f"{rel}: key/certificate file extension")
            continue
        path = root / rel
        if not path.is_file():
            continue
        data = path.read_bytes()
        pem = REAL_PEM_BLOCK if rel in MALFORMED_INPUT_FILES else PEM_BLOCK
        for pattern, what in ((pem, "PEM block"), (BARE_DER, "base64 DER blob")):
            match = pattern.search(data)
            if match:
                line = data.count(b"\n", 0, match.start()) + 1
                found.append(f"{rel}:{line}: embedded {what}")
    return found


def main() -> int:
    root = Path(__file__).resolve().parents[2]
    found = findings(root, tracked_files(root))
    for finding in found:
        print(f"::error::{finding}")
    if found:
        print(
            "Generate keys and certificates at test run time (rcgen) into the "
            "test's tempdir; never check them in.",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
