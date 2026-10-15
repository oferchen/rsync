#!/usr/bin/env python3
"""Prints the protocol a client and an rsync daemon negotiate, read off the wire.

Relays one client connection to the daemon on --port and decodes the first
bytes each way, as the oracle cells do (interop_oracle.py DaemonTap and
decode_handshake). Every `@PORT@` in the client command is replaced by the
relay's port. Prints the negotiated protocol, or `unknown` when no handshake
was seen, and exits with the client's exit code.
"""

from __future__ import annotations

import argparse
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from interop_oracle import DaemonTap  # noqa: E402


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--port", type=int, required=True, help="daemon port to relay to")
    ap.add_argument("--timeout", type=float, default=60.0)
    ap.add_argument("client", nargs=argparse.REMAINDER, help="client command after --")
    a = ap.parse_args(argv)
    client = a.client[1:] if a.client[:1] == ["--"] else a.client
    if not client:
        ap.error("missing client command")
    tap = DaemonTap(a.port, a.timeout)
    cmd = [arg.replace("@PORT@", str(tap.port)) for arg in client]
    try:
        rc = subprocess.run(cmd, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                            stderr=subprocess.DEVNULL, timeout=a.timeout).returncode
    except subprocess.TimeoutExpired:
        rc = 124
    proto, _ = tap.handshake()
    print("unknown" if proto is None else proto)
    return rc


if __name__ == "__main__":
    sys.exit(main())
