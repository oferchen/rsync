# Daemon Async Listener RFC

Tracking: oc-rsync task #1934.

> RFC, design only. No code lands in this PR. Implementation is task #1935.

## 1. Summary

This RFC sketches a feature-gated tokio-based async accept loop for the
oc-rsync daemon. The new path lives behind a `--features async-daemon`
Cargo feature on the `daemon` crate and is off by default. It is an
opt-in alternative to the current synchronous thread-per-connection
accept loop, intended to scale daemon deployments that hold many idle
connections (target: 10k idle, 1k active).

What this RFC proposes:

- A new Cargo feature `async-daemon` on the `daemon` crate that pulls
  tokio in as an optional dependency, mirroring the shape of the
  existing `async` feature
  (`crates/daemon/Cargo.toml:20`,
  `crates/daemon/Cargo.toml:43`).
- A new `crates/daemon/src/async_listener/` module, gated behind
  `#[cfg(feature = "async-daemon")]`, that runs the accept loop and
  the pre-handshake (`@RSYNCD:` greeting, capability advertisement,
  module-select, optional auth) on the tokio runtime.
- A hand-off boundary at module-select: once the connection has chosen
  a module (or chosen `#list`), the accepted `tokio::net::TcpStream` is
  converted to a blocking `std::net::TcpStream` via `into_std()` and
  passed to a worker (`tokio::task::spawn_blocking`) that runs the
  existing synchronous `handle_session` body unchanged.

What this RFC does NOT change:

- The transfer hot path stays synchronous. Sender, receiver,
  generator, `core::session`, and the `engine`, `protocol`,
  `transfer`, `compress`, `checksums`, `signature`, `bandwidth`,
  `filters`, `metadata`, and `rsync_io` crates do not gain any tokio
  dependency or become async. Per the project's tokio-scope policy
  (tasks #1779 and #1818), tokio MUST stay confined to `daemon` (and
  behind `core`'s optional `async` feature, which we reuse without
  changes).
- No wire protocol change. No new capability flag. No new option on
  `oc-rsyncd.conf` beyond an opt-in toggle wired in #1935. No
  observable on-the-wire behaviour difference vs the synchronous
  path.
- The synchronous thread-per-connection accept loop is not removed.
  Both paths coexist behind separate features. Default stays sync.

This RFC complements the prior audit at
`docs/audits/async-daemon-listener.md` (also tracking #1934) by
adding the explicit phasing tied to #1933 (benchmark) and #1935
(implementation), the test plan, and the hand-off-at-module-select
boundary that is narrower than the prior audit's generic
"accept-only" framing. The two documents are consistent; this RFC is
the implementation-oriented sibling.

Last verified: 2026-05-01 against
`crates/daemon/Cargo.toml`,
`crates/daemon/src/lib.rs`,
`crates/daemon/src/daemon/sections/server_runtime/accept_loop.rs`,
`crates/daemon/src/daemon/sections/server_runtime/connection.rs`,
`crates/daemon/src/daemon/sections/server_runtime/listener.rs`,
`crates/daemon/src/daemon/sections/server_runtime/workers.rs`,
`crates/daemon/src/daemon/sections/signals.rs`,
`crates/daemon/src/daemon/async_session/mod.rs`,
`crates/daemon/src/daemon/async_session/listener.rs`,
`crates/daemon/src/daemon/async_session/shutdown.rs`,
`crates/daemon/src/systemd.rs`,
`Cargo.toml`,
and upstream `target/interop/upstream-src/rsync-3.4.1/socket.c`.

## 2. Current daemon architecture

The daemon is one mode of the `oc-rsync` binary. The accept side lives
in `crates/daemon/src/daemon/sections/server_runtime/`:

### 2.1 Listener setup

`serve_connections`
(`crates/daemon/src/daemon/sections/server_runtime/accept_loop.rs:11`)
is the daemon entry point. It registers `SignalFlags` (four
`Arc<AtomicBool>`s at
`crates/daemon/src/daemon/sections/signals.rs:8-21`:
`reload_config`, `shutdown`, `graceful_exit`, `progress_dump`) via
`register_signal_handlers` (`accept_loop.rs:22`,
`signals.rs:52`); computes the bind address list (single, IPv4-only,
IPv6-only, or dual-stack IPv6+IPv4 at
`accept_loop.rs:107-120`); either uses an injected
`pre_bound_listener` (test path, `accept_loop.rs:128-133`) or calls
`bind_with_backlog` per address
(`accept_loop.rs:144-161`,
`crates/daemon/src/daemon/sections/server_runtime/listener.rs:87`,
default backlog 5 at `accept_loop.rs:138` matching upstream
`target/interop/upstream-src/rsync-3.4.1/socket.c:533`); applies
socket options (`accept_loop.rs:178-206`); optionally daemonises
and drops privileges (`accept_loop.rs:212-246`); notifies systemd
via `ServiceNotifier::ready` (`accept_loop.rs:255`,
`crates/daemon/src/systemd.rs:43`); builds `AcceptLoopState`
(`crates/daemon/src/daemon/sections/server_runtime/connection.rs:5-24`)
and dispatches to `run_single_listener_loop` or
`run_dual_stack_loop` based on listener count
(`accept_loop.rs:288-294`).

### 2.2 Connection handling thread spawning

`run_single_listener_loop`
(`crates/daemon/src/daemon/sections/server_runtime/connection.rs:216`)
sets the listener non-blocking (`connection.rs:222`), then on each
iteration calls `check_signals_and_maintain` (`connection.rs:30-97`,
which also reaps finished workers via
`crates/daemon/src/daemon/sections/server_runtime/workers.rs:7`),
calls `listener.accept()`, and on `WouldBlock` sleeps
`SIGNAL_CHECK_INTERVAL` (500 ms,
`crates/daemon/src/daemon/sections/server_runtime/listener.rs:45`).
Accepted streams are set blocking (`connection.rs:232`), socket
options applied (`connection.rs:243`), and
`spawn_connection_worker` is called (`connection.rs:245`).
`max_sessions` is enforced post-accept (`connection.rs:264-270`).

`run_dual_stack_loop` (`connection.rs:281`) spawns one acceptor
thread per listener (`connection.rs:305`); each polls its
non-blocking listener with a 50 ms sleep on `WouldBlock`
(`connection.rs:316`) and forwards accepted streams through an
`mpsc::channel` (`connection.rs:288`). The main loop uses
`rx.recv_timeout(100ms)` (`connection.rs:342`) and the same
`spawn_connection_worker` / `max_sessions` logic
(`connection.rs:346-359`).

`spawn_connection_worker`
(`crates/daemon/src/daemon/sections/server_runtime/connection.rs:106`)
calls `thread::spawn` (`connection.rs:121`) and wraps
`handle_session` in `std::panic::catch_unwind` (`connection.rs:127`)
to isolate panics - the documented thread-equivalent of upstream's
per-connection fork at
`target/interop/upstream-src/rsync-3.4.1/socket.c:599`
(`accept_loop.rs:1-10`). Per-worker shared state
(`Arc<Vec<ModuleRuntime>>`, motd, log sink, socket options,
bandwidth, flags) is cloned out of `AcceptLoopState`
(`connection.rs:112-119`). `reap_finished_workers`
(`workers.rs:7`) joins finished handles between accepts;
`drain_workers` (`workers.rs:23`) joins all on shutdown
(`accept_loop.rs:296`); `join_worker` (`workers.rs:38`) treats
`BrokenPipe` / `ConnectionReset` / `ConnectionAborted` as success.

### 2.3 Where blocking I/O sits

Today, every accepted connection's session body is blocking I/O on
its own OS thread. The session body (called from
`spawn_connection_worker` at `connection.rs:131`) runs the
`@RSYNCD:` greeting, MOTD emission, capability advertisement,
module-select, optional auth challenge/response, then the entire
transfer (file list, generator, sender/receiver). All of this uses
synchronous `std::io::Read`/`Write` on the `TcpStream`.

Idle connections (e.g. a fleet of backup agents holding TCP
connections open for periodic poll-for-listing) park one OS thread
each on a blocking read inside the greeting parser. On glibc, the
default thread stack is 8 MiB committed RSS (less on musl). 10k idle
connections is roughly 80 GiB of address space and tens of GiB of
actual RSS - well past where the synchronous model is the right
choice.

### 2.4 Existing async-feature scaffold

A parallel tokio-based listener already exists behind the existing
`async` feature
(`crates/daemon/Cargo.toml:20`,
`async = ["dep:tokio", "core/async"]`). `AsyncDaemonListener::serve`
(`crates/daemon/src/daemon/async_session/listener.rs:180`) runs
`tokio::select!` over `listener.accept()` and a `broadcast` shutdown
channel (`listener.rs:184-255`); a `tokio::sync::Semaphore`
(`listener.rs:113-114`) caps concurrent connections at
`DEFAULT_MAX_CONNECTIONS = 200` (`listener.rs:25`); each accepted
connection becomes a `tokio::spawn` task
(`listener.rs:216-249`). The module is `#[cfg(test)]`-gated for the
public re-export (`async_session/mod.rs:34-35`) and carries
`#![allow(dead_code)]` (`async_session/mod.rs:28`); it is not on a
production code path today, and it runs the entire session body on
tokio I/O via `crates/daemon/src/daemon/async_session/session.rs`.
This RFC keeps tokio at the accept-and-greeting boundary only and
hands off to `spawn_blocking` for the data path. The two approaches
can coexist (Section 6).

### 2.5 References to the prior audits

Tracker #1675 (`docs/audits/daemon-event-loop-multiplexing.md`)
evaluated `mio` (option a) versus extending the in-tree async
scaffold (option b). Recommendation: option (b). This RFC is the
implementation-oriented narrowing of option (b).

Tracker #1779 / #1818 audited tokio's dependency scope and concluded
tokio must not leak into other crates. This RFC enforces that
boundary at the `spawn_blocking` hand-off: the worker receives a
`std::net::TcpStream`, never a `tokio::net::TcpStream`, and calls
into `handle_session` unchanged. No new tokio import in
`crates/transfer`, `crates/engine`, `crates/protocol`,
`crates/checksums`, `crates/filters`, `crates/compress`,
`crates/bandwidth`, `crates/metadata`, `crates/rsync_io`,
`crates/signature`, `crates/logging`, `crates/logging-sink`,
`crates/branding`, `crates/cli`, `crates/batch`, or
`crates/platform`.

## 3. Goals

In rough priority order:

1. **Concurrent idle connections at scale.** Target: 10k idle TCP
   connections held open against the daemon (typical of fleets of
   backup agents periodically polling for module listings) without
   the daemon's RSS exceeding ~200 MiB (kernel buffers excluded).
   The status quo costs ~80 GiB committed (glibc) for 10k blocked
   threads.
2. **Concurrent active transfers at scale.** Target: 1k concurrent
   active transfers without the daemon's accept loop becoming the
   bottleneck. Active transfers still consume one OS thread each
   (via `spawn_blocking`); the win at this layer is freeing the
   accept thread from poll-and-sleep behaviour
   (`connection.rs:251-253`, `listener.rs:45`).
3. **Per-connection memory minimised.** A tokio task on
   `current_thread` or `multi_thread` runtime is on the order of a
   few KiB (futures, channel handles) versus an OS thread's
   reserved-stack allocation. A connection that has not yet selected
   a module never spawns a worker thread under this design.
4. **Wire-compatible upstream semantics preserved.** The `@RSYNCD:`
   greeting, capability advertisement, module-select, auth
   challenge/response, and the transfer body must produce the
   byte-identical wire output as the synchronous path. Golden tests
   in `crates/protocol/tests/golden/` apply equally to both paths.
5. **No async leakage.** Per the tokio-scope policy enforced in #1779
   and #1818, no crate other than `daemon` (and `core` behind its
   existing optional `async` feature) gains tokio. The accept loop
   converts back to `std::net::TcpStream` before any cross-crate
   call.
6. **Crash isolation parity.** A panic in any per-connection task
   must not tear down the daemon. tokio's `JoinHandle` returns
   `Err(JoinError)` on panic (the existing scaffold documents this at
   `crates/daemon/src/daemon/async_session/listener.rs:211-215`); the
   new path mirrors `catch_unwind` semantics on
   `connection.rs:127`.
7. **Default-off, opt-in.** Operators on the synchronous path see no
   behavioural change. The async path is reachable only via a new
   `--features async-daemon` Cargo feature (Phase 2) and will only
   become the default if Phase 3 benchmarks justify it.

## 4. Non-goals

Explicitly out of scope:

- Migrating the transfer pipeline to async. Sender, receiver,
  generator, `core::session`, and the engine pipeline stay
  synchronous and run on `spawn_blocking` worker threads.
- Replacing rayon for CPU-bound work. Rayon's parallelism (e.g.
  `par_iter()` in receiver, `PARALLEL_STAT_THRESHOLD = 64`) is
  orthogonal. The future improvement of routing rayon-dispatched
  CPU work through `tokio::task::spawn_blocking` is tracked
  separately as #1751 and is out of scope here.
- Introducing a second async runtime. Only tokio. No `async-std`,
  no `smol`, no custom executor.
- Replacing the synchronous accept loop. Both coexist behind
  separate features. The synchronous path remains the parity-tested
  default.
- Changing `oc-rsyncd.conf` parser semantics, configuration schema,
  or any directive's behaviour beyond an opt-in dispatch toggle in
  Phase 2.
- Re-fitting the embedded SSH path (russh client/server) onto the
  new accept loop. The SSH path is client-initiated; the daemon's
  TCP accept loop is the only relevant entry point here.
- Touching `cli` argument parsing. The Cargo feature is the only
  knob; the binary's `daemon_main` chooses the path at compile
  time / via a runtime config directive landed in Phase 2.
- Removing `catch_unwind`-based panic isolation on the synchronous
  path. The async path's `JoinError` handling is additive, not a
  replacement.
- io_uring integration on the daemon listener. The fast_io io_uring
  work for write paths is tracked separately (#1937 covers a session
  ring pool); it is orthogonal to the accept-loop discussion here.

## 5. Proposed design

### 5.1 Module placement

```
crates/daemon/src/async_listener/
    mod.rs           # public surface, feature-gate guard
    runtime.rs       # tokio runtime selection and Builder
    accept.rs        # async accept loop, semaphore, signal wiring
    handoff.rs       # tokio -> std stream conversion, spawn_blocking
    shutdown.rs      # broadcast channel, signal forwarder, drain
```

Every file in `async_listener/` carries a top-level
`#![cfg(feature = "async-daemon")]` (or its module-level equivalent
on `mod.rs`). The module is invisible to the rest of the daemon when
the feature is off. No re-export from `lib.rs` outside the feature
gate, mirroring the conditional re-export at
`crates/daemon/src/daemon/async_session/mod.rs:34-35` (which uses
`#[cfg(test)]`).

The new module sits alongside the existing `async_session/`
scaffold; the two do not import each other. The scaffold is the
"fully async session" track (deferred); `async_listener/` is the
"async accept, sync session" track this RFC proposes. If Phase 3
benchmarks favour `async_listener/`, the scaffold can be retired in
a follow-up.

### 5.2 Cargo feature

Add to `crates/daemon/Cargo.toml`:

```toml
[features]
async-daemon = ["dep:tokio", "core/async"]
```

This sits next to the existing
`async = ["dep:tokio", "core/async"]` at
`crates/daemon/Cargo.toml:20`. Both features pull tokio from the
workspace pin at `Cargo.toml:180`
(`tokio = { version = "1.45", features = ["rt-multi-thread",
"io-util", "net", "fs", "sync", "time", "process", "macros"] }`).
The `daemon` crate's optional dep declaration at
`crates/daemon/Cargo.toml:43` already requests
`["net", "io-util", "sync", "rt", "time"]`; we additionally need
`signal` for `tokio::signal::unix::signal`. The implementation in
#1935 will widen the daemon-level feature list accordingly.

The `async-daemon` feature does NOT imply `async`, and `async` does
not imply `async-daemon`. They are independent toggles.

### 5.3 Runtime selection

The accept loop is I/O-bound and does very little CPU work. The
session body that follows is heavy CPU + heavy blocking I/O, but
runs on `spawn_blocking` threads outside the runtime's executor
threads. Two choices:

**Recommendation: tokio `current_thread` runtime.**

```rust
let runtime = tokio::runtime::Builder::new_current_thread()
    .enable_io()
    .enable_time()
    .build()?;
```

Justification:

- The accept loop is single-threaded by nature: only one task is
  blocked on `listener.accept()` at a time. Multi-threading the
  reactor yields nothing for accept.
- The pre-handshake greeting / module-select phase is a few hundred
  bytes of I/O per connection and a small amount of parsing
  (`crates/daemon/src/daemon/sections/greeting.rs`,
  `crates/daemon/src/daemon/sections/module_access/`). With 10k idle
  connections the steady-state CPU is negligible.
- A single executor thread minimises memory: tokio's
  `multi_thread` runtime defaults to one worker per CPU, each with
  its own stack and parker.
- Per the tokio-scope policy (#1779, #1818), keeping one tokio thread
  is a smaller blast radius than N.
- Blocking work uses `spawn_blocking`, which lives on a separate
  pool independent of the executor mode.

Cons of `current_thread`: a long-running task on the executor
thread (e.g. an accidentally-blocking call) stalls all other tasks.
This is mitigated by the strict rule that no session-body code runs
on the runtime thread; all of it runs on `spawn_blocking`.

If profiling shows the single executor thread is contended (a
realistic risk if the pre-handshake phase grows significantly),
flipping to `multi_thread` is one Builder-method change. Defer that
to #1935 once benchmarks are available.

### 5.4 Accept loop shape

Sketch (sketch-level only; full implementation is #1935):

```rust
// crates/daemon/src/async_listener/accept.rs (new, feature-gated)

#[cfg(feature = "async-daemon")]
pub async fn run_accept_loop(
    listeners: Vec<tokio::net::TcpListener>,
    state: AcceptLoopState<'_>,
    shutdown: ShutdownToken,
) -> Result<(), DaemonError> {
    let sem = Arc::new(tokio::sync::Semaphore::new(
        state.max_sessions.unwrap_or(DEFAULT_ASYNC_MAX_CONN),
    ));
    let mut shutdown_rx = shutdown.subscribe();
    loop {
        tokio::select! {
            accepted = accept_any(&listeners) => {
                let (stream, peer) = accepted?;
                let permit = match sem.clone().try_acquire_owned() {
                    Ok(p) => p,
                    Err(_) => { drop(stream); continue; }
                };
                spawn_connection(stream, peer, &state, permit);
            }
            _ = shutdown_rx.recv() => return Ok(()),
        }
    }
}
```

`accept_any` fans in across listeners. With at most two or three
bound addresses (IPv4 + IPv6 dual-stack at `accept_loop.rs:113-118`)
a fixed-arm `tokio::select!` is sufficient; richer fan-in via
`futures::future::select_all` is overkill. On semaphore rejection
the stream is dropped, mirroring upstream's silent
`lp_max_connections()` rejection (no wire banner).

### 5.5 Hand-off to sync transfer

Each `spawn_connection` task runs the greeting / module-select /
auth phase on tokio I/O, then converts the `tokio::net::TcpStream`
to a blocking `std::net::TcpStream` via `into_std()` plus
`set_nonblocking(false)` (matching the sync path at
`crates/daemon/src/daemon/sections/server_runtime/connection.rs:232`).
The std stream is then handed to `tokio::task::spawn_blocking`,
which calls into the existing synchronous `handle_session` body
unchanged - same `SessionParams` shape as
`connection.rs:134-142`.

`JoinError::is_panic()` mirrors `catch_unwind` semantics
(`connection.rs:127`); panics are logged and the daemon
continues. No async types cross the spawn_blocking boundary.

### 5.6 Backpressure / connection cap

`max_sessions` semantics on the synchronous path are after-the-fact:
`run_single_listener_loop` increments `state.served` and breaks when
`served >= limit` (`connection.rs:264-270`). On the async path we
use the cleaner admission-time semaphore pattern from the existing
scaffold (`async_session/listener.rs:113-114`,
`:189-196`):

- A `tokio::sync::Semaphore` with capacity =
  `max_sessions.unwrap_or(DEFAULT_ASYNC_MAX_CONN)`.
  `DEFAULT_ASYNC_MAX_CONN` should default high (e.g. 4096) so
  high-fanout idle deployments are unblocked; the operator overrides
  via `max_sessions` in the config.
- `try_acquire_owned()` (non-blocking) on accept. If the permit is
  rejected, the stream is dropped immediately; no greeting is sent.
  This matches upstream rsync's behaviour when the daemon is at its
  configured limit
  (`target/interop/upstream-src/rsync-3.4.1/clientserver.c`'s lock
  on `lp_max_connections()` rejects similarly without a wire
  message).
- The permit is held by the session task and released on exit (RAII
  via `_permit`), so a long-running session blocks future admits in
  the same way the sync path blocks new accepts via the
  `state.served` counter.

Wire-error code on rejection: the sync path returns
`SOCKET_IO_EXIT_CODE` (or similar) on a fatal accept failure, but
does NOT emit a textual error to the rejected client today.
Upstream's `lp_max_connections()` enforcement also drops without a
banner. The async path mirrors this: drop the stream, log on the
daemon side via the `log_sink`, no client-visible message.

### 5.7 Cancellation / graceful shutdown

The synchronous accept loop polls `SignalFlags`
(`crates/daemon/src/daemon/sections/signals.rs:8-21`) every
iteration via `check_signals_and_maintain`
(`crates/daemon/src/daemon/sections/server_runtime/connection.rs:30-97`):
SIGTERM/SIGINT (`shutdown`) stops accepting and drains
(`accept_loop.rs:296`); SIGUSR1 (`graceful_exit`) stops accepting
and drains; SIGHUP (`reload_config`) swaps-and-clears, calling
`reload_daemon_config`; SIGUSR2 (`progress_dump`) swaps-and-clears,
logging a summary. The async accept loop must consume the same
signals without a second source of truth:

1. `register_signal_handlers`
   (`crates/daemon/src/daemon/sections/signals.rs:52`) stays
   unchanged. `SignalFlags` remains the canonical store.
2. One tokio task per `SignalKind` registers via
   `tokio::signal::unix::signal` and on each event writes both the
   existing `AtomicBool` and a `tokio::sync::broadcast` channel.
   The broadcast wakes the accept loop instantly, removing the
   500 ms `SIGNAL_CHECK_INTERVAL` wait
   (`crates/daemon/src/daemon/sections/server_runtime/listener.rs:45`).
3. The accept loop's `tokio::select!` branches on
   `shutdown_rx.recv()` (Section 5.4). `JoinHandle::abort()` cannot
   interrupt `spawn_blocking` work, so SIGTERM and SIGUSR1 drain
   semantics match `drain_workers`
   (`crates/daemon/src/daemon/sections/server_runtime/workers.rs:23`)
   exactly - both wait for outstanding sessions.
4. Drain: collect `JoinHandle`s in a `FuturesUnordered` and
   `select!` between drain completion and a configurable deadline.
5. systemd integration is unchanged: `ServiceNotifier::ready` on
   bind, `::status` on each active-connection change, `::stopping`
   at exit (`accept_loop.rs:255`, `:266`, `:303`, `:306`,
   `crates/daemon/src/systemd.rs:43`, `:61`, `:75`). `sd_notify` is
   a single `sendmsg(2)` and safe to call from inside an async
   task without `spawn_blocking`.

### 5.8 Logging

All daemon log output flows through the existing `logging-sink`
crate. Per-connection workers receive `Option<Arc<SharedLogSink>>`
(`crates/daemon/src/daemon/sections/server_runtime/connection.rs:114`,
`:139`); the async path clones the same `Arc` into each
spawned task. No per-connection log silos. No new sink, no new
formatting, no new env-var override.

stderr in the async path: tokio does not redirect stderr; the
daemon's existing `eprintln!` fallback in
`crates/daemon/src/daemon/sections/server_runtime/listener.rs:77`
and `workers.rs:54` continues to work. When `--detach` is active
(`accept_loop.rs:213`), stderr is closed by `become_daemon`; the
async path inherits this behaviour because `become_daemon` runs
before the runtime is built.

## 6. Trade-offs vs thread-per-connection

### 6.1 Memory per connection

| Path                          | Idle cost                   | 10k idle |
|-------------------------------|-----------------------------|----------|
| Sync thread-per-connection    | ~8 MiB stack reservation    | ~80 GiB  |
| Async accept + spawn_blocking | ~few KiB per task; 0 worker | <100 MiB |
| Async accept (active)         | 1 worker thread per active  | 1k x 8 MiB = 8 GiB |

Sync stack: glibc default 8 MiB reserved (only touched pages
committed). The address-space cost is real and the kernel
thread-table entries are not free. Async tasks carry only their
state plus a per-task `Header` (~120 bytes on 64-bit) plus held
`Arc` clones; idle connections never spawn a worker thread.

### 6.2 Context-switch cost

Sync: the kernel scheduler picks among N runnable threads; idle
threads are parked on `read()` and pay no scheduler cost. Async:
the tokio reactor wakes the executor when an fd is ready and runs
ready tasks until they yield. For idle connections, async wins
(zero context switches per connection per second). For active
transfers on `spawn_blocking`, each blocked syscall is one context
switch on a blocking-pool thread, identical to the sync path. Net:
comparable for active load, strictly better for idle load.

### 6.3 Compatibility risk

Wire-format compatibility: zero risk if the hand-off boundary
holds. The bytes on the wire are produced by the same
`handle_session` body. Golden tests in
`crates/protocol/tests/golden/` apply equally to both paths.

Behaviour-under-stress compatibility: tokio's blocking-pool default
of 512 threads (`max_blocking_threads`) caps concurrent active
transfers. If `max_sessions` exceeds 512, the daemon will queue
sessions on the blocking pool, delaying their start. Mitigation:
tune `Builder::max_blocking_threads` from `max_sessions` in #1935
(see Open question 5 in Section 8).

Crash isolation: tokio surfaces a panic in `spawn_blocking` as
`Err(JoinError)` with `is_panic() == true`. The async path logs
and continues, matching the synchronous path's `catch_unwind` at
`crates/daemon/src/daemon/sections/server_runtime/connection.rs:127`
and the `join_worker` fallback at
`crates/daemon/src/daemon/sections/server_runtime/workers.rs:49-56`.

Signal compatibility: SIGUSR1 / SIGUSR2 / SIGHUP semantics are
preserved by the dual-write in Section 5.7 (forward to both
`AtomicBool` and broadcast). Tests that pump signals at the daemon
work on both paths.

### 6.4 Build-matrix risk

A new `async-daemon` Cargo feature adds one combination to the
feature matrix. The CI feature combinations under
`cargo nextest run --workspace --all-features` and the
fmt/clippy/test matrix already cover `--all-features`, which by
construction enables `async-daemon`. The risk is in
selective-feature builds (e.g. `--no-default-features
--features async-daemon`) which are not currently exercised.
Mitigation: #1935 adds a CI matrix row for
`--no-default-features --features async-daemon` if the feature
graph is non-trivial.

## 7. Phasing

### Phase 1 - this RFC (#1934)

Docs only. Establishes:

- The design (Section 5).
- The boundary (`spawn_blocking` hand-off, no async leakage into
  other crates per #1779 and #1818).
- The Cargo feature surface (Section 5.2).
- The test plan (Section 9).

No code change. This RFC ships and the `daemon` crate's compiled
output is byte-identical to the prior commit.

### Phase 2 - implement async listener (#1935)

Implementation behind `--features async-daemon`, default off.
Work, in order: add the Cargo feature; create
`crates/daemon/src/async_listener/{mod,runtime,accept,handoff,shutdown}.rs`;
port the production wiring from `serve_connections`
(`accept_loop.rs:11-319`) - `become_daemon` (`:214`),
`drop_privileges` (`:236`), PID file (`:223`), syslog
(`:71-83`), log sink (`:62`), socket options
(`:178-206`), proxy-protocol pre-read (`:285`),
reverse-DNS, bandwidth limiter; implement the signal forwarder
(Section 5.7); implement the accept loop (Section 5.4) with
semaphore admission and `spawn_blocking` hand-off (Section 5.5);
add pre-bound listener support via
`tokio::net::TcpListener::from_std` mirroring
`accept_loop.rs:128-133`; add an `event_loop = sync | async-daemon`
directive to `oc-rsyncd.conf` defaulting to `sync`; rustdoc + tests
(Section 9). Default off.

### Phase 3 - benchmark (#1933)

Deferred until Phase 2 lands. A `crates/daemon/benches` harness
that:

- Opens N long-lived idle TCP connections to the daemon, holding
  the `@RSYNCD:` greeting.
- Measures: daemon RSS, thread count (`/proc/self/status`),
  accept latency (time from `connect()` to greeting-line
  receipt), SIGTERM-to-drain latency.
- Runs against three configurations: synchronous path, async path
  via `async-daemon`, and upstream rsync 3.4.1 in
  `target/interop/upstream-src/`.
- N values: 100, 1000, 10000.
- Active-transfer follow-up: bench 1000 concurrent active
  transfers (1 MiB / 100 MiB / 1 GiB files) to measure
  blocking-pool saturation.

Numbers decide whether to flip the default in Phase 4.

### Phase 4 - flip the default if benchmarks warrant

If Phase 3 shows the async path wins on idle scaling without
regressing active-transfer throughput, switch the
`event_loop` directive default from `sync` to `async-daemon`.
Otherwise, keep `async-daemon` as a documented opt-in path. The
synchronous path is not removed in either case.

## 8. Open questions

Flagged here, not resolved.

1. **Does tokio `current_thread` actually scale better than
   thread-per-connection at 1k active?** The accept-only win on
   idle connections is uncontroversial. The active-transfer case
   is where `spawn_blocking` and the kernel scheduler converge in
   behaviour. We need empirical numbers from #1933 to know whether
   the runtime overhead is detectable. If the answer is "no
   difference at 1k active", `async-daemon` remains valuable for
   the 10k-idle workload; if the answer is "async is slower at 1k
   active", we keep it strictly opt-in for the idle scenario.
2. **Can `oc-rsyncd.conf` be reused unchanged?** The runtime
   configuration is parsed once at startup
   (`crates/daemon/src/daemon/sections/config_parsing.rs`) and
   rebuilt on SIGHUP via `reload_daemon_config`. The existing
   parser knows nothing about an event-loop choice; the proposal
   in Phase 2 is to add a single `event_loop = sync | async-daemon`
   directive. The open question: does adding this directive break
   any existing operator's config validation, given that upstream
   `rsyncd.conf` does not have it? Recommendation in #1935 is to
   accept-and-ignore unknown directives the way the existing
   parser already handles forward-compatible additions.
3. **Does the embedded SSH path (russh) need an async listener
   too?** Today the SSH path is client-initiated: the client opens
   an SSH connection, runs `oc-rsync --server`, and the daemon-mode
   accept loop is not in the picture. The daemon's TCP listener is
   the only relevant accept path. If a future "rsync-over-SSH
   daemon-style listener" exists, it would need its own design.
   Out of scope for #1934 / #1935.
4. **How do we test cancellation correctness without flakes?**
   The synchronous path's signal-flag polling has a 500 ms
   resolution
   (`crates/daemon/src/daemon/sections/server_runtime/listener.rs:45`),
   so SIGTERM-during-accept tests have an inherent latency
   tolerance. The async path's broadcast wake is sub-millisecond,
   which makes test assertions tighter. The risk: tests that
   measure exact timing become flaky on slow CI runners.
   Mitigation: assert "drain completed within 5 s" rather than
   "within 50 ms"; verify causality (drain happened) rather than
   latency.
5. **Blocking-pool sizing.** Tokio's default
   `max_blocking_threads` is 512. For thousands of concurrent
   active transfers the pool becomes a bottleneck. Should the
   default be tied to `max_sessions`? Recommendation:
   `max_blocking_threads = max(max_sessions.unwrap_or(0) + 32,
   512)` so the pool always has slack above the configured cap;
   final value lands in #1935.
6. **`max-connections` semantics.** Sync path enforces post-accept
   (`connection.rs:264-270`); async scaffold uses an admission-time
   semaphore (`async_session/listener.rs:113`). Reusing the
   semaphore is the proposal; whether to expose a separate
   `--async-max-connections` knob or piggyback on `max_sessions` is
   open.
7. **Dual-stack partial-bind fallback.** The sync path tolerates a
   per-family bind failure when in dual-stack mode
   (`accept_loop.rs:152-160`). The async port must mirror this:
   try IPv6 + IPv4, accept whichever subset binds. Mechanical with
   `tokio::net::TcpListener::bind` per address.
8. **Windows.** Tokio on Windows uses IOCP and accept semantics
   differ from Unix epoll/kqueue. Prior tracker #1682 already
   covers cross-platform validation; follow that thread.

## 9. Test plan

### 9.1 Unit tests

In `crates/daemon/src/async_listener/` (Phase 2 only):
admission-drops-when-semaphore-full, shutdown-broadcasts-to-accept
within 100 ms, `into_std()` yields a blocking stream after
`set_nonblocking(false)`, signal-forwarder writes to both
`AtomicBool` and broadcast, runtime builds and stops cleanly. The
synchronous path's tests
(`crates/daemon/src/daemon/sections/server_runtime/tests.rs`,
`crates/daemon/src/daemon/concurrent_tests.rs`) are unaffected.

### 9.2 Integration tests

Reuse the existing daemon harness in `tools/ci/run_interop.sh` and
`scripts/rsync-interop-server.sh`. Run upstream rsync 3.0.9 / 3.1.3
/ 3.4.1 against `oc-rsync --daemon --features async-daemon` and
compare wire bytes against the goldens referenced in
`docs/audits/tcpdump-daemon-*.md`. Cover handshake, module list,
push, pull, error injection. Add an idle-fanout test (10k TCP
sockets, send `@RSYNCD: 32\n`, do nothing) asserting RSS < 200 MiB
and thread count < 16. Add a graceful-shutdown-under-load test
(100 active transfers + SIGUSR1) asserting all complete within the
drain deadline.

### 9.3 Golden tests

`crates/protocol/tests/golden/` already contains byte-level wire
traces. The async path goes through the same `protocol` crate,
so existing goldens are sufficient. No new golden files needed.

### 9.4 Benchmark hooks

`crates/daemon/benches/daemon_benchmark.rs`
(`crates/daemon/Cargo.toml:71-73`) is the existing criterion
harness. Phase 3 (#1933) extends it with `accept_idle_throughput`
(N idle connections, ops/sec on accept) and `transfer_concurrency`
(1k concurrent active transfers, aggregate throughput).
`scripts/benchmark.sh` and `scripts/benchmark_remote.sh` add a
`--features async-daemon` invocation alongside the default for
direct comparison.

### 9.5 Cross-platform CI

Linux (gnu + musl): both paths. macOS: tokio uses kqueue; verify
`tokio::signal::unix` works for the signal-forwarder. Windows:
tokio uses IOCP and accept semantics differ (Open question 8;
follow #1682); the signal-forwarder uses `tokio::signal::ctrl_c`
rather than `unix::signal`, and the abstraction in
`crates/daemon/src/daemon/sections/signals.rs:8-21` already wraps
the platform difference.

## 10. References

- Sync accept loop:
  `crates/daemon/src/daemon/sections/server_runtime/accept_loop.rs`,
  `crates/daemon/src/daemon/sections/server_runtime/connection.rs`,
  `crates/daemon/src/daemon/sections/server_runtime/listener.rs`,
  `crates/daemon/src/daemon/sections/server_runtime/workers.rs`.
- Existing async scaffold:
  `crates/daemon/src/daemon/async_session/mod.rs`,
  `crates/daemon/src/daemon/async_session/listener.rs`,
  `crates/daemon/src/daemon/async_session/session.rs`,
  `crates/daemon/src/daemon/async_session/shutdown.rs`.
- Signals: `crates/daemon/src/daemon/sections/signals.rs:8`,
  `:52`. systemd: `crates/daemon/src/systemd.rs:16`.
- Daemon Cargo features:
  `crates/daemon/Cargo.toml:16-29`. Workspace tokio pin:
  `Cargo.toml:180`.
- Upstream baseline (fork-per-connection):
  `target/interop/upstream-src/rsync-3.4.1/socket.c:533`
  (`start_accept_loop`),
  `target/interop/upstream-src/rsync-3.4.1/socket.c:599`
  (`fork()`).
- Prior audits:
  `docs/audits/async-daemon-listener.md` (companion to this RFC,
  same tracker #1934),
  `docs/audits/daemon-event-loop-multiplexing.md` (#1675).
- Process-model rationale:
  `docs/DAEMON_PROCESS_MODEL.md`.
- Related trackers: #1933 (benchmark), #1935 (implementation),
  #1937 (io_uring session ring pool), #1751 (rayon via
  `spawn_blocking`), #1674 (broader daemon process-model
  rationale), #1779 + #1818 (tokio scope policy).
- Tokio:
  <https://docs.rs/tokio/1/tokio/runtime/struct.Builder.html>,
  <https://docs.rs/tokio/1/tokio/task/fn.spawn_blocking.html>,
  <https://docs.rs/tokio/1/tokio/signal/unix/index.html>,
  <https://docs.rs/tokio/1/tokio/sync/struct.Semaphore.html>.
