#!/usr/bin/env python3
"""Oracle-based interop cells: oc-rsync against upstream rsync, judged by upstream.

Every cell runs the same transfer twice. The baseline uses upstream on both
ends; the candidate puts oc-rsync in one role. The cell passes only when the
candidate matches the baseline on exit code, destination tree (content, type,
mode, mtime, link targets, hard-link groups, xattrs), itemize lines and the
core --stats counts. A tree-only check cannot see a wrong exit code, a
dropped deletion count or a directory itemized as a file, so the comparison
covers all four.

Cells that currently diverge are listed in an expectation file with the task
that owns the fix. A listed cell that fails is XFAIL. A listed cell that
passes is XPASS and fails the run, so the fix that flips a cell must also
delete its row. An unlisted cell that fails is FAIL.
"""

from __future__ import annotations

import argparse
import hashlib
import itertools
import os
import re
import shutil
import signal
import socket
import stat
import subprocess
import sys
import tempfile
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass, field
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import interop_oracle_cells as cells_mod  # noqa: E402

# Fixture mtimes are fixed below this; anything newer was stamped with the
# wall clock by the transfer itself (a created directory whose time is not
# preserved), so two runs cannot agree on it and it is not compared.
WALL_CLOCK_NS = 1_700_000_000 * 10**9

ITEM_RE = re.compile(r"^([<>ch.*][fdLDSp][^ ]{7,9}|\*deleting|\*[a-z]+) ")
STAT_KEYS = (
    "Number of files",
    "Number of created files",
    "Number of deleted files",
    "Number of regular files transferred",
    "Total file size",
    "Total transferred file size",
    "Literal data",
    "Matched data",
)
STAT_NUM_RE = re.compile(r"^\s*([\d,]+)")

RSH_STANDIN = """#!/bin/sh
# Local stand-in for ssh: drop the host argument and run the command, as ssh does.
if [ "$1" = "-l" ]; then shift 2; fi
shift
exec sh -c "$*"
"""


# --- snapshot -------------------------------------------------------------

def snapshot(root: str | Path) -> dict[str, dict]:
    """Returns a per-path record of everything a transfer is meant to preserve."""
    root_b = os.fsencode(str(root))
    if not os.path.lexists(root_b):
        return {"<missing>": {"t": 0}}
    out: dict[str, dict] = {}
    inodes: dict[int, list[str]] = {}
    for dirpath, dirs, files in os.walk(root_b):
        dirs.sort()
        names = sorted(dirs + files)
        if dirpath == root_b:
            names = [b""] + names
        for name in names:
            path = os.path.join(dirpath, name) if name else dirpath
            rel = os.path.relpath(path, root_b).decode("utf-8", "backslashreplace")
            st = os.lstat(path)
            mode = st.st_mode
            rec: dict = {
                "t": stat.S_IFMT(mode) >> 12,
                "mode": oct(stat.S_IMODE(mode)),
                "uid": st.st_uid,
                "gid": st.st_gid,
            }
            if not stat.S_ISLNK(mode):
                rec["mtime"] = st.st_mtime_ns
            if stat.S_ISREG(mode):
                with open(path, "rb") as f:
                    rec["sha"] = hashlib.sha256(f.read()).hexdigest()[:16]
                rec["size"] = st.st_size
            elif stat.S_ISLNK(mode):
                rec["target"] = os.readlink(path).decode("utf-8", "backslashreplace")
            elif stat.S_ISCHR(mode) or stat.S_ISBLK(mode):
                rec["rdev"] = (os.major(st.st_rdev), os.minor(st.st_rdev))
            if not stat.S_ISDIR(mode) and st.st_nlink > 1:
                inodes.setdefault(st.st_ino, []).append(rel)
            xattrs = _xattrs(path)
            if xattrs:
                rec["xattr"] = xattrs
            out[rel] = rec
    for group in inodes.values():
        group.sort()
        for rel in group:
            out[rel]["hlgroup"] = group[0]
    return out


def _xattrs(path: bytes) -> dict[str, str]:
    if not hasattr(os, "listxattr"):
        return {}
    try:
        return {
            k: hashlib.sha256(os.getxattr(path, k, follow_symlinks=False)).hexdigest()[:12]
            for k in sorted(os.listxattr(path, follow_symlinks=False))
        }
    except OSError:
        return {}


def tree_diff(base: dict, got: dict, ignore: tuple[str, ...] = ()) -> list[str]:
    """Lists every difference between two snapshots, masking wall-clock mtimes."""
    res = []
    for key in sorted(set(base) | set(got)):
        if key not in got:
            res.append(f"-{key}")
            continue
        if key not in base:
            res.append(f"+{key}")
            continue
        for fld in sorted(set(base[key]) | set(got[key])):
            if fld in ignore:
                continue
            a, b = base[key].get(fld), got[key].get(fld)
            if fld == "mtime" and a and b and a > WALL_CLOCK_NS and b > WALL_CLOCK_NS:
                continue
            if a != b:
                res.append(f"{key}: {fld} {a!r} != {b!r}")
    return res


# --- output parsing -------------------------------------------------------

def parse_output(stdout: str) -> tuple[list[str], dict[str, int]]:
    """Extracts sorted itemize lines and the leading integer of each core stat.

    Only the leading count is kept: rsync < 3.1 prints "Number of files: 6"
    where newer releases print "Number of files: 6 (reg: 2, dir: 4)" and group
    digits with commas. The count itself is what a wrong transfer changes.
    """
    items, stats = [], {}
    for line in stdout.splitlines():
        if ITEM_RE.match(line) or line.startswith("deleting "):
            items.append(line.rstrip())
            continue
        for key in STAT_KEYS:
            if line.startswith(key + ":"):
                m = STAT_NUM_RE.match(line.split(":", 1)[1])
                if m:
                    stats[key] = int(m.group(1).replace(",", ""))
    return sorted(items), stats


def normalize_items(items: list[str], legacy: bool) -> list[str]:
    """Reduces itemize lines to update type, file type and name for pre-3.1 peers.

    rsync < 3.1 prints a 9-character change string where newer releases print
    11. When one side of a comparison is such a client, only the first two
    columns and the name are comparable.
    """
    if not legacy:
        return items
    out = []
    for line in items:
        flags, _, name = line.partition(" ")
        out.append(f"{flags[:2]} {name.strip()}")
    return sorted(out)


# --- processes ------------------------------------------------------------

# Daemon ports come from a range below every platform's ephemeral range
# (Linux 32768+, macOS/Windows 49152+). A port from bind(0) is ephemeral, so
# the kernel can hand it to a concurrent cell's client socket before the
# daemon binds it, and a start probe can then reach another cell's daemon.
# The pid offset keeps two oracle runs on one host apart.
_PORT_BASE = 20000 + (os.getpid() % 12) * 1000
_ports = itertools.count()
_ports_lock = threading.Lock()


def free_port() -> int:
    """A loopback port no other cell of this run will use, and free right now."""
    while True:
        with _ports_lock:
            n = next(_ports)
        if n >= 1000:
            raise RuntimeError("daemon port range exhausted")
        port = _PORT_BASE + n
        with socket.socket() as s:
            try:
                s.bind(("127.0.0.1", port))
            except OSError:
                continue
        return port


def run(cmd: list[str], timeout: float, env: dict | None = None) -> tuple[object, str, str]:
    """Runs one transfer in its own session so a timeout kills the whole tree."""
    proc = subprocess.Popen(cmd, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE, env=env, start_new_session=True)
    try:
        out, err = proc.communicate(timeout=timeout)
        rc: object = proc.returncode
    except subprocess.TimeoutExpired:
        os.killpg(proc.pid, signal.SIGKILL)
        out, err = proc.communicate()
        rc = "TIMEOUT"
    return rc, out.decode("utf-8", "replace"), err.decode("utf-8", "replace")


class Daemon:
    """A daemon on a free loopback port, killed with its process group on exit."""

    def __init__(self, binary: str, conf: Path, log: Path):
        self.port = free_port()
        env = dict(os.environ, OC_RSYNC_DAEMON_FALLBACK="0")
        self.proc = subprocess.Popen(
            [binary, "--daemon", "--no-detach", f"--config={conf}", f"--port={self.port}",
             "--address=127.0.0.1", f"--log-file={log}"],
            stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            env=env, start_new_session=True)
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            try:
                socket.create_connection(("127.0.0.1", self.port), timeout=0.2).close()
                return
            except OSError:
                if self.proc.poll() is not None:
                    break
                time.sleep(0.05)
        self.stop()
        raise RuntimeError(f"daemon {binary} did not start")

    def stop(self) -> None:
        try:
            os.killpg(self.proc.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        self.proc.wait(timeout=10)

    def __enter__(self) -> "Daemon":
        return self

    def __exit__(self, *exc) -> None:
        self.stop()


# --- execution ------------------------------------------------------------

@dataclass
class Outcome:
    rc: object
    items: list[str]
    stats: dict[str, int]
    tree: dict
    stderr: str
    cmd: str


@dataclass
class Result:
    cell_id: str
    problems: dict = field(default_factory=dict)
    candidate: Outcome | None = None
    baseline: Outcome | None = None


def execute(cell: "cells_mod.Cell", client: str, server: str, workdir: Path,
            fixtures: Path, rsh: str) -> Outcome:
    """Runs one side of a cell (baseline or candidate) in a fresh directory."""
    if workdir.exists():
        shutil.rmtree(workdir)
    workdir.mkdir(parents=True)
    fx = fixtures / cell.fixture
    subprocess.run(["cp", "-a", str(fx / "src"), str(workdir / "src")], check=True)
    if (fx / "pre").is_dir():
        subprocess.run(["cp", "-a", str(fx / "pre"), str(workdir / "dst")], check=True)
    else:
        (workdir / "dst").mkdir()
    args = [client, *cell.opts, "-i", "--stats"]
    args += [a.replace("@FIX@", str(fx)) for a in cell.extra_args]
    if cell.batch_reader:
        return _execute_batch(cell, client, server, workdir, rsh, args)
    if cell.transport == "rsh":
        args += ["-e", rsh, f"--rsync-path={server}"]
        if cell.direction == "push":
            args += [f"{workdir}/src/", f"localhost:{workdir}/dst/"]
        else:
            args += [f"localhost:{workdir}/src/", f"{workdir}/dst/"]
        rc, out, err = run(args, cell.timeout)
    else:
        modroot = workdir / ("dst" if cell.direction == "push" else "src")
        conf = workdir / "d.conf"
        conf.write_text("use chroot = false\nmunge symlinks = false\n"
                        f"[m]\n  path = {modroot}\n  read only = false\n{cell.module_extra}")
        try:
            with Daemon(server, conf, workdir / "d.log") as d:
                url = f"rsync://127.0.0.1:{d.port}/m/"
                if cell.direction == "push":
                    args += [f"{workdir}/src/", url]
                else:
                    args += [url, f"{workdir}/dst/"]
                rc, out, err = run(args, cell.timeout)
        except RuntimeError as e:
            rc, out, err = "DAEMON_START_FAIL", "", str(e)
    items, stats = parse_output(out)
    return Outcome(rc, items, stats, snapshot(workdir / "dst"), err[-2000:], " ".join(args))


def _execute_batch(cell, writer, reader, workdir, rsh, args) -> Outcome:
    """Writes a batch with `writer` (pushing to `reader`, or locally), then replays it with `reader`."""
    batch = workdir / "b"
    subprocess.run(["cp", "-a", str(workdir / "dst"), str(workdir / "sink")], check=True)
    write = args + [f"--write-batch={batch}"]
    if cell.transport == "rsh":
        write += ["-e", rsh, f"--rsync-path={reader}", f"{workdir}/src/",
                  f"localhost:{workdir}/sink/"]
    else:
        write += [f"{workdir}/src/", f"{workdir}/sink/"]
    rc_w, _, err_w = run(write, cell.timeout)
    if rc_w != 0:
        return Outcome(f"write:{rc_w}", [], {}, snapshot(workdir / "dst"), err_w[-2000:],
                       " ".join(write))
    read = [reader, *cell.opts, "-i", "--stats", f"--read-batch={batch}", f"{workdir}/dst/"]
    rc, out, err = run(read, cell.timeout)
    items, stats = parse_output(out)
    return Outcome(rc, items, stats, snapshot(workdir / "dst"), err[-2000:], " ".join(read))


def compare(base: Outcome, got: Outcome, legacy_items: bool, ignore_owner: bool) -> dict:
    """Returns the problems found in `got` relative to the baseline; empty means pass."""
    probs: dict = {}
    if got.rc != base.rc:
        probs["rc"] = f"{base.rc} -> {got.rc}"
    td = tree_diff(base.tree, got.tree, ("uid", "gid") if ignore_owner else ())
    if td:
        probs["tree"] = td[:20]
    bi, gi = normalize_items(base.items, legacy_items), normalize_items(got.items, legacy_items)
    if bi != gi:
        probs["itemize"] = {"missing": sorted(set(bi) - set(gi))[:10],
                            "extra": sorted(set(gi) - set(bi))[:10]}
    sd = {k: (v, got.stats[k]) for k, v in base.stats.items()
          if k in got.stats and got.stats[k] != v}
    if sd:
        probs["stats"] = sd
    return probs


def run_cell(cell, oc: str, upstream: dict[str, str], scratch: Path, fixtures: Path,
             rsh: str) -> Result:
    up = upstream[cell.version]
    base_client = upstream.get(cell.baseline_client, up)
    base = execute(cell, base_client, up, scratch / cell.slug / "base", fixtures, rsh)
    client, server = (oc, up) if cell.role == "oc-client" else (base_client, oc)
    got = execute(cell, client, server, scratch / cell.slug / "cand", fixtures, rsh)
    legacy = cell.role == "oc-client" and cells_mod.version_tuple(cell.baseline_client) < (3, 1)
    probs = compare(base, got, legacy, ignore_owner=os.geteuid() != 0)
    if got.rc == "TIMEOUT":
        probs["timeout"] = cell.timeout
    return Result(cell.cell_id, probs, got, base)


# --- expectations ---------------------------------------------------------

def load_expectations(path: Path) -> dict[str, str]:
    """Maps cell id to owner. Lines are `<cell-id> <owner> [reason...]`."""
    rows: dict[str, str] = {}
    for n, line in enumerate(path.read_text().splitlines(), 1):
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        parts = line.split(None, 2)
        if len(parts) < 2 or not parts[1].startswith("task-"):
            raise ValueError(f"{path}:{n}: expected '<cell-id> task-N [reason]'")
        if parts[0] in rows:
            raise ValueError(f"{path}:{n}: duplicate row {parts[0]}")
        rows[parts[0]] = parts[1]
    return rows


def classify(results: list[Result], expected: dict[str, str],
             catalogue_ids: set[str]) -> dict[str, list[str]]:
    """Sorts results into PASS/XFAIL/FAIL/XPASS and flags expectation rows naming no cell."""
    out = {"PASS": [], "XFAIL": [], "FAIL": [], "XPASS": [], "STALE": []}
    for r in results:
        failed = bool(r.problems)
        listed = r.cell_id in expected
        key = {(False, False): "PASS", (True, True): "XFAIL",
               (True, False): "FAIL", (False, True): "XPASS"}[(failed, listed)]
        out[key].append(r.cell_id)
    out["STALE"] = sorted(set(expected) - catalogue_ids)
    return out


# --- main -----------------------------------------------------------------

def parse_upstreams(specs: list[str]) -> dict[str, str]:
    out = {}
    for spec in specs:
        ver, _, path = spec.partition("=")
        if not path or not os.access(path, os.X_OK):
            raise SystemExit(f"--upstream {spec}: expected VERSION=EXECUTABLE")
        out[ver] = path
    return out


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--oc", required=True, help="oc-rsync binary under test")
    ap.add_argument("--upstream", action="append", default=[], metavar="VER=PATH")
    ap.add_argument("--expect", type=Path,
                    default=Path(__file__).with_name("interop_oracle_expect.txt"))
    ap.add_argument("--only", default="", help="comma-separated case names")
    ap.add_argument("--jobs", type=int, default=4)
    ap.add_argument("--keep", action="store_true", help="keep scratch directories")
    a = ap.parse_args(argv)
    upstream = parse_upstreams(a.upstream)
    if not upstream:
        raise SystemExit("at least one --upstream VER=PATH is required")
    cells = cells_mod.catalogue(set(upstream))
    if a.only:
        wanted = set(a.only.split(","))
        cells = [c for c in cells if c.case in wanted]
    expected = load_expectations(a.expect)
    known_ids = {c.cell_id for c in cells_mod.catalogue(cells_mod.ALL_VERSIONS)}
    scratch = Path(tempfile.mkdtemp(prefix="interop-oracle-"))
    try:
        fixtures = scratch / "fixtures"
        cells_mod.build_fixtures(fixtures)
        rsh = scratch / "rsh"
        rsh.write_text(RSH_STANDIN)
        rsh.chmod(0o755)
        t0 = time.monotonic()
        with ThreadPoolExecutor(a.jobs) as ex:
            results = list(ex.map(
                lambda c: run_cell(c, a.oc, upstream, scratch, fixtures, str(rsh)), cells))
        elapsed = time.monotonic() - t0
    finally:
        if not a.keep:
            subprocess.run(["chmod", "-R", "u+rwx", str(scratch)], check=False)
            shutil.rmtree(scratch, ignore_errors=True)
    verdict = classify(results, expected, known_ids)
    for r in results:
        tag = next(k for k in ("PASS", "XFAIL", "FAIL", "XPASS") if r.cell_id in verdict[k])
        owner = f" ({expected[r.cell_id]})" if r.cell_id in expected else ""
        print(f"{tag:5} {r.cell_id}{owner}")
        if tag == "FAIL":
            print(f"      problems: {r.problems}")
            print(f"      candidate: {r.candidate.cmd}")
            print(f"      stderr: {r.candidate.stderr[-400:]!r}")
    for sid in verdict["STALE"]:
        print(f"STALE {sid}: expectation row names no cell in the catalogue")
    counts = {k: len(v) for k, v in verdict.items()}
    print(f"oracle cells: {len(results)}  " + "  ".join(f"{k}={v}" for k, v in counts.items())
          + f"  wall={elapsed:.0f}s")
    for xp in verdict["XPASS"]:
        print(f"XPASS {xp}: now matches upstream; delete its row in {a.expect.name}")
    return 1 if verdict["FAIL"] or verdict["XPASS"] or verdict["STALE"] else 0


if __name__ == "__main__":
    sys.exit(main())
