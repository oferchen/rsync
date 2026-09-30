#!/usr/bin/env python3
"""Daemon transport benchmark: rsync over TCP against oc-rsync over QUIC.

Every cell is one client talking to one daemon on loopback:

- `upstream_tcp`      upstream rsync client -> upstream rsync daemon (TCP)
- `oc_tcp`            oc-rsync client -> oc-rsync daemon (TCP)
- `oc_quic_aes`       oc-rsync client -> oc-rsync daemon (QUIC, AES-GCM)
- `oc_quic_chacha20`  oc-rsync client -> oc-rsync daemon (QUIC, ChaCha20-Poly1305)

Upstream rsync has no QUIC transport, so the two TCP rows are the reference
points a QUIC row is read against, not contestants in the same race. Both oc
rows use the same `quic`-feature binary, so the TCP/QUIC difference is the
transport and nothing else.

Each transport is crossed with push and pull, an initial copy and an
incremental re-sync, and both incremental-recursion modes: the default (the
peers negotiate INC_RECURSE when both allow it) and `--no-inc-recursive`.
Requesting a mode is not the same as getting it, so every TCP cell also runs
an untimed probe through a loopback relay that reads the compat flags the
daemon actually sent (upstream compat.c:setup_protocol() writes them raw,
right after `@RSYNCD: OK`, before multiplexing starts). A QUIC stream is
encrypted, so a QUIC cell's negotiated mode cannot be read off the wire; the
report says so instead of guessing.

The daemon is started fresh for every cell. That is what makes the server
columns per-cell figures: CPU is the delta of the daemon's own and reaped
children's CPU over the timed runs (`/proc/<pid>/stat` utime, stime, cutime,
cstime), and peak RSS is `wait4()`'s maxrss for the daemon and every session
it reaped. oc-rsync's QUIC listener is a child that lives as long as the
daemon and is never reaped while it runs, so its CPU and `VmHWM` are read
from `/proc` directly and folded into the same columns.

Linux only: `/proc` supplies the server figures and the QUIC daemon listener
is Unix-only.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import signal
import socket
import statistics
import subprocess
import sys
import tempfile
import threading
import time
from dataclasses import dataclass

MIB = 1024 * 1024
GIB = 1024 * MIB
HOST = "127.0.0.1"

# upstream compat.c: CF_INC_RECURSE is bit 0 of the negotiated compat flags.
CF_INC_RECURSE = 0x01

# upstream io.c: int_byte_extra[], indexed by the first varint byte / 4.
INT_BYTE_EXTRA = (
    [0] * 32
    + [1] * 16
    + [2, 2, 2, 2, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 5, 6]
)


@dataclass(frozen=True)
class Dataset:
    """A generated source tree: `files` files of `file_size` bytes each."""

    key: str
    label: str
    files: int
    file_size: int
    dirs: int
    # Files touched before each incremental run, and how many bytes of each.
    stale_files: int
    stale_bytes: int

    @property
    def total_bytes(self) -> int:
        return self.files * self.file_size


@dataclass(frozen=True)
class Transport:
    """One client/daemon pairing."""

    key: str
    label: str
    impl: str  # "upstream" or "oc"
    quic: bool = False
    cipher: str | None = None
    cc: str | None = None


TRANSPORTS = (
    Transport("upstream_tcp", "upstream rsync TCP", "upstream"),
    Transport("oc_tcp", "oc-rsync TCP", "oc"),
    Transport("oc_quic_aes", "oc-rsync QUIC aes", "oc", True, "aes"),
    Transport("oc_quic_chacha20", "oc-rsync QUIC chacha20", "oc", True, "chacha20"),
)

# Congestion control only changes pacing, and on loopback there is no loss to
# react to, so the variants run once per direction (initial copy, default
# incremental mode, few-large-files set) rather than across the whole matrix.
# BBR is the default, so `oc_quic_aes` already is the BBR row.
CC_VARIANTS = (
    Transport("oc_quic_aes_cubic", "oc-rsync QUIC aes cubic", "oc", True, "aes", "cubic"),
    Transport("oc_quic_aes_newreno", "oc-rsync QUIC aes newreno", "oc", True, "aes", "newreno"),
)

DIRECTIONS = ("push", "pull")
SCENARIOS = ("initial", "incremental")
INC_MODES = ("default", "no-inc-recursive")

PROFILES = {
    # Sized for a hosted CI runner: a few minutes, not an hour.
    "ci": {
        "runs": 3,
        "timeout": 600,
        "datasets": (
            Dataset("large", "2 x 256 MiB", 2, 256 * MIB, 1, 2, 64 * 1024),
            Dataset("small", "20K x 1 KiB", 20_000, 1024, 100, 200, 512),
        ),
    },
    # The full-size run for manual dispatch and host measurements.
    "full": {
        "runs": 3,
        "timeout": 3600,
        "datasets": (
            Dataset("large", "4 x 1 GiB", 4, GIB, 1, 4, 64 * 1024),
            Dataset("small", "1M x 1 KiB", 1_000_000, 1024, 1000, 10_000, 512),
        ),
    },
}


def cells(datasets):
    """Every (dataset, transport, direction, scenario, inc_mode) the run times."""
    out = []
    for ds in datasets:
        for tr in TRANSPORTS:
            for direction in DIRECTIONS:
                for inc_mode in INC_MODES:
                    for scenario in SCENARIOS:
                        out.append((ds, tr, direction, scenario, inc_mode))
        if ds.key == "large":
            for tr in CC_VARIANTS:
                for direction in DIRECTIONS:
                    out.append((ds, tr, direction, "initial", "default"))
    return out


# ---------------------------------------------------------------------------
# Wire parsing
# ---------------------------------------------------------------------------


def read_varint(buf: bytes) -> int | None:
    """Decode one rsync varint (upstream io.c:read_varint()) from `buf`."""
    if not buf:
        return None
    ch = buf[0]
    extra = INT_BYTE_EXTRA[ch // 4]
    if not extra:
        return ch
    if len(buf) < 1 + extra:
        return None
    bit = 1 << (8 - extra)
    raw = bytearray(buf[1 : 1 + extra]) + bytes([ch & (bit - 1)])
    raw += bytes(max(0, 4 - len(raw)))
    return int.from_bytes(raw[:4], "little")


def server_compat_flags(server_stream: bytes) -> int | None:
    """The compat flags a daemon sent, read from its side of the TCP stream."""
    marker = b"@RSYNCD: OK\n"
    at = server_stream.find(marker)
    if at < 0:
        return None
    return read_varint(server_stream[at + len(marker) :])


def client_capabilities(client_stream: bytes) -> str | None:
    """The `-e.<caps>` capability letters a client sent in its daemon args."""
    match = re.search(rb"e\.([A-Za-z]*)", client_stream)
    return match.group(1).decode() if match else None


STATS_RE = re.compile(r"sent ([\d,]+) bytes\s+received ([\d,]+) bytes")


def protocol_bytes(stdout: str) -> tuple[int, int] | None:
    """`(sent, received)` from the client's closing stats line."""
    match = STATS_RE.search(stdout)
    if not match:
        return None
    return tuple(int(v.replace(",", "")) for v in match.groups())


# ---------------------------------------------------------------------------
# Measurement
# ---------------------------------------------------------------------------

CLK_TCK = os.sysconf("SC_CLK_TCK") if hasattr(os, "sysconf") else 100


def proc_cpu_split(pid: int) -> tuple[float, float]:
    """`(user, sys)` seconds of `pid` including reaped children."""
    with open(f"/proc/{pid}/stat") as f:
        stat = f.read()
    fields = stat[stat.rindex(")") + 2 :].split()
    utime, stime, cutime, cstime = (int(v) for v in fields[11:15])
    return (utime + cutime) / CLK_TCK, (stime + cstime) / CLK_TCK


def live_children(pid: int) -> list[int]:
    """Children of `pid` not yet reaped (zombies included)."""
    kids = []
    try:
        tasks = os.listdir(f"/proc/{pid}/task")
    except OSError:
        return kids
    for tid in tasks:
        try:
            with open(f"/proc/{pid}/task/{tid}/children") as f:
                kids += [int(k) for k in f.read().split()]
        except OSError:
            continue
    return kids


def peak_rss_kb(pid: int) -> int:
    """`VmHWM` of a live process, in KiB (0 if it is gone)."""
    try:
        with open(f"/proc/{pid}/status") as f:
            for line in f:
                if line.startswith("VmHWM:"):
                    return int(line.split()[1])
    except OSError:
        pass
    return 0


def wait_reaped(pid: int, resident=frozenset(), timeout: float = 30.0) -> None:
    """Block until the daemon has reaped every session it forked.

    A session child the daemon has not reaped yet is missing from both the
    `cutime`/`cstime` delta and the final `wait4()` maxrss, so reading either
    before the reap would under-report the server. `resident` children live
    as long as the daemon (the QUIC listener) and are not waited for.
    """
    deadline = time.monotonic() + timeout
    while set(live_children(pid)) - resident and time.monotonic() < deadline:
        time.sleep(0.05)


def timed_run(cmd, env, timeout):
    """Run `cmd` once; wall, CPU and peak RSS from `wait4()`."""
    with tempfile.TemporaryFile() as out, tempfile.TemporaryFile() as err:
        start = time.perf_counter()
        proc = subprocess.Popen(cmd, stdout=out, stderr=err, env=env)
        killer = threading.Timer(timeout, proc.kill)
        killer.start()
        _, status, ru = os.wait4(proc.pid, 0)
        wall = time.perf_counter() - start
        killer.cancel()
        proc.returncode = os.waitstatus_to_exitcode(status)
        out.seek(0)
        err.seek(0)
        stdout = out.read().decode(errors="replace")
        stderr = err.read().decode(errors="replace")
    return {
        "wall": wall,
        "user": ru.ru_utime,
        "sys": ru.ru_stime,
        "rss_kb": ru.ru_maxrss,
        "exit": proc.returncode,
        "stdout": stdout,
        "stderr": stderr,
    }


def summarize(runs, server, corpus_bytes):
    """Collapse per-run samples into the published cell figures."""
    walls = [r["wall"] for r in runs]
    median = statistics.median(walls)
    failures = sum(1 for r in runs if r["exit"] != 0)
    wire = [protocol_bytes(r["stdout"]) for r in runs]
    wire = [w for w in wire if w]
    cell = {
        "runs": len(runs),
        "wall_min": round(min(walls), 3),
        "wall_median": round(median, 3),
        "client_user": round(statistics.median(r["user"] for r in runs), 3),
        "client_sys": round(statistics.median(r["sys"] for r in runs), 3),
        "client_rss_kb": max(r["rss_kb"] for r in runs),
        "server_user": round(server["user"] / len(runs), 3),
        "server_sys": round(server["sys"] / len(runs), 3),
        "server_rss_kb": server["rss_kb"],
        "corpus_mibps": round(corpus_bytes / median / MIB, 1) if median > 0 else 0.0,
    }
    if wire:
        cell["protocol_sent"], cell["protocol_received"] = wire[-1]
    if failures:
        cell["failures"] = failures
        cell["stderr"] = next(r["stderr"] for r in runs if r["exit"] != 0)[-400:]
    return cell


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


def remove_tree(path: str) -> None:
    """Delete a scratch tree. Argument list, no shell, a fixed path."""
    subprocess.run(["rm", "-rf", "--", path], check=True)


def fresh_dir(path: str) -> None:
    remove_tree(path)
    os.makedirs(path)


def make_dataset(ds: Dataset, root: str) -> None:
    """Write `ds` under `root`, random bytes so compression cannot flatter it."""
    os.makedirs(root)
    per_dir = -(-ds.files // ds.dirs)
    chunk = min(ds.file_size, 4 * MIB)
    for i in range(ds.files):
        d = os.path.join(root, f"d{i // per_dir:04d}")
        if i % per_dir == 0:
            os.makedirs(d, exist_ok=True)
        with open(os.path.join(d, f"f{i:07d}"), "wb") as f:
            left = ds.file_size
            while left:
                n = min(chunk, left)
                f.write(os.urandom(n))
                left -= n


def stale_paths(ds: Dataset, root: str) -> list[str]:
    """The files an incremental run finds changed, spread across the tree."""
    per_dir = -(-ds.files // ds.dirs)
    step = max(1, ds.files // ds.stale_files)
    return [
        os.path.join(root, f"d{i // per_dir:04d}", f"f{i:07d}")
        for i in range(0, ds.files, step)
    ][: ds.stale_files]


def make_stale(ds: Dataset, root: str) -> None:
    """Overwrite `stale_bytes` mid-file in each stale file of the copy.

    Writing on the destination also moves its mtime, so the quick check sees
    a difference and the next run is a real delta transfer rather than a
    no-change scan.
    """
    for path in stale_paths(ds, root):
        with open(path, "r+b") as f:
            f.seek(max(0, ds.file_size // 2 - ds.stale_bytes // 2))
            f.write(os.urandom(ds.stale_bytes))


def make_cert(workdir: str) -> dict:
    """A throwaway ECDSA P-256 end-entity certificate for the QUIC daemon.

    Generated per run and deleted with the scratch directory; no key material
    is ever committed. `CA:FALSE` matters: webpki refuses a certificate that
    claims to be a CA as the server's end-entity certificate.
    """
    cert = os.path.join(workdir, "quic-cert.pem")
    key = os.path.join(workdir, "quic-key.pem")
    subprocess.run(
        [
            "openssl", "req", "-x509", "-newkey", "ec",
            "-pkeyopt", "ec_paramgen_curve:prime256v1", "-nodes",
            "-keyout", key, "-out", cert, "-days", "2", "-subj", "/CN=localhost",
            "-addext", "subjectAltName=DNS:localhost,IP:127.0.0.1",
            "-addext", "basicConstraints=critical,CA:FALSE",
            "-addext", "keyUsage=critical,digitalSignature",
            "-addext", "extendedKeyUsage=serverAuth",
        ],
        check=True,
        capture_output=True,
    )
    return {
        "cert": cert,
        "key": key,
        "key_algorithm": "ECDSA P-256",
        "signature_algorithm": "ecdsa-with-SHA256",
    }


def free_port(kind=socket.SOCK_STREAM) -> int:
    with socket.socket(socket.AF_INET, kind) as s:
        s.bind((HOST, 0))
        return s.getsockname()[1]


# ---------------------------------------------------------------------------
# Daemons
# ---------------------------------------------------------------------------


class Daemon:
    """One daemon process, started for one cell and stopped after it."""

    def __init__(
        self, binary, transport, workdir, src, dst, probe_src, probe_dst, cert, client_env
    ):
        self.transport = transport
        self.port = free_port()
        self.quic_port = free_port(socket.SOCK_DGRAM) if transport.quic else None
        conf = os.path.join(workdir, "rsyncd.conf")
        lines = [
            f"port = {self.port}",
            f"address = {HOST}",
            "use chroot = false",
            f"log file = {os.path.join(workdir, 'daemon.log')}",
        ]
        if transport.quic:
            lines += [
                f"quic cert file = {cert['cert']}",
                f"quic key file = {cert['key']}",
                f"quic port = {self.quic_port}",
            ]
        for name, path, ro in (
            ("src", src, "true"),
            ("dest", dst, "false"),
            ("probe_src", probe_src, "true"),
            ("probe_dest", probe_dst, "false"),
        ):
            lines += [f"[{name}]", f"    path = {path}", f"    read only = {ro}"]
        with open(conf, "w") as f:
            f.write("\n".join(lines) + "\n")
        env = dict(os.environ)
        if transport.cc:
            # The daemon is the sender on a pull; `--quic-cc` only reaches the
            # client endpoint, so the server's controller comes from the env.
            env["OC_RSYNC_QUIC_CC"] = transport.cc
        self.binary = binary
        self.cert = cert
        self.client_env = client_env
        self.stderr = open(os.path.join(workdir, "daemon.stderr"), "ab")
        self.proc = subprocess.Popen(
            [binary, "--daemon", "--no-detach", "--config", conf],
            stdout=subprocess.DEVNULL,
            stderr=self.stderr,
            env=env,
        )
        self._wait_tcp()
        if transport.quic:
            self._wait_quic()
        self.resident = self._resident_children()

    def _wait_tcp(self, timeout=15.0):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.proc.poll() is not None:
                raise RuntimeError(f"{self.transport.key} daemon exited at startup")
            try:
                with socket.create_connection((HOST, self.port), timeout=1):
                    return
            except OSError:
                time.sleep(0.05)
        raise RuntimeError(f"{self.transport.key} daemon did not listen")

    def _wait_quic(self, timeout=15.0):
        """The TCP listener answering does not mean the UDP one is bound yet."""
        cmd = [self.binary, "--quic-ca", self.cert["cert"], f"quic://{HOST}:{self.quic_port}/"]
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if timed_run(cmd, self.client_env, 10)["exit"] == 0:
                return
            time.sleep(0.1)
        raise RuntimeError(f"{self.transport.key} QUIC listener did not answer")

    def _resident_children(self, settle=1.0) -> frozenset:
        """Children that outlive a session: the oc-rsync QUIC listener.

        The readiness probes above leave short-lived session children behind;
        whatever is still alive after `settle` seconds lives as long as the
        daemon. Its CPU and peak RSS are server cost, but it is never reaped
        while the daemon runs, so it has to be read directly.
        """
        first = set(live_children(self.proc.pid))
        time.sleep(settle)
        return frozenset(first & set(live_children(self.proc.pid)))

    def cpu(self) -> tuple[float, float]:
        """Server `(user, sys)` seconds: daemon, reaped sessions, residents."""
        wait_reaped(self.proc.pid, self.resident)
        user = system = 0.0
        for pid in (self.proc.pid, *self.resident):
            u, s = proc_cpu_split(pid)
            user += u
            system += s
        return user, system

    def stop(self) -> int:
        """Stop the daemon; peak RSS (KiB) over it, its sessions and residents."""
        wait_reaped(self.proc.pid, self.resident)
        resident_hwm = max((peak_rss_kb(pid) for pid in self.resident), default=0)
        self.proc.send_signal(signal.SIGTERM)
        killer = threading.Timer(15, self.proc.kill)
        killer.start()
        _, status, ru = os.wait4(self.proc.pid, 0)
        killer.cancel()
        self.proc.returncode = os.waitstatus_to_exitcode(status)
        for pid in self.resident:
            try:
                os.kill(pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
        self.stderr.close()
        return max(ru.ru_maxrss, resident_hwm)


class Relay:
    """Single-connection TCP relay that records both directions' first bytes."""

    CAP = 64 * 1024

    def __init__(self, target_port):
        self.target_port = target_port
        self.listener = socket.socket()
        self.listener.bind((HOST, 0))
        self.listener.listen(1)
        self.port = self.listener.getsockname()[1]
        self.client_bytes = bytearray()
        self.server_bytes = bytearray()
        self.thread = threading.Thread(target=self._serve, daemon=True)
        self.thread.start()

    def _pump(self, src, dst, sink):
        try:
            while data := src.recv(65536):
                if len(sink) < self.CAP:
                    sink += data[: self.CAP - len(sink)]
                dst.sendall(data)
        except OSError:
            pass
        finally:
            try:
                dst.shutdown(socket.SHUT_WR)
            except OSError:
                pass

    def _serve(self):
        self.listener.settimeout(60)
        try:
            client, _ = self.listener.accept()
        except OSError:
            return
        server = socket.create_connection((HOST, self.target_port))
        up = threading.Thread(target=self._pump, args=(client, server, self.client_bytes))
        up.start()
        self._pump(server, client, self.server_bytes)
        up.join()
        client.close()
        server.close()

    def close(self):
        self.thread.join(timeout=60)
        self.listener.close()


# ---------------------------------------------------------------------------
# Commands
# ---------------------------------------------------------------------------


def client_cmd(binary, transport, daemon, direction, inc_mode, local, module, cert, port=None):
    """argv for one client run against `daemon`."""
    cmd = [binary, "-a", "--info=stats1"]
    if inc_mode == "no-inc-recursive":
        cmd.append("--no-inc-recursive")
    if transport.quic:
        cmd += ["--quic-ca", cert["cert"], "--quic-cipher", transport.cipher]
        if transport.cc:
            cmd += ["--quic-cc", transport.cc]
        url = f"quic://{HOST}:{port or daemon.quic_port}/{module}/"
    else:
        url = f"rsync://{HOST}:{port or daemon.port}/{module}/"
    if direction == "push":
        cmd += [f"{local}/", url]
    else:
        cmd += [url, f"{local}/"]
    return cmd


def probe_inc_recurse(binary, transport, daemon, direction, inc_mode, ctx):
    """What INC_RECURSE state the daemon actually negotiated for this cell."""
    if transport.quic:
        return {
            "negotiated": None,
            "oracle": "not observable: the QUIC stream is encrypted",
        }
    fresh_dir(ctx["probe_dst"])
    relay = Relay(daemon.port)
    local = ctx["probe_src"] if direction == "push" else ctx["probe_dst"]
    module = "probe_dest" if direction == "push" else "probe_src"
    cmd = client_cmd(
        binary, transport, daemon, direction, inc_mode, local, module,
        ctx["cert"], port=relay.port,
    )
    run = timed_run(cmd, ctx["env"], 120)
    relay.close()
    flags = server_compat_flags(bytes(relay.server_bytes))
    return {
        "negotiated": None if flags is None else bool(flags & CF_INC_RECURSE),
        "oracle": "wire: daemon compat flags",
        "compat_flags": None if flags is None else f"0x{flags:x}",
        "client_caps": client_capabilities(bytes(relay.client_bytes)),
        "probe_exit": run["exit"],
    }


def binary_for(transport, args):
    return args.upstream_path if transport.impl == "upstream" else args.oc


def run_cell(ds, transport, direction, scenario, inc_mode, ctx, args):
    binary = binary_for(transport, args)
    workdir = ctx["daemon_dir"]
    fresh_dir(workdir)
    daemon = Daemon(
        binary, transport, workdir, ctx["src"], ctx["dst"],
        ctx["probe_src"], ctx["probe_dst"], ctx["cert"], ctx["env"],
    )
    try:
        probe = probe_inc_recurse(binary, transport, daemon, direction, inc_mode, ctx)
        local = ctx["src"] if direction == "push" else ctx["dst"]
        module = "dest" if direction == "push" else "src"
        cmd = client_cmd(
            binary, transport, daemon, direction, inc_mode, local, module, ctx["cert"]
        )
        if scenario == "incremental":
            fresh_dir(ctx["dst"])
            subprocess.run(
                [args.upstream_path, "-a", f"{ctx['src']}/", f"{ctx['dst']}/"],
                check=True,
            )
        runs = []
        user0, sys0 = daemon.cpu()
        for _ in range(ctx["runs"]):
            if scenario == "initial":
                fresh_dir(ctx["dst"])
            else:
                make_stale(ds, ctx["dst"])
            # Write back the previous run's and the fixture's dirty pages
            # outside the timed window, so one run does not pay for another.
            os.sync()
            runs.append(timed_run(cmd, ctx["env"], ctx["timeout"]))
        user1, sys1 = daemon.cpu()
    finally:
        rss = daemon.stop()
    server = {"user": user1 - user0, "sys": sys1 - sys0, "rss_kb": rss}
    cell = {
        "dataset": ds.key,
        "transport": transport.key,
        "direction": direction,
        "scenario": scenario,
        "inc_mode": inc_mode,
        "inc_recurse": probe,
    }
    cell.update(summarize(runs, server, ds.total_bytes))
    pinned = any("pinning new host key" in r["stderr"] for r in runs)
    if transport.quic:
        cell["tofu_pin_notice"] = pinned
    return cell


def measure_handshakes(args, ctx, count=20):
    """Per-connection cost: list the daemon's modules `count` times.

    A module listing is a connect, the greeting exchange and a close, so its
    elapsed time is process start plus one handshake. The TCP rows are the
    same measurement without TLS, which isolates what QUIC's handshake adds.
    """
    rows = []
    for transport in TRANSPORTS:
        binary = binary_for(transport, args)
        fresh_dir(ctx["daemon_dir"])
        daemon = Daemon(
            binary, transport, ctx["daemon_dir"], ctx["probe_src"], ctx["probe_dst"],
            ctx["probe_src"], ctx["probe_dst"], ctx["cert"], ctx["env"],
        )
        try:
            cmd = [binary]
            if transport.quic:
                cmd += ["--quic-ca", ctx["cert"]["cert"], "--quic-cipher", transport.cipher]
                cmd.append(f"quic://{HOST}:{daemon.quic_port}/")
            else:
                cmd.append(f"rsync://{HOST}:{daemon.port}/")
            timed_run(cmd, ctx["env"], 60)
            user0, sys0 = daemon.cpu()
            runs = [timed_run(cmd, ctx["env"], 60) for _ in range(count)]
            user1, sys1 = daemon.cpu()
        finally:
            daemon.stop()
        rows.append({
            "transport": transport.key,
            "runs": count,
            "wall_median_ms": round(statistics.median(r["wall"] for r in runs) * 1000, 2),
            "client_cpu_ms": round(
                statistics.median(r["user"] + r["sys"] for r in runs) * 1000, 2
            ),
            "server_cpu_ms": round((user1 - user0 + sys1 - sys0) / count * 1000, 2),
            "failures": sum(1 for r in runs if r["exit"] != 0),
        })
    return rows


def version_of(binary):
    out = subprocess.run([binary, "--version"], capture_output=True, text=True).stdout
    return out.splitlines()[0] if out else ""


def parse_args(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--oc", required=True, help="quic-feature oc-rsync binary")
    parser.add_argument(
        "--upstream", required=True, help="LABEL=PATH of the upstream rsync baseline"
    )
    parser.add_argument("--profile", choices=sorted(PROFILES), default="ci")
    parser.add_argument("--runs", type=int, help="override the profile's run count")
    parser.add_argument("--workdir", help="scratch parent (default: $TMPDIR)")
    parser.add_argument(
        "--datasets", help="comma-separated subset of the profile's datasets"
    )
    args = parser.parse_args(argv)
    label, sep, path = args.upstream.partition("=")
    if not sep:
        parser.error("--upstream must be LABEL=PATH")
    args.upstream_label = label
    args.upstream_path = os.path.abspath(path)
    args.oc = os.path.abspath(args.oc)
    for binary in (args.oc, args.upstream_path):
        if not os.access(binary, os.X_OK):
            parser.error(f"not an executable: {binary}")
    return args


def main(argv=None):
    args = parse_args(argv)
    profile = PROFILES[args.profile]
    datasets = profile["datasets"]
    if args.datasets:
        wanted = set(args.datasets.split(","))
        datasets = tuple(d for d in datasets if d.key in wanted)
    started = time.monotonic()
    scratch = tempfile.mkdtemp(prefix="oc_transport_bench_", dir=args.workdir)
    try:
        xdg = os.path.join(scratch, "xdg")
        os.makedirs(xdg)
        env = dict(os.environ, XDG_CONFIG_HOME=xdg)
        ctx = {
            "runs": args.runs or profile["runs"],
            "timeout": profile["timeout"],
            "cert": make_cert(scratch),
            "env": env,
            "src": os.path.join(scratch, "src"),
            "dst": os.path.join(scratch, "dst"),
            "probe_src": os.path.join(scratch, "probe_src"),
            "probe_dst": os.path.join(scratch, "probe_dst"),
            "daemon_dir": os.path.join(scratch, "daemon"),
        }
        os.makedirs(os.path.join(ctx["probe_src"], "sub", "deeper"))
        for rel in ("top", "sub/a", "sub/deeper/b"):
            with open(os.path.join(ctx["probe_src"], rel), "wb") as f:
                f.write(os.urandom(512))
        os.makedirs(ctx["probe_dst"])

        result = {
            "profile": args.profile,
            "oc_version": version_of(args.oc),
            "upstream": {
                "label": args.upstream_label,
                "version": version_of(args.upstream_path),
            },
            "certificate": {
                k: ctx["cert"][k] for k in ("key_algorithm", "signature_algorithm")
            },
            "client_flags": "-a --info=stats1",
            "datasets": {},
            "cells": [],
        }
        print("Measuring per-connection handshake cost...", file=sys.stderr)
        result["handshake"] = measure_handshakes(args, ctx)

        for ds in datasets:
            print(f"Generating {ds.label}...", file=sys.stderr)
            make_dataset(ds, ctx["src"])
            result["datasets"][ds.key] = {
                "label": ds.label,
                "files": ds.files,
                "bytes": ds.total_bytes,
                "stale_files": ds.stale_files,
                "stale_bytes": ds.stale_bytes,
            }
            for cell in cells([ds]):
                _, tr, direction, scenario, inc_mode = cell
                print(
                    f"  [{ds.key}] {tr.key} {direction} {scenario} {inc_mode}",
                    file=sys.stderr,
                )
                result["cells"].append(
                    run_cell(ds, tr, direction, scenario, inc_mode, ctx, args)
                )
            remove_tree(ctx["src"])
            remove_tree(ctx["dst"])
        result["elapsed_s"] = round(time.monotonic() - started, 1)
        json.dump({"transport": result}, sys.stdout, indent=2)
        print()
    finally:
        remove_tree(scratch)


if __name__ == "__main__":
    main()
