"""Tests for the transfer-failure report in tools/ci/run_interop.sh.

A forced-protocol sweep leg once failed with exit 12 and left only the first
five lines of client stderr: the harness then deleted the daemon log, so the
cause was lost. The report must carry the whole client stderr, the daemon log
tail and the listeners on the daemon port.
"""
from __future__ import annotations

import re
import subprocess
import tempfile
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


class TransferFailureReportTests(unittest.TestCase):
    def _report(self, url: str) -> str:
        with tempfile.TemporaryDirectory() as tmp:
            err = Path(tmp) / "client.err"
            log = Path(tmp) / "daemon.log"
            err.write_text("".join(f"client line {i}\n" for i in range(1, 9)))
            log.write_text("".join(f"daemon line {i}\n" for i in range(1, 31)))
            script = _function_source("report_transfer_failure")
            result = subprocess.run(
                ["bash", "-c", f'{script}\nreport_transfer_failure "$1" "$2" "$3"',
                 "_", str(err), str(log), url],
                capture_output=True, text=True, check=True,
            )
            return result.stdout

    def test_whole_client_stderr_is_printed(self) -> None:
        out = self._report("rsync://127.0.0.1:40861/interop")
        # Line 8 is past the old five-line cut-off.
        for i in range(1, 9):
            self.assertIn(f"client line {i}\n", out)

    def test_daemon_log_tail_is_the_last_twenty_lines(self) -> None:
        out = self._report("rsync://127.0.0.1:40861/interop")
        self.assertIn("daemon line 30\n", out)
        self.assertIn("daemon line 11\n", out)
        self.assertNotIn("daemon line 10\n", out)

    def test_daemon_port_is_named(self) -> None:
        out = self._report("rsync://127.0.0.1:40861/interop")
        self.assertIn("listeners on port 40861:", out)

    def test_failure_paths_call_the_report(self) -> None:
        body = _function_source("comp_run_scenario")
        self.assertIn('report_transfer_failure "$transfer_log.err" "$log" "$url"', body)
        self.assertNotIn('head -5 "$transfer_log.err"', body)


if __name__ == "__main__":
    unittest.main()
