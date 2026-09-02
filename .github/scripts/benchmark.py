#!/usr/bin/env python3
"""Run benchmark suite comparing oc-rsync against upstream rsync.

Tests local copy, SSH (push + pull), and daemon (push + pull) modes against
every upstream baseline named in `UPSTREAM_RSYNC`, and outputs JSON results
to stdout.

Two upstream releases are in circulation at once -- the current one and the
one distributions still ship -- so a single-baseline comparison answers only
half the question a release note is asked. Every upstream cell therefore runs
once per baseline, and the results JSON carries a per-baseline dimension.
"""

import json
import os
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass

import benchmark_env

OC_RSYNC = "target/release/oc-rsync"


@dataclass(frozen=True)
class Baseline:
    """One upstream rsync build to compare against.

    `label` names the release in the results JSON, the report and the chart;
    `path` is absolute because the SSH cells pass it to the remote end via
    `--rsync-path`, where the working directory is the login shell's, not the
    repository's.
    """

    label: str
    path: str

    @property
    def slug(self) -> str:
        """Filesystem-safe form of the label, for per-baseline directories."""
        return re.sub(r"[^A-Za-z0-9._-]", "_", self.label)


def _version_output(binary, *args):
    """Capture a binary's version output, or `""` if it cannot be probed."""
    try:
        return subprocess.run(
            [binary, *args], capture_output=True, timeout=10, text=True,
        ).stdout
    except (OSError, subprocess.SubprocessError):
        return ""


def binary_version(binary):
    """Return the `x.y.z` release number reported by an upstream rsync build.

    Upstream's banner is `rsync  version 3.5.0  protocol version 32`, so the
    first release-shaped number in it is the release. This is deliberately
    *not* used for oc-rsync: see `oc_rsync_versions` for why that binary needs
    its own parser.
    """
    out = _version_output(binary, "--version")
    match = re.search(r"\b(\d+\.\d+\.\d+)\b", out)
    return match.group(1) if match else ""


def oc_rsync_versions(binary):
    """Return `(release, wire_compat)` for an oc-rsync build.

    oc-rsync's banner leads with its own release and then names the upstream
    wire protocol it speaks:

        oc-rsync v0.6.4 (revision #9d99155e1) protocol version 32
        Compatible with rsync 3.4.4 wire protocol

    `binary_version`'s "first release-shaped number" rule cannot read this.
    `\\b` does not match between `v` and `0`, so `v0.6.4` is skipped entirely
    and the first number the pattern accepts is the *compatibility* version --
    which is how a published release recorded `oc_rsync_version = '3.4.4'` for
    a 0.6.4 build. The two numbers are different facts and are read from
    different places: the release from the machine-readable `-VV` document
    that exists for exactly this purpose, the compatibility version from the
    banner line that states it.
    """
    release = ""
    try:
        release = json.loads(_version_output(binary, "-VV") or "{}").get(
            "version", ""
        )
    except ValueError:
        release = ""
    banner = _version_output(binary, "--version")
    if not release:
        match = re.search(r"^\S+\s+v(\d+\.\d+\.\d+)", banner, re.M)
        release = match.group(1) if match else ""
        if not release:
            print(
                f"WARNING: cannot read oc-rsync's own release from {binary}",
                file=sys.stderr,
            )
    compat = re.search(
        r"Compatible with rsync (\d+\.\d+\.\d+) wire protocol", banner
    )
    return release, (compat.group(1) if compat else "")


def parse_baselines(spec):
    """Parse `UPSTREAM_RSYNC` into an ordered list of baselines.

    Accepts a comma-separated list of `label=path` entries, or bare paths
    whose label is then taken from the binary's own `--version` output. One
    bare path is the historical single-baseline form and still works, so a
    local run and CI drive the same knob.

    The first entry is the primary baseline: it backs the legacy `upstream`
    and `ratio` fields in the results JSON, serves the SSH and daemon cells
    that are not run per baseline, and is the release the headline summary is
    stated against.
    """
    baselines = []
    seen = set()
    for item in spec.split(","):
        item = item.strip()
        if not item:
            continue
        label, sep, path = item.partition("=")
        if not sep:
            path, label = label, ""
        path = os.path.expanduser(path)
        # A bare command name means "whatever PATH resolves", which is how
        # some callers name the system rsync. Resolve it here rather than
        # treating it as a relative path, because the SSH cells hand this
        # value to a remote shell that has its own working directory.
        if os.sep not in path:
            path = shutil.which(path) or path
        path = os.path.abspath(path)
        label = label.strip() or binary_version(path) or os.path.basename(path)
        if label in seen:
            sys.exit(f"duplicate baseline label {label!r} in UPSTREAM_RSYNC")
        seen.add(label)
        baselines.append(Baseline(label, path))
    return baselines


BASELINES = parse_baselines(os.environ.get("UPSTREAM_RSYNC", ""))
if not BASELINES:
    sys.exit(
        "UPSTREAM_RSYNC is not set: name the upstream rsync binaries to "
        "compare against as a comma-separated list, e.g. "
        "'3.5.0=target/interop/upstream-src/rsync-3.5.0/rsync,"
        "3.4.4=target/interop/upstream-src/rsync-3.4.4/rsync'. "
        "The caller that builds those binaries owns the versions, so this "
        "script does not name one."
    )
for _b in BASELINES:
    if not os.path.isfile(_b.path):
        sys.exit(f"upstream baseline {_b.label!r} not found at {_b.path}")

PRIMARY = BASELINES[0]
OC_RSYNC_OPENSSL = os.environ.get("OC_RSYNC_OPENSSL", "")
OC_RSYNC_RUSSH = os.environ.get("OC_RSYNC_RUSSH", "")
IS_LINUX = sys.platform.startswith("linux")

TESTS = [
    # Local copy
    {
        "id": "local_initial",
        "name": "Initial sync",
        "mode": "local",
        "args": "-av {src}/ {dst}/",
        "reset": True,
    },
    {
        "id": "local_nochange",
        "name": "No-change sync",
        "mode": "local",
        "args": "-av {src}/ {dst}/",
        "reset": False,
    },
    {
        "id": "local_checksum",
        "name": "Checksum sync",
        "mode": "local",
        "args": "-avc {src}/ {dst}/",
        "reset": False,
    },
    # SSH pull (local=receiver, remote=sender)
    {
        "id": "ssh_pull_initial",
        "name": "Initial sync",
        "mode": "ssh_pull",
        "args": "-av --timeout=30 localhost:{src}/ {dst}/",
        "reset": True,
    },
    {
        "id": "ssh_pull_nochange",
        "name": "No-change sync",
        "mode": "ssh_pull",
        "args": "-av --timeout=30 localhost:{src}/ {dst}/",
        "reset": False,
    },
    # SSH push (local=sender, remote=receiver)
    {
        "id": "ssh_push_initial",
        "name": "Initial sync",
        "mode": "ssh_push",
        "args": "-av --timeout=30 {src}/ localhost:{dst}/",
        "reset": True,
    },
    {
        "id": "ssh_push_nochange",
        "name": "No-change sync",
        "mode": "ssh_push",
        "args": "-av --timeout=30 {src}/ localhost:{dst}/",
        "reset": False,
    },
    # Daemon pull
    {
        "id": "daemon_pull_initial",
        "name": "Initial sync",
        "mode": "daemon_pull",
        "args": "-av --timeout=30 rsync://localhost:{port}/bench/ {dst}/",
        "reset": True,
    },
    {
        "id": "daemon_pull_nochange",
        "name": "No-change sync",
        "mode": "daemon_pull",
        "args": "-av --timeout=30 rsync://localhost:{port}/bench/ {dst}/",
        "reset": False,
    },
    # Daemon push
    {
        "id": "daemon_push_initial",
        "name": "Initial sync",
        "mode": "daemon_push",
        "args": "-av --timeout=30 {src}/ rsync://localhost:{port}/dest/",
        "reset": True,
    },
    {
        "id": "daemon_push_nochange",
        "name": "No-change sync",
        "mode": "daemon_push",
        "args": "-av --timeout=30 {src}/ rsync://localhost:{port}/dest/",
        "reset": False,
    },
]

# OpenSSL vs pure-Rust checksum comparison (only run if OC_RSYNC_OPENSSL is set)
OPENSSL_TESTS = [
    {
        "id": "openssl_checksum_initial",
        "name": "Initial checksum sync",
        "mode": "checksum_openssl",
        "args": "-avc {src}/ {dst}/",
        "reset": True,
    },
    {
        "id": "openssl_checksum_nochange",
        "name": "No-change checksum sync",
        "mode": "checksum_openssl",
        "args": "-avc {src}/ {dst}/",
        "reset": False,
    },
]

# SSH transport comparison: upstream rsync over OpenSSH subprocess, oc-rsync
# over OpenSSH subprocess (`host:path` operand), and oc-rsync over embedded
# russh (`ssh://host/path` URI operand). Only run if OC_RSYNC_RUSSH is set.
# The default oc-rsync binary handles the subprocess form; the russh-built
# binary handles the URI form via the embedded transport. Upstream rsync is
# always invoked through OpenSSH.
RUSSH_TESTS = [
    {
        "id": "ssh_transport_pull_initial",
        "name": "Initial pull",
        "mode": "ssh_transport",
        "upstream_args": "-av --timeout=30 localhost:{src}/ {dst}/",
        "subprocess_args": "-av --timeout=30 localhost:{src}/ {dst}/",
        "russh_args": "-av --timeout=30 ssh://localhost{src}/ {dst}/",
        "reset": True,
    },
    {
        "id": "ssh_transport_pull_nochange",
        "name": "No-change pull",
        "mode": "ssh_transport",
        "upstream_args": "-av --timeout=30 localhost:{src}/ {dst}/",
        "subprocess_args": "-av --timeout=30 localhost:{src}/ {dst}/",
        "russh_args": "-av --timeout=30 ssh://localhost{src}/ {dst}/",
        "reset": False,
    },
    {
        "id": "ssh_transport_push_initial",
        "name": "Initial push",
        "mode": "ssh_transport",
        "upstream_args": "-av --timeout=30 {src}/ localhost:{dst}/",
        "subprocess_args": "-av --timeout=30 {src}/ localhost:{dst}/",
        "russh_args": "-av --timeout=30 {src}/ ssh://localhost{dst}/",
        "reset": True,
    },
    {
        "id": "ssh_transport_push_nochange",
        "name": "No-change push",
        "mode": "ssh_transport",
        "upstream_args": "-av --timeout=30 {src}/ localhost:{dst}/",
        "subprocess_args": "-av --timeout=30 {src}/ localhost:{dst}/",
        "russh_args": "-av --timeout=30 {src}/ ssh://localhost{dst}/",
        "reset": False,
    },
]


COMPRESSION_TESTS = [
    {
        "id": "compress_zlib_initial",
        "name": "zlib initial sync",
        "mode": "compression",
        "args": "-avz {src}/ {dst}/",
        "reset": True,
    },
    {
        "id": "compress_zlib_nochange",
        "name": "zlib no-change sync",
        "mode": "compression",
        "args": "-avz {src}/ {dst}/",
        "reset": False,
    },
    {
        "id": "compress_zstd_initial",
        "name": "zstd initial sync",
        "mode": "compression",
        "args": "-av --compress-choice=zstd {src}/ {dst}/",
        "reset": True,
    },
    {
        "id": "compress_zstd_nochange",
        "name": "zstd no-change sync",
        "mode": "compression",
        "args": "-av --compress-choice=zstd {src}/ {dst}/",
        "reset": False,
    },
]

DELTA_TESTS = [
    {
        "id": "delta_local",
        "name": "Local delta sync",
        "mode": "delta",
        "args": "-av {src}/ {dst}/",
    },
    {
        "id": "delta_checksum",
        "name": "Local delta checksum sync",
        "mode": "delta",
        "args": "-avc {src}/ {dst}/",
    },
]

LARGE_FILE_TESTS = [
    {
        "id": "large_file_initial",
        "name": "1GB file initial sync",
        "mode": "large_file",
        "args": "-av {src}/ {dst}/",
        "reset": True,
    },
    {
        "id": "large_file_nochange",
        "name": "1GB file no-change sync",
        "mode": "large_file",
        "args": "-av {src}/ {dst}/",
        "reset": False,
    },
    {
        "id": "large_file_delta",
        "name": "1GB file delta sync",
        "mode": "large_file",
        "args": "-av {src}/ {dst}/",
        "reset": False,
    },
]

MANY_SMALL_FILES_TESTS = [
    {
        "id": "many_small_initial",
        "name": "100K files initial sync",
        "mode": "many_small",
        "args": "-av {src}/ {dst}/",
        "reset": True,
    },
    {
        "id": "many_small_nochange",
        "name": "100K files no-change sync",
        "mode": "many_small",
        "args": "-av {src}/ {dst}/",
        "reset": False,
    },
]

SPARSE_TESTS = [
    {
        "id": "sparse_initial",
        "name": "Sparse initial sync",
        "mode": "sparse",
        "args": "-avS {src}/ {dst}/",
        "reset": True,
    },
    {
        "id": "sparse_nochange",
        "name": "Sparse no-change sync",
        "mode": "sparse",
        "args": "-avS {src}/ {dst}/",
        "reset": False,
    },
]


def find_free_port():
    """Find an available TCP port."""
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("", 0))
        return s.getsockname()[1]


def wait_for_port(port, timeout=10):
    """Block until a TCP port accepts connections."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            with socket.create_connection(("localhost", port), timeout=1):
                return True
        except OSError:
            time.sleep(0.25)
    return False


PER_RUN_TIMEOUT = 600  # seconds per individual rsync invocation

# `/usr/bin/time` is what supplies peak RSS. Probe for it once rather than
# wrapping every command and discovering per run that the wrapper itself is
# missing, which would turn a missing memory column into a suite of failed
# transfers.
HAVE_TIME_CMD = os.access("/usr/bin/time", os.X_OK)


def parse_peak_rss_kb(stderr_text):
    """Extract peak RSS in KB from /usr/bin/time output.

    Linux (-v): 'Maximum resident set size (kbytes): 12345'
    macOS (-l): '12345  maximum resident set size' (bytes, convert to KB)
    """
    m = re.search(r"Maximum resident set size \(kbytes\):\s*(\d+)", stderr_text)
    if m:
        return int(m.group(1))
    m = re.search(r"(\d+)\s+maximum resident set size", stderr_text)
    if m:
        return int(m.group(1)) // 1024
    return None


def benchmark_rss(cmd, runs=4, before_each=None):
    """Run a command with /usr/bin/time and return timing + peak RSS stats."""
    time_flag = "-v" if IS_LINUX else "-l"
    wrapped = f"/usr/bin/time {time_flag} {cmd}"
    times = []
    rss_values = []
    failures = 0
    for i in range(runs):
        if before_each is not None:
            before_each()
        start = time.perf_counter()
        try:
            result = subprocess.run(
                wrapped, shell=True, capture_output=True, timeout=PER_RUN_TIMEOUT,
            )
            elapsed = time.perf_counter() - start
            stderr = result.stderr.decode(errors="replace")
            if result.returncode != 0:
                failures += 1
                print(f"WARNING: exit {result.returncode}: {cmd}", file=sys.stderr)
                if stderr.strip():
                    print(f"  stderr: {stderr[:200]}", file=sys.stderr)
            rss = parse_peak_rss_kb(stderr)
            if rss is not None:
                rss_values.append(rss)
        except subprocess.TimeoutExpired:
            failures += 1
            elapsed = time.perf_counter() - start
            print(
                f"ERROR: timeout after {PER_RUN_TIMEOUT}s (run {i+1}/{runs}): {cmd}",
                file=sys.stderr,
            )
        times.append(elapsed)
    result = stats(times, failures)
    if rss_values:
        result["peak_rss_kb"] = max(rss_values)
        result["avg_rss_kb"] = sum(rss_values) // len(rss_values)
    return result


def representative_secs(times):
    """Median of the timed runs after dropping the first (warm-up) run.

    The median resists the occasional cold-cache or scheduler outlier that a
    plain mean lets dominate a small sample, and dropping the warm-up run
    removes first-touch page-cache and connection-setup cost that does not
    reflect steady-state throughput. On a shared CI runner this is the main
    lever against the ratio noise that made small-workload cells swing wildly.
    """
    timed = times[1:] if len(times) > 1 else times
    ordered = sorted(timed)
    n = len(ordered)
    mid = n // 2
    return ordered[mid] if n % 2 else (ordered[mid - 1] + ordered[mid]) / 2


def stats(times, failures=0):
    """Timing statistics for one series of runs.

    `spread_pct` states how far the fastest and slowest runs are apart as a
    fraction of the representative value. A ratio built from two medians can
    look decisive while resting on runs that varied by half their own
    magnitude; publishing the spread next to the median is what makes a noisy
    cell visible instead of hidden. `runs` is recorded so a reader can tell a
    three-run cell from a six-run one.

    `failures` counts runs the binary did not complete. A command that exits
    immediately with an error is otherwise indistinguishable from one that
    finished the work in a millisecond -- an upstream build without zstd
    support rejects `--compress-choice=zstd` in about a millisecond and was
    duly recorded as oc-rsync being 581x slower. Counting the failures is what
    lets the summary refuse to average such a cell.
    """
    mean = representative_secs(times)
    lo, hi = min(times), max(times)
    result = {
        "mean": mean,
        "min": lo,
        "max": hi,
        "runs": len(times),
        "spread_pct": round((hi - lo) / mean * 100, 1) if mean > 0 else 0.0,
    }
    if failures:
        result["failures"] = failures
    return result


def series_failed(series):
    """True when any run in this series did not complete."""
    return bool(series.get("failures"))


def benchmark(cmd, runs=6, before_each=None):
    """Run a command multiple times and return timing statistics.

    Reports the warm-up-discarded median under `mean` (kept under that key so
    the report/chart consumers are unchanged); `min`/`max` span every run.

    `before_each` runs before every timed invocation, not once before the
    series. A cell that resets its destination only once measures an initial
    transfer in run 1 and a no-change transfer in runs 2..n -- and
    `representative_secs` discards run 1 as the warm-up, so the published
    "Initial sync" figure was the median of the no-change runs. Resetting per
    run is what makes an initial-transfer cell measure an initial transfer.
    """
    times = []
    failures = 0
    for i in range(runs):
        if before_each is not None:
            before_each()
        start = time.perf_counter()
        try:
            result = subprocess.run(
                cmd, shell=True, capture_output=True, timeout=PER_RUN_TIMEOUT,
            )
            elapsed = time.perf_counter() - start
            if result.returncode != 0:
                failures += 1
                print(
                    f"WARNING: exit {result.returncode}: {cmd}",
                    file=sys.stderr,
                )
                stderr = result.stderr.decode(errors="replace").strip()
                if stderr:
                    print(f"  stderr: {stderr[:200]}", file=sys.stderr)
        except subprocess.TimeoutExpired:
            failures += 1
            elapsed = time.perf_counter() - start
            print(
                f"ERROR: timeout after {PER_RUN_TIMEOUT}s (run {i+1}/{runs}): {cmd}",
                file=sys.stderr,
            )
        times.append(elapsed)
    return stats(times, failures)


MIB = 1024.0 * 1024.0


def add_throughput(result, corpus_bytes):
    """Annotate one series with the corpus rate it sustained, in MiB/s.

    Defined as *corpus size / elapsed*, not bytes-on-the-wire: a no-change
    sync moves almost nothing yet still has to stat and compare the whole
    tree, and the rate a user cares about there is how fast the tool gets
    through the tree. The harness knows the corpus size exactly; it does not
    parse rsync's own transferred-byte accounting, so no figure here claims to
    be wire throughput.
    """
    if corpus_bytes and result.get("mean", 0) > 0:
        result["corpus_mibps"] = round(corpus_bytes / result["mean"] / MIB, 1)
    return result


def compare(
    results,
    test_id,
    name,
    mode,
    up_cmd,
    oc_cmd,
    *,
    runs=6,
    runner=None,
    up_before=None,
    oc_before=None,
    oc_per_baseline=False,
    corpus_bytes=None,
):
    """Time oc-rsync against every upstream baseline and record one row.

    `up_cmd` and `oc_cmd` take a `Baseline` and return the command line to
    time, so a cell whose peer version matters (SSH, daemon) can point both
    sides at the same release while a purely local cell ignores the argument.

    `oc_per_baseline` says whether oc-rsync has to be re-timed for each
    baseline. It must be true wherever the baseline is the *peer* -- an SSH
    server or an rsync daemon -- because comparing oc-against-3.5.0 with
    upstream-3.4.4-against-3.4.4 would credit or blame the client for a
    difference in the server. Where the baseline is only the other contestant
    in a local copy, oc-rsync is timed once and shared.

    The row keeps the single-baseline `upstream`/`ratio` fields pointing at
    the primary baseline so every existing consumer of this JSON still reads
    a coherent comparison, and adds `upstreams`/`ratios` keyed by label.
    """
    runner = runner or benchmark
    upstreams = {}
    oc_results = {}
    shared_oc = None

    for baseline in BASELINES:
        print(f"  baseline {baseline.label}...", file=sys.stderr)
        upstreams[baseline.label] = runner(
            up_cmd(baseline),
            runs=runs,
            before_each=(lambda b=baseline: up_before(b)) if up_before else None,
        )
        if oc_per_baseline or shared_oc is None:
            measured = runner(
                oc_cmd(baseline),
                runs=runs,
                before_each=(
                    (lambda b=baseline: oc_before(b)) if oc_before else None
                ),
            )
            if not oc_per_baseline:
                shared_oc = measured
        else:
            measured = shared_oc
        oc_results[baseline.label] = measured

    oc_primary = oc_results[PRIMARY.label]
    ratios = {
        label: (
            oc_results[label]["mean"] / up["mean"] if up["mean"] > 0 else 0.0
        )
        for label, up in upstreams.items()
    }

    if corpus_bytes:
        for series in list(upstreams.values()) + list(oc_results.values()):
            add_throughput(series, corpus_bytes)

    row = {
        "id": test_id,
        "name": name,
        "mode": mode,
        "upstreams": upstreams,
        "ratios": {label: round(r, 2) for label, r in ratios.items()},
        # Legacy single-baseline shape, pinned to the primary baseline.
        "upstream": upstreams[PRIMARY.label],
        "oc_rsync": oc_primary,
        "ratio": round(ratios[PRIMARY.label], 2),
    }
    # A ratio against a binary that errored out is not a comparison. Name the
    # series that failed so the report can say which side broke, and so the
    # summary can leave the row out of its averages instead of reporting a
    # 581x regression that is really a missing zstd in the upstream build.
    failed = [
        label for label, s in upstreams.items() if series_failed(s)
    ] + [
        f"oc-rsync/{label}"
        for label, s in oc_results.items()
        if series_failed(s)
    ]
    if failed:
        row["failed_series"] = sorted(set(failed))
    if oc_per_baseline:
        row["oc_rsync_per_baseline"] = oc_results
    if corpus_bytes:
        row["corpus_bytes"] = corpus_bytes
    results["tests"].append(row)
    return row


def tree_size(path):
    """Total byte size of every regular file under `path`."""
    return sum(
        os.path.getsize(os.path.join(dp, f))
        for dp, _, fn in os.walk(path)
        for f in fn
    )


def wipe(*paths):
    """Recreate each path as an empty directory."""
    for path in paths:
        shutil.rmtree(path, ignore_errors=True)
        os.makedirs(path, exist_ok=True)


def main():
    tmpdir = tempfile.mkdtemp(prefix="rsync_bench_")
    daemons = []
    results = {"tests": [], "summary": {}}

    try:
        src = f"{tmpdir}/src"
        # One destination per baseline: a shared destination would leave the
        # second baseline's "initial" transfer facing a tree the first
        # baseline had already written.
        dst_up = {b.label: f"{tmpdir}/dst_upstream_{b.slug}" for b in BASELINES}
        dst_oc = f"{tmpdir}/dst_oc"
        dst_oc_per = {b.label: f"{tmpdir}/dst_oc_{b.slug}" for b in BASELINES}
        daemon_dst = {b.label: f"{tmpdir}/daemon_dest_{b.slug}" for b in BASELINES}

        os.makedirs(f"{src}/small", exist_ok=True)
        os.makedirs(f"{src}/medium", exist_ok=True)
        os.makedirs(f"{src}/large", exist_ok=True)

        # Create test data
        print("Creating test data...", file=sys.stderr)

        # Small files (9500 x 1KB = ~9.5 MB)
        for i in range(9500):
            with open(f"{src}/small/file_{i}.txt", "wb") as f:
                f.write(os.urandom(1024))

        # Medium files (400 x 100KB = ~40 MB)
        for i in range(400):
            with open(f"{src}/medium/file_{i}.bin", "wb") as f:
                f.write(os.urandom(100 * 1024))

        # Large files (200 x 3MB = ~600 MB). Sized so the local/SSH/daemon
        # transfers run for ~1s+ rather than sub-second, keeping per-run
        # connection/setup overhead from dominating the ratio on a shared
        # CI runner (the main cause of the previously skewed cells).
        for i in range(200):
            with open(f"{src}/large/file_{i}.dat", "wb") as f:
                f.write(os.urandom(3 * 1024 * 1024))

        total_size = tree_size(src)
        total_files = sum(len(fn) for _, _, fn in os.walk(src))

        results["test_data"] = {
            "size_mb": round(total_size / 1024 / 1024, 1),
            "files": total_files,
        }
        results["baselines"] = [
            {
                "label": b.label,
                "version": binary_version(b.path),
                "path": b.path,
                "primary": b.label == PRIMARY.label,
            }
            for b in BASELINES
        ]
        # Retained for consumers that predate the per-baseline dimension:
        # the primary baseline is the one the legacy fields describe.
        results["upstream_version"] = binary_version(PRIMARY.path)
        oc_release, oc_compat = oc_rsync_versions(OC_RSYNC)
        results["oc_rsync_version"] = oc_release
        results["oc_rsync_wire_compat_version"] = oc_compat
        results["environment"] = benchmark_env.capture(
            os.environ.get("OC_RSYNC_SEND_ZC_DISPATCH", "")
        )
        print(
            benchmark_env.send_zc_verdict(results["environment"]),
            file=sys.stderr,
        )

        # One daemon per baseline, each on its own port with its own module
        # tree. The daemon is the *peer* for the daemon cells, so timing
        # oc-rsync against a single daemon while comparing it to two different
        # upstream clients would fold a server-version difference into the
        # client ratio.
        ports = {}
        for baseline in BASELINES:
            port = find_free_port()
            ports[baseline.label] = port
            conf_path = f"{tmpdir}/rsyncd_{baseline.slug}.conf"
            os.makedirs(daemon_dst[baseline.label], exist_ok=True)
            with open(conf_path, "w") as f:
                f.write(
                    f"port = {port}\n"
                    f"use chroot = false\n"
                    f"\n"
                    f"[bench]\n"
                    f"    path = {src}\n"
                    f"    read only = true\n"
                    f"\n"
                    f"[dest]\n"
                    f"    path = {daemon_dst[baseline.label]}\n"
                    f"    read only = false\n"
                )
            print(
                f"Starting rsync {baseline.label} daemon on port {port}...",
                file=sys.stderr,
            )
            daemons.append(
                subprocess.Popen(
                    [
                        baseline.path,
                        "--daemon",
                        "--config",
                        conf_path,
                        "--no-detach",
                    ],
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                )
            )
            if not wait_for_port(port):
                print(
                    f"ERROR: rsync {baseline.label} daemon failed to start",
                    file=sys.stderr,
                )
                sys.exit(1)
        print("Daemons ready.", file=sys.stderr)

        # The oc-vs-oc cells below (OpenSSL, io_uring, SSH transport) compare
        # build variants of oc-rsync rather than releases of upstream, so they
        # have no baseline dimension and speak to the primary baseline's
        # daemon only.
        port = ports[PRIMARY.label]

        # `--rsync-path` pins the remote end of an SSH cell to the same
        # release as the local end. Without it the remote server is whatever
        # `rsync` the login PATH resolves, so a 3.4.4 client would be timed
        # against a 3.5.0 server and the row would name a version it did not
        # measure. oc-rsync takes the same option, so its SSH cells are timed
        # against each baseline's server too and the comparison isolates the
        # client.
        def ssh_pin(baseline):
            return f"--rsync-path={baseline.path}"

        for test in TESTS:
            mode = test["mode"]
            args_tpl = test["args"]
            do_reset = test["reset"]
            is_daemon = mode.startswith("daemon_")
            is_ssh = mode.startswith("ssh_")
            per_baseline = is_daemon or is_ssh

            print(f"Running: [{mode}] {test['name']}...", file=sys.stderr)

            def oc_dst_for(baseline, per_baseline=per_baseline):
                return dst_oc_per[baseline.label] if per_baseline else dst_oc

            def up_args(baseline, args_tpl=args_tpl):
                return args_tpl.format(
                    src=src,
                    dst=dst_up[baseline.label],
                    port=ports[baseline.label],
                )

            def oc_args(baseline, args_tpl=args_tpl):
                return args_tpl.format(
                    src=src,
                    dst=oc_dst_for(baseline),
                    port=ports[baseline.label],
                )

            pin = ssh_pin if is_ssh else (lambda b: "")

            def up_cmd(baseline):
                return f"{baseline.path} {pin(baseline)} {up_args(baseline)}"

            def oc_cmd(baseline):
                return f"{OC_RSYNC} {pin(baseline)} {oc_args(baseline)}"

            def up_before(baseline):
                wipe(dst_up[baseline.label])
                if is_daemon:
                    wipe(daemon_dst[baseline.label])

            def oc_before(baseline):
                wipe(oc_dst_for(baseline))
                if is_daemon:
                    wipe(daemon_dst[baseline.label])

            if do_reset:
                # An initial-transfer cell has to start from an empty
                # destination on every run, not only the first.
                before_up, before_oc, runs = up_before, oc_before, 3
            else:
                before_up = before_oc = None
                runs = 6

            compare(
                results,
                test["id"],
                test["name"],
                mode,
                up_cmd,
                oc_cmd,
                runs=runs,
                # Peak RSS is collected alongside the timing here, not only in
                # the dedicated memory mode. The report has always had columns
                # for it on these rows; without the /usr/bin/time wrapper they
                # were dashes, so a throughput win bought with memory was
                # invisible on the very table that claimed to show it.
                runner=benchmark_rss if HAVE_TIME_CMD else benchmark,
                up_before=before_up,
                oc_before=before_oc,
                oc_per_baseline=per_baseline,
                corpus_bytes=total_size,
            )

        # OpenSSL vs pure-Rust comparison
        if OC_RSYNC_OPENSSL and os.path.isfile(OC_RSYNC_OPENSSL):
            print("Running OpenSSL vs pure-Rust comparison...", file=sys.stderr)
            dst_pure = f"{tmpdir}/dst_pure"
            dst_ssl = f"{tmpdir}/dst_ssl"
            wipe(dst_pure, dst_ssl)

            for test in OPENSSL_TESTS:
                test_id = test["id"]
                name = test["name"]
                mode = test["mode"]
                args_tpl = test["args"]

                print(f"Running: [{mode}] {name}...", file=sys.stderr)

                runs = 3 if test["reset"] else 6
                pure_args = args_tpl.format(src=src, dst=dst_pure, port=port)
                ssl_args = args_tpl.format(src=src, dst=dst_ssl, port=port)

                pure_result = benchmark(
                    f"{OC_RSYNC} {pure_args}",
                    runs=runs,
                    before_each=(
                        (lambda: wipe(dst_pure)) if test["reset"] else None
                    ),
                )
                ssl_result = benchmark(
                    f"{OC_RSYNC_OPENSSL} {ssl_args}",
                    runs=runs,
                    before_each=(
                        (lambda: wipe(dst_ssl)) if test["reset"] else None
                    ),
                )
                add_throughput(pure_result, total_size)
                add_throughput(ssl_result, total_size)

                ratio = (
                    ssl_result["mean"] / pure_result["mean"]
                    if pure_result["mean"] > 0
                    else 0
                )

                results["tests"].append(
                    {
                        "id": test_id,
                        "name": name,
                        "mode": mode,
                        "upstream": pure_result,
                        "oc_rsync": ssl_result,
                        "ratio": round(ratio, 2),
                    }
                )
        elif OC_RSYNC_OPENSSL:
            print(
                f"WARNING: OC_RSYNC_OPENSSL={OC_RSYNC_OPENSSL} not found, skipping",
                file=sys.stderr,
            )

        # SSH transport: 3-way comparison
        #   1. upstream rsync over OpenSSH subprocess (host:path)
        #   2. oc-rsync over OpenSSH subprocess (host:path)
        #   3. oc-rsync over embedded russh (ssh://host/path)
        if OC_RSYNC_RUSSH and os.path.isfile(OC_RSYNC_RUSSH):
            print(
                "Running SSH transport (upstream vs oc-rsync subprocess vs russh)...",
                file=sys.stderr,
            )
            dst_upstream_pull = f"{tmpdir}/dst_upstream_pull"
            dst_sub_pull = f"{tmpdir}/dst_sub_pull"
            dst_russh_pull = f"{tmpdir}/dst_russh_pull"
            dst_upstream_push = f"{tmpdir}/dst_upstream_push"
            dst_sub_push = f"{tmpdir}/dst_sub_push"
            dst_russh_push = f"{tmpdir}/dst_russh_push"

            wipe(
                dst_upstream_pull, dst_sub_pull, dst_russh_pull,
                dst_upstream_push, dst_sub_push, dst_russh_push,
            )

            for test in RUSSH_TESTS:
                test_id = test["id"]
                name = test["name"]
                mode = test["mode"]
                is_push = "push" in test_id

                print(f"Running: [{mode}] {name}...", file=sys.stderr)

                if is_push:
                    upstream_dst = dst_upstream_push
                    sub_dst, russh_dst = dst_sub_push, dst_russh_push
                else:
                    upstream_dst = dst_upstream_pull
                    sub_dst, russh_dst = dst_sub_pull, dst_russh_pull

                upstream_args = test["upstream_args"].format(src=src, dst=upstream_dst)
                sub_args = test["subprocess_args"].format(src=src, dst=sub_dst)
                russh_args = test["russh_args"].format(src=src, dst=russh_dst)

                runs = 3 if test["reset"] else 6
                reset = test["reset"]
                pin = f"--rsync-path={PRIMARY.path}"
                upstream_result = benchmark(
                    f"{PRIMARY.path} {pin} {upstream_args}",
                    runs=runs,
                    before_each=(
                        (lambda d=upstream_dst: wipe(d)) if reset else None
                    ),
                )
                sub_result = benchmark(
                    f"{OC_RSYNC} {pin} {sub_args}",
                    runs=runs,
                    before_each=(lambda d=sub_dst: wipe(d)) if reset else None,
                )
                russh_result = benchmark(
                    f"{OC_RSYNC_RUSSH} {pin} {russh_args}",
                    runs=runs,
                    before_each=(
                        (lambda d=russh_dst: wipe(d)) if reset else None
                    ),
                )
                for series in (upstream_result, sub_result, russh_result):
                    add_throughput(series, total_size)

                ratio_russh_vs_sub = (
                    russh_result["mean"] / sub_result["mean"]
                    if sub_result["mean"] > 0
                    else 0
                )
                ratio_sub_vs_upstream = (
                    sub_result["mean"] / upstream_result["mean"]
                    if upstream_result["mean"] > 0
                    else 0
                )

                # The new fields `upstream_ssh`, `oc_subprocess`, `oc_russh`
                # describe the 3-way comparison directly. The legacy `upstream`,
                # `oc_rsync`, and `ratio` fields are kept (subprocess vs russh)
                # so older chart/report renderers continue to work unchanged.
                results["tests"].append(
                    {
                        "id": test_id,
                        "name": name,
                        "mode": mode,
                        "upstream_ssh": upstream_result,
                        "oc_subprocess": sub_result,
                        "oc_russh": russh_result,
                        "ratio_russh_vs_sub": round(ratio_russh_vs_sub, 2),
                        "ratio_sub_vs_upstream": round(ratio_sub_vs_upstream, 2),
                        # Backwards-compat shape for two-bar renderers.
                        "upstream": sub_result,
                        "oc_rsync": russh_result,
                        "ratio": round(ratio_russh_vs_sub, 2),
                    }
                )
        elif OC_RSYNC_RUSSH:
            print(
                f"WARNING: OC_RSYNC_RUSSH={OC_RSYNC_RUSSH} not found, skipping",
                file=sys.stderr,
            )

        # io_uring vs standard I/O comparison (Linux only)
        if IS_LINUX:
            print("Running io_uring vs standard I/O comparison...", file=sys.stderr)
            dst_uring = f"{tmpdir}/dst_uring"
            dst_no_uring = f"{tmpdir}/dst_no_uring"

            io_uring_tests = [
                {
                    "id": "io_uring_local",
                    "name": "Local initial sync",
                    "mode": "io_uring",
                    "args": "-av {src}/ {dst}/",
                },
                {
                    "id": "io_uring_daemon_pull",
                    "name": "Daemon pull initial",
                    "mode": "io_uring",
                    "args": "-av --timeout=30 rsync://localhost:{port}/bench/ {dst}/",
                },
                {
                    "id": "io_uring_ssh_pull",
                    "name": "SSH pull initial",
                    "mode": "io_uring",
                    "args": "-av --timeout=30 localhost:{src}/ {dst}/",
                },
            ]

            for test in io_uring_tests:
                print(f"Running: [io_uring] {test['name']}...", file=sys.stderr)
                args_tpl = test["args"]

                # Every io_uring cell is an initial transfer, so both arms
                # reset before each run.
                uring_args = args_tpl.format(src=src, dst=dst_uring, port=port)
                uring_result = benchmark(
                    f"{OC_RSYNC} --io-uring {uring_args}",
                    runs=3,
                    before_each=lambda: wipe(dst_uring),
                )

                no_uring_args = args_tpl.format(src=src, dst=dst_no_uring, port=port)
                no_uring_result = benchmark(
                    f"{OC_RSYNC} --no-io-uring {no_uring_args}",
                    runs=3,
                    before_each=lambda: wipe(dst_no_uring),
                )
                add_throughput(uring_result, total_size)
                add_throughput(no_uring_result, total_size)

                ratio = (
                    uring_result["mean"] / no_uring_result["mean"]
                    if no_uring_result["mean"] > 0
                    else 0
                )

                results["tests"].append(
                    {
                        "id": test["id"],
                        "name": test["name"],
                        "mode": "io_uring",
                        "upstream": no_uring_result,
                        "oc_rsync": uring_result,
                        "ratio": round(ratio, 2),
                    }
                )
        else:
            print("Skipping io_uring tests (not Linux).", file=sys.stderr)

        # Compression benchmarks (zlib and zstd)
        print("Running compression benchmarks...", file=sys.stderr)
        dst_comp_up = {b.label: f"{tmpdir}/dst_comp_up_{b.slug}" for b in BASELINES}
        dst_comp_oc = f"{tmpdir}/dst_comp_oc"
        wipe(dst_comp_oc, *dst_comp_up.values())

        for test in COMPRESSION_TESTS:
            print(f"Running: [compression] {test['name']}...", file=sys.stderr)
            reset = test["reset"]
            args_tpl = test["args"]
            compare(
                results,
                test["id"],
                test["name"],
                "compression",
                lambda b, a=args_tpl: (
                    f"{b.path} "
                    + a.format(src=src, dst=dst_comp_up[b.label], port=port)
                ),
                lambda b, a=args_tpl: (
                    f"{OC_RSYNC} "
                    + a.format(src=src, dst=dst_comp_oc, port=port)
                ),
                runs=3 if reset else 6,
                up_before=(lambda b: wipe(dst_comp_up[b.label])) if reset else None,
                oc_before=(lambda b: wipe(dst_comp_oc)) if reset else None,
                corpus_bytes=total_size,
            )

        # Delta transfer benchmarks (modify files then re-sync)
        print("Running delta transfer benchmarks...", file=sys.stderr)
        dst_delta_up = {
            b.label: f"{tmpdir}/dst_delta_up_{b.slug}" for b in BASELINES
        }
        dst_delta_oc = f"{tmpdir}/dst_delta_oc"
        wipe(dst_delta_oc, *dst_delta_up.values())

        # Initial sync to populate destinations
        for path in [dst_delta_oc, *dst_delta_up.values()]:
            subprocess.run(
                f"{PRIMARY.path} -av {src}/ {path}/",
                shell=True, capture_output=True, timeout=PER_RUN_TIMEOUT,
            )

        # Modify ~10% of medium files (append 4KB to trigger delta)
        MODIFIED_MEDIUM = range(0, 400, 10)
        for i in MODIFIED_MEDIUM:
            with open(f"{src}/medium/file_{i}.bin", "ab") as f:
                f.write(os.urandom(4096))

        def stale_delta_dest(path):
            """Roll the destination back to its pre-modification content.

            Without this the first run performs the delta and every later run
            has nothing left to send, so a delta cell would report the cost of
            a no-change scan. Truncating to the original length restores the
            size mismatch that makes the next run a real delta transfer.
            """
            for i in MODIFIED_MEDIUM:
                with open(f"{path}/medium/file_{i}.bin", "r+b") as f:
                    f.truncate(100 * 1024)

        for test in DELTA_TESTS:
            print(f"Running: [delta] {test['name']}...", file=sys.stderr)
            args_tpl = test["args"]
            compare(
                results,
                test["id"],
                test["name"],
                "delta",
                lambda b, a=args_tpl: (
                    f"{b.path} "
                    + a.format(src=src, dst=dst_delta_up[b.label], port=port)
                ),
                lambda b, a=args_tpl: (
                    f"{OC_RSYNC} "
                    + a.format(src=src, dst=dst_delta_oc, port=port)
                ),
                runs=4,
                up_before=lambda b: stale_delta_dest(dst_delta_up[b.label]),
                oc_before=lambda b: stale_delta_dest(dst_delta_oc),
                corpus_bytes=total_size,
            )

        # Restore modified files to original size for subsequent benchmarks
        for i in MODIFIED_MEDIUM:
            with open(f"{src}/medium/file_{i}.bin", "r+b") as f:
                f.truncate(100 * 1024)

        # Large single file benchmark (1GB)
        print("Running large file benchmarks...", file=sys.stderr)
        large_src = f"{tmpdir}/large_src"
        dst_large_up = {
            b.label: f"{tmpdir}/dst_large_up_{b.slug}" for b in BASELINES
        }
        dst_large_oc = f"{tmpdir}/dst_large_oc"
        os.makedirs(large_src, exist_ok=True)
        wipe(dst_large_oc, *dst_large_up.values())

        large_file_path = f"{large_src}/bigfile.dat"
        with open(large_file_path, "wb") as f:
            # Write 1GB in 1MB chunks
            for _ in range(1024):
                f.write(os.urandom(1024 * 1024))
        large_size = os.path.getsize(large_file_path)

        DELTA_OFFSET = 512 * 1024 * 1024
        DELTA_SPAN = 64 * 1024

        def stale_large_dest(path):
            """Overwrite the destination's middle 64KB so the next run deltas.

            Writing on the destination side also moves its mtime, so rsync's
            quick check sees a difference and re-runs the block search instead
            of skipping a file whose size never changed.
            """
            target = f"{path}/bigfile.dat"
            if not os.path.isfile(target):
                return
            with open(target, "r+b") as f:
                f.seek(DELTA_OFFSET)
                f.write(os.urandom(DELTA_SPAN))

        for test in LARGE_FILE_TESTS:
            print(f"Running: [large_file] {test['name']}...", file=sys.stderr)
            args_tpl = test["args"]
            if test["reset"]:
                up_before = lambda b: wipe(dst_large_up[b.label])
                oc_before = lambda b: wipe(dst_large_oc)
            elif test["id"] == "large_file_delta":
                up_before = lambda b: stale_large_dest(dst_large_up[b.label])
                oc_before = lambda b: stale_large_dest(dst_large_oc)
            else:
                up_before = oc_before = None

            compare(
                results,
                test["id"],
                test["name"],
                "large_file",
                lambda b, a=args_tpl: (
                    f"{b.path} "
                    + a.format(
                        src=large_src, dst=dst_large_up[b.label], port=port,
                    )
                ),
                lambda b, a=args_tpl: (
                    f"{OC_RSYNC} "
                    + a.format(src=large_src, dst=dst_large_oc, port=port)
                ),
                runs=3,
                up_before=up_before,
                oc_before=oc_before,
                corpus_bytes=large_size,
            )

        # Many small files benchmark (100K files)
        print("Running many small files benchmarks...", file=sys.stderr)
        many_src = f"{tmpdir}/many_src"
        dst_many_up = {
            b.label: f"{tmpdir}/dst_many_up_{b.slug}" for b in BASELINES
        }
        dst_many_oc = f"{tmpdir}/dst_many_oc"
        wipe(dst_many_oc, *dst_many_up.values())

        # Create 100K files x 100B across 100 directories
        for d in range(100):
            dir_path = f"{many_src}/d{d:03d}"
            os.makedirs(dir_path, exist_ok=True)
            for i in range(1000):
                with open(f"{dir_path}/f{i:04d}.txt", "wb") as f:
                    f.write(os.urandom(100))
        many_size = tree_size(many_src)

        for test in MANY_SMALL_FILES_TESTS:
            print(f"Running: [many_small] {test['name']}...", file=sys.stderr)
            args_tpl = test["args"]
            reset = test["reset"]
            compare(
                results,
                test["id"],
                test["name"],
                "many_small",
                lambda b, a=args_tpl: (
                    f"{b.path} "
                    + a.format(
                        src=many_src, dst=dst_many_up[b.label], port=port,
                    )
                ),
                lambda b, a=args_tpl: (
                    f"{OC_RSYNC} "
                    + a.format(src=many_src, dst=dst_many_oc, port=port)
                ),
                runs=3,
                up_before=(lambda b: wipe(dst_many_up[b.label])) if reset else None,
                oc_before=(lambda b: wipe(dst_many_oc)) if reset else None,
                corpus_bytes=many_size,
            )

        # Memory usage (peak RSS) benchmark
        print("Running memory usage benchmarks...", file=sys.stderr)
        dst_mem_up = {
            b.label: f"{tmpdir}/dst_mem_up_{b.slug}" for b in BASELINES
        }
        dst_mem_oc = f"{tmpdir}/dst_mem_oc"
        wipe(dst_mem_oc, *dst_mem_up.values())

        memory_tests = [
            {
                "id": "memory_initial",
                "name": "Initial sync (10K files)",
                "src": src,
                "bytes": total_size,
                "args": "-av {src}/ {dst}/",
            },
            {
                "id": "memory_large_file",
                "name": "1GB file sync",
                "src": large_src,
                "bytes": large_size,
                "args": "-av {src}/ {dst}/",
            },
            {
                "id": "memory_many_files",
                "name": "100K files sync",
                "src": many_src,
                "bytes": many_size,
                "args": "-av {src}/ {dst}/",
            },
        ]

        for test in memory_tests:
            print(f"Running: [memory] {test['name']}...", file=sys.stderr)
            args_tpl = test["args"]
            test_src = test["src"]
            compare(
                results,
                test["id"],
                test["name"],
                "memory",
                lambda b, a=args_tpl, s=test_src: (
                    f"{b.path} "
                    + a.format(src=s, dst=dst_mem_up[b.label], port=port)
                ),
                lambda b, a=args_tpl, s=test_src: (
                    f"{OC_RSYNC} "
                    + a.format(src=s, dst=dst_mem_oc, port=port)
                ),
                runs=3,
                runner=benchmark_rss if HAVE_TIME_CMD else benchmark,
                up_before=lambda b: wipe(dst_mem_up[b.label]),
                oc_before=lambda b: wipe(dst_mem_oc),
                corpus_bytes=test["bytes"],
            )

        # Sparse file benchmark
        print("Running sparse file benchmarks...", file=sys.stderr)
        sparse_src = f"{tmpdir}/sparse_src"
        dst_sparse_up = {
            b.label: f"{tmpdir}/dst_sparse_up_{b.slug}" for b in BASELINES
        }
        dst_sparse_oc = f"{tmpdir}/dst_sparse_oc"
        os.makedirs(sparse_src, exist_ok=True)
        wipe(dst_sparse_oc, *dst_sparse_up.values())

        # Create files with large zero runs (simulating sparse data)
        for i in range(50):
            path = f"{sparse_src}/sparse_{i}.dat"
            with open(path, "wb") as f:
                # 10MB file: 1MB data, 8MB zeros, 1MB data
                f.write(os.urandom(1024 * 1024))
                f.write(b"\0" * (8 * 1024 * 1024))
                f.write(os.urandom(1024 * 1024))
        sparse_size = tree_size(sparse_src)

        for test in SPARSE_TESTS:
            print(f"Running: [sparse] {test['name']}...", file=sys.stderr)
            args_tpl = test["args"]
            reset = test["reset"]
            compare(
                results,
                test["id"],
                test["name"],
                "sparse",
                lambda b, a=args_tpl: (
                    f"{b.path} "
                    + a.format(
                        src=sparse_src, dst=dst_sparse_up[b.label], port=port,
                    )
                ),
                lambda b, a=args_tpl: (
                    f"{OC_RSYNC} "
                    + a.format(src=sparse_src, dst=dst_sparse_oc, port=port)
                ),
                runs=3,
                up_before=(
                    (lambda b: wipe(dst_sparse_up[b.label])) if reset else None
                ),
                oc_before=(lambda b: wipe(dst_sparse_oc)) if reset else None,
                corpus_bytes=sparse_size,
            )

        # Calculate summary. `ratios` per row is keyed by baseline label, so
        # the headline numbers are stated once per baseline as well; the
        # unsuffixed fields keep describing the primary baseline for consumers
        # that predate the dimension.
        # Rows where a binary failed are excluded: averaging a ratio against a
        # command that errored out in a millisecond states a performance
        # verdict the run never measured.
        sound = [
            t for t in results["tests"]
            if "ratios" in t and not t.get("failed_series")
        ]
        excluded = [
            t["id"] for t in results["tests"] if t.get("failed_series")
        ]

        by_baseline = {}
        for baseline in BASELINES:
            label = baseline.label
            values = [t["ratios"][label] for t in sound]
            if not values:
                continue
            modes = {}
            for t in sound:
                modes.setdefault(t["mode"], []).append(t["ratios"][label])
            by_baseline[label] = {
                "avg_ratio": round(sum(values) / len(values), 2),
                "best_ratio": round(min(values), 2),
                "worst_ratio": round(max(values), 2),
                "by_mode": {
                    m: round(sum(r) / len(r), 2) for m, r in modes.items()
                },
            }

        ratios = [
            t["ratio"] for t in results["tests"] if not t.get("failed_series")
        ]
        by_mode = {}
        for t in results["tests"]:
            if t.get("failed_series"):
                continue
            by_mode.setdefault(t["mode"], []).append(t["ratio"])

        results["summary"] = {
            "avg_ratio": round(sum(ratios) / len(ratios), 2) if ratios else 0.0,
            "best_ratio": round(min(ratios), 2) if ratios else 0.0,
            "worst_ratio": round(max(ratios), 2) if ratios else 0.0,
            "by_mode": {
                m: round(sum(r) / len(r), 2) for m, r in by_mode.items()
            },
            "by_baseline": by_baseline,
            "excluded_tests": excluded,
        }
        if excluded:
            print(
                "WARNING: excluded from the summary because a binary failed: "
                + ", ".join(excluded),
                file=sys.stderr,
            )

        print(json.dumps(results, indent=2))

    finally:
        for proc in daemons:
            proc.terminate()
        for proc in daemons:
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()
        shutil.rmtree(tmpdir, ignore_errors=True)


if __name__ == "__main__":
    main()
