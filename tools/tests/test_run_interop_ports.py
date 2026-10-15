"""Tests for daemon port allocation in tools/ci/run_interop.sh.

The parallel version workers once each called allocate_ephemeral_port inside
their own subshell. Each call bound port 0, read the port and released it, so
two workers could be handed the same port: the second oc daemon then failed to
bind and every test in that version failed with a socket error (exit 10).
These tests pin the fix: ports come from one allocation made before the fork,
and that allocation never repeats a port.
"""

from __future__ import annotations

import re
import subprocess
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
SCRIPT = REPO / "tools" / "ci" / "run_interop.sh"


def _function_source(name: str) -> str:
    text = SCRIPT.read_text()
    match = re.search(rf"^{name}\(\) \{{\n.*?^\}}\n", text, re.M | re.S)
    if match is None:
        raise AssertionError(f"{name} not found in {SCRIPT}")
    return match.group(0)


def _parallel_loop_body() -> str:
    text = SCRIPT.read_text()
    match = re.search(r"^# Run all version tests.*?^  \) &\n", text, re.M | re.S)
    if match is None:
        raise AssertionError("parallel version loop not found")
    return match.group(0)


class DistinctPortAllocationTests(unittest.TestCase):
    def test_every_allocated_port_is_distinct(self) -> None:
        count = 64
        script = _function_source("allocate_distinct_ephemeral_ports")
        result = subprocess.run(
            ["bash", "-c", f"{script}\nallocate_distinct_ephemeral_ports {count}"],
            capture_output=True, text=True, check=True,
        )
        ports = [int(line) for line in result.stdout.split()]
        self.assertEqual(len(ports), count)
        self.assertEqual(len(set(ports)), count, "a port was handed out twice")
        self.assertTrue(all(0 < p < 65536 for p in ports))

    def test_parallel_workers_do_not_allocate_their_own_ports(self) -> None:
        # Allocating inside the subshell reintroduces the collision: the
        # workers run concurrently and each releases its port before binding.
        body = _parallel_loop_body()
        subshell = body[body.index("  (\n"):]
        self.assertNotIn("allocate_ephemeral_port", subshell)
        self.assertIn("allocate_distinct_ephemeral_ports", body)


class DaemonPortPairTests(unittest.TestCase):
    """The forced-protocol sweep and the standalone leg each start an oc and
    an upstream daemon. Their ports must be distinct even when the kernel
    would hand the same free port to two consecutive bind-0 probes."""

    def _pair_with_reusing_allocator(self) -> tuple[int, int]:
        # Stub the single-port allocator to always return one port, the way
        # the kernel may after the probe socket is closed. The pair helper
        # must not depend on it.
        stub = "allocate_ephemeral_port() { echo 40000; }"
        script = "\n".join([
            stub,
            _function_source("allocate_distinct_ephemeral_ports"),
            _function_source("allocate_daemon_port_pair"),
            'allocate_daemon_port_pair; echo "$oc_port $up_port"',
        ])
        result = subprocess.run(["bash", "-c", script], capture_output=True,
                                text=True, check=True)
        oc, up = (int(v) for v in result.stdout.split())
        return oc, up

    def test_pair_is_distinct(self) -> None:
        oc, up = self._pair_with_reusing_allocator()
        self.assertNotEqual(oc, up)
        self.assertTrue(0 < oc < 65536 and 0 < up < 65536)

    def test_sweep_and_standalone_use_the_pair_helper(self) -> None:
        text = SCRIPT.read_text()
        sweep = text[text.index("for proto in \"${protos[@]}\"; do"):]
        sweep = sweep[:sweep.index("done")]
        self.assertIn("allocate_daemon_port_pair", sweep)
        self.assertNotIn("allocate_ephemeral_port", sweep)
        standalone = text[text.index('if [[ -x "$standalone_binary" ]]; then'):]
        standalone = standalone[:standalone.index("fi\n")]
        self.assertIn("allocate_daemon_port_pair", standalone)
        self.assertNotIn("allocate_ephemeral_port", standalone)


if __name__ == "__main__":
    unittest.main()
