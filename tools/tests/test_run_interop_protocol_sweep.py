"""Tests for the forced-protocol sweep in tools/ci/run_interop.sh.

The sweep forces each protocol oc speaks against every extended-matrix
release. 33 is forced explicitly rather than only reached as the native
default, and every leg asserts what the session negotiated: a forced value the
peer silently ignores would otherwise pass as if it had been tested.
"""

from __future__ import annotations

import re
import socket
import subprocess
import sys
import threading
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
SCRIPT = REPO / "tools" / "ci" / "run_interop.sh"
PROBE = REPO / "tools" / "ci" / "interop_wire_probe.py"


def _function_source(name: str) -> str:
    match = re.search(rf"^{name}\(\) \{{\n.*?^\}}\n", SCRIPT.read_text(), re.M | re.S)
    if match is None:
        raise AssertionError(f"{name} not found in {SCRIPT}")
    return match.group(0)


class SweepListTests(unittest.TestCase):
    def test_sweep_forces_every_protocol_oc_speaks(self) -> None:
        match = re.search(r"^  protos=\(([^)]*)\)$", SCRIPT.read_text(), re.M)
        self.assertIsNotNone(match, "sweep protocol list not found")
        self.assertEqual(match.group(1).split(), [str(p) for p in range(28, 34)])

    def test_negotiated_protocol_is_min_of_forced_and_native(self) -> None:
        # A release's own client also refuses a --protocol above its native
        # one, so the same value is what the upstream client gets forced to.
        natives = {"3.4.4": 32, "3.5.0": 32, "3.5.1": 33}
        calls = "\n".join(f"clamped_protocol {v} {p}"
                          for v in natives for p in range(28, 34))
        script = _function_source("native_protocol") + _function_source("clamped_protocol")
        out = subprocess.run(["bash", "-c", f"{script}\n{calls}"], capture_output=True,
                             text=True, check=True).stdout.split()
        want = [str(min(p, n)) for n in natives.values() for p in range(28, 34)]
        self.assertEqual(out, want)


class CaseRunnerWiringTests(unittest.TestCase):
    BODY = _function_source("run_comprehensive_interop_case")

    def test_upstream_client_is_forced_no_higher_than_its_own_protocol(self) -> None:
        self.assertIn('up_protocol_flag="--protocol=${eff_proto}"', self.BODY)
        up_loop = self.BODY[self.BODY.index("Direction 1"):self.BODY.index("Direction 2")]
        self.assertIn('flags="$flags $up_protocol_flag"', up_loop)
        self.assertNotIn('flags="$flags $protocol_flag"', up_loop)

    def test_both_directions_assert_the_negotiated_protocol(self) -> None:
        up_dir = self.BODY[self.BODY.index("Direction 1"):self.BODY.index("Direction 2")]
        oc_dir = self.BODY[self.BODY.index("Direction 2"):self.BODY.index("Remote-shell")]
        self.assertIn('check_negotiated_protocol "$upstream_binary" "$up_protocol_flag"', up_dir)
        self.assertIn('check_negotiated_protocol "$oc_client" "$protocol_flag"', oc_dir)


def _fake_daemon(greeting: bytes) -> tuple[socket.socket, threading.Thread]:
    """A listener that answers one connection with `greeting` once the client spoke."""
    srv = socket.socket()
    srv.bind(("127.0.0.1", 0))
    srv.listen(1)

    def serve() -> None:
        conn, _ = srv.accept()
        with conn:
            conn.recv(64)
            conn.sendall(greeting)

    thread = threading.Thread(target=serve, daemon=True)
    thread.start()
    return srv, thread


CLIENT = """
import socket, sys
s = socket.create_connection(("127.0.0.1", int(sys.argv[1])))
s.sendall(sys.argv[2].encode())
s.recv(64)
"""


class WireProbeTests(unittest.TestCase):
    def _probe(self, server_greeting: bytes, client_greeting: str) -> str:
        srv, thread = _fake_daemon(server_greeting)
        try:
            out = subprocess.run(
                [sys.executable, str(PROBE), "--port", str(srv.getsockname()[1]), "--timeout",
                 "10", "--", sys.executable, "-c", CLIENT, "@PORT@", client_greeting],
                capture_output=True, text=True, timeout=30)
        finally:
            thread.join(timeout=10)
            srv.close()
        return out.stdout.strip()

    def test_reports_the_lower_of_the_two_announced_protocols(self) -> None:
        self.assertEqual(self._probe(b"@RSYNCD: 32.0\n", "@RSYNCD: 33.0\n"), "32")
        self.assertEqual(self._probe(b"@RSYNCD: 33.0\n", "@RSYNCD: 30.0\n"), "30")

    def test_reports_unknown_when_the_daemon_never_answers(self) -> None:
        self.assertEqual(self._probe(b"", "@RSYNCD: 33.0\n"), "unknown")


if __name__ == "__main__":
    unittest.main()
