# Daemon Thread-per-Connection Benchmark Plan

Tracking: oc-rsync task #1933.

> Empirical follow-up to the static analysis in
> `docs/audits/daemon-thread-per-connection-scalability.md` (task #1673,
> merged via PR #3705). This plan defines the harness, workloads,
> metrics, and decision criteria that gate the async listener migration
> tracked under #1935. No code lands in this PR.

## 1. Goal

Quantify the ceiling that the synchronous thread-per-connection daemon
model imposes on concurrent client load, on the three default desktop
and server platforms the project targets:

- Default Linux x86_64 (Debian / Ubuntu glibc, kernel 6.x, default
  `RLIMIT_NOFILE = 1024`, default `vm.max_map_count = 65530`).
- Default macOS arm64 (Sonoma or later, default `kern.maxproc`,
  default `RLIMIT_NOFILE = 256`).
- Default Windows x86_64 (Windows 11, default soft fd limit
  effectively unbounded at the Win32 layer, see #1682).

The audit (`docs/audits/daemon-thread-per-connection-scalability.md`,
"Per-connection resource cost") fixed the upper-bound estimates on
paper: 8 GiB of stack address space at 1 000 connections on glibc,
80 GiB at 10 000. This plan turns those upper bounds into measured
numbers for committed RSS, file-descriptor count, accept latency, and
soft-limit failure mode.

The benchmark output answers four operator-facing questions:

1. At what concurrency does the sync path stop being viable on each
   platform's default tunables?
2. Where does the cost come from - thread stacks, file descriptors,
   shared-state contention, or kernel scheduling?
3. How does the sync path compare to upstream rsync 3.4.1's fork model
   under identical load?
4. Does the gap justify the async listener migration in #1935, or
   does the short-term `max_sessions` fix in
   `docs/audits/daemon-thread-per-connection-scalability.md` Section 9.1
   close enough of it that the async work stays deferred?

## 2. Workloads

Three concurrency waypoints, matching the audit's framing
(`docs/audits/daemon-thread-per-connection-scalability.md` Section 11):

- **W100** - 100 concurrent clients. Baseline. Asserts the daemon
  serves cleanly with no resource pressure on any platform's default
  tunables.
- **W1k** - 1 000 concurrent clients. Operating point. Hits the
  default Debian fd ulimit and the default macOS fd ulimit.
- **W10k** - 10 000 concurrent clients. Stress. Exceeds every
  platform's default soft limits and, on glibc, reserves more thread
  stack address space than the host's physical RAM.

Each client performs the same minimal session: TCP connect, exchange
the `@RSYNCD:` greeting
(`crates/daemon/src/daemon/sections/greeting.rs:42`), select a single
read-only module, then pull 100 small files from it. "Small" means
files in the 1 KiB to 16 KiB range; the harness pre-generates a fixed
fileset with deterministic content so quick-check skips do not
perturb runs (see "Test flakiness" notes in the project rules). The
fileset lives on tmpfs so I/O on the host is not the bottleneck.

100 small files per client is enough to exercise the file-list,
generator, and sender paths through `crates/transfer` and
`crates/engine`, but small enough that each session terminates in
seconds at idle CPU. The aggregate transfer per run is bounded:
W10k peaks at 1 000 000 file transfers and roughly 16 GiB of bytes
on the wire, which fits in the tmpfs working set.

Two arrival shapes per waypoint:

- **All-at-once** - the harness fires `N` `connect()` calls within a
  10 ms window. Stresses the `listen(2)` backlog
  (`crates/daemon/src/daemon/sections/server_runtime/accept_loop.rs:138`,
  default 5 from the audit's Section 6.3) and the per-accept thread
  spawn cost
  (`crates/daemon/src/daemon/sections/server_runtime/connection.rs:106`).
- **Steady-state** - the harness opens new connections at a fixed
  rate to maintain `N` concurrent sessions while old ones complete
  and exit. Stresses sustained accept throughput, the
  `JoinHandle` reaper
  (`crates/daemon/src/daemon/sections/server_runtime/workers.rs:7`),
  and shared mutexes (the audit's Section 7).

## 3. Pre-condition fix

The benchmark requires the active-counter fix from
`docs/audits/daemon-thread-per-connection-scalability.md` Section 9.1
to land first. Without it the `max_sessions` directive measures
*total served*, not *currently active*
(`crates/daemon/src/daemon/sections/server_runtime/connection.rs:264-270`,
`connection.rs:352-358`), so any cap the harness sets is a lifetime
cap on the daemon process and not a concurrency cap. That makes
W10k uninstrumentable: an operator who sets `max_sessions = 10000`
expecting "10 000 concurrent" instead gets "the daemon stops
accepting after 10 000 lifetime connections, ever, until restart".

The fix swaps the post-accept check to consult
`ConnectionCounter::active`
(`crates/daemon/src/daemon/sections/server_runtime/connection_counter.rs:33`),
which is already plumbed but currently `#[allow(dead_code)]`
(`connection_counter.rs:32`). The atomic increment on accept is at
`connection_counter.rs:39`; the RAII decrement on worker exit is at
`connection_counter.rs:71-73`. The change is a roughly 30-line edit
in `connection.rs` plus tests; it is a strict precondition for this
benchmark, not a contribution of it.

The benchmark runs this fix as the first commit in its branch and
treats the dead-code-ready counter as the admission gate. Failure
mode under load (audit Section 9.1, mirroring upstream
`lp_max_connections()`): drop the new TCP stream silently after
accept, log at info level, do not write any bytes back to the client.

## 4. Test harness

The harness lives at `crates/daemon/benches/daemon_concurrency.rs`,
extending the existing single-file
`crates/daemon/benches/daemon_benchmark.rs:1` (which today only
exercises config parsing and auth). It uses Criterion's custom-run
harness mode so each waypoint is a separate sub-benchmark with its
own setup and teardown.

Components:

1. **Daemon under test** - one `oc-rsync --daemon` process started by
   the harness on a unique port via
   `scripts/rsync-interop-server.sh:1`. The daemon binds dual-stack
   IPv4 + IPv6 by default
   (`crates/daemon/src/daemon/sections/server_runtime/accept_loop.rs:107`)
   so the harness exercises both the single-listener path
   (`crates/daemon/src/daemon/sections/server_runtime/connection.rs:216`)
   and the dual-stack path
   (`connection.rs:281`) by toggling the bind address.
2. **Client driver** - a multi-threaded Rust binary
   `crates/daemon/benches/concurrency_driver/main.rs` that spawns
   `N` rayon tasks; each task opens one TCP connection, reads the
   greeting, sends `@RSYNCD: 32`, selects the test module, and
   issues `rsync :: ...` over the open socket. The driver does not
   reuse the production CLI; it speaks the wire protocol directly
   via `crates/protocol` so the client side imposes no extra thread
   overhead beyond one OS thread per connection (matches what
   upstream's fork model exposes from the server's perspective).
3. **Comparison oracle** - upstream rsync 3.4.1 daemon, started by
   the same harness through `scripts/rsync-interop-server.sh:1`. The
   harness reuses `tools/ci/run_interop.sh:1`'s tarball fetcher to
   ensure the upstream binary is present. The oracle daemon serves
   an identical tmpfs-backed fileset.
4. **Resource sampler** - a sidecar thread in the harness that polls
   `/proc/$pid/status` on Linux (`VmRSS`, `VmSize`, `Threads`),
   `task_info` via `mach_task_basic_info` on macOS, and
   `GetProcessMemoryInfo` on Windows. Sample period is 100 ms during
   the active phase, 1 s during steady state. Output is a CSV per
   run.
5. **Soft-limit harness** - a wrapper script
   `scripts/daemon_tpc_run.sh` that sets `prlimit --nofile=N
   --nproc=M` on Linux, `launchctl limit maxfiles N N` on macOS, and
   the equivalent `Set-ItemProperty` on Windows, then invokes the
   driver. Soft-limit failure modes (Section 6) are captured by
   varying `N` and `M` across runs.

The driver does not call `cargo` or `rustc` during the benchmark
run; both binaries are pre-built. The harness records the git
commit, daemon flags, kernel version, glibc / musl version, and
ulimit values as run metadata.

CI integration: the harness has a fast tier (W100 only, both arrival
shapes, ~30 s wall) gated to every PR that touches `crates/daemon/`
and a slow tier (W1k + W10k, ~10 min wall) gated to nightly. The
slow tier runs in the existing benchmark container
(`localhost/oc-rsync-bench:latest`, see project rules) on dedicated
runner hardware to avoid noisy-neighbor variance.

## 5. Metrics

Per run, per waypoint, per arrival shape:

| Metric | Source | Why it matters |
|--------|--------|----------------|
| Max sustained concurrent sessions | Resource sampler (`Threads` on Linux) | Direct answer to the headline question. |
| Connect latency p50 / p99 / max | Driver clock (between `connect()` return and greeting receipt) | Catches accept-loop saturation and listen-backlog drops. |
| Bytes-per-session p50 / p99 | Driver | Confirms each session ran the full 100-file pull. |
| Daemon RSS peak | Resource sampler | The cost the operator pays in physical memory. |
| Daemon `VmSize` peak | Resource sampler | The address-space cost (the audit's headline number, 80 GiB at W10k on glibc). |
| Daemon thread count peak | Resource sampler | One per active session, plus listeners and signal pipe. |
| Daemon fd count peak | `/proc/$pid/fd` count on Linux, `lsof -p` on macOS, `GetProcessHandleCount` on Windows | Hard ceiling against `RLIMIT_NOFILE`. |
| CPU utilisation, sender side | `getrusage` on the daemon at run end | Identifies CPU-bound vs IO-bound regimes. |
| `accept(2)` failures | Daemon log sink lines containing "accept" | Direct evidence of listen-backlog overrun or fd exhaustion. |
| SIGTERM-to-drain latency | Harness wall clock from signal sent to `wait()` return | Worst-case operator visibility, dominated by `SIGNAL_CHECK_INTERVAL = 500 ms` (`crates/daemon/src/daemon/sections/server_runtime/listener.rs:45`). |
| Lock-contention sample | Per-mutex hit / wait counters from `parking_lot` if compiled with the `deadlock_detection` feature, otherwise `perf lock` on Linux | Quantifies the audit's Section 7 hot spots. |

Raw outputs land in `target/bench/daemon-tpc/$RUNID/`. The harness
writes a Markdown summary table per platform per waypoint plus a JSON
bundle for downstream charting.

## 6. Soft-limit triggers

The benchmark deliberately drives each soft limit to failure to
characterise the failure mode and the operator-visible signal. The
audit (Section 5, Section 6) enumerates the limits; this plan
specifies the trigger sequence.

### 6.1 `max_sessions` admission cap

After the precondition fix from Section 3 lands, run W10k with
`max_sessions = 100`, `max_sessions = 1000`, and `max_sessions =
10000`. Expectation: the (101 + N)th, (1001 + N)th, and arriving
connection during cap saturation is dropped silently with one info
log line, no bytes written to the client. The harness asserts the
client side observes a clean RST or TCP close on the rejected
attempt.

### 6.2 `RLIMIT_NOFILE`

Run W1k with `prlimit --nofile=1024:1024` (Debian default). Expected
failure: at roughly 1 020 connections (1024 minus listeners minus
stdio minus the systemd notify fd from `crates/daemon/src/systemd.rs`,
opened on first use), `accept(2)` returns `EMFILE`. The accept loop
already handles non-`WouldBlock` errors by returning
(`crates/daemon/src/daemon/sections/server_runtime/connection.rs:259-261`),
which is the correct upstream-aligned behaviour but operationally
brittle. Run with `prlimit --nofile=8192:8192` and
`prlimit --nofile=65536:65536` to confirm the ceiling moves with the
ulimit and W10k is reachable.

### 6.3 `RLIMIT_NPROC`

Run W10k with `prlimit --nproc=4096:4096`. Linux counts threads
against `RLIMIT_NPROC` per the audit's Section 5.2; the daemon hits
the cap when `pthread_create` returns `EAGAIN`. The current accept
loop does not gracefully degrade on thread-spawn failure:
`spawn_connection_worker`
(`crates/daemon/src/daemon/sections/server_runtime/connection.rs:106`)
calls `thread::spawn` (`connection.rs:121`) which panics on failure
and the panic propagates back through the accept loop.

That panic-on-spawn-failure path is itself a finding worth recording.
The benchmark documents it; the fix lands separately under the
async-listener work in #1935 (where `tokio::task::spawn_blocking`
returns a `JoinError` instead of panicking) or, if the async
migration is deferred, as a small `Result`-returning wrapper around
`thread::Builder::spawn`.

### 6.4 `vm.max_map_count`

Each thread stack costs one mapping. Default Linux
`vm.max_map_count = 65530`. At W10k the daemon uses roughly 10 005
mappings for thread stacks plus the rayon pool plus `mmap`-backed
buffers in `crates/fast_io`. Comfortable margin, but the number is
recorded so the W10k-on-glibc combination is auditable.

## 7. Lock contention measurements

The audit (Section 7) enumerates the shared mutable structures
visible from a session thread. The benchmark measures each.

### 7.1 `SharedLogSink` mutex

At W1k and W10k, run two passes: default verbosity (one log line per
accept, auth, and close) and verbose (`-vv`, one line per file
transferred). Sample with `parking_lot` deadlock-detection counters
or `perf lock contend -p $PID` for the daemon process. Expectation
from the audit: at default verbosity the mutex is invisible; at
verbose it serialises all session threads through one writer. The
benchmark records the wait-time distribution.

### 7.2 `ConnectionLimiter` lock file

Run W1k and W10k twice: once with no `lock_file` directive (the
default), once with `lock_file = /dev/shm/oc-rsyncd.lock` and the
test module configured with `max_connections = N` for each waypoint.
The flock round-trip
(`crates/daemon/src/daemon/module_state/connection_limiter.rs:67-91`)
serialises across all session threads when the file is configured.
The benchmark records median and p99 acquire latency under
contention.

### 7.3 `ModuleRuntime::active_connections`

Per-module `AtomicU32` CAS loop
(`crates/daemon/src/daemon/module_state/runtime.rs:55-95`). The
audit predicts negligible contention; the benchmark verifies by
counting CAS retries via a debug-build instrumentation hook (gated
behind `--features bench-instrumentation`).

### 7.4 `ConnectionCounter`

Single shared `AtomicUsize`
(`crates/daemon/src/daemon/sections/server_runtime/connection_counter.rs:17`).
Two atomic ops per session, no locks. Expected to stay invisible at
all waypoints; the benchmark records the contended-cycles count
from `perf stat -e cache-misses,cache-references` for the daemon
process as a sanity check.

### 7.5 `BufferPool`

`crossbeam_queue::ArrayQueue` plus thread-local cache in
`crates/engine/src/local_copy/buffer_pool/pool.rs`. The audit
predicts no first-order bottleneck within W10k. The benchmark
verifies by reading the buffer-pool admission counter (the
soft-cap rejection path the audit calls the "safety valve") at run
end and asserting it stays at zero for W100 and W1k.

### 7.6 Rayon global pool

Multiple session threads dispatching `par_iter()` queue onto the
single `rayon::ThreadPool::current()`. The audit (Section 6.4)
predicts queue depth growth, not thread count growth. The benchmark
records `rayon`'s `current_num_threads()` at run end and the
per-session p99 of any `par_iter()`-bounded code path called from
the transfer body. If queue saturation is visible at W1k, the
finding feeds into #1751 (rayon-via-`spawn_blocking`).

## 8. Comparison oracle

Upstream rsync 3.4.1 in `target/interop/upstream-src/rsync-3.4.1/`
forks one child process per accepted TCP connection (the audit's
Section 8 cites `clientserver.c` and `socket.c:599`). Identical
workloads run against:

- A: oc-rsync sync path (this audit's subject), with the Section 3
  precondition fix applied.
- B: oc-rsync async path behind `--features async-daemon`
  (post #1935 - this row is recorded as a placeholder until the
  feature lands; the benchmark plan does not block on it).
- C: Upstream rsync 3.4.1 daemon (fork model).

For each `(workload, platform, configuration)` triple the harness
records the metrics from Section 5. The interesting cells in the
matrix:

| Cell | What it answers |
|------|-----------------|
| W100 / Linux / A vs C | Does the sync path match upstream at low load? Within 5% of upstream is the project performance rule. |
| W1k / Linux / A vs C | Does upstream's COW page table cost at fork time exceed our thread stack reservation under sustained load? |
| W10k / Linux / A vs C | Does the sync path die before upstream does on the same default ulimits? |
| W10k / Linux / A with vs without lock_file | Does the lock-file flock round-trip dominate at scale? |
| W10k / macOS / A vs C | Does macOS's lower default `RLIMIT_NOFILE = 256` make the sync path even more constrained than Linux? |
| W10k / Windows / A vs C | Does Windows' uncapped fd budget (#1682) and IOCP-vs-epoll difference favour or punish the sync path? |

Upstream's fork model is the floor: anything the sync path does
worse than upstream is a regression against the wire-equivalent
target. Anything the sync path does better is bonus. The async path
(when available) is the ceiling.

## 9. Decision criteria for the async migration (#1935)

The benchmark exists to gate the question "do we ship #1935 or do we
defer it indefinitely?" The audit's Section 9.3 already names the
gate: idle RSS under 200 MiB at 10 000 connections, accept latency
comparable to upstream, drain latency on SIGTERM under 5 s.

This plan refines those into hard pass / fail thresholds, evaluated
against the W10k waypoint on default Linux x86_64 with the Section 3
precondition fix applied:

| Threshold | Sync-path target | Trigger #1935 if |
|-----------|------------------|------------------|
| Daemon RSS at peak | <= 1.5 GiB | RSS > 2 GiB or unable to reach 10 000 sessions on a 16 GiB host. |
| Connect latency p99 (steady state) | <= 50 ms | p99 > 200 ms or accept failures > 0.1% of attempts. |
| SIGTERM-to-drain latency | <= 1 s for sessions in `Greeting` state, <= 30 s for sessions actively transferring | > 5 s for greeting-state sessions. |
| Throughput vs upstream (W10k) | Within 5% of upstream rsync 3.4.1 daemon under identical workload | More than 10% slower than upstream. |
| Failure mode at `max_sessions` cap | Silent drop, info log, no client-visible error code corruption | Anything else (timeout, RST mid-greeting, error code != upstream). |

Any single threshold tripped in W10k triggers #1935. All thresholds
clean closes #1935 and keeps the sync path the default. W1k thresholds
are advisory; the operating point is the gate, but only W10k results
make the migration call.

The decision is recorded in this document as an addendum after the
first full benchmark run on hardware, not in any README or changelog.
The async listener stays behind the `async-daemon` Cargo feature
either way, per the RFC at
`docs/audits/daemon-async-listener-rfc.md` Section 5.2 (#1934).

## 10. Open questions

These are flagged here for the benchmark author. Each maps to an
existing tracker.

1. **macOS default fd limit.** macOS `RLIMIT_NOFILE = 256` even with
   `launchctl limit` is the soft-process default unless raised. The
   benchmark cannot reach W1k on macOS without an explicit raise.
   Question: does the benchmark report "macOS sync path caps at 256
   concurrent sessions on default tunables" as the headline, or
   raise the limit via `launchctl limit maxfiles 65536 65536` and
   report at the higher waypoint? Recommendation: report both. The
   default-tunables number is the operator-visible truth; the
   raised number isolates the daemon's behaviour from the platform's.
2. **Windows IOCP semantics.** Tokio uses IOCP on Windows; the sync
   path uses blocking `accept` per `connection.rs:230`. Tracker
   #1682 owns the Windows-specific accept-semantics question. The
   benchmark records Windows results without speculating on whether
   the sync path's blocking accept on Windows behaves differently
   from Linux's. If a divergence is visible the finding feeds into
   #1682.
3. **Rayon contention attribution.** If the benchmark sees rayon
   queue saturation at W1k, the cause could be (a) genuine CPU
   oversubscription, (b) lock-step dispatch where every session
   reaches `par_iter()` simultaneously, or (c) a single hot path in
   `crates/transfer` that fans out wider than necessary. Triage
   feeds into #1751.
4. **Reproducibility across CPU vendors.** Default GitHub Actions
   Linux runners run on Intel Cascade Lake; the dedicated runner
   used for the slow tier is AMD Ryzen. Thread-spawn cost and
   atomic-CAS cost both vary by vendor. Question: does the
   benchmark report median across both, the worst case, or both
   numbers separately? Recommendation: both numbers separately,
   tagged with CPU model in run metadata.
5. **Driver-side overhead.** The driver opens 10 000 client TCP
   sockets on the same host as the daemon. The driver's fd budget
   competes with the daemon's. Question: should the driver run on
   a separate host? Recommendation: same host for W100 and W1k
   (eliminates network jitter from the latency measurement),
   separate host for W10k (avoids fd starvation in the driver).
6. **Cold vs warm runs.** The first run after `oc-rsync` daemon
   start pays page-fault costs that subsequent runs do not. The
   benchmark records both; the headline number is the warm run
   (third measured run after two warmup runs).
7. **Drain semantics under SIGTERM at peak.** The accept loop polls
   signal flags every 500 ms
   (`crates/daemon/src/daemon/sections/server_runtime/listener.rs:45`).
   At W10k with sessions actively in transfer, drain latency is
   bounded below by the slowest session's `read`/`write` syscall,
   not by the poll interval. Tracker #1683 (under #1675) addresses
   the poll interval; this benchmark records the interaction.

## 11. References

- Parent audit:
  `docs/audits/daemon-thread-per-connection-scalability.md`
  (task #1673, PR #3705).
- Async listener RFC:
  `docs/audits/daemon-async-listener-rfc.md` (task #1934).
- Daemon accept loop:
  `crates/daemon/src/daemon/sections/server_runtime/accept_loop.rs`,
  `crates/daemon/src/daemon/sections/server_runtime/connection.rs`,
  `crates/daemon/src/daemon/sections/server_runtime/connection_counter.rs`,
  `crates/daemon/src/daemon/sections/server_runtime/listener.rs`,
  `crates/daemon/src/daemon/sections/server_runtime/workers.rs`.
- Module state:
  `crates/daemon/src/daemon/module_state/runtime.rs`,
  `crates/daemon/src/daemon/module_state/connection_limiter.rs`.
- Greeting:
  `crates/daemon/src/daemon/sections/greeting.rs`.
- Existing daemon bench harness to extend:
  `crates/daemon/benches/daemon_benchmark.rs`.
- Interop harness reused for upstream comparison:
  `scripts/rsync-interop-server.sh`,
  `tools/ci/run_interop.sh`.
- Related trackers: #1673 (audit, completed via PR #3705),
  #1675 (epoll / kqueue evaluation, completed),
  #1933 (this plan), #1934 (RFC, completed),
  #1935 (async listener implementation, pending),
  #1683 (lower `SIGNAL_CHECK_INTERVAL`),
  #1682 (Windows accept semantics),
  #1751 (rayon via `spawn_blocking`).
