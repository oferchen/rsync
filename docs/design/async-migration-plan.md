# Async Migration Plan (#1594)

Status: Design - canonical plan, supersedes the earlier sketch under the
same path.
Audience: maintainers of `daemon`, `core`, `transfer`, `engine`,
`rsync_io`, `fast_io`.
Scope: a coherent, opinionated migration plan for incrementally adopting
async I/O in `oc-rsync` without breaking the synchronous transfer engine
or wire-protocol compatibility.

The codebase is almost entirely synchronous and threaded today. Several
issues (#1367, #1411, #1593, #1595, #1751, #1796, #1797, #1805, #1806,
#1889, #1890, #1891, #1892, #1935, #2136) circle around async, but each
decision risks dragging the project in a different direction. This plan
draws the lines once so individual issues can be resolved against a
shared anchor.

## 1. Status quo (sync, threaded)

### 1.1 Daemon (`crates/daemon`)

- Production accept loop is sync: `fn serve_connections` at
  `crates/daemon/src/daemon/sections/server_runtime/accept_loop.rs:11`
  binds `std::net::TcpListener` sockets, then either
  `run_single_listener_loop` (single bind) or `run_dual_stack_loop`
  (dual stack via `std::sync::mpsc`) in
  `crates/daemon/src/daemon/sections/server_runtime/connection.rs`
  takes each `(TcpStream, SocketAddr)` and calls
  `spawn_connection_worker` (`connection.rs:163`) which does
  `thread::spawn` wrapped in `catch_unwind` (`connection.rs:178`).
- Each worker walks the rsync state machine synchronously through
  `Handshaking -> Authenticating -> Listing | Transferring ->
  Completed | Failed` (states in
  `crates/daemon/src/daemon/session_registry.rs`).
- An async listener prototype exists but is dead-coded and gated on
  the `async` cargo feature plus `#[cfg(test)]` exposure:
  `crates/daemon/src/daemon/async_session/mod.rs:22` carries
  `#[allow(dead_code)] // not yet wired to production`, the
  re-exports under `crates/daemon/src/daemon/async_session/mod.rs:28`
  are test-only, and `AsyncDaemonListener` itself is in
  `crates/daemon/src/daemon/async_session/listener.rs:108`.
- Tokio dependency is declared optional with the feature
  `async = ["dep:tokio", "core/async"]` at
  `crates/daemon/Cargo.toml:20`. Default build ships zero tokio.

### 1.2 SSH transport (`crates/rsync_io/src/ssh`)

- Default transport is a forked `ssh` subprocess. Implementation:
  - `crates/rsync_io/src/ssh/builder.rs` for `std::process::Command`
    construction.
  - `crates/rsync_io/src/ssh/connection.rs:30` for the `SshConnection`
    type that holds `Arc<Mutex<Option<Child>>>`, `ChildStdin`,
    `ChildStdout`, and a stderr drain thread.
  - `crates/rsync_io/src/ssh/aux_channel.rs` for the background
    thread that drains `ChildStderr` to avoid pipe-buffer deadlocks.
- Both halves use blocking pipe I/O (`Read`/`Write` impls on
  `ChildStdout`/`ChildStdin`).
- The `embedded-ssh` feature pulls in `russh` (`crates/rsync_io/Cargo.toml:22,32`)
  and currently *internally* builds a tokio current-thread runtime
  inside a synchronous facade: `pub fn connect_and_exec` at
  `crates/rsync_io/src/ssh/embedded/connect.rs:107` calls
  `tokio::runtime::Builder::new_current_thread()` then `rt.block_on()`.
  No async surface is exposed to callers.

### 1.3 Engine (`crates/engine`)

- Pure synchronous + rayon. The concurrent delta pipeline at
  `crates/engine/src/concurrent_delta/mod.rs` dispatches work via
  `rayon::scope` and a bounded `crossbeam_channel` work queue
  (`work_queue/bounded.rs:8,89-104`, capacity
  `2 * rayon::current_num_threads()`).
- Result reorder uses `std::sync::mpsc`
  (`concurrent_delta/consumer.rs:47`) feeding a `ReorderBuffer`
  (`reorder/`) that restores file-list order before wire output.
- The audit table at `concurrent_delta/mod.rs:55-165` enumerates
  every `par_iter` site and classifies it SAFE / GUARDED. Ordering
  invariants assume rayon's `IndexedParallelIterator::collect`
  preserves order; an async-task fanout would invalidate that.

### 1.4 Transfer (`crates/transfer`)

- Sync receiver pipeline runs `run_pipeline_loop_decoupled` in
  `crates/transfer/src/receiver/transfer/pipeline.rs`.
- Network -> disk handoff is the hand-rolled lock-free SPSC at
  `crates/transfer/src/pipeline/spsc.rs:1-21` over
  `crossbeam_queue::ArrayQueue` with `AtomicBool` disconnect flags
  and `std::hint::spin_loop` waits. Zero syscalls on the hot path.
- A parallel async file-job pipeline exists behind the `async`
  feature: `crates/transfer/src/pipeline/async_pipeline.rs:21` uses
  `tokio::sync::mpsc` and `tokio_util::sync::CancellationToken`, plus
  `crates/transfer/src/pipeline/async_dispatch.rs`. Not the default
  receive path.
- The disk-commit thread is a plain `std::thread` (see
  `crates/transfer/src/disk_commit/`); it never enters tokio.

### 1.5 fast_io (`crates/fast_io`)

- All submission paths are synchronous. `io_uring` integration in
  `crates/fast_io/src/io_uring/` calls `submit_and_wait()` and
  blocks the caller until CQEs are ready. There is no event-loop
  integration. The IOCP path on Windows
  (`crates/fast_io/src/iocp/`) is similarly sync-facing despite
  using overlapped I/O internally.

### 1.6 Checksums (`crates/checksums`)

- Pure CPU. Rayon `par_iter` for the rolling+strong batches
  (`rolling/parallel.rs`, `parallel/blocks.rs`, `parallel/files.rs`)
  with SIMD inner loops. No I/O involvement. No threading model
  change needed.

### 1.7 CLI / core

- `cli` and `core` are sync from CLI parse through `core::session()`.
  `core` has an optional `async` feature
  (`Cargo.toml:107: async = ["daemon/async", "core/async"]`) that
  cascades into the daemon's async types but does not expose an
  async API to CLI consumers today.

### 1.8 Workspace tokio surface

The whole workspace has tokio as `[workspace.dependencies] tokio`
(version 1.52, `Cargo.toml:188`). Today's consumers:

- `daemon` (optional, behind `async` feature) - `Cargo.toml:45`.
- `rsync_io` (optional, behind `embedded-ssh`).
- `protocol` (`crates/protocol/src/negotiation/sniffer/async_read.rs`
  is the only `AsyncRead` site in the protocol crate; gated on test).
- `transfer` (optional, for the async pipeline prototype).

`--no-default-features` builds today do *not* include tokio. That
invariant must survive the migration.

## 2. Why async matters - per subsystem

The honest answer is "it depends per subsystem". A blanket "go async"
decision would oversubscribe rayon, churn the hot path, and bring no
benefit to CPU-bound code. The breakdown below names winners and
losers.

### 2.1 Daemon accept layer - HIGH value

- One-thread-per-connection breaks down past roughly 1k concurrent
  connections (see `docs/design/daemon-async-accept-sync-workers.md`
  section 1). Each spawned worker reserves an 8 MiB virtual stack;
  10k connections is 80 GiB of address space.
- Module listings and short probes complete in milliseconds. Thread
  creation cost rivals the productive work.
- The accept path itself serialises: blocking `accept`, blocking
  `thread::spawn`, then loop. An async listener using
  `tokio::net::TcpListener` and a per-connection
  `tokio::spawn` collapses this to per-task scheduling cost.
- **Verdict**: async pays back. Target operators wanting fan-in
  daemons (CI artifact stores, mirror pulls) at 1k-10k connections.
  This is the only subsystem where async unlocks scale that sync
  cannot reach.

### 2.2 SSH transport - MAYBE

- The bottleneck is two-half I/O across a pipe. With a sync
  `ChildStdout::read`, one thread is blocked on the pipe while the
  other half is idle. Async `tokio::process::Child` lets one task
  drive `select!(read, write)` and overlap RTT and disk latency.
- Workloads where overlap wins (per
  `docs/design/async-ssh-transport.md` section 4): high-RTT links
  (>= 50 ms), slow destination disks, many-small-files transfers
  with frequent flushes.
- Workloads where overlap is neutral or negative: LAN/loopback,
  single-large-file already saturating the pipe buffer, CPU-bound
  paths.
- The current sync model already uses an SPSC pipeline
  (`crates/transfer/src/pipeline/spsc.rs`) that hides some of the
  serialisation for daemon TCP but cannot help SSH pipes.
- **Verdict**: worth a bench gate. Recommendation is async behind a
  feature flag, promoted to default only if the benchmark shows
  > 10% sustained wall-clock improvement on at least one supported
  corpus without LAN regression. Until then, sync stays default.

### 2.3 Engine / delta pipeline - NO

- Delta computation, block matching, and the reorder buffer are
  pure CPU + memory. No I/O wait to overlap. Adding async to this
  layer would only insert wakeup cost on a path the SPSC + reorder
  buffer already paces.
- The wire-ordering audit in `concurrent_delta/mod.rs:55-165`
  assumes rayon's indexed collect order. An async fanout would
  break the GUARDED classification.
- **Verdict**: stay sync. Rayon owns CPU parallelism here.

### 2.4 Checksums - NO

- Pure CPU; SIMD inner loops. No `.await` site exists or is
  meaningful. Async would only add overhead.
- **Verdict**: stay sync. Rayon + SIMD is the right model.

### 2.5 Disk I/O - LOW-MEDIUM value, separate track

- Disk writes go through the disk-commit thread
  (`crates/transfer/src/disk_commit/`). io_uring already overlaps
  submission within the thread without involving tokio.
- Async file I/O over the standard library (`tokio::fs::File`) just
  hands work back to the blocking pool; no real benefit over a
  dedicated commit thread. The only async win on disk would come
  from `tokio-uring` (`docs/design/async-io-uring-impact.md`
  section 3.5), which requires a single-threaded runtime and is
  incompatible with our multi-threaded daemon.
- **Verdict**: stay sync; revisit only when #1595 closes the
  io_uring composition question.

### 2.6 CLI / one-shot client - NO

- A single-shot CLI never multiplexes connections. Threading is
  cheaper than spinning up a tokio runtime. Async here would only
  bloat startup time.
- **Verdict**: stay sync. Async daemon and async client are not
  the same problem.

## 3. Incremental adoption strategy

Migrate in disciplined phases. Each phase gates on the previous one
landing in production, not just behind a feature flag.

### Phase 1 - Async daemon accept loop (tracking: #1934 RFC, #1935 impl, #1367)

Promote `crates/daemon/src/daemon/async_session/listener.rs` to a
default-on alternative to `serve_connections`. Constraints:

- Accept loop, socket option application, optional reverse DNS, and
  hand-off live on tokio.
- Per-connection transfer state machine stays sync. The handoff is a
  bounded channel between an async accept task and a pre-spawned
  sync worker (the hybrid model documented in
  `docs/design/daemon-async-accept-sync-workers.md`).
- `--no-default-features` builds keep the sync `thread::spawn` path
  bit-for-bit.
- Promotion criteria: golden byte tests pass, criterion regression
  budget within 5% on the LAN baseline, `tools/ci/run_interop.sh`
  passes against upstream 3.0.9, 3.1.3, and 3.4.1, and the
  10k-connection bench in `docs/design/daemon-tpc-benchmark-plan.md`
  shows fan-in scaling that sync cannot match.
- Kill switch: `OC_RSYNC_DAEMON_ASYNC=0` reverts to sync at next
  process start without rebuild.

### Phase 2 - Async sync/async bridge crate (tracking: #1591, #1751)

Land `crates/transfer/src/async_compat.rs` (or a sibling crate) that
provides:

- `rayon_bridge(min_units, units, job)` per
  `docs/design/tokio-spawn-blocking-rayon.md` section 6. Single
  entry point for all bridged rayon work.
- `TransferChannel<T>` trait per
  `docs/design/async-channel-abstraction.md`. Sites that cross the
  sync/async boundary use a `flume` (or equivalent) channel that
  supports both `send`/`recv` and `send_async`/`recv_async`. Pure
  sync hot paths keep `crossbeam_channel`. Pure async paths use
  `tokio::sync::mpsc`.

Phase 2 has no user-visible behaviour change. It exists so phase 3
has a vocabulary.

### Phase 3 - Async SSH transport behind a feature (tracking: #1593, #1411, #1796, #1797, #1805, #1806, #1889, #1890, #1891, #1892)

- Add `--features async-ssh`. Implementation order:
  1. Tokio-process backend: swap `std::process::Command` for
     `tokio::process::Command`. Expose `AsyncRead`/`AsyncWrite`
     halves. Drive the bidirectional pump with
     `tokio::io::copy_bidirectional`.
  2. Run `scripts/benchmark_remote.sh` against LAN and a netem-shaped
     high-RTT link. Promotion threshold: > 10% sustained wall-clock
     improvement on at least one corpus, no LAN regression.
  3. Embedded russh (`crates/rsync_io/src/ssh/embedded/`) is a
     separate, longer track. The sync facade at `connect.rs:107`
     stays until russh is production-ready; then swap the transport
     behind the same `async-ssh` feature.
- The sync `std::process` SSH transport remains default and supported
  throughout phase 3.

### Phase 4 - Async io_uring composition (tracking: #1595)

- Decision deferred until phase 1 and phase 3 give us measured data.
- Two options on the table:
  1. Keep io_uring sync, drive it from `spawn_blocking` on the
     blocking pool. Lowest risk, no runtime constraints.
  2. Adopt `tokio-uring` for single-threaded callers behind a
     separate feature. Forbidden in the same process as the
     multi-threaded daemon runtime (see
     `docs/design/async-io-uring-impact.md` section 3.5).
- No phase 4 commitment in this plan; #1595 owns the decision.

### Phase 5 - Async daemon session protocol (tracking: #2136 actor pattern)

- Only after phases 1-4 land. Move the per-session greeting,
  authentication, and module-select state machine to async tasks
  (the actor pattern in #2136). The transfer state machine stays
  sync via a bounded handoff channel.
- This is the maximalist phase. It may never land; sync workers may
  remain forever if phase 1 fan-in is enough. The plan does not
  commit to phase 5.

## 4. Runtime choice

### Recommendation: tokio. No second runtime, ever.

### Justification

- **It is already in the workspace.** `tokio = { version = "1.52", ... }`
  at `Cargo.toml:188` is paid for by `russh` (embedded SSH) and the
  optional `async` feature on `daemon`. The marginal cost of using
  more of it is zero; the cost of adding a *second* async runtime is
  bridging adapters (`async-compat`), duplicate timer wheels, and
  split reactor responsibilities. The #1779 audit already concluded
  this; #1780 codifies the "no second runtime" rule.
- **russh is tokio-native** (`crates/rsync_io/Cargo.toml:22`). Any
  async SSH path that uses russh must run on tokio. async-std and
  smol would force `async-compat` shims, the timer wheel costs
  double, and the reactor would split responsibilities.
- **tokio's `spawn_blocking` blocking pool** (default 512 threads)
  is the canonical bridge to rayon and to blocking syscalls. The
  pattern documented in `docs/design/tokio-spawn-blocking-rayon.md`
  composes cleanly with our existing rayon-driven CPU paths.
- **tokio is the only runtime that integrates with `russh`,
  `tokio-uring`, and `tokio-process` without third-party shims.**
  Every async crate we would want to adopt assumes tokio.

### Why not smol

- Small, dependency-light, fashionable. But would become a *second*
  runtime alongside tokio. The bridging cost (`async-compat`,
  duplicate timer wheels, separate reactor) erases the footprint
  win on any non-trivial integration. Rejected.

### Why not async-std

- API shape mirrors std. But maintenance has slowed; the project
  has effectively gone dormant. Same second-runtime problem as
  smol. Rejected.

### Why not glommio / monoio (thread-per-core, io_uring native)

- Tempting for the daemon fan-in workload. But thread-per-core
  forbids `Send` futures, which is incompatible with rayon
  parallelism and with the existing sync transfer engine. Adopting
  it would require rewriting the engine to be thread-pinned, a
  scope this plan rejects. Cited and rejected here so it does not
  resurface.

### Tokio feature set

The `async` feature in `daemon/Cargo.toml:45` pulls
`tokio = { features = ["net", "io-util", "sync", "rt", "time"] }`,
i.e. the multi-thread runtime, networking, timers, and sync
primitives. The workspace `Cargo.toml:188` declares
`["rt-multi-thread", "io-util", "net", "fs", "sync", "time",
"process", "macros"]`. No need to enable `signal` or `tracing`;
they are gated separately.

## 5. Sync/async bridge points

The migration always lives at a sync/async boundary. The rules
below name where the boundary sits and which pattern bridges it.

### 5.1 Async accept -> sync transfer worker

- Pattern: bounded sync channel
  (`crossbeam_channel::bounded(N)` or `flume::bounded(N)`).
- Producer: tokio accept task, sends `(TcpStream, SocketAddr)`.
- Consumer: pre-spawned sync worker thread, blocking `recv`.
- N is `2 * worker_count` to keep workers fed without growing the
  in-flight queue past memory bounds.
- See `docs/design/daemon-async-accept-sync-workers.md` section 3
  for the full topology.

### 5.2 Async session -> sync rayon CPU work

- Pattern: `tokio::task::spawn_blocking(move || rayon_job())`
  with a threshold short-circuit. Wrap as `rayon_bridge`.
- Never call `rayon::par_iter` directly from an `async fn` on a
  tokio worker; it stalls that worker for the entire parallel job
  (see `docs/design/tokio-spawn-blocking-rayon.md` section 2).
- Never call `tokio::runtime::Handle::block_on` from a rayon
  worker; it deadlocks if the runtime worker count is low.
- Bound concurrent bridges via a semaphore so they cannot exhaust
  the 512-thread blocking pool under fan-out.

### 5.3 Sync producer -> async consumer (and vice versa)

- Pattern: `flume::bounded(N)`. Flume exposes both
  `send`/`recv` (sync) and `send_async`/`recv_async` (async) on
  the same channel object, so we do not need wrapper threads.
- Alternative: `tokio::sync::mpsc` with a `blocking_send` /
  `blocking_recv` adapter on the sync side. Acceptable when the
  sync side is already a blocking thread that can park.
- Pure hot paths (network ingest -> disk commit SPSC at
  `crates/transfer/src/pipeline/spsc.rs`) keep their lock-free
  `crossbeam_queue::ArrayQueue` and never cross an async boundary.

### 5.4 Blocking syscall in an async context

- Pattern: `tokio::task::spawn_blocking` if the call may take
  longer than 100 microseconds. Below that, the spawn cost
  dominates and direct invocation is acceptable on a multi-thread
  runtime.
- io_uring `submit_and_wait` blocks until CQEs are ready; wrap in
  `spawn_blocking` when called from async (see
  `docs/design/async-io-uring-impact.md` section 3.1).
- `std::process::Command::spawn` blocks on `fork`+`execve`; prefer
  `tokio::process::Command::spawn` from async contexts.

### 5.5 Async I/O in a sync context

- Pattern: build a current-thread tokio runtime, `rt.block_on(fut)`.
  This is what `crates/rsync_io/src/ssh/embedded/connect.rs:107`
  already does for embedded SSH. Acceptable when the sync caller
  is genuinely one-shot (CLI invocation, test harness).
- Forbidden when the sync caller is already inside another
  runtime's worker thread; that nests runtimes and deadlocks tokio.

## 6. Backward-compat strategy

### 6.1 Feature flags

- Top-level `--features async` (workspace `Cargo.toml:107`) cascades
  to `daemon/async` and `core/async`. Default off until phase 1's
  promotion gates clear.
- `--features embedded-ssh` (existing) pulls in russh. Independent
  of the migration.
- New: `--features async-ssh` for phase 3, default off.
- New: `--features async-transfer` reserved for phase 5, default
  off, unused until then.

### 6.2 Default-build invariant

- `--no-default-features` MUST produce a tokio-free binary
  throughout the migration. Enforce by adding
  `tools/ci/check_tokio_boundary.sh` (does not exist today; tracked
  as a follow-up): grep the dependency tree of a
  `--no-default-features` build, fail if `tokio` appears.
- The sync `thread::spawn` daemon accept path
  (`crates/daemon/src/daemon/sections/server_runtime/accept_loop.rs:11`)
  must compile and pass tests with default features off.

### 6.3 Runtime kill switches

- `OC_RSYNC_DAEMON_ASYNC=0` reverts to the sync daemon path at
  next process start. No rebuild required.
- `OC_RSYNC_SSH_ASYNC=0` reverts to the sync SSH path at next
  process start.
- Documented in `docs/man/oc-rsyncd.conf.5` and friends when each
  flag goes live.

### 6.4 Wire protocol and CLI surface

- Wire protocol stays exactly the same. The async migration is
  purely an implementation detail. Golden byte tests in
  `crates/protocol/tests/golden/` gate this.
- CLI flag surface stays the same. No new user-visible flag for
  the async daemon; operators use the cargo feature at build time
  or the kill switch at runtime.
- Exit codes stay 1:1 with upstream. `JoinError::is_panic()` from
  spawned tasks maps to `ExitCode::PROTOCOL` with a `[server]`
  trailer, matching upstream's fork-per-connection crash report.

### 6.5 Interop coverage

- Every phase that lands in default-on form must pass
  `tools/ci/run_interop.sh` against upstream rsync 3.0.9, 3.1.3,
  and 3.4.1.
- Interop suite includes both push and pull, daemon mode, SSH
  mode, and large-file as well as many-small-files corpora.

## 7. Risk register

Risks ordered by likelihood times blast radius.

### R1 - Rayon thread pool fighting tokio runtime (HIGH likelihood, HIGH blast)

- The global rayon pool defaults to `num_cpus`. The tokio
  multi-thread runtime defaults to `num_cpus`. Together they
  spawn `2 * num_cpus` threads contending for the same cores.
- Mitigation: when the daemon owns the runtime, pin rayon to a
  smaller dedicated pool (`rayon::ThreadPoolBuilder::new()
  .num_threads(num_cpus - tokio_workers).build()`) and use
  `pool.install(|| par_iter(...))` from `spawn_blocking`.
- Cited in `docs/design/tokio-spawn-blocking-rayon.md` section 5.

### R2 - async `std::process::Child` for SSH inherits a fork problem (HIGH likelihood, MEDIUM blast)

- `tokio::process::Command::spawn` still does `fork+execve` under
  the hood. The cost-per-connection is unchanged from sync; only
  the I/O wait between fork and exec yields cooperatively. SSH
  setup is dominated by the SSH handshake, not the fork.
- Mitigation: do not promise that async SSH eliminates the
  subprocess overhead. The win is overlap of `read` and `write`
  on the established pipe, not connection setup.
- The embedded-russh path eliminates fork entirely but at the
  cost of taking on key/agent/known-hosts compatibility ourselves.

### R3 - SPSC pipeline turns into a thread parker (HIGH likelihood, HIGH blast)

- `crates/transfer/src/pipeline/spsc.rs` deliberately uses
  `std::hint::spin_loop` to avoid futex/park syscalls on the hot
  network->disk path. If async leaks into this pipeline, the
  spin loop becomes a busy-wait inside a tokio worker, starving
  the runtime.
- Mitigation: the SPSC stays sync forever. Any async caller
  enters it only via `spawn_blocking` (or via a dedicated
  long-lived thread). Enforce by keeping the SPSC `Sender`/`Receiver`
  types `!Send + !Sync` across async boundaries (review check, not
  type-level).

### R4 - Double feature explosion (MEDIUM likelihood, MEDIUM blast)

- We already have `async`, `embedded-ssh`, `io_uring`, `iocp`,
  `parallel`, `concurrent-sessions`, `tracing`. Adding
  `async-ssh` and `async-transfer` brings the matrix to a size
  CI cannot exhaustively cover.
- Mitigation: define and enforce a small set of "interesting"
  combinations in CI (default, full async, no async, no tokio).
  Document the combination in `docs/feature-flags.md`. Other
  combinations are best-effort.

### R5 - `tokio-uring` single-thread requirement (MEDIUM likelihood, HIGH blast if chosen)

- `tokio-uring` requires a single-threaded runtime. Cannot run
  on the multi-thread runtime the daemon already needs.
- Mitigation: tokio-uring is *only* considered for the
  single-shot CLI path under phase 4, never for the daemon.
  Documented in `docs/design/async-io-uring-impact.md` section 3.5
  with the same conclusion.

### R6 - russh API churn (MEDIUM likelihood, LOW blast)

- russh is actively maintained but has had API breaks between
  major versions. Pinning to a workspace version pin and gating
  the embedded SSH crate behind `embedded-ssh` isolates the
  blast radius.
- Mitigation: `russh = { workspace = true, optional = true }` is
  the current pattern (`crates/rsync_io/Cargo.toml:22`). Keep it.

### R7 - Async cancellation drops in-flight io_uring SQEs (LOW likelihood, HIGH blast)

- `spawn_blocking` futures cannot be cancelled. Dropping the
  join handle leaves the blocked syscall running on the blocking
  thread. If io_uring SQEs are in flight when cancelled, the
  ring is left in an inconsistent state.
- Mitigation: cooperative cancellation tokens checked between
  batches. Never use `spawn_blocking` for an unbounded io_uring
  loop; submit small batches and re-poll the cancellation token.
- Documented in `docs/design/tokio-spawn-blocking-rayon.md`
  section 5.

### R8 - Panic isolation regression (LOW likelihood, HIGH blast)

- The sync daemon uses `catch_unwind` per worker
  (`connection.rs:184`) so a single connection panic does not
  tear down the daemon. The async daemon at
  `async_session/listener.rs:212` relies on `tokio::spawn`
  capturing panics as `JoinError::is_panic()`.
- Mitigation: every promotion gate includes a panic-isolation
  test that triggers a deliberate panic mid-session and asserts
  the daemon still accepts new connections. Same coverage as
  `crates/daemon/src/tests/chunks/run_daemon_panic_isolation_keeps_daemon_alive.rs`,
  but exercising the async path.

### R9 - Cross-platform divergence (MEDIUM likelihood, MEDIUM blast)

- tokio's `io_uring` integration is Linux-only. macOS and
  Windows targets use kqueue and IOCP respectively. The async
  daemon must not assume Linux-specific behaviour. The IOCP
  path on Windows (`crates/fast_io/src/iocp/`) is sync-facing
  today; an async-IOCP bridge would be a separate evaluation.
- Mitigation: every async-feature gate compiles and tests on all
  three platforms in CI. Use `#[cfg]` only when the async
  primitive itself is platform-specific.

### R10 - Memory regression from blocking-pool fan-out (LOW likelihood, MEDIUM blast)

- tokio's default blocking pool is 512 threads. Under heavy
  fan-out with many `spawn_blocking` calls, RSS can spike.
- Mitigation: bound concurrent bridges via a semaphore; tune
  `tokio::runtime::Builder::max_blocking_threads` to fit the
  daemon's memory budget. Surface as a runtime knob if needed.

## 8. Open questions

This document cannot decide the following. Each open question has
the right owner and the right trigger to resolve.

### Q1 - Should phase 5 ever ship?

The async session protocol (#2136 actor pattern) is the
maximalist phase. If phase 1's fan-in scaling proves enough for
the realistic operator workloads, phase 5 is dead weight.

- Owner: daemon maintainers.
- Trigger: phase 1 in production for 6 months with measured
  connection-rate distributions. If fan-in still bottlenecks,
  reopen #2136. Otherwise close it as won't-do.

### Q2 - flume or tokio::sync::mpsc for the bridge channel?

`docs/design/async-channel-abstraction.md` proposes a
`TransferChannel<T>` trait with `flume` as the default backing
for sync-async bridges. `flume` is a third-party dependency we
do not currently use. The alternative is `tokio::sync::mpsc`
with a `blocking_send` wrapper on the sync side.

- Owner: transfer maintainers.
- Trigger: when the first phase-2 call site lands.
  Benchmark: round-trip latency through the bridge under load.
- Bias: prefer `flume` because it exposes a unified API; treat
  the dependency cost as acceptable.

### Q3 - Should the embedded SSH facade expose an async surface?

Today `crates/rsync_io/src/ssh/embedded/connect.rs:107` builds
a private current-thread runtime and `block_on`s it. The async
work is paid for but the surface is sync. Lifting it to a
public async surface would let async callers skip the runtime
nest, but it would force the sync facade to live alongside.

- Owner: rsync_io maintainers.
- Trigger: when phase 3 (async SSH) starts. If the call site is
  always async-from-async, lift the surface. If sync callers
  are still common, keep the dual surface.

### Q4 - Adopt `tokio-uring` for the single-shot CLI?

The single-shot CLI never multiplexes connections, so the
single-threaded runtime restriction of `tokio-uring` is not a
blocker. But the gain over the existing sync io_uring path
(driven from a dedicated thread) is unproven.

- Owner: fast_io maintainers (composed with #1595).
- Trigger: phase 4. Benchmark sync io_uring (current) vs
  `tokio-uring` on the CLI hot path. Promote only on > 10%
  wall-clock gain.

### Q5 - Per-thread io_uring rings vs shared ring under async?

The shared ring at `crates/fast_io/src/io_uring/shared_ring.rs`
serialises submissions under a mutex. The async daemon's
spawn-per-connection model would amplify this contention. The
fix is per-thread rings (the alternative documented in
`project_io_uring_shared_ring_bottleneck`).

- Owner: fast_io maintainers.
- Trigger: phase 4. Decide before any `tokio-uring` adoption.

### Q6 - Drop the legacy `std::sync::mpsc` sites?

`crates/daemon/src/daemon/sections/server_runtime/connection.rs:286`
still uses `std::sync::mpsc` for legacy server-side wakeups
(audit target under #1592). Phase 1 may obviate them.

- Owner: daemon maintainers.
- Trigger: when phase 1 promotes the async listener to default.

### Q7 - Should the engine ever go async?

Section 2.3 says no. But if the entire workspace moves to async
in phases 1-3, the engine's sync hand-off points become
friction. The alternative is a fully async receive loop with
rayon `spawn_blocking` for CPU.

- Owner: engine + transfer maintainers.
- Trigger: only after phases 1-3 land and the friction is
  measured. The default answer remains "no" unless a concrete
  workload demands it.

## 9. Cross-references

| Tracker | Subject |
|---------|---------|
| #1367   | Daemon async migration (phase 1 origin) |
| #1411   | Async runtime evaluation for SSH transport (see `docs/design/async-runtime-ssh-eval.md`) |
| #1591   | Async-compatible channel abstraction (see `docs/design/async-channel-abstraction.md`) |
| #1592   | Legacy `std::sync::mpsc` audit in daemon connection code |
| #1593   | Async SSH transport evaluation (see `docs/design/async-ssh-transport.md`) |
| #1594   | This plan |
| #1595   | Async vs io_uring composition (see `docs/design/async-io-uring-impact.md`) |
| #1674   | Daemon async accept + sync workers hybrid model (see `docs/design/daemon-async-accept-sync-workers.md`) |
| #1732   | Async channel abstraction landed |
| #1751   | `spawn_blocking` bridge for rayon CPU (see `docs/design/tokio-spawn-blocking-rayon.md`) |
| #1779   | Audit of tokio dependency scope (done) |
| #1780   | No second async runtime rule (done) |
| #1782   | Embedded SSH staging via russh |
| #1796/#1797/#1805/#1806 | SSH transport async-process work |
| #1818   | Sync receiver baseline measured (done) |
| #1889/#1890/#1891/#1892 | SSH transport async hardening |
| #1934   | Async daemon listener RFC |
| #1935   | Async daemon listener implementation |
| #2136   | Actor-pattern session model (phase 5) |
| `docs/design/daemon-tokio-async-listener-impl.md` | Listener implementation notes |
| `docs/design/daemon-thread-per-conn-bench.md` | Sync thread-per-conn baseline |
| `docs/design/daemon-tpc-benchmark-plan.md` | Promotion benchmark plan |
| `docs/design/iouring-daemon-tcp.md` | io_uring TCP integration |
| `docs/design/io-uring-rayon-composition.md` | Rayon-side io_uring composition |
