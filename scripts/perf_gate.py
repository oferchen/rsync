#!/usr/bin/env python3
"""Performance-gate harness: same-run A/B of oc-rsync against upstream rsync.

Builds one cell's fixture once, then runs every side alternately (after one
discarded warm-up per side) over each requested transport and records, per
run: wall time, user and system CPU, peak RSS and exit code. One further run
per side is traced with `strace -f -c` for syscall counts. The script only
measures and reports; it holds no timing assertion and no threshold.

Sides
  oc        the release binary given by --oc
  upstream  rsync 3.5.2 (--upstream, default ~/up352/rsync-3.5.2/rsync)
  third     optional (--third): another binary, or the same oc binary with
            different arguments (--third-args), environment (--third-env) or
            daemon module parameters (--third-daemon-param) for a before/after

By default each side talks to itself: its own binary is the daemon and the
remote-shell server (--rsync-path), so a row never mixes two implementations.
--oc-peer / --third-peer name a different binary for that far end, and
--oc-env / --third-env set variables that reach both ends (the remote-shell
server and the daemon inherit them).

Transports
  local        BIN ARGS src/ dst/
  daemon-pull  BIN ARGS rsync://127.0.0.1:PORT/src/ dst/   (fresh daemon per run)
  daemon-push  BIN ARGS src/ rsync://127.0.0.1:PORT/dst/
  rsh-pull     BIN ARGS -e WRAPPER --rsync-path=BIN localhost:src/ dst/
  rsh-push     BIN ARGS -e WRAPPER --rsync-path=BIN src/ localhost:dst/
  WRAPPER is a two-line script that drops the host argument and execs the rest.

Columns
  wall_s          spawn-to-exit of the client, monotonic clock
  user_s, sys_s   client plus its reaped descendants (the remote-shell server),
                  plus the daemon process tree on daemon transports
  maxrss_kb       largest single-process peak RSS among the client and its
                  reaped descendants, from GNU time's %M. (A child forked from
                  this script would inherit the interpreter's own peak, so the
                  command runs under /usr/bin/time, whose footprint is ~2 MB.)
                  Blank on kind=strace rows.
  srv_maxrss_kb   the same for the daemon process tree (daemon transports only)
  bytes_sent, bytes_recv   from the client's --stats output
  sc_*            strace -f -c call counts (kind=strace rows only): total and
                  the write, read, open and stat families, client and server

Data safety
  --data-dir is required. Everything is created inside one new directory
  below it, tagged with a marker file; only directories this run created and
  recorded are ever removed, and only after the marker is found again.

Examples
  scripts/perf_gate.py --list-cells
  scripts/perf_gate.py --cell small-100k,nochange-10k \\
      --data-dir /dev/shm/perf-gate --oc target/release/oc-rsync --runs 5
  scripts/perf_gate.py --cell delta-1x4g --transports rsh-push --data-dir D \\
      --oc target/release/oc-rsync --third target/release/oc-rsync \\
      --third-name oc-seq --third-args=--no-parallel-delta
  scripts/perf_gate.py --self-test --data-dir /dev/shm/perf-gate \\
      --oc target/release/oc-rsync

Linux only: needs GNU time at /usr/bin/time and strace(1).
"""

from __future__ import annotations

import argparse
import dataclasses
import filecmp
import hashlib
import os
import platform
import random
import re
import shlex
import shutil
import signal
import socket
import statistics
import subprocess
import sys
import time
from pathlib import Path
from typing import Callable, Iterable, Optional

DEFAULT_UPSTREAM = "~/up352/rsync-3.5.2/rsync"
MARKER = ".perf-gate-owned"
TRANSPORTS = ("local", "daemon-pull", "daemon-push", "rsh-pull", "rsh-push")
KIB = 1024
MIB = 1024 * KIB
GIB = 1024 * MIB
BACKDATED = 978307200  # 2001-01-01: never equal to a fixture mtime
SELF_TEST_SCALE = 0.01

SYSCALL_FAMILIES = {
    "write": {"write", "writev", "pwrite64", "pwritev", "pwritev2", "sendto", "sendmsg"},
    "read": {"read", "readv", "pread64", "preadv", "preadv2", "recvfrom", "recvmsg"},
    "open": {"open", "openat", "openat2", "creat"},
    "stat": {"stat", "lstat", "fstat", "newfstatat", "statx", "fstatat64"},
}

ROW_FIELDS = (
    "cell", "transport", "side", "kind", "iter", "exit", "wall_s", "user_s",
    "sys_s", "maxrss_kb", "srv_maxrss_kb", "bytes_sent", "bytes_recv",
    "sc_total", "sc_write", "sc_read", "sc_open", "sc_stat", "verified",
)
SUMMARY_FIELDS = (
    "cell", "transport", "side", "runs", "wall_med_s", "wall_min_s",
    "wall_max_s", "wall_spread_pct", "cpu_med_s", "maxrss_med_kb",
    "srv_maxrss_med_kb", "wall_vs_upstream", "rss_vs_upstream",
    "wall_vs_oc", "rss_vs_oc",
)


class HarnessError(Exception):
    """A condition that stops the harness (never a performance verdict)."""


# --------------------------------------------------------------------------
# Owned-directory bookkeeping
# --------------------------------------------------------------------------

class Workspace:
    """The one directory tree this run may create in and remove from."""

    def __init__(self, data_dir: Path, label: str) -> None:
        data_dir = data_dir.expanduser().resolve()
        data_dir.mkdir(parents=True, exist_ok=True)
        self.root = data_dir / f"perf-gate-{label}-{os.getpid()}"
        self.root.mkdir()  # must be new: never adopt an existing directory
        (self.root / MARKER).write_text("created by scripts/perf_gate.py\n")
        self._created: set[Path] = set()

    def make_dir(self, name: str) -> Path:
        """Create and record a directory directly below the workspace root."""
        path = self.root / name
        path.mkdir()
        self._created.add(path)
        return path

    def remove_dir(self, path: Path) -> None:
        """Remove a directory this workspace created; refuse anything else."""
        if path not in self._created:
            raise HarnessError(f"refusing to remove a directory not created by this run: {path}")
        self._check_marker()
        if path.is_symlink() or path.parent != self.root:
            raise HarnessError(f"refusing to remove {path}: not a plain child of {self.root}")
        shutil.rmtree(path)
        self._created.discard(path)

    def recreate_dir(self, path: Path) -> None:
        """Empty a recorded directory by removing and re-creating it."""
        name = path.name
        self.remove_dir(path)
        self.make_dir(name)

    def destroy(self) -> None:
        """Remove the workspace root itself, after re-checking its marker."""
        self._check_marker()
        if self.root.is_symlink():
            raise HarnessError(f"refusing to remove {self.root}: it is a symlink")
        shutil.rmtree(self.root)
        self._created.clear()

    def _check_marker(self) -> None:
        if not (self.root / MARKER).is_file():
            raise HarnessError(f"marker {MARKER} missing in {self.root}: refusing to remove anything")


# --------------------------------------------------------------------------
# Cells
# --------------------------------------------------------------------------

def _scaled(count: int, scale: float, floor: int = 1) -> int:
    return max(floor, int(round(count * scale)))


def _write_random(path: Path, size: int, rng: random.Random) -> None:
    with open(path, "wb") as out:
        remaining = size
        while remaining > 0:
            chunk = min(remaining, MIB)
            out.write(rng.randbytes(chunk))
            remaining -= chunk


def _make_files(root: Path, dirs: int, per_dir: int, size_of: Callable[[random.Random], int],
                seed: int) -> None:
    rng = random.Random(seed)
    pool = rng.randbytes(256 * KIB)
    for d in range(dirs):
        directory = root / f"d{d:03d}"
        directory.mkdir()
        for f in range(per_dir):
            size = size_of(rng)
            if size <= len(pool) // 2:
                start = rng.randrange(len(pool) - size)
                (directory / f"f{f:05d}").write_bytes(pool[start:start + size])
            else:
                _write_random(directory / f"f{f:05d}", size, rng)


def _copy_tree(src: Path, dst: Path) -> None:
    """Copy `src` into the existing directory `dst`, preserving mtimes."""
    shutil.copytree(src, dst, symlinks=True, dirs_exist_ok=True)


def _mutation_plan(size: int, seed: int) -> tuple[int, list[int]]:
    """Chunk size and the chunk offsets that make up ~5% of a file."""
    chunk = min(64 * KIB, max(KIB, size // 64))
    chunks = max(1, size // chunk)
    picks = max(1, round(chunks * 0.05))
    rng = random.Random(seed)
    return chunk, sorted(rng.sample(range(chunks), picks))


def _mutate_tree(dst: Path) -> None:
    """Overwrite ~5% of every regular file in place and backdate it."""
    for index, path in enumerate(sorted(p for p in dst.rglob("*") if p.is_file())):
        size = path.stat().st_size
        chunk, picks = _mutation_plan(size, index)
        filler = random.Random(index ^ 0x5EED).randbytes(chunk)
        fd = os.open(path, os.O_WRONLY)
        try:
            for pick in picks:
                os.pwrite(fd, filler[:max(0, size - pick * chunk)], pick * chunk)
        finally:
            os.close(fd)
        os.utime(path, (BACKDATED, BACKDATED))


@dataclasses.dataclass(frozen=True)
class Cell:
    """One workload: how to build it, how to arm each run, what to pass."""

    name: str
    description: str
    args: tuple[str, ...]
    build_src: Callable[[Path, float], None]
    # "fresh": empty destination before every run.
    # "seeded": destination pre-populated once from the source, then `arm`
    # re-applies the per-run difference in place.
    dest: str = "fresh"
    arm: Optional[Callable[[Path, float], None]] = None
    check: Optional[Callable[[Path, Path], Optional[str]]] = None


def _src_small(root: Path, scale: float) -> None:
    _make_files(root, 100, _scaled(1000, scale), lambda r: r.randrange(KIB, 4 * KIB + 1), 1)


def _src_large(root: Path, scale: float) -> None:
    _write_random(root / "large.bin", _scaled(4 * GIB, scale), random.Random(2))


def _src_nochange(root: Path, scale: float) -> None:
    _make_files(root, 100, _scaled(100, scale), lambda r: r.randrange(KIB, 4 * KIB + 1), 3)


def _src_delete(root: Path, scale: float) -> None:
    _make_files(root, 1, 100, lambda r: KIB, 4)


def _arm_delete(dst: Path, scale: float) -> None:
    per_dir = _scaled(1000, scale)
    for d in range(20):
        directory = dst / f"extra{d:02d}"
        directory.mkdir(exist_ok=True)
        for f in range(per_dir):
            (directory / f"x{f:05d}").touch()


def _src_hardlink(root: Path, scale: float) -> None:
    # Same shape as the hard-link profiling fixture: 100 directories, each
    # holding two-name groups aN/bN of 4 bytes.
    per_dir = _scaled(500, scale)
    for d in range(100):
        directory = root / f"d{d}"
        directory.mkdir()
        for g in range(per_dir):
            leader = directory / f"a{g}"
            leader.write_bytes(b"%04d" % (g % 10000))
            os.link(leader, directory / f"b{g}")


def _check_hardlink(src: Path, dst: Path) -> Optional[str]:
    for path in dst.rglob("a*"):
        if path.is_file() and path.stat().st_ino != (path.parent / ("b" + path.name[1:])).stat().st_ino:
            return f"{path} is not linked to its b-name"
    return None


def _src_sparse(root: Path, scale: float) -> None:
    size = _scaled(64 * MIB, scale, floor=MIB)
    rng = random.Random(5)
    with open(root / "sparse.bin", "wb") as out:
        for offset in range(0, size, MIB):
            out.seek(offset)
            out.write(rng.randbytes(4 * KIB))
        out.truncate(size)


def _check_sparse(src: Path, dst: Path) -> Optional[str]:
    st = (dst / "sparse.bin").stat()
    if st.st_blocks * 512 >= st.st_size:
        return f"destination is not sparse: {st.st_blocks * 512} allocated of {st.st_size}"
    return None


def _src_delta(count: int, size: int, scale_count: bool) -> Callable[[Path, float], None]:
    def build(root: Path, scale: float) -> None:
        files = _scaled(count, scale) if scale_count else count
        each = size if scale_count else _scaled(size, scale, floor=MIB)
        rng = random.Random(6)
        for f in range(files):
            sub = root / f"d{f % 100:02d}"
            sub.mkdir(exist_ok=True)
            _write_random(sub / f"f{f:05d}.bin", each, rng)
    return build


def _arm_delta(dst: Path, scale: float) -> None:
    _mutate_tree(dst)


# Add new cells here. A cell needs a source builder; `dest="seeded"` plus an
# `arm` hook covers any workload that starts from an existing destination.
CELLS: dict[str, Cell] = {c.name: c for c in (
    Cell("small-100k", "100,000 files of 1-4 KiB in 100 directories, empty destination",
         ("-a",), _src_small),
    Cell("large-4g", "one 4 GiB file, empty destination", ("-a",), _src_large),
    Cell("nochange-10k", "10,000 files, destination already identical",
         ("-a",), _src_nochange, dest="seeded"),
    Cell("delete-20k", "100 files kept, 20,000 extraneous destination entries, --delete",
         ("-a", "--delete"), _src_delete, dest="seeded", arm=_arm_delete),
    Cell("hardlink-50k", "50,000 two-name hard-link groups, -H",
         ("-aH",), _src_hardlink, check=_check_hardlink),
    Cell("sparse-64m", "one 64 MiB file, 4 KiB of data per MiB, --sparse",
         ("-a", "--sparse"), _src_sparse, check=_check_sparse),
    Cell("delta-20x256m", "delta resync, 20 x 256 MiB, ~5% of the basis mutated and backdated",
         ("-a", "--no-whole-file"), _src_delta(20, 256 * MIB, False), dest="seeded", arm=_arm_delta),
    Cell("delta-1x4g", "delta resync, 1 x 4 GiB, ~5% of the basis mutated and backdated",
         ("-a", "--no-whole-file"), _src_delta(1, 4 * GIB, False), dest="seeded", arm=_arm_delta),
    Cell("delta-10kx64k", "delta resync, 10,000 x 64 KiB, ~5% of each basis mutated and backdated",
         ("-a", "--no-whole-file"), _src_delta(10000, 64 * KIB, True), dest="seeded", arm=_arm_delta),
)}


# --------------------------------------------------------------------------
# Sides and commands
# --------------------------------------------------------------------------

@dataclasses.dataclass(frozen=True)
class Side:
    """One contender: a binary plus what distinguishes its invocation."""

    name: str
    binary: str
    args: tuple[str, ...] = ()
    env: tuple[tuple[str, str], ...] = ()
    daemon_params: tuple[str, ...] = ()
    peer: str = ""  # far-end binary (daemon, --rsync-path); empty means `binary`

    @property
    def server(self) -> str:
        return self.peer or self.binary


def client_command(side: Side, cell: Cell, transport: str, src: Path, dst: Path,
                   port: Optional[int], rsh: Path) -> list[str]:
    """The client argv for one side, cell and transport."""
    cmd = [side.binary, *cell.args, "--stats", *side.args]
    if transport == "local":
        return cmd + [f"{src}/", f"{dst}/"]
    if transport == "daemon-pull":
        return cmd + [f"rsync://127.0.0.1:{port}/src/", f"{dst}/"]
    if transport == "daemon-push":
        return cmd + [f"{src}/", f"rsync://127.0.0.1:{port}/dst/"]
    cmd += ["-e", str(rsh), f"--rsync-path={side.server}"]
    if transport == "rsh-pull":
        return cmd + [f"localhost:{src}/", f"{dst}/"]
    if transport == "rsh-push":
        return cmd + [f"{src}/", f"localhost:{dst}/"]
    raise HarnessError(f"unknown transport {transport}")


def daemon_config(side: Side, src: Path, dst: Path, port: int, state: Path) -> str:
    """rsyncd.conf text serving `src` read-only and `dst` writable."""
    extra = "".join(f"    {line}\n" for line in side.daemon_params)
    return (
        f"pid file = {state}/daemon.pid\n"
        f"log file = {state}/daemon.log\n"
        f"port = {port}\n"
        "address = 127.0.0.1\n"
        "use chroot = no\n"
        f"[src]\n    path = {src}\n    read only = yes\n{extra}"
        f"[dst]\n    path = {dst}\n    read only = no\n{extra}"
    )


def free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


# --------------------------------------------------------------------------
# Parsers
# --------------------------------------------------------------------------

_STATS_RE = re.compile(r"^Total bytes (sent|received): ([\d,.]+)", re.M)


def parse_stats_bytes(output: str) -> tuple[Optional[int], Optional[int]]:
    """(sent, received) from a --stats block; None for a line not present."""
    found = {kind: int(re.sub(r"[,.]", "", value)) for kind, value in _STATS_RE.findall(output)}
    return found.get("sent"), found.get("received")


def parse_strace_summary(text: str) -> dict[str, int]:
    """Per-syscall call counts from `strace -c` output."""
    counts: dict[str, int] = {}
    for line in text.splitlines():
        parts = line.split()
        # "% time  seconds  usecs/call  calls  [errors]  syscall"
        if len(parts) < 5 or not parts[0].replace(".", "").isdigit() or parts[-1] == "total":
            continue
        counts[parts[-1]] = counts.get(parts[-1], 0) + int(parts[3])
    return counts


def syscall_families(counts: dict[str, int]) -> dict[str, int]:
    """Totals for the write, read, open and stat families, plus everything."""
    result = {"total": sum(counts.values())}
    for family, names in SYSCALL_FAMILIES.items():
        result[family] = sum(calls for name, calls in counts.items() if name in names)
    return result


def filesystem_of(path: Path, mounts: str) -> str:
    """Filesystem type of the longest mount point containing `path`."""
    best, fstype = "", "unknown"
    target = str(path)
    for line in mounts.splitlines():
        fields = line.split()
        if len(fields) < 3:
            continue
        mount = fields[1]
        inside = target == mount or target.startswith(mount.rstrip("/") + "/")
        if inside and len(mount) > len(best):
            best, fstype = mount, fields[2]
    return fstype


def summarize(rows: Iterable[dict]) -> list[dict]:
    """Medians per (cell, transport, side) and ratios against upstream and oc."""
    groups: dict[tuple, list[dict]] = {}
    for row in rows:
        if row["kind"] == "run":
            groups.setdefault((row["cell"], row["transport"], row["side"]), []).append(row)
    medians: dict[tuple, dict] = {}
    for key, members in groups.items():
        walls = [m["wall_s"] for m in members]
        srv = [m["srv_maxrss_kb"] for m in members if m["srv_maxrss_kb"] != ""]
        med = statistics.median(walls)
        medians[key] = {
            "cell": key[0], "transport": key[1], "side": key[2], "runs": len(members),
            "wall_med_s": med, "wall_min_s": min(walls), "wall_max_s": max(walls),
            "wall_spread_pct": (max(walls) - min(walls)) / med * 100 if med else 0.0,
            "cpu_med_s": statistics.median(m["user_s"] + m["sys_s"] for m in members),
            "maxrss_med_kb": statistics.median(m["maxrss_kb"] for m in members if m["maxrss_kb"] != ""),
            "srv_maxrss_med_kb": statistics.median(srv) if srv else "",
        }
    for key, entry in medians.items():
        for base in ("upstream", "oc"):
            ref = medians.get((key[0], key[1], base))
            same = ref is None or base == key[2]
            entry[f"wall_vs_{base}"] = "" if same or not ref["wall_med_s"] else entry["wall_med_s"] / ref["wall_med_s"]
            entry[f"rss_vs_{base}"] = "" if same or not ref["maxrss_med_kb"] else entry["maxrss_med_kb"] / ref["maxrss_med_kb"]
    return list(medians.values())


def format_tsv(fields: tuple[str, ...], rows: Iterable[dict]) -> str:
    def cell(value) -> str:
        return f"{value:.3f}" if isinstance(value, float) else str(value)
    lines = ["\t".join(fields)]
    lines += ["\t".join(cell(row.get(f, "")) for f in fields) for row in rows]
    return "\n".join(lines) + "\n"


# --------------------------------------------------------------------------
# Environment
# --------------------------------------------------------------------------

def binary_identity(binary: str) -> str:
    """First `--version` line, a revision line if printed, and a content hash."""
    out = subprocess.run([binary, "--version"], capture_output=True, text=True, check=True).stdout
    lines = out.splitlines()
    revision = next((line.strip() for line in lines[1:] if re.search(r"\brev(ision)?\b", line, re.I)), "")
    digest = hashlib.sha256(Path(binary).read_bytes()).hexdigest()[:12]
    return " | ".join(part for part in (lines[0].strip(), revision, f"sha256:{digest}") if part)


def read_text(path: str) -> str:
    try:
        return Path(path).read_text().strip()
    except OSError:
        return "unavailable"


def environment_lines(sides: list[Side], src: Path, dst: Path) -> list[str]:
    mounts = read_text("/proc/mounts")
    lines = [f"side {s.name}: {binary_identity(s.binary)}"
             + (f" peer={binary_identity(s.peer)}" if s.peer else "")
             + (f" args={' '.join(s.args)}" if s.args else "")
             + (f" env={dict(s.env)}" if s.env else "")
             + (f" daemon={list(s.daemon_params)}" if s.daemon_params else "")
             for s in sides]
    lines += [
        f"kernel: {platform.release()}",
        f"filesystem src: {filesystem_of(src, mounts)}  dst: {filesystem_of(dst, mounts)}  ({src.parent})",
        f"transparent_hugepage: {read_text('/sys/kernel/mm/transparent_hugepage/enabled')}",
        f"cpus: {os.cpu_count()}",
        "loadavg at start: " + " ".join(f"{v:.2f}" for v in os.getloadavg()),
        "page cache: hot (fixture just written; caches are not dropped)",
    ]
    return lines


# --------------------------------------------------------------------------
# Running
# --------------------------------------------------------------------------

GNU_TIME = "/usr/bin/time"


def _rusage_of(pid: int) -> tuple[int, float, float]:
    _, status, usage = os.wait4(pid, 0)
    return os.waitstatus_to_exitcode(status), usage.ru_utime, usage.ru_stime


def _measured(cmd: list[str], rss_out: Path, strace_out: Optional[Path]) -> list[str]:
    """Wrap `cmd` in strace when tracing, otherwise in GNU time for peak RSS."""
    if strace_out is not None:
        return ["strace", "-f", "-c", "-o", str(strace_out)] + cmd
    return [GNU_TIME, "-f", "%M", "-o", str(rss_out)] + cmd


def _read_rss(rss_out: Path, traced: bool) -> object:
    """Peak RSS in kB from GNU time's output file; blank for a traced run."""
    if traced:
        return ""
    # A command killed by a signal makes GNU time print a notice line first.
    return int(rss_out.read_text().split()[-1])


class Daemon:
    """A daemon serving one run, started fresh so its rusage is that run's."""

    def __init__(self, side: Side, src: Path, dst: Path, state: Path, strace_out: Optional[Path]) -> None:
        self.port = free_port()
        self.pid_file = state / "daemon.pid"
        self.pid_file.unlink(missing_ok=True)
        conf = state / "rsyncd.conf"
        conf.write_text(daemon_config(side, src, dst, self.port, state))
        self.rss_out = state / "daemon.rss"
        self.traced = strace_out is not None
        cmd = _measured([side.server, "--daemon", "--no-detach", f"--config={conf}"],
                        self.rss_out, strace_out)
        self.proc = subprocess.Popen(cmd, env=_side_env(side), stdin=subprocess.DEVNULL,
                                     stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
                                     start_new_session=True)
        self._wait_ready()

    def _wait_ready(self) -> None:
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            if self.proc.poll() is not None:
                raise HarnessError(f"daemon exited at startup: {self.proc.stderr.read().decode(errors='replace')}")
            try:
                socket.create_connection(("127.0.0.1", self.port), timeout=1).close()
                if self.pid_file.exists():
                    return
            except OSError:
                pass
            time.sleep(0.01)
        self.stop()
        raise HarnessError("daemon did not start listening within 20 s")

    def stop(self) -> tuple[float, float, object]:
        """Terminate the daemon; return its tree's user, system and peak RSS."""
        try:
            os.kill(int(self.pid_file.read_text().split()[0]), signal.SIGTERM)
        except (OSError, ValueError, IndexError):
            os.killpg(self.proc.pid, signal.SIGTERM)
        _, user, system = _rusage_of(self.proc.pid)
        self.proc.stderr.close()
        return user, system, _read_rss(self.rss_out, self.traced)


def _side_env(side: Side) -> dict[str, str]:
    env = dict(os.environ)
    env.update(side.env)
    return env


@dataclasses.dataclass
class Plan:
    cell: Cell
    scale: float
    sides: list[Side]
    transports: list[str]
    runs: int
    warmup: int
    strace: bool
    verify: bool
    timeout: float


class Runner:
    def __init__(self, plan: Plan, work: Workspace, log: Callable[[str], None]) -> None:
        self.plan = plan
        self.work = work
        self.log = log
        self.src = work.make_dir("src")
        self.dst = work.make_dir("dst")
        self.state = work.make_dir("state")
        self.rsh = self.state / "rsh"
        self.rsh.write_text("#!/bin/sh\nshift\nexec \"$@\"\n")
        self.rsh.chmod(0o755)
        self.rows: list[dict] = []
        self.dst_synced = False

    def build(self) -> None:
        started = time.monotonic()
        self.plan.cell.build_src(self.src, self.plan.scale)
        self.log(f"fixture {self.plan.cell.name} built in {time.monotonic() - started:.1f} s")

    def _prepare_dst(self) -> None:
        cell = self.plan.cell
        if cell.dest == "fresh":
            self.work.recreate_dir(self.dst)
            self.dst = self.work.root / "dst"
            return
        if not self.dst_synced:
            self.work.recreate_dir(self.dst)
            _copy_tree(self.src, self.dst)
        if cell.arm is not None:
            cell.arm(self.dst, self.plan.scale)
        self.dst_synced = False

    def _verify(self) -> str:
        """Empty string when the destination equals the source, else why not."""
        problems = _tree_difference(self.src, self.dst)
        if not problems and self.plan.cell.check is not None:
            problems = self.plan.cell.check(self.src, self.dst) or ""
        return problems

    def _one(self, side: Side, transport: str, kind: str, iteration: int) -> dict:
        self._prepare_dst()
        traced = kind == "strace"
        client_trace = self.state / "client.strace"
        daemon_trace = self.state / "daemon.strace"
        daemon = None
        if transport.startswith("daemon"):
            daemon = Daemon(side, self.src, self.dst, self.state, daemon_trace if traced else None)
        cmd = client_command(side, self.plan.cell, transport, self.src, self.dst,
                             daemon.port if daemon else None, self.rsh)
        rss_out = self.state / "client.rss"
        cmd = _measured(cmd, rss_out, client_trace if traced else None)
        out_path = self.state / "client.out"
        with open(out_path, "wb") as out:
            started = time.monotonic()
            proc = subprocess.Popen(cmd, env=_side_env(side), stdin=subprocess.DEVNULL,
                                    stdout=out, stderr=subprocess.STDOUT, start_new_session=True)
            previous = signal.signal(signal.SIGALRM, lambda *_: os.killpg(proc.pid, signal.SIGKILL))
            signal.setitimer(signal.ITIMER_REAL, self.plan.timeout)
            try:
                code, user, system = _rusage_of(proc.pid)
            finally:
                signal.setitimer(signal.ITIMER_REAL, 0)
                signal.signal(signal.SIGALRM, previous)
            wall = time.monotonic() - started
        proc.returncode = code
        maxrss = _read_rss(rss_out, traced)
        srv_rss: object = ""
        if daemon is not None:
            srv_user, srv_sys, srv_rss = daemon.stop()
            user, system = user + srv_user, system + srv_sys
        output = out_path.read_text(errors="replace")
        sent, received = parse_stats_bytes(output)
        row = {
            "cell": self.plan.cell.name, "transport": transport, "side": side.name,
            "kind": kind, "iter": iteration, "exit": code, "wall_s": wall,
            "user_s": user, "sys_s": system, "maxrss_kb": maxrss, "srv_maxrss_kb": srv_rss,
            "bytes_sent": "" if sent is None else sent,
            "bytes_recv": "" if received is None else received,
            "verified": "",
        }
        if traced:
            counts = parse_strace_summary(client_trace.read_text())
            if daemon is not None:
                for name, calls in parse_strace_summary(daemon_trace.read_text()).items():
                    counts[name] = counts.get(name, 0) + calls
            row.update({f"sc_{k}": v for k, v in syscall_families(counts).items()})
        if code != 0:
            self.log(f"  {side.name} {transport} exit {code}: {output.strip().splitlines()[-3:]}")
        elif self.plan.verify:
            difference = self._verify()
            row["verified"] = "yes" if not difference else f"NO: {difference}"
            self.dst_synced = not difference
        else:
            self.dst_synced = True
        return row

    def run(self) -> list[dict]:
        plan = self.plan
        for transport in plan.transports:
            for side in plan.sides:
                for _ in range(plan.warmup):
                    self._one(side, transport, "warmup", 0)
            for iteration in range(1, plan.runs + 1):
                # Rotate the starting side so no side always runs first.
                shift = (iteration - 1) % len(plan.sides)
                for side in plan.sides[shift:] + plan.sides[:shift]:
                    row = self._one(side, transport, "run", iteration)
                    self.rows.append(row)
                    self.log(f"  {transport} {side.name} #{iteration}: {row['wall_s']:.3f} s "
                             f"rss {row['maxrss_kb']} kB exit {row['exit']} {row['verified']}")
            if plan.strace:
                for side in plan.sides:
                    self.rows.append(self._one(side, transport, "strace", 0))
        return self.rows


def _tree_difference(src: Path, dst: Path) -> str:
    """First difference in names or regular-file content, or an empty string."""
    def names(root: Path) -> set[str]:
        return {str(p.relative_to(root)) for p in root.rglob("*")}
    src_names, dst_names = names(src), names(dst)
    if src_names != dst_names:
        odd = sorted(src_names ^ dst_names)
        return f"{len(odd)} names differ, first {odd[0]}"
    for name in sorted(src_names):
        a, b = src / name, dst / name
        if a.is_file() and not a.is_symlink() and not filecmp.cmp(a, b, shallow=False):
            return f"content differs: {name}"
    return ""


# --------------------------------------------------------------------------
# Command line
# --------------------------------------------------------------------------

def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="perf_gate.py", description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--cell", help="cell to run, or a comma-separated list (see --list-cells)")
    parser.add_argument("--list-cells", action="store_true", help="print the cells and exit")
    parser.add_argument("--self-test", action="store_true",
                        help="run every cell and transport once at 1/100 scale and check the "
                             "harness itself (exit codes, destination content, row counts)")
    parser.add_argument("--data-dir", help="REQUIRED. Directory under which this run creates its "
                                           "own working directory (e.g. /dev/shm/perf-gate)")
    parser.add_argument("--oc", help="REQUIRED. oc-rsync release binary")
    parser.add_argument("--upstream", default=DEFAULT_UPSTREAM,
                        help=f"upstream rsync binary (default {DEFAULT_UPSTREAM})")
    parser.add_argument("--oc-args", default="", help="extra arguments for the oc side")
    parser.add_argument("--oc-env", action="append", default=[], metavar="KEY=VALUE",
                        help="environment for the oc side's client, server and daemon")
    parser.add_argument("--oc-peer", help="binary for the oc side's far end (default: --oc)")
    parser.add_argument("--third", help="optional third side: another binary (may equal --oc)")
    parser.add_argument("--third-name", default="third", help="row label of the third side")
    parser.add_argument("--third-args", default="", help="extra arguments for the third side")
    parser.add_argument("--third-env", action="append", default=[], metavar="KEY=VALUE",
                        help="environment for the third side's client, server and daemon")
    parser.add_argument("--third-peer", help="binary for the third side's far end (default: --third)")
    parser.add_argument("--third-daemon-param", action="append", default=[], metavar="'NAME = VALUE'",
                        help="rsyncd.conf module parameter for the third side's daemon")
    parser.add_argument("--transports", default=",".join(TRANSPORTS),
                        help="comma-separated subset of: " + ", ".join(TRANSPORTS))
    parser.add_argument("--runs", type=int, default=5, help="measured iterations per side (default 5)")
    parser.add_argument("--scale", type=float, default=1.0,
                        help="fixture scale factor (default 1.0; the self-test uses 0.01)")
    parser.add_argument("--max-load", type=float, default=1.0,
                        help="refuse to start when the 1-minute load average is above this (default 1.0)")
    parser.add_argument("--force", action="store_true", help="run even above --max-load")
    parser.add_argument("--no-strace", action="store_true", help="skip the traced run")
    parser.add_argument("--no-verify", action="store_true",
                        help="do not compare the destination with the source after each run")
    parser.add_argument("--timeout", type=float, default=1800.0, help="per-run limit in seconds")
    parser.add_argument("--tsv", help="also write the per-run rows and the summary to this file")
    parser.add_argument("--keep", action="store_true", help="keep the working directory")
    return parser


def _require(parser: argparse.ArgumentParser, args: argparse.Namespace, names: Iterable[str]) -> None:
    missing = [f"--{n.replace('_', '-')}" for n in names if not getattr(args, n)]
    if missing:
        parser.error("missing required argument(s): " + ", ".join(missing))


def _binary(path: str) -> str:
    resolved = Path(path).expanduser().resolve()
    if not (resolved.is_file() and os.access(resolved, os.X_OK)):
        raise HarnessError(f"not an executable file: {path}")
    return str(resolved)


def _env_pairs(items: list[str], option: str) -> tuple[tuple[str, str], ...]:
    pairs = []
    for item in items:
        key, sep, value = item.partition("=")
        if not sep or not key:
            raise HarnessError(f"{option} needs KEY=VALUE, got {item!r}")
        pairs.append((key, value))
    return tuple(pairs)


def sides_from_args(args: argparse.Namespace) -> list[Side]:
    sides = [
        Side("oc", _binary(args.oc), tuple(shlex.split(args.oc_args)),
             _env_pairs(args.oc_env, "--oc-env"),
             peer=_binary(args.oc_peer) if args.oc_peer else ""),
        Side("upstream", _binary(args.upstream)),
    ]
    if args.third:
        if args.third_name in ("oc", "upstream"):
            raise HarnessError("--third-name must differ from oc and upstream")
        sides.append(Side(args.third_name, _binary(args.third), tuple(shlex.split(args.third_args)),
                          _env_pairs(args.third_env, "--third-env"), tuple(args.third_daemon_param),
                          peer=_binary(args.third_peer) if args.third_peer else ""))
    return sides


def run_cell(plan: Plan, data_dir: Path, keep: bool, log: Callable[[str], None]) -> tuple[list[str], list[dict]]:
    work = Workspace(data_dir, plan.cell.name)
    try:
        runner = Runner(plan, work, log)
        env = environment_lines(plan.sides, runner.src, runner.dst)
        runner.build()
        return env, runner.run()
    finally:
        if keep:
            log(f"kept {work.root}")
        else:
            work.destroy()


def report(env: list[str], rows: list[dict]) -> str:
    return ("".join(f"# {line}\n" for line in env)
            + format_tsv(ROW_FIELDS, rows) + "\n"
            + format_tsv(SUMMARY_FIELDS, summarize(rows)))


def self_test(args: argparse.Namespace, sides: list[Side], log: Callable[[str], None]) -> int:
    failures: list[str] = []
    started = time.monotonic()
    for cell in CELLS.values():
        plan = Plan(cell, SELF_TEST_SCALE, sides, list(TRANSPORTS), runs=1, warmup=0,
                    strace=not args.no_strace, verify=True, timeout=args.timeout)
        _, rows = run_cell(plan, Path(args.data_dir), False, lambda _msg: None)
        expected = len(TRANSPORTS) * len(sides) * (2 if plan.strace else 1)
        if len(rows) != expected:
            failures.append(f"{cell.name}: {len(rows)} rows, expected {expected}")
        for row in rows:
            where = f"{cell.name} {row['transport']} {row['side']} {row['kind']}"
            if row["exit"] != 0:
                failures.append(f"{where}: exit {row['exit']}")
            elif row["verified"] != "yes":
                failures.append(f"{where}: {row['verified']}")
            elif row["kind"] == "strace" and not row.get("sc_total"):
                failures.append(f"{where}: no syscall counts")
        log(f"self-test {cell.name}: {len(rows)} rows")
    elapsed = time.monotonic() - started
    for failure in failures:
        log(f"FAIL {failure}")
    log(f"self-test: {len(CELLS)} cells, {len(failures)} failure(s), {elapsed:.1f} s")
    return 1 if failures else 0


def log(message: str) -> None:
    print(message, file=sys.stderr, flush=True)


def main(argv: Optional[list[str]] = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    if args.list_cells:
        for cell in CELLS.values():
            print(f"{cell.name:16} {cell.description}  [{' '.join(cell.args)}]")
        return 0
    _require(parser, args, ("data_dir", "oc") + (() if args.self_test else ("cell",)))
    try:
        sides = sides_from_args(args)
        if not args.no_strace and shutil.which("strace") is None:
            raise HarnessError("strace not found; install it or pass --no-strace")
        if not os.access(GNU_TIME, os.X_OK):
            raise HarnessError(f"{GNU_TIME} not found; it supplies the peak-RSS column")
        if args.self_test:
            return self_test(args, sides, log)
        cells = [c for c in args.cell.split(",") if c]
        unknown_cells = [c for c in cells if c not in CELLS]
        if unknown_cells or not cells:
            raise HarnessError(f"unknown cell(s): {', '.join(unknown_cells) or '(none given)'}; see --list-cells")
        transports = [t for t in args.transports.split(",") if t]
        unknown = [t for t in transports if t not in TRANSPORTS]
        if unknown or not transports:
            raise HarnessError(f"unknown transport(s): {', '.join(unknown) or '(none given)'}")
        if args.runs < 1:
            raise HarnessError("--runs must be at least 1")
        load = os.getloadavg()[0]
        if load > args.max_load and not args.force:
            raise HarnessError(f"1-minute load average {load:.2f} is above --max-load "
                               f"{args.max_load:.2f}; wait, or pass --force")
        text = ""
        for name in cells:
            plan = Plan(CELLS[name], args.scale, sides, transports, args.runs, warmup=1,
                        strace=not args.no_strace, verify=not args.no_verify, timeout=args.timeout)
            env, rows = run_cell(plan, Path(args.data_dir), args.keep, log)
            env.append("loadavg at end: " + " ".join(f"{v:.2f}" for v in os.getloadavg()))
            section = report(env, rows)
            sys.stdout.write(section)
            sys.stdout.flush()
            text += section
            if args.tsv:
                Path(args.tsv).write_text(text)
        return 0
    except HarnessError as error:
        print(f"perf_gate: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
