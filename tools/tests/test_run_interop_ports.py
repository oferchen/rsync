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


if __name__ == "__main__":
    unittest.main()
