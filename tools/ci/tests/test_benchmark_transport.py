#!/usr/bin/env python3
"""Regression tests for the daemon transport (TCP vs QUIC) benchmark.

What these guard, and why each matters:

1. The negotiated incremental-recursion mode is read from the daemon's compat
   flags on the wire. Those flags are an rsync varint, and any client that
   sends the `v` capability gets CF_VARINT_FLIST_FLAGS (0x80) back, which
   pushes the value into a two-byte encoding whose *second* byte carries bit
   0. A parser that tested the first byte would report every such cell as
   "no inc-recurse". The varint tests round-trip upstream's own encoder.

2. A QUIC stream is encrypted, so a QUIC cell's negotiated mode is not a
   measurement. The report must say so rather than print a yes/no it never
   observed.

3. AES-GCM and ChaCha20-Poly1305 are mandatory rows, and every transport runs
   in both incremental-recursion modes. Dropping either silently would
   publish a transport comparison missing the half it was asked for.

4. The report prints one value per column, and the chart plots the QUIC rows
   against oc-rsync's own TCP daemon, not against upstream.
"""

from __future__ import annotations

import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[3]
SCRIPTS = REPO_ROOT / ".github" / "scripts"
sys.path.insert(0, str(SCRIPTS))


def load_script(name: str):
    spec = importlib.util.spec_from_file_location(name, SCRIPTS / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


bt = load_script("benchmark_transport")
chart = load_script("benchmark_chart")


def write_varint(x: int) -> bytes:
    """upstream io.c:write_varint(), transcribed."""
    b = bytearray(1) + bytearray(x.to_bytes(4, "little"))
    cnt = 4
    while cnt > 1 and b[cnt] == 0:
        cnt -= 1
    bit = 1 << (7 - cnt + 1)
    if b[cnt] >= bit:
        cnt += 1
        b[0] = ~(bit - 1) & 0xFF
    elif cnt > 1:
        b[0] = b[cnt] | (~(bit * 2 - 1) & 0xFF)
    else:
        b[0] = b[cnt]
    return bytes(b[:cnt])


class VarintTests(unittest.TestCase):
    def test_round_trips_upstream_encoder(self):
        for value in (0, 1, 0x7F, 0x80, 0x81, 0x1FF, 0x3FFF, 0x4000, 0x02000081, 0x7FFFFFFF):
            with self.subTest(value=hex(value)):
                self.assertEqual(bt.read_varint(write_varint(value)), value)

    def test_inc_recurse_bit_lives_in_the_second_byte_once_flags_pass_0x7f(self):
        encoded = write_varint(0x81)
        self.assertEqual(len(encoded), 2)
        self.assertEqual(encoded[0] & bt.CF_INC_RECURSE, 0, "first byte alone would say no")
        self.assertTrue(bt.read_varint(encoded) & bt.CF_INC_RECURSE)

    def test_truncated_varint_is_unknown_not_zero(self):
        self.assertIsNone(bt.read_varint(write_varint(0x81)[:1]))
        self.assertIsNone(bt.read_varint(b""))


class WireProbeTests(unittest.TestCase):
    GREETING = b"@RSYNCD: 32.0 sha512 sha256 md5 md4\n"

    def test_flags_follow_the_ok_line(self):
        stream = self.GREETING + b"motd line\n@RSYNCD: OK\n" + write_varint(0x1FE) + b"\x00" * 4
        flags = bt.server_compat_flags(stream)
        self.assertEqual(flags, 0x1FE)
        self.assertFalse(flags & bt.CF_INC_RECURSE)

    def test_inc_recurse_set(self):
        stream = self.GREETING + b"@RSYNCD: OK\n" + write_varint(0x1FF)
        self.assertTrue(bt.server_compat_flags(stream) & bt.CF_INC_RECURSE)

    def test_refused_module_has_no_flags(self):
        self.assertIsNone(bt.server_compat_flags(self.GREETING + b"@ERROR: Unknown module\n"))

    def test_client_capabilities(self):
        args = b"probe_src\n--server\x00--sender\x00-logDtpre.iLsfxCIvu\x00.\x00probe_src/\x00\x00"
        self.assertEqual(bt.client_capabilities(args), "iLsfxCIvu")
        self.assertIsNone(bt.client_capabilities(b"--server\x00-a\x00"))

    def test_protocol_bytes(self):
        out = "sent 1,234,567 bytes  received 89 bytes  2,469,312.00 bytes/sec\n"
        self.assertEqual(bt.protocol_bytes(out), (1234567, 89))
        self.assertIsNone(bt.protocol_bytes("no stats here"))


class MatrixTests(unittest.TestCase):
    def setUp(self):
        self.datasets = bt.PROFILES["ci"]["datasets"]
        self.cells = bt.cells(self.datasets)

    def test_both_ciphers_are_mandatory(self):
        keys = {tr.key for _, tr, *_ in self.cells}
        self.assertTrue({"oc_quic_aes", "oc_quic_chacha20"} <= keys)
        ciphers = {tr.cipher for _, tr, *_ in self.cells if tr.quic}
        self.assertEqual(ciphers, {"aes", "chacha20"})

    def test_every_transport_runs_every_mode(self):
        for ds in self.datasets:
            for tr in bt.TRANSPORTS:
                combos = {
                    (d, s, i)
                    for c_ds, c_tr, d, s, i in self.cells
                    if c_ds is ds and c_tr is tr
                }
                self.assertEqual(
                    combos,
                    {
                        (d, s, i)
                        for d in bt.DIRECTIONS
                        for s in bt.SCENARIOS
                        for i in bt.INC_MODES
                    },
                    f"{ds.key}/{tr.key}",
                )

    def test_cc_variants_are_bounded(self):
        cc = [c for c in self.cells if c[1].cc]
        self.assertEqual(len(cc), len(bt.CC_VARIANTS) * len(bt.DIRECTIONS))
        self.assertTrue(all(c[0].key == "large" and c[3] == "initial" for c in cc))

    def test_no_inc_recursive_flag_reaches_the_client(self):
        tr = bt.TRANSPORTS[2]

        class D:
            port, quic_port = 1, 2

        cert = {"cert": "/c.pem"}
        cmd = bt.client_cmd("oc", tr, D, "pull", "no-inc-recursive", "/dst", "src", cert)
        self.assertIn("--no-inc-recursive", cmd)
        self.assertEqual(cmd[cmd.index("--quic-cipher") + 1], "aes")
        self.assertEqual(cmd[-2:], ["quic://127.0.0.1:2/src/", "/dst/"])
        default = bt.client_cmd("oc", tr, D, "push", "default", "/src", "dest", cert)
        self.assertNotIn("--no-inc-recursive", default)
        self.assertEqual(default[-1], "quic://127.0.0.1:2/dest/")


class SummaryTests(unittest.TestCase):
    def run_(self, wall, exit_=0):
        return {
            "wall": wall, "user": 0.5, "sys": 0.25, "rss_kb": 1000 + int(wall),
            "exit": exit_, "stdout": "sent 10 bytes  received 20 bytes\n",
            "stderr": "boom" if exit_ else "",
        }

    def test_min_median_and_per_run_server(self):
        cell = bt.summarize(
            [self.run_(3.0), self.run_(1.0), self.run_(2.0)],
            {"user": 3.0, "sys": 1.5, "rss_kb": 4096},
            4 * bt.MIB,
        )
        self.assertEqual(cell["wall_min"], 1.0)
        self.assertEqual(cell["wall_median"], 2.0)
        self.assertEqual(cell["server_user"], 1.0)
        self.assertEqual(cell["server_sys"], 0.5)
        self.assertEqual(cell["client_rss_kb"], 1003)
        self.assertEqual(cell["corpus_mibps"], 2.0)
        self.assertEqual((cell["protocol_sent"], cell["protocol_received"]), (10, 20))
        self.assertNotIn("failures", cell)

    def test_failure_is_counted_with_its_stderr(self):
        cell = bt.summarize(
            [self.run_(1.0), self.run_(1.0, exit_=5)],
            {"user": 0, "sys": 0, "rss_kb": 0},
            1,
        )
        self.assertEqual(cell["failures"], 1)
        self.assertEqual(cell["stderr"], "boom")


class ResidentChildTests(unittest.TestCase):
    """The QUIC listener is a daemon child that is never reaped while the
    daemon runs. Waiting for it to go away stalled every QUIC cell for the
    full timeout, and leaving it out of the server columns hid the QUIC
    server's cost entirely."""

    @unittest.skipUnless(os.path.exists("/proc/self/task"), "needs Linux /proc")
    def test_resident_child_is_not_waited_for_but_is_counted(self):
        parent = subprocess.Popen(
            [sys.executable, "-c",
             "import subprocess,sys,time;"
             "subprocess.Popen([sys.executable,'-c','import time;time.sleep(30)']);"
             "time.sleep(30)"],
        )
        try:
            deadline = bt.time.monotonic() + 10
            while not bt.live_children(parent.pid) and bt.time.monotonic() < deadline:
                bt.time.sleep(0.05)
            resident = frozenset(bt.live_children(parent.pid))
            self.assertEqual(len(resident), 1)
            start = bt.time.monotonic()
            bt.wait_reaped(parent.pid, resident, timeout=5)
            self.assertLess(bt.time.monotonic() - start, 1.0)
            self.assertGreater(bt.peak_rss_kb(next(iter(resident))), 0)
        finally:
            for pid in bt.live_children(parent.pid):
                os.kill(pid, 9)
            parent.kill()
            parent.wait()


class StaleTests(unittest.TestCase):
    def test_incremental_touches_only_the_stale_files(self):
        ds = bt.Dataset("t", "t", 40, 4096, 4, 4, 256)
        with tempfile.TemporaryDirectory() as tmp:
            root = os.path.join(tmp, "tree")
            bt.make_dataset(ds, root)
            before = {}
            for dirpath, _, names in os.walk(root):
                for n in names:
                    p = os.path.join(dirpath, n)
                    before[p] = Path(p).read_bytes()
            self.assertEqual(len(before), 40)
            stale = set(bt.stale_paths(ds, root))
            self.assertEqual(len(stale), 4)
            bt.make_stale(ds, root)
            for p, data in before.items():
                after = Path(p).read_bytes()
                self.assertEqual(len(after), 4096)
                self.assertEqual(after != data, p in stale, p)


def transport_fixture():
    base = {
        "runs": 3, "wall_min": 1.0, "wall_median": 1.5, "corpus_mibps": 100.0,
        "client_user": 0.4, "client_sys": 0.2, "server_user": 0.3, "server_sys": 0.1,
        "client_rss_kb": 2048, "server_rss_kb": 4096,
        "protocol_sent": 111, "protocol_received": 222,
    }
    cells = []
    for tr, neg in (
        ("upstream_tcp", True),
        ("oc_tcp", False),
        ("oc_quic_aes", None),
        ("oc_quic_chacha20", None),
    ):
        for inc_mode in ("default", "no-inc-recursive"):
            cell = dict(base)
            cell.update({
                "dataset": "large", "transport": tr, "direction": "pull",
                "scenario": "initial", "inc_mode": inc_mode,
                "inc_recurse": {"negotiated": neg},
            })
            if tr == "oc_quic_aes":
                cell["wall_median"] = 3.0
            cells.append(cell)
    cells[-1]["failures"] = 1
    return {
        "upstream": {"label": "3.5.1"},
        "certificate": {"key_algorithm": "ECDSA P-256", "signature_algorithm": "ecdsa-with-SHA256"},
        "client_flags": "-a --info=stats1",
        "datasets": {"large": {"label": "4 x 1 GiB", "files": 4}},
        "cells": cells,
        "handshake": [{
            "transport": "oc_quic_aes", "runs": 10, "wall_median_ms": 12.5,
            "client_cpu_ms": 8.0, "server_cpu_ms": 3.0, "failures": 0,
        }],
        "elapsed_s": 42.0,
    }


class ReportTests(unittest.TestCase):
    def render(self, transport):
        results = {
            "upstream_version": "3.5.0",
            "test_data": {"size_mb": 1, "files": 1},
            "summary": {"by_mode": {}, "avg_ratio": 1, "best_ratio": 1, "worst_ratio": 1},
            "tests": [],
        }
        with tempfile.TemporaryDirectory() as tmp:
            Path(tmp, "benchmark_results.json").write_text(json.dumps(results))
            if transport is not None:
                Path(tmp, "benchmark_transport.json").write_text(
                    json.dumps({"transport": transport})
                )
            proc = subprocess.run(
                [sys.executable, str(SCRIPTS / "benchmark_report.py")],
                cwd=tmp, capture_output=True, text=True,
            )
        self.assertEqual(proc.returncode, 0, proc.stderr)
        return proc.stdout

    def table_rows(self, out):
        lines = out.splitlines()
        start = lines.index(next(l for l in lines if l.startswith("| Direction")))
        header = lines[start]
        rows = []
        for line in lines[start + 2 :]:
            if not line.startswith("|"):
                break
            rows.append(line)
        return header, rows

    def test_section_absent_without_transport_results(self):
        self.assertNotIn("TCP vs QUIC", self.render(None))

    def test_one_value_per_column(self):
        out = self.render(transport_fixture())
        header, rows = self.table_rows(out)
        width = header.count("|")
        self.assertEqual(len(rows), 8)
        for row in rows:
            self.assertEqual(row.count("|"), width, row)

    def test_quic_negotiated_mode_is_not_claimed(self):
        _, rows = self.table_rows(self.render(transport_fixture()))
        quic = [r for r in rows if "QUIC aes" in r]
        self.assertTrue(quic)
        for row in quic:
            self.assertIn("not observable (TLS); TCP twin no", row)
        upstream = next(r for r in rows if "upstream rsync 3.5.1 TCP" in r)
        self.assertIn("| yes |", upstream)

    def test_failures_and_handshake_are_reported(self):
        out = self.render(transport_fixture())
        self.assertIn("(failed 1/3)", out)
        self.assertIn("| oc-rsync QUIC aes | 10 | 12.50 | 8.00 | 3.00 |", out)
        self.assertIn("ECDSA P-256", out)
        self.assertIn("upstream rsync 3.5.1 TCP", out)


class ChartTests(unittest.TestCase):
    def test_rows_compare_quic_to_oc_tcp(self):
        rows = chart.transport_rows(transport_fixture())
        self.assertEqual(len(rows), 1, "only the default inc-recurse mode is charted")
        row = rows[0]
        self.assertEqual(
            list(row["bars"]), ["upstream_tcp", "oc_tcp", "oc_quic_aes", "oc_quic_chacha20"]
        )
        ratios = dict(row["annotations"])
        self.assertAlmostEqual(ratios["quic/tcp"], 2.0)
        self.assertAlmostEqual(ratios["oc/up"], 1.0)

    def test_failed_cell_is_not_plotted(self):
        transport = transport_fixture()
        for cell in transport["cells"]:
            if cell["transport"] == "oc_quic_chacha20":
                cell["failures"] = 1
        self.assertNotIn("oc_quic_chacha20", chart.transport_rows(transport)[0]["bars"])

    def test_svg_carries_the_group_and_legend(self):
        svg = chart.generate_chart({"tests": [], "transport": transport_fixture()})
        self.assertIn("Daemon Transport: TCP vs QUIC", svg)
        self.assertIn("oc-rsync (QUIC, ChaCha20)", svg)
        self.assertIn("4 x 1 GiB pull initial", svg)


if __name__ == "__main__":
    unittest.main()
