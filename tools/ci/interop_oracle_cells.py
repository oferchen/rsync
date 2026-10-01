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
    # Protocol both ends must negotiate, checked on the baseline too; 0 = unchecked.
    expect_proto: int = 0
    # Touched-blocks stats value both runs must print ("absent" = no line); "" = unchecked.
    expect_touched: str = ""
    # Incremental recursion requested: "on" (the default) or "off"
    # (--no-inc-recursive); "" = unchecked. See expected_inc().
    inc: str = ""

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
    # No 3.0.9 outgoing-chmod cell: 3.0.9's parse_chmod has no octal state
    # (chmod.c:140-141), so its own daemon ignores F600 and the baseline can
    # never match the 3.1.3+ behaviour oc mirrors.
    add("3.5.1", case="outgoing-chmod", transport="daemon", direction="pull", role="oc-server",
        fixture="basic", opts=("-a",), module_extra="  outgoing chmod = F600\n")
    # Old clients against an oc server, with the 3.5.1 client as the control.
    for v in ("3.0.9", "3.5.1"):
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


# --- protocol 33 ----------------------------------------------------------
#
# rsync 3.5.1 bumped PROTOCOL_VERSION to 33 (rsync.h:114) for one feature: the
# receiver counts the 4 KiB logical blocks it writes (fileio.c:218
# track_block_touches), sends the count in MSG_BLOCK_STATS (main.c:1113), the
# server generator relays it to a client sender (io.c:1721-1731), and --stats
# prints it only at protocol >= 33 (main.c:446). Each case mirrors a scenario
# of upstream's testsuite/write-touched-blocks_test.py and pins its value.

DELTA_INPLACE = ("-a", "--inplace", "-I", "--no-whole-file")
TOUCHED_CASES = (
    ("touched-contiguous", "tb-contiguous", DELTA_INPLACE, "1"),
    ("touched-scattered", "tb-scattered", DELTA_INPLACE, "10"),
    ("touched-identical", "tb-identical", DELTA_INPLACE, "0"),
    ("touched-full", "tb-full", DELTA_INPLACE, "1,024"),
    ("touched-sparse", "tb-sparse", ("-a", "--sparse"), "2"),
    ("touched-multi-file", "tb-multi", ("-a", "--inplace"), "2"),
    # Several directories, so INC_RECURSE sends several sub-lists. The block
    # size divides 4 KiB, so each one-byte edit rewrites exactly one block.
    ("touched-tree", "tb-tree", DELTA_INPLACE + ("--block-size=1024",), "32"),
)
SHAPES = tuple((t, d, r) for t in ("rsh", "daemon") for d in ("push", "pull")
               for r in ("oc-client", "oc-server"))
INC_MODES = (("on", "", ()), ("off", "-no-inc", ("--no-inc-recursive",)))


def expected_inc(cell: Cell, candidate: bool) -> bool:
    """Whether INC_RECURSE is negotiated on the wire for a cell's baseline or candidate run.

    Upstream negotiates it whenever the client did not turn it off (compat.c:
    set_allow_inc_recurse, compat.c:724 on the server). oc never negotiates
    it when oc is the receiver: an oc client does not put 'i' in its -e
    string on a pull, and an oc server receiver does not set CF_INC_RECURSE on
    a push, so those candidates run without it whatever was requested. The
    cell pins that, so the day oc starts negotiating it the cell fails and
    this line is deleted with the change.
    """
    oc_receives = cell.role == ("oc-client" if cell.direction == "pull" else "oc-server")
    return cell.inc == "on" and not (candidate and oc_receives)


def _proto33(have: set[str]) -> list[Cell]:
    """Protocol-33 cells: negotiation and the touched-blocks count, in every shape.

    3.5.1 must negotiate 33 and print the count upstream prints. A 3.5.0 peer
    and a 3.5.1 peer held to --protocol=32 are the controls: 32 on the wire,
    and no stats line. The --protocol=32 is given to whichever client runs,
    so in the oc-client cells it is oc that asks for 32. Every cell runs with
    incremental recursion negotiated and with --no-inc-recursive, and checks
    which one the wire carried.
    """
    cells = []
    for inc, suffix, inc_opts in INC_MODES:
        def add(case: str, version: str, fixture: str, opts: tuple[str, ...], proto: int,
                touched: str, shapes=SHAPES, **kw) -> None:
            if version in have:
                cells.extend(Cell(case=case + suffix, version=version, transport=t,
                                  direction=d, role=r, fixture=fixture, opts=opts + inc_opts,
                                  expect_proto=proto, expect_touched=touched, inc=inc, **kw)
                             for t, d, r in shapes)

        for case, fx, opts, touched in TOUCHED_CASES:
            add(case, "3.5.1", fx, opts, 33, touched)
        add("touched-protocol-32", "3.5.1", "tb-scattered", DELTA_INPLACE + ("--protocol=32",),
            32, "absent")
        # -M--protocol=32 holds the oc server itself to 32, which the server
        # must parse (options.c popt table) rather than take for a path. Not
        # over a daemon: its greeting has fixed the protocol before the args.
        add("touched-protocol-32-remote", "3.5.1", "tb-scattered",
            DELTA_INPLACE + ("-M--protocol=32",), 32, "absent",
            shapes=[s for s in SHAPES if s[0] == "rsh" and s[2] == "oc-server"])
        add("touched-scattered", "3.5.0", "tb-scattered", DELTA_INPLACE, 32, "absent")
        # A batch header records the writer's negotiated protocol (io.c:2754)
        # and the replay adopts it (compat.c:602-615): oc writes and upstream
        # replays, and the reverse.
        batch_shapes = [("rsh", "push", r) for r in ("oc-client", "oc-server")]
        add("touched-batch", "3.5.1", "tb-scattered", DELTA_INPLACE, 33, "10",
            shapes=batch_shapes, batch_reader=True)
        add("touched-batch", "3.5.0", "tb-scattered", DELTA_INPLACE, 32, "absent",
            shapes=batch_shapes, batch_reader=True)
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
    cells += _proto33(have)
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

    fx.update(_touched_fixtures(root))

    for path in fx.values():
        _stamp(path / "src", SRC_MTIME)
        if (path / "pre").is_dir():
            _stamp(path / "pre", PRE_MTIME)


def _touched_fixtures(root: Path) -> dict[str, Path]:
    """Fixtures of upstream's write-touched-blocks test: a 4 MiB basis and one edit each."""
    mib4 = 4 * 1024 * 1024
    base = _blob(20, mib4)
    fx = {}

    def delta(name: str, data: bytes) -> None:
        _write(root / name / "src/base.bin", data)
        _write(root / name / "pre/base.bin", base)
        fx[name] = root / name

    contiguous = bytearray(base)
    contiguous[:3000] = bytes(3000)
    delta("tb-contiguous", bytes(contiguous))
    scattered = bytearray(base)
    for i in range(1, 11):
        scattered[i * 4096] ^= 0xFF
    delta("tb-scattered", bytes(scattered))
    delta("tb-identical", base)
    delta("tb-full", _blob(21, mib4))

    # One data block, a 4 MiB hole, one data block: only the two data blocks
    # are written (fileio.c:169-180 skip the hole with a seek).
    sp = root / "tb-sparse/src/sparse.bin"
    sp.parent.mkdir(parents=True)
    with open(sp, "wb") as f:
        f.write(_blob(22, 4096))
        f.seek(mib4, os.SEEK_CUR)
        f.write(_blob(23, 4096))
    fx["tb-sparse"] = root / "tb-sparse"

    # Two one-block files: a tracker not reset per file would count 1.
    _write(root / "tb-multi/src/fileA.bin", _blob(24, 4096))
    _write(root / "tb-multi/src/fileB.bin", _blob(25, 4096))
    fx["tb-multi"] = root / "tb-multi"

    # 8 directories of two levels: 16 files with one edited block each and 8
    # new two-block files, 16 + 16 = 32 blocks.
    for d in range(8):
        for sub in (f"d{d}", f"d{d}/sub"):
            n = d * 2 + sub.count("/")
            old = _blob(30 + n, 64 * 1024)
            new = bytearray(old)
            new[(d + 1) * 4096] ^= 0xFF
            _write(root / f"tb-tree/pre/{sub}/f.bin", old)
            _write(root / f"tb-tree/src/{sub}/f.bin", bytes(new))
        _write(root / f"tb-tree/src/d{d}/new.bin", _blob(60 + d, 8192))
    fx["tb-tree"] = root / "tb-tree"
    return fx
