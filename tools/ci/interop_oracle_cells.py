"""Cell catalogue and fixtures for interop_oracle.py.

A cell names one transfer shape: upstream release, transport, direction and
the role oc-rsync plays. The catalogue is a pure function of the available
upstream releases, so a cell id is stable across runs and hosts and can be
listed in the expectation file.
"""

from __future__ import annotations

import os
from dataclasses import dataclass, field
from pathlib import Path

ALL_VERSIONS = {"2.6.9", "3.0.9", "3.1.3", "3.2.7", "3.4.4", "3.5.0", "3.5.1"}

# Fixture mtimes. Both lie before interop_oracle.WALL_CLOCK_NS, so any time a
# transfer preserves is compared exactly; the basis ("pre") is older than the
# source so quick-check always sees a change.
SRC_MTIME = 1_600_000_000
PRE_MTIME = 1_500_000_000


def version_tuple(v: str) -> tuple[int, ...]:
    return tuple(int(x) for x in v.split("."))


@dataclass(frozen=True)
class Cell:
    case: str
    version: str
    transport: str  # "rsh", "daemon", or "local" (batch cells only)
    direction: str  # "push" or "pull"
    role: str  # "oc-client" or "oc-server"
    fixture: str
    opts: tuple[str, ...]
    extra_args: tuple[str, ...] = ()
    module_extra: str = ""
    timeout: float = 60
    # Upstream release that plays the client in the baseline and, for an
    # oc-server cell, in the candidate too. Defaults to `version`.
    client_version: str = ""
    # Batch cells write a batch with the client while pushing, then replay it
    # with the upstream release under test.
    batch_reader: bool = False

    @property
    def cell_id(self) -> str:
        return f"{self.case}/{self.version}/{self.transport}/{self.direction}/{self.role}"

    @property
    def slug(self) -> str:
        return self.cell_id.replace("/", "_")

    @property
    def baseline_client(self) -> str:
        return self.client_version or self.version


# Everyday transfers checked in every transport, direction and role.
CORE_CASES = (
    ("basic", "basic", ("-a",)),
    ("delete", "delete", ("-a", "--delete")),
    ("delta", "delta", ("-a", "--no-whole-file")),
    ("compress", "delta", ("-az",)),
    ("checksum", "delta", ("-ac",)),
    ("hardlinks", "hardlinks", ("-aH",)),
    ("inplace", "delta", ("-a", "--inplace")),
    ("dir-merge", "filters", ("-a", "-F")),
    ("safe-links", "symlinks", ("-a", "--safe-links")),
)


def _targeted(have: set[str]) -> list[Cell]:
    """Cells pinned to one shape because that shape is where a defect lives."""
    cells = []

    def add(version: str, **kw) -> None:
        if version in have and kw.get("client_version", version) in have:
            cells.append(Cell(version=version, **kw))

    for v in ("3.5.1",):
        add(v, case="non-utf8", transport="rsh", direction="push", role="oc-server",
            fixture="nonutf8", opts=("-a",))
        add(v, case="non-utf8", transport="rsh", direction="pull", role="oc-client",
            fixture="nonutf8", opts=("-a",))
        add(v, case="non-utf8", transport="daemon", direction="push", role="oc-server",
            fixture="nonutf8", opts=("-a",))
        add(v, case="copy-unsafe-links", transport="rsh", direction="pull", role="oc-server",
            fixture="symlinks", opts=("-a", "--copy-unsafe-links"))
        add(v, case="append-longer-dest", transport="rsh", direction="push", role="oc-server",
            fixture="append", opts=("-a", "--append"))
        add(v, case="prune-empty-dirs", transport="rsh", direction="push", role="oc-client",
            fixture="filters", opts=("-am", "--include=*/", "--include=*.txt", "--exclude=*"))
        add(v, case="max-delete", transport="rsh", direction="push", role="oc-server",
            fixture="delete", opts=("-a", "--delete", "--max-delete=1"))
        add(v, case="copy-links-dangling", transport="rsh", direction="pull", role="oc-server",
            fixture="dangling", opts=("-aL",))
        add(v, case="acls-protocol-29", transport="rsh", direction="push", role="oc-client",
            fixture="basic", opts=("-aA", "--protocol=29"))
        add(v, case="hardlinked-fifo", transport="rsh", direction="push", role="oc-server",
            fixture="fifo", opts=("-aH",))
        for t in ("rsh", "daemon"):
            add(v, case="inc-recurse-dirs", transport=t, direction="push", role="oc-client",
                fixture="incdirs", opts=("-a",))
    # Old clients against an oc server, with the 3.5.1 client as the control.
    for v in ("3.0.9", "3.5.1"):
        add(v, case="outgoing-chmod", transport="daemon", direction="pull", role="oc-server",
            fixture="basic", opts=("-a",), module_extra="  outgoing chmod = F600\n")
        add(v, case="files-from", transport="rsh", direction="pull", role="oc-server",
            fixture="filters", opts=("-a",), extra_args=("--files-from=@FIX@/files-from.txt",),
            timeout=30)
    add("2.6.9", case="prune-empty-dirs-protocol-28", transport="rsh", direction="push",
        role="oc-client", fixture="filters",
        opts=("-am", "--protocol=28", "--include=*/", "--include=*.txt", "--exclude=*"))
    # A batch written by oc while pushing must replay with the peer's release,
    # and so must a local batch written with --protocol set to the reader's
    # newest protocol, which upstream records in the batch header.
    for v, proto in (("3.1.3", 31), ("3.4.4", 32), ("3.5.0", 32), ("3.5.1", 33)):
        add(v, case="batch", transport="rsh", direction="push", role="oc-client",
            fixture="delta", opts=("-a",), batch_reader=True)
        add(v, case="batch-local", transport="local", direction="push", role="oc-client",
            fixture="delta", opts=("-a",), extra_args=(f"--protocol={proto}",),
            batch_reader=True)
    return cells


def _core_opts(opts: tuple[str, ...], version: str, direction: str, role: str) -> tuple[str, ...]:
    """Core options, minus incremental recursion where task-2520 makes the output racy.

    An oc client pushing under INC_RECURSE itemizes some directories from a
    segment it has already reclaimed, so whether a directory prints as `cd`
    or as an empty-named `cf` depends on timing. A cell that passes or fails
    by timing cannot carry an expectation row, so the core oc-client push
    cells turn incremental recursion off and the deterministic
    `inc-recurse-dirs` cell carries the defect instead. Delete this when
    task-2520 lands. rsync < 3.0 has neither the feature nor the option.
    """
    if role == "oc-client" and direction == "push" and version_tuple(version) >= (3, 0):
        return opts + ("--no-inc-recursive",)
    return opts


def catalogue(have: set[str]) -> list[Cell]:
    """Every cell runnable with the upstream releases in `have`."""
    cells = [
        Cell(case=case, version=v, transport=t, direction=d, role=r, fixture=fx,
             opts=_core_opts(opts, v, d, r))
        for v in sorted(have, key=version_tuple)
        for case, fx, opts in CORE_CASES
        for t in ("rsh", "daemon")
        for d in ("push", "pull")
        for r in ("oc-client", "oc-server")
    ]
    cells += _targeted(have)
    ids = [c.cell_id for c in cells]
    assert len(ids) == len(set(ids)), "duplicate cell ids"
    return cells


# --- fixtures -------------------------------------------------------------

def _write(path: Path, data: bytes | str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data.encode() if isinstance(data, str) else data)


def _stamp(root: Path, mtime: int) -> None:
    """Sets every entry below `root` (deepest first) to one fixed mtime."""
    for dirpath, dirs, files in os.walk(os.fsencode(str(root)), topdown=False):
        for name in files + dirs:
            os.utime(os.path.join(dirpath, name), (mtime, mtime), follow_symlinks=False)
    os.utime(root, (mtime, mtime))


def _blob(seed: int, size: int) -> bytes:
    """Deterministic pseudo-random bytes (a 64-bit LCG), stable across Python versions."""
    out = bytearray()
    x = seed
    while len(out) < size:
        x = (x * 6364136223846793005 + 1442695040888963407) & (2**64 - 1)
        out += x.to_bytes(8, "little")
    return bytes(out[:size])


def build_fixtures(root: Path) -> None:
    """Creates every fixture as `<root>/<name>/src` plus an optional `pre` basis."""
    fx = {}

    b = root / "basic"
    _write(b / "src/a.txt", "alpha\n")
    _write(b / "src/sub/b.txt", "bravo\n" * 50)
    _write(b / "src/sub/deep/c.bin", _blob(1, 5000))
    (b / "src/empty").mkdir(parents=True)
    os.symlink("sub/b.txt", b / "src/link")
    fx["basic"] = b

    d = root / "delete"
    _write(d / "src/keep.txt", "keep\n")
    _write(d / "src/dir/keep2.txt", "keep2\n")
    _write(d / "pre/keep.txt", "old\n")
    _write(d / "pre/gone1.txt", "x\n")
    _write(d / "pre/gone2.txt", "y\n")
    _write(d / "pre/dir/gone3.txt", "z\n")
    fx["delete"] = d

    dl = root / "delta"
    base = _blob(2, 200_000)
    changed = bytearray(base)
    changed[50_000:50_100] = _blob(3, 100)
    _write(dl / "src/big.bin", bytes(changed) + _blob(4, 3000))
    _write(dl / "src/small.txt", "small file\n")
    _write(dl / "pre/big.bin", base)
    _write(dl / "pre/small.txt", "older\n")
    fx["delta"] = dl

    h = root / "hardlinks"
    _write(h / "src/one", "linked\n")
    os.link(h / "src/one", h / "src/two")
    _write(h / "src/sub/three", "other\n")
    os.link(h / "src/sub/three", h / "src/four")
    fx["hardlinks"] = h

    f = root / "filters"
    _write(f / "src/top.txt", "t\n")
    _write(f / "src/top.tmp", "tmp\n")
    _write(f / "src/a/one.txt", "1\n")
    _write(f / "src/a/one.o", "obj\n")
    _write(f / "src/a/.rsync-filter", "- *.o\n")
    _write(f / "src/b/only.tmp", "tmp\n")
    (f / "src/c/empty").mkdir(parents=True)
    _write(f / "files-from.txt", "top.txt\na/one.txt\n")
    fx["filters"] = f

    s = root / "symlinks"
    _write(s / "outside/secret", "outside the tree\n")
    _write(s / "src/in.txt", "inside\n")
    os.symlink("in.txt", s / "src/safe")
    os.symlink("../outside/secret", s / "src/up")
    os.symlink(str(s / "outside/secret"), s / "src/abs")
    fx["symlinks"] = s

    n = root / "nonutf8"
    _write(n / b"src/na\xefve/a.txt".decode("utf-8", "surrogateescape"), "in latin1 dir\n")
    _write(n / b"src/f\xef".decode("utf-8", "surrogateescape"), "f\n")
    _write(n / b"src/g\xef".decode("utf-8", "surrogateescape"), "g\n")
    _write(n / "src/ok", "ok\n")
    fx["nonutf8"] = n

    a = root / "append"
    _write(a / "src/grow", "0123456789")
    _write(a / "pre/grow", "0123456789" + "longer destination\n")
    fx["append"] = a

    g = root / "dangling"
    _write(g / "src/real.txt", "real\n")
    os.symlink("does-not-exist", g / "src/dangling")
    fx["dangling"] = g

    # Several sibling directories, each with content, so every sub-list after
    # the first is served once its parent segment has been reclaimed.
    ic = root / "incdirs"
    for name in "abcdef":
        _write(ic / f"src/{name}/f", f"{name}\n")
        _write(ic / f"src/{name}/sub/g", f"{name}{name}\n")
    fx["incdirs"] = ic

    ff = root / "fifo"
    _write(ff / "src/plain", "plain\n")
    os.mkfifo(ff / "src/pipe")
    os.link(ff / "src/pipe", ff / "src/pipe2")
    fx["fifo"] = ff

    for path in fx.values():
        _stamp(path / "src", SRC_MTIME)
        if (path / "pre").is_dir():
            _stamp(path / "pre", PRE_MTIME)
