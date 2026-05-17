# Daemon Async Runtime Choice - tokio vs async-std vs threaded

Status: Design (tracks #1367 with companion task #1590).
Audience: daemon maintainers, release engineering, anyone landing follow-up
work under the broader async migration (#1751, #1935, #1411, #1412).
Scope: pick the runtime (or non-runtime) that the daemon accept loop will
adopt to serve high concurrent connection counts. This is the *which*
question. The *how* lives in
`docs/design/daemon-tokio-async-listener-impl.md` (#1935) and the *why*
lives in `docs/design/daemon-async-accept-sync-workers.md` (the hybrid
accept-plus-sync-workers model). Neither is restated here.

## 1. The question

`crates/daemon/src/daemon/sections/server_runtime/accept_loop.rs` and
`connection.rs` currently bind `std::net::TcpListener` and dispatch each
accepted socket onto an `std::thread::spawn` worker. The thread-per-
connection (TPC) model is the production path today; the bench plan in
`docs/design/daemon-tpc-benchmark-plan.md` (#1933) measures its
ceiling.

A parallel async listener prototype exists at
`crates/daemon/src/daemon/async_session/listener.rs` behind the
`async = ["dep:tokio", ...]` cargo feature
(`crates/daemon/Cargo.toml:20`). The prototype already uses tokio; the
question is whether that choice is correct, and whether async should
ship at all.

This document resolves three sub-questions:

1. async-std vs tokio - which runtime if we go async?
2. async vs threaded - do we go async at all?
3. If async, which tokio flavour and what triggers the cutover?

## 2. async-std vs tokio

### 2.1 Maintenance posture

- **async-std**: development effectively stalled. The last meaningful
  release on crates.io is `1.13.0` (Sep 2023). The repository has had no
  feature work through 2024-2025; open issues sit unanswered, and the
  upstream maintainer has publicly signalled the project is in
  long-term maintenance mode at best. Picking async-std today is
  picking a soft-frozen runtime.
- **tokio**: active. Monthly point releases through 1.40+, dedicated
  maintainers, a release engineering process, and a security advisory
  pipeline (`RUSTSEC-*`). The runtime ships fixes within days of
  upstream kernel quirks (recent example: io_uring `accept_multi`
  edge cases on Linux 6.x).

For a long-lived system daemon shipping across Linux, macOS, and
Windows, runtime maintenance is not negotiable. async-std fails this
gate before any technical comparison.

### 2.2 Feature parity for what the daemon needs

| Capability                       | tokio                             | async-std                            |
|----------------------------------|-----------------------------------|--------------------------------------|
| `TcpListener` accept             | `tokio::net::TcpListener`         | `async_std::net::TcpListener`        |
| Conversion to blocking `std`     | `into_std()` + `set_nonblocking` | `into_std()` available, less polished |
| Signal handling (SIGTERM, Ctrl-C)| `tokio::signal` (cross-platform) | requires `async-ctrlc` third-party    |
| Subprocess / pipe                | `tokio::process` (full)           | `async_std::process` (limited)        |
| Blocking offload                 | `spawn_blocking` (tuned pool)     | `task::spawn_blocking` (less tuned)   |
| Time / timeouts                  | `tokio::time::timeout`           | `async_std::future::timeout`          |
| Filesystem async I/O             | `tokio::fs`                       | `async_std::fs`                       |
| Sync primitives (Semaphore etc.) | `tokio::sync::*`                  | `async_std::sync::*` (thinner)        |

Both runtimes can technically do what the daemon needs (bind, accept,
handoff to sync workers). Tokio's offering is the more complete and
better-instrumented one; the gap matters most in the areas the daemon
actually touches: signal handling and `spawn_blocking`.

### 2.3 Ecosystem alignment

- **russh** (already an optional dependency at
  `crates/rsync_io/Cargo.toml:22,32` and slated for the async SSH path
  per `docs/design/async-runtime-ssh-eval.md` and #1411) is
  tokio-native. async-std would require an `async-compat` shim.
- **hyper / reqwest** are tokio-native. If we ever expose an HTTP
  health endpoint or download module metadata over HTTPS, tokio is the
  zero-shim option.
- The workspace's #1780 rule forbids a second runtime. Tokio is
  already in the workspace via the optional `async` feature
  (`crates/daemon/Cargo.toml:20`) and via the embedded SSH facade
  (`crates/rsync_io/src/ssh/embedded/connect.rs:107`). Adding
  async-std would violate #1780 even if it were technically attractive.

**Verdict**: async-std is rejected on maintenance and #1780 grounds
before performance enters the discussion. Tokio is the only viable
async runtime for this workspace.

## 3. The case for staying threaded

The threaded model has real virtues, and they should be named so the
decision below is not handwaved:

- **No async colouring.** `crates/protocol`, `crates/engine`,
  `crates/checksums`, `crates/transfer`, `crates/core` are 100% sync,
  blocking, rayon-parallel. Keeping the daemon sync keeps the boundary
  clean: no `async fn` viruses, no `.await` insertions in hot paths,
  no `Pin<Box<dyn Future>>` show-ups in error types.
- **Panic isolation is mature.** `catch_unwind(AssertUnwindSafe(|| {
  handle_session(...) }))` in
  `connection.rs:184` is byte-for-byte the upstream-equivalent
  "fork crash kills only the child" guarantee. Async tasks isolate via
  `JoinHandle`, which is comparable but newer to this codebase.
- **Debuggability.** `gdb thread apply all bt` Just Works on a TPC
  daemon. A tokio worker stuck inside `poll_*` requires
  `tokio-console` or careful tracing.
- **No runtime startup cost.** Tokio's multi-thread runtime allocates
  a reactor, a timer wheel, N worker threads, and a blocking pool. A
  daemon serving five concurrent connections pays that cost for nothing.
- **Lower binary size.** Default builds with `async-daemon` off keep
  tokio out of the binary entirely. Embedded operators (small fleet
  rsync mirrors, OpenWRT-style hosts) benefit.

The threaded model fails only at scale. The audit
(`docs/audits/daemon-thread-per-connection-scalability.md`) and the
benchmark plan (`docs/design/daemon-tpc-benchmark-plan.md`) bracket
where: somewhere between W1k and W10k concurrent sessions on default
Linux ulimits, depending on glibc stack reservation and whether the
operator has raised `RLIMIT_NOFILE` and `RLIMIT_NPROC`.

## 4. The case for tokio

Tokio is already in the workspace and is the right anchor for the
remaining async work:

- **#1751** (rayon-via-spawn_blocking) is closed; the pattern is
  proven inside this codebase.
- **#1935** (the implementation companion doc) is pending and assumes
  tokio.
- **#1411** picks tokio + russh for SSH.
- **#1412** (async daemon listener admission control) builds on
  `tokio::sync::Semaphore`.
- The accept-and-dispatch boundary is a sweet spot for async:
  thousands of mostly-idle sockets, dominated by syscalls, no
  CPU-heavy work until handoff. epoll/kqueue/IOCP under tokio's
  reactor scales to 10k+ sockets per thread with kilobytes of memory
  per future, vs megabytes per OS thread stack.
- Sharing a single runtime with the eventual async SSH path
  (russh) keeps the workspace under one timer wheel, one reactor,
  one set of cancellation primitives. This is the #1780 invariant
  cashed in.

The threaded path's only structural advantage - no async colouring
inside transfer code - is preserved by the hybrid model from
`daemon-async-accept-sync-workers.md`: tokio runs the accept layer,
`spawn_blocking` hands sockets to the existing sync workers, and the
transfer state machine never sees an `.await`. The choice of tokio
does *not* commit the project to going async anywhere else.

## 5. Decision

**Adopt tokio with the `rt-multi-thread` flavour for the daemon
async listener path, behind the existing `async = ["dep:tokio", ...]`
cargo feature, gated at runtime by an opt-in
`use-async-listener = true` `oc-rsyncd.conf` directive.**

Rationale, in priority order:

1. **#1780 already decided this.** The workspace is single-runtime and
   that runtime is tokio. async-std is out of scope.
2. **Maintenance posture.** async-std is soft-frozen; tokio is
   actively maintained with a security pipeline.
3. **`rt-multi-thread`, not `current_thread`.** The daemon serves
   independent connections that can run on independent reactor cores.
   `current_thread` would serialise accept and limit reactor
   parallelism to one core; the only reason to pick it would be a
   single-threaded process invariant the daemon does not have. The
   multi-thread runtime can be sized to `min(available_parallelism(),
   8)` per the impl doc (#1935), keeping the reactor footprint small
   without artificially capping accept throughput.
4. **Hybrid model preserves the sync transfer path.** Per
   `daemon-async-accept-sync-workers.md`, the transfer engine never
   becomes async-coloured. Tokio touches only the cheap parts.
5. **Feature-gate keeps small daemons free.** Operators who build
   without `--features async-daemon` ship a tokio-free binary.
   Operators who build with the feature but leave
   `use-async-listener = false` ship a binary that links tokio but
   never starts the runtime.

This is consistent with the conclusions in
`docs/design/async-runtime-ssh-eval.md` (#1411) and
`docs/design/async-migration-plan.md` (#1594): one runtime, opt-in,
hybrid with the existing sync engine.

## 6. Migration cost

The cost is modest because most of it is already paid:

- **Already in tree**: `AsyncDaemonListener` at
  `crates/daemon/src/daemon/async_session/listener.rs:108` binds
  `tokio::net::TcpListener`, drives the accept loop, applies a
  `tokio::sync::Semaphore` for connection limits, and respects a
  `tokio::sync::broadcast` shutdown signal. The async session
  scaffold sits at `crates/daemon/src/daemon/async_session/session.rs`
  and `shutdown.rs`.
- **Already in tree**: optional dependency wiring at
  `crates/daemon/Cargo.toml:45` for `tokio = { ..., features = ["net",
  "io-util", "sync", "rt", "time"] }`. The `async` feature gate is
  live.
- **Already in tree**: the rule that the sync transfer state machine
  is untouched. The bridge is `spawn_blocking` (the pattern proven
  under #1751).

What still needs to happen:

1. Swap the production accept loop in
   `crates/daemon/src/daemon/sections/server_runtime/connection.rs`
   (the `run_single_listener_loop` and `run_dual_stack_loop` paths)
   so that when `use-async-listener = true` is set, the daemon starts
   a tokio runtime and delegates to `AsyncDaemonListener`. When the
   directive is unset or false, the existing
   `spawn_connection_worker` path runs unchanged.
2. Inside the async per-connection task, convert
   `tokio::net::TcpStream -> std::net::TcpStream` via `into_std()` +
   `set_nonblocking(false)`, then call
   `tokio::task::spawn_blocking(|| run_sync_worker(stream, ...))` and
   `.await` the join handle. The blocking closure is the existing
   worker body factored out of `connection.rs`.
3. Add `use-async-listener` as a boolean directive in
   `crates/daemon/src/rsyncd_config/sections.rs` and validate it in
   `crates/daemon/src/rsyncd_config/validation.rs`. Default false.
4. The async features the daemon needs are exactly
   `["rt-multi-thread", "net", "macros", "signal", "sync", "time"]`.
   `rt-multi-thread` and `signal` are not currently in the daemon's
   tokio feature list (`crates/daemon/Cargo.toml:45` declares
   `["net", "io-util", "sync", "rt", "time"]`); they get added under
   #1935.

The cost is bounded: this is a swap-in at the accept boundary, not a
rewrite. The implementation diff under #1935 is expected to stay
inside `crates/daemon` and not touch `crates/core`, `crates/engine`,
`crates/protocol`, `crates/transfer`, or `crates/checksums`.

## 7. Trigger conditions for adopting the async path by default

The cutover is a separate decision from the implementation. The
implementation under #1935 ships the path; the default flip is a
follow-up that needs evidence from the bench plan in #1933. The
gating signals:

- **Sustained concurrent connections > 1 000.** Below 1k the TPC
  ceiling is comfortably clear of typical deployments and the tokio
  reactor's fixed cost is a regression in straight-line latency.
- **Thread-spawn cost > 200 us p99 in the TPC bench.** This is the
  bench plan's signal that `pthread_create` is contending with the
  protocol handshake for wall-clock time. Below 200 us, the spawn
  cost is in the noise next to the rsync handshake itself.
- **Daemon RSS > 1.5 GiB at W10k**, per the threshold table in
  `docs/design/daemon-tpc-benchmark-plan.md` Section 9. Above this,
  the operator cost of TPC stops being acceptable on common 8-16 GiB
  hosts.
- **Connect latency p99 > 200 ms (steady state)** under W10k. This
  is direct evidence the accept loop is serialising on
  `thread::spawn` rather than on accept.
- **Accept failure rate > 0.1%** under W10k arrival shape "all at
  once". This is the operational signal that the kernel listen
  backlog is overflowing because the accept loop cannot drain it
  between spawn syscalls.

Any one threshold tripped flips the default. None tripped keeps the
sync path the default and the async path strictly opt-in.

Below 100 concurrent connections, async stays off regardless. The
small-daemon tokio overhead is not worth paying.

## 8. Five-step migration plan (complementary to #1935)

#1935 owns the *implementation*. This plan owns the *adoption*. The
two are coordinated but separable.

1. **Land the implementation behind the existing feature gate.**
   Merge #1935. Default build remains tokio-free; `--features
   async-daemon` builds the async path. Default behaviour is
   unchanged because `use-async-listener` defaults to false.

2. **Stand up CI coverage for both paths.** Add a `daemon-async`
   CI matrix entry that runs the daemon integration tests under
   `--features async-daemon` with `use-async-listener = true`. The
   existing matrix continues to test the sync path. Both must stay
   green on Linux, macOS, and Windows. No default flip until two
   consecutive release cycles pass clean on the async matrix.

3. **Run the TPC benchmark plan.** Execute
   `docs/design/daemon-tpc-benchmark-plan.md` (#1933) on dedicated
   hardware once the Section 3 precondition fix (active-counter
   admission gate) lands. Record W100, W1k, W10k results for the
   sync path. Compare against the trigger thresholds in Section 7
   of this document.

4. **Flip the default in a separate PR** if any trigger fires. The
   PR changes the default of `use-async-listener` to true and
   updates the daemon operator guide. The cargo feature stays in
   place so distributions that want to ship without tokio can still
   do so via `--no-default-features`. The sync path is not deleted
   for at least one release after the flip; it remains the fallback
   if a regression is reported.

5. **Retire the sync accept path** only after one full release
   cycle of the async default with no rollback. Even then, the
   `std::thread::spawn` worker bodies stay: only the accept loop
   moves. The hybrid model means the actual transfer code is
   untouched throughout this five-step sequence.

## 9. Out of scope

- Async rewrite of `crates/core`, `crates/engine`, `crates/transfer`,
  `crates/checksums`, `crates/protocol`, or `crates/metadata`. The
  hybrid model exists precisely so that work never has to happen.
- Async SSH transport. Covered by #1411 and
  `docs/design/async-runtime-ssh-eval.md`.
- async-std bridging via `async-compat`. Rejected outright per #1780.
- `current_thread` runtime flavour. Rejected per Section 5.
- io_uring as the accept-loop reactor. Covered by
  `docs/design/iouring-daemon-tcp.md` and is a tokio internal
  implementation choice (`tokio-uring`), not a runtime choice.

## 10. References

- Hybrid model: `docs/design/daemon-async-accept-sync-workers.md`
  (the "what runs where" answer).
- Implementation plan: `docs/design/daemon-tokio-async-listener-impl.md`
  (the "how to build it" answer, #1935).
- TPC benchmark plan: `docs/design/daemon-tpc-benchmark-plan.md`
  (the "when to flip the default" data source, #1933).
- Audit: `docs/audits/daemon-thread-per-connection-scalability.md`
  (the "why we need this at all" data source, #1673).
- Workspace-wide async direction:
  `docs/design/async-migration-plan.md` (#1594).
- SSH runtime parity:
  `docs/design/async-runtime-ssh-eval.md` (#1411) and
  `docs/design/ssh-transport-async-io-eval.md` (#1593).
- Single-runtime rule: #1780 (already resolved).
- Tokio dependency audit: #1779 (already resolved).
- Related trackers: #1367 (this doc), #1590 (companion), #1751,
  #1935, #1933, #1411, #1412.
