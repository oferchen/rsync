# Daemon Concurrency Benchmark Plan

Tracking: oc-rsync task #1933.

> Empirical follow-up to the static analysis in
> `docs/audits/daemon-thread-per-connection-scalability.md` (#1673). This
> plan defines the methodology, the test rig, the metrics, the
> upstream-rsyncd comparison, and the acceptance gate that decides
> whether the tokio async listener (#1934 RFC, #1935 implementation)
> is promoted to the default daemon I/O path.

## 1. Goals

1. Quantify the per-connection cost of the synchronous
   thread-per-connection daemon at three concurrency waypoints:
   100, 1 000, 10 000 simultaneous client connections.
2. Reproduce the same measurement against upstream rsyncd 3.4.1 from
   `target/interop/upstream-src/`, on the same host, at the same
   moment, against the same workload.
3. Capture a small, fixed set of metrics (Section 4) that the audit
   in #1673 predicts as the bottlenecks: connections accepted per
   second, p99 handshake latency, peak RSS, peak file-descriptor
   count.
4. Define the acceptance bar (Section 7) that the async listener
   (#1935) must clear in the same harness before the default
   daemon path flips from sync to async.

Non-goals: throughput of an active transfer body, delta-pipeline
latency, compression speed, encryption layer cost. Those are covered
by `crates/daemon/benches/daemon_benchmark.rs` (already wired) and
the existing hyperfine harness at
`scripts/benchmark_hyperfine.sh`. This plan measures *connection
admission*, not *bytes per second per connection*.

## 2. Methodology

### 2.1 Workload shape

Three fixed waypoints, run in this order:

| Waypoint | `N` clients | Duration | Per-client work |
|----------|-------------|----------|-----------------|
| Baseline | 100         | 30 s sustained | 1 KiB synthetic transfer |
| Operating | 1 000      | 30 s sustained | 1 KiB synthetic transfer |
| Stress  | 10 000       | 30 s sustained | 1 KiB synthetic transfer |

The 1 KiB body is intentional: it forces every client through the
full lifecycle (`@RSYNCD:` greeting, capability handshake, module
select, file-list, transfer, goodbye) without dwarfing the
connection cost in transfer time. The audit
`docs/audits/daemon-thread-per-connection-scalability.md` Section 11
predicts that connection admission, not transfer body, is the
bottleneck at these scales; this workload exposes that.

Each run is preceded by a 5 s warm-up at the same `N` and discarded.
Steady state is the next 30 s. Three runs per waypoint per
configuration; report median and worst.

### 2.2 Configurations

Three daemon configurations measured in the same harness invocation:

1. **`oc-rsync-sync`**: `oc-rsync --daemon` (current default,
   thread-per-connection from the audit subject).
2. **`oc-rsync-async`**: `oc-rsync --daemon` built with
   `--features async-daemon` (post #1935). Skipped until #1935
   lands; the harness records `n/a` for this column meanwhile.
3. **`upstream-rsyncd`**: `rsync --daemon` from
   `target/interop/upstream-src/rsync-3.4.1/`.

Each configuration runs against the same backing module
(`bench-module`) pointing at the same fixture path
(`/tmp/oc-rsync-bench/fixture/`), with `max connections = 0` so the
daemon does not refuse on its own. `max sessions` (oc-rsync) and
the equivalent upstream behaviour stays at the default for
configurations 1 and 3; configuration 2 sets `max_sessions = N + 32`
per the RFC default.

### 2.3 Knobs raised before the run

The harness raises kernel and process limits up front, with one
`prlimit` invocation per daemon process, before connecting any
client:

```sh
prlimit --pid "$DAEMON_PID" --nofile=65536:65536
prlimit --pid "$DAEMON_PID" --nproc=65536:65536
prlimit --pid "$DAEMON_PID" --as=unlimited
```

The kernel-side `net.core.somaxconn` is raised to 16 384 and
`net.ipv4.tcp_max_syn_backlog` to 16 384 inside the container
(Section 5). `listen_backlog` in the daemon config is set to 4096
to match. These changes are scoped to the container; the host is
untouched.

### 2.4 Determinism

- Same kernel: container-pinned (Section 5). Linux 6.x baseline.
- Same CPU affinity: daemon pinned to cores 0-3 with `taskset`,
  client driver pinned to cores 4-7. No cross-pin.
- Same network: `lo` only. No TCP traversal off-host. Loopback
  removes nic queue depth and ring-buffer sizing as a variable.
- Same fixture: a single 1 KiB file at a fixed path; never
  regenerated between runs.
- Same clocks: `clock_gettime(CLOCK_MONOTONIC)` on both daemon
  and driver, captured per accept and per greeting receipt.

## 3. Test rig

### 3.1 Client-side fan-out

The driver is a bash loop. `oc-rsync` invocations spawn in the
background; the loop captures their PIDs into an array and waits
for all of them at the end:

```sh
#!/bin/bash
# scripts/bench/daemon_concurrency_fanout.sh
set -euo pipefail
N="${N:-100}"
PORT="${PORT:-8730}"
HOST="${HOST:-127.0.0.1}"
MODULE="${MODULE:-bench-module}"
DEST="${DEST:-/tmp/oc-rsync-bench/dest}"
LOG="${LOG:-/tmp/oc-rsync-bench/client.log}"

mkdir -p "$DEST" "$(dirname "$LOG")"
: > "$LOG"

declare -a pids=()
for i in $(seq 1 "$N"); do
  /usr/bin/time -f '%e' \
    oc-rsync --quiet --no-motd \
      "rsync://${HOST}:${PORT}/${MODULE}/file.bin" \
      "${DEST}/file.${i}.bin" \
      >>"$LOG" 2>&1 &
  pids+=("$!")
done

wait "${pids[@]}"
```

A second variant uses upstream `rsync` for the upstream-rsyncd
configuration; only the binary name changes. Both binaries accept
identical CLI for `rsync://` URLs (we mirror upstream by design).

### 3.2 Why bash, not a Rust harness

The `for ... &` shell loop reproduces what an operator sees on a
production host when a service redeploys: hundreds to thousands of
clients connecting near-simultaneously, each in its own process.
A native Rust harness using a single multiplexed connection pool
would smear arrivals over a single process's syscall window and
underestimate the kernel's `accept` cost. The bash fan-out is
faithful to the failure mode the audit predicts.

The scripted form has a known floor: shell `&` with 10 000 children
can hit `ulimit -u` on the *driver* side. The harness raises
`ulimit -u 65536` on the driver shell before the loop. If the
driver still saturates at 10 000, the fan-out splits across two
driver processes (`N=5000` on each, started 50 ms apart) and the
harness merges their logs.

### 3.3 Driver-side timing instrumentation

`/usr/bin/time -f '%e'` captures wall-clock per client. The
client-side p99 is computed from these 100 / 1 000 / 10 000 samples.
For finer breakdowns - `connect()` to greeting receipt only, not
the full session - the harness optionally swaps `oc-rsync` for a
small companion binary, `bench-greet-only`, that performs only the
TCP connect and reads the `@RSYNCD:` line:

```sh
# scripts/bench/bench_greet_only.sh
exec 3<>/dev/tcp/${HOST}/${PORT}
read -r -u 3 greeting
echo "$greeting"
exec 3>&-
```

Wall-time of this script is the pure handshake-latency probe.
Either probe is acceptable; the full-session form is the default
because it stresses the full lifecycle.

### 3.4 Daemon-side instrumentation

A sidecar polls `/proc/$DAEMON_PID/status` at 100 ms cadence for
30 s and records:

- `VmRSS` (resident set size).
- `VmSize` (virtual size, sanity-check against the audit's
  reservation predictions).
- `Threads`.
- `FDSize` (high-water of fd table; we cross-check with
  `ls /proc/$DAEMON_PID/fd | wc -l`).

Polling with `cat /proc/$pid/status` plus `awk` is sufficient and
adds negligible load to the daemon (the read is from kernel-side
seq files; no IPC to the daemon process).

### 3.5 Optional: tcpdump capture

For diagnostic runs only, `tcpdump -i lo -w /tmp/bench.pcap port
$PORT` runs in a third sidecar. Off by default - the pcap is
multi-GB at 10 000 connections and the kernel-side capture cost
distorts the daemon RSS measurement. Enabled with
`BENCH_TCPDUMP=1`. When enabled, the harness runs tcpdump in a
separate process from the daemon and metric sidecars so capture
contention is visible separately.

The existing pcap fixtures in `docs/audits/pcap-samples/` cover
single-connection wire-format diagnostics. Connection-storm
captures are not committed; the harness writes them to
`/tmp/oc-rsync-bench/pcap/` and the operator reviews them locally.

## 4. Metrics

The four headline metrics, each captured per waypoint per
configuration:

### 4.1 Connections per second

Definition: total successful client completions divided by
steady-state duration (30 s). A successful completion is a client
that received the file and exited 0. Failures (any non-zero exit
or no-output) are reported separately and do not contribute.

Source: client log `LOG` from Section 3.1, parsed by line count of
`/usr/bin/time` output. The driver records `start_ts`,
`end_ts`, and `connections_per_sec = N / (end_ts - start_ts)` for
the steady-state window.

Read: higher is better. Audit prediction (#1673 Section 4): sync
path peaks around the host's per-thread spawn rate (~10 000-30 000
spawns/s on Linux), bounded by greeting write latency.

### 4.2 p99 handshake latency

Definition: the 99th percentile of (greeting-line-received-at -
connect-syscall-returned-at) across all `N` clients in the
steady-state window. Reported in milliseconds.

Source: per-client `/usr/bin/time -f '%e'` for the full-session
probe, or the bash `read` form in Section 3.3 for the
greeting-only probe. Computed by the harness with `awk` from the
sorted `.elapsed` values.

Read: lower is better. Audit prediction (#1673 Section 5.2): sync
path p99 grows with `N` because thread spawn is serial in the
single-listener accept loop; async path p99 stays flat until the
spawn-blocking pool saturates.

### 4.3 Peak RSS

Definition: max `VmRSS` observed during the steady-state window,
in MiB. Sampled at 100 ms cadence (Section 3.4); the peak is the
max sample.

Source: `/proc/$DAEMON_PID/status` `VmRSS:` field.

Read: lower is better. Audit prediction (#1673 Section 4): sync
path RSS at 10 000 connections is dominated by committed thread
stacks (8-32 KiB per thread on Linux, so 80-320 MiB) plus shared
heap. Async path predicted at < 200 MiB total. We also capture
`VmSize` for the audit's address-space prediction (8 GiB at 1 000,
80 GiB at 10 000) but it is a sanity check, not a gate.

### 4.4 Peak file-descriptor count

Definition: max count of files in `/proc/$DAEMON_PID/fd/` during
the steady-state window. Sampled at 100 ms cadence.

Source: `ls /proc/$DAEMON_PID/fd | wc -l` and cross-checked
against `cat /proc/$DAEMON_PID/status | grep '^FDSize:'`.

Read: lower is better, but a value close to `N + small_constant`
is correct - the daemon must hold one fd per active connection.
The metric is interesting at the limit: at `N = 10 000`,
`peak_fd > 10 010` means the daemon is leaking; `peak_fd ~= 10
008` (10 000 conns + 4 listeners + log + signal pipe) is healthy.
At `N = 1 000` on a host with default `RLIMIT_NOFILE = 1024`,
`peak_fd` will hit the cap and the daemon will start failing
`accept(2)` - the harness raises this limit (Section 2.3) so the
metric exposes app-level fd hygiene, not the kernel default.

### 4.5 Secondary metrics (reported, not gating)

- Thread / process count from `/proc/$DAEMON_PID/status` `Threads:`.
- `accept(2)` errno distribution from
  `strace -e trace=accept4 -p $DAEMON_PID -c -f` for a 5 s
  sub-window. Diagnostic only.
- Driver-side error rate: count of clients that exited non-zero,
  binned by exit code.
- Daemon log line count during the run: a coarse proxy for log
  mutex contention (audit Section 7.3).

## 5. Container setup

The benchmark runs inside the existing `rsync-profile` container
(parent `CLAUDE.md` "Containers (Podman)"), which already has
upstream rsync 3.4.1 and the `oc-rsync-dev` build. No new image is
needed.

```sh
# Start (or reuse) the container with the workspace bind-mounted.
podman exec -it rsync-profile bash

# Inside the container:
cd /workspace
sysctl -w net.core.somaxconn=16384
sysctl -w net.ipv4.tcp_max_syn_backlog=16384
ulimit -n 65536
ulimit -u 65536

# Build oc-rsync with sync default, then with async-daemon feature.
cargo build --release -p oc-rsync
cargo build --release -p oc-rsync --features async-daemon

# Run the harness:
bash scripts/bench/daemon_concurrency.sh
```

The `rsync-profile` container is preferred over
`localhost/oc-rsync-bench:latest` because it is long-running, has
the workspace bind-mount, and matches the profiling runs documented
in `docs/audits/daemon-thread-per-connection-scalability.md`.

The benchmark host is the container's view of the loopback
interface. No external network is involved. Per the project
container-safety rule (parent `CLAUDE.md` "Containers & Bind
Mounts"), the harness writes all transient data under
`/tmp/oc-rsync-bench/` *not* under the bind-mounted `/workspace/`
tree, so a misquoted cleanup never touches the host repo. The
harness has zero `rm -rf` calls; cleanup is `find /tmp/oc-rsync-
bench -mindepth 1 -delete`, which fails harmlessly on empty.

## 6. Comparison harness

A single shell driver, `scripts/bench/daemon_concurrency.sh`,
drives all three configurations in the same invocation:

```sh
for cfg in oc-rsync-sync oc-rsync-async upstream-rsyncd; do
  start_daemon "$cfg"
  for n in 100 1000 10000; do
    record_run "$cfg" "$n"
  done
  stop_daemon
done
```

`record_run` performs warm-up, steady-state, and the metric
sweep, then writes a row to `/tmp/oc-rsync-bench/results.tsv`:

```
cfg  N  conn_per_s  p99_handshake_ms  peak_rss_mib  peak_fd  threads  errors
```

The harness emits a markdown summary table at the end:

```
| cfg              | N     | conn/s | p99 ms | RSS MiB | fd    | threads | err |
|------------------|-------|--------|--------|---------|-------|---------|-----|
| oc-rsync-sync    | 100   | ...    | ...    | ...     | ...   | ...     | 0   |
| oc-rsync-sync    | 1000  | ...    | ...    | ...     | ...   | ...     | 0   |
| oc-rsync-sync    | 10000 | ...    | ...    | ...     | ...   | ...     | 0   |
| upstream-rsyncd  | 100   | ...    | ...    | ...     | ...   | ...     | 0   |
| upstream-rsyncd  | 1000  | ...    | ...    | ...     | ...   | ...     | 0   |
| upstream-rsyncd  | 10000 | ...    | ...    | ...     | ...   | ...     | 0   |
| oc-rsync-async   | 100   | ...    | ...    | ...     | ...   | ...     | 0   |
| oc-rsync-async   | 1000  | ...    | ...    | ...     | ...   | ...     | 0   |
| oc-rsync-async   | 10000 | ...    | ...    | ...     | ...   | ...     | 0   |
```

The summary is committed under `docs/benchmarks/` only when a run
materially changes the table, with the commit message scoped to
the run date. Continuous-run history lives outside the repo (the
existing `scripts/benchmark.sh` history pattern applies).

## 7. Acceptance bar

The async listener (#1935) is promoted to the default daemon I/O
path when *all* of the following hold simultaneously, on the same
benchmark run, on the `rsync-profile` container, with the harness
in this document:

| Gate | Sync baseline | Async target | Hard requirement |
|------|---------------|--------------|------------------|
| `connections_per_sec` at `N = 100` | measured | >= sync | within 5% (no regression on the easy case) |
| `connections_per_sec` at `N = 1 000` | measured | >= sync | within 5% (no regression at the operating point) |
| `connections_per_sec` at `N = 10 000` | likely fails or saturates | >= 5 000/s | hard floor |
| `p99_handshake_ms` at `N = 1 000` | measured | <= sync | within 10% (no regression) |
| `p99_handshake_ms` at `N = 10 000` | likely > 1 s | <= 250 ms | hard ceiling |
| `peak_rss_mib` at `N = 10 000` | predicted 200-1 000 MiB | <= 300 MiB | hard ceiling per audit prediction |
| `peak_fd` at `N = 10 000` | `~= 10 008` | `~= 10 008` | identical (one fd per accepted conn either way) |
| `errors` at all `N` | 0 | 0 | hard floor |
| SIGTERM-to-drain latency at `N = 1 000` | measured | <= sync | <= 1 s wall-clock |
| Wire-format parity vs upstream | 100% (existing goldens) | 100% | non-negotiable |

The promotion is by branch flip in
`crates/daemon/Cargo.toml`: the `async-daemon` feature moves
from opt-in to a `default = ["async-daemon"]` change. The sync
path remains code-resident and selectable via
`--no-default-features` for one minor release for revert
safety, mirroring the deprecation pattern used elsewhere in the
project.

If any gate fails, the harness emits the failing row in red, the
async path stays opt-in, and the open question is filed against
#1935 with the failing row attached.

## 8. Open questions

These are flagged here, not resolved. Each maps to an existing
tracker.

1. **`max_sessions` semantics fix (#1933 prerequisite).** The
   audit Section 9.1 recommends switching `max_sessions` from
   "total served lifetime" to "active concurrent". The benchmark
   here assumes the fix is in place; if it is not, the
   `oc-rsync-sync` configuration must run with `max_sessions`
   unset (default unlimited) to avoid hitting the lifetime cap
   mid-run. The harness defaults to unset for safety.
2. **Driver-host pin vs single host.** Running daemon and clients
   on the same host (loopback) inflates kernel-side scheduling
   cost and undercounts NIC effects. The benchmark here is
   deliberately host-local (Section 2.4) to isolate the daemon's
   admission cost; a follow-up two-host benchmark (#1933 sub-task)
   adds the NIC dimension.
3. **Driver fan-out at 10 000.** The bash `for ... &` form may
   itself become the bottleneck. Validation: re-run with two
   driver processes at 5 000 each (Section 3.2 fallback) and
   confirm the headline metrics agree within 5%. If they do not,
   the driver gets re-implemented as a small C program that
   `fork()`s `N` clients in tight loop. This is a harness fix,
   not a daemon issue; tracked under #1933.
4. **Coverage of TLS / proxy-protocol modes.** Out of scope here;
   the daemon's TLS layer is feature-gated and the proxy-protocol
   path adds bytes to the greeting only. Both are tested by the
   existing interop suite. A future expansion of this plan adds
   them as separate columns.
5. **Windows / macOS coverage.** Out of scope here. The async
   listener's IOCP path on Windows is a separate gate (#1934
   Section 8 question 8, parent audit Section 9.3). macOS
   `kqueue` semantics differ from epoll and require their own
   harness pass.

## 9. References

- Sibling audits:
  `docs/audits/daemon-thread-per-connection-scalability.md`
  (#1673), `docs/audits/daemon-event-loop-multiplexing.md`
  (#1675), `docs/audits/daemon-async-listener-rfc.md`
  (#1934), `docs/audits/async-daemon-listener.md` (#1934
  sketch).
- Async scaffold:
  `crates/daemon/src/daemon/async_session/mod.rs`,
  `async_session/listener.rs`,
  `async_session/session.rs`,
  `async_session/shutdown.rs`.
- Sync accept loop:
  `crates/daemon/src/daemon/sections/server_runtime/accept_loop.rs`,
  `connection.rs`, `connection_counter.rs`, `listener.rs`,
  `workers.rs`, `reload.rs`.
- Existing harness:
  `crates/daemon/benches/daemon_benchmark.rs`,
  `scripts/benchmark.sh`,
  `scripts/benchmark_hyperfine.sh`,
  `scripts/rsync-interop-server.sh`,
  `tools/ci/run_interop.sh`.
- Upstream source:
  `target/interop/upstream-src/rsync-3.4.1/clientserver.c`,
  `socket.c`.
- Related trackers: #1673 (audit), #1934 (RFC, completed),
  #1935 (implement async listener), #1683 (signal latency),
  #1595 (io_uring async), #1593 (cross-runtime SSH),
  #1751 (rayon via `spawn_blocking`).
- Linux limits: `man 2 getrlimit`,
  `/proc/sys/net/core/somaxconn`,
  `/proc/sys/net/ipv4/tcp_max_syn_backlog`.
