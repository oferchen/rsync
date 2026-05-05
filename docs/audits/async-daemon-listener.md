# Async Daemon Listener RFC

Tracking: oc-rsync task #1934.

> RFC, sketch only. No implementation in this PR.

This RFC proposes a feature-gated tokio-based async accept loop for the
oc-rsync daemon, as an opt-in alternative to the current
thread-per-connection model. It complements - it does not replace - the
existing event-loop multiplexing audit at
`docs/audits/daemon-event-loop-multiplexing.md` (task #1675), which
evaluated `mio` (option a) and extending the in-tree `async`-feature
scaffold (option b). This RFC narrows option (b) to a concrete sketch
gated behind a new `async-daemon` Cargo feature, with a strict boundary:
tokio handles only the accept side; the per-session transfer code stays
synchronous and unchanged, called via `tokio::task::spawn_blocking`.

Last verified: 2026-05-01 against
`crates/daemon/src/daemon/sections/server_runtime/accept_loop.rs`,
`crates/daemon/src/daemon/sections/server_runtime/connection.rs`,
`crates/daemon/src/daemon/sections/server_runtime/listener.rs`,
`crates/daemon/src/daemon/async_session/listener.rs`,
`crates/daemon/src/daemon/async_session/mod.rs`,
`crates/daemon/src/daemon/sections/signals.rs`,
`crates/daemon/src/systemd.rs`, and `crates/daemon/Cargo.toml`.

## Status

Sketch / design only. This RFC ships docs only. Phase 2 (implementation)
is task #1935 and is intentionally out of scope here.

## Current model

The daemon uses one OS thread per accepted TCP connection. The accept
machinery lives in `crates/daemon/src/daemon/sections/server_runtime/`:

- `serve_connections`
  (`crates/daemon/src/daemon/sections/server_runtime/accept_loop.rs:11`)
  is the entry point. It registers signal handlers via
  `register_signal_handlers`
  (`accept_loop.rs:22`,
  `crates/daemon/src/daemon/sections/signals.rs:52`), binds one
  listener per address family (`accept_loop.rs:107-173`), applies
  socket options (`accept_loop.rs:178-206`), optionally daemonises
  and drops privileges (`accept_loop.rs:212-246`), notifies systemd
  via `ServiceNotifier` (`accept_loop.rs:248-257`,
  `crates/daemon/src/systemd.rs:16`), then dispatches to either
  `run_single_listener_loop` or `run_dual_stack_loop`
  (`accept_loop.rs:288-294`).
- `run_single_listener_loop`
  (`crates/daemon/src/daemon/sections/server_runtime/connection.rs:216`)
  sets the listener non-blocking, polls `listener.accept()` with a
  500 ms `thread::sleep(SIGNAL_CHECK_INTERVAL)` on `WouldBlock`
  (`connection.rs:251-253`,
  `crates/daemon/src/daemon/sections/server_runtime/listener.rs:45`),
  and calls `spawn_connection_worker` per accepted stream
  (`connection.rs:245`).
- `run_dual_stack_loop` (`connection.rs:281`) spawns one acceptor
  thread per listener (`connection.rs:305`); accepted streams fan
  through an `mpsc::channel` (`connection.rs:288`) into the same
  `spawn_connection_worker` (`connection.rs:346`).
- `spawn_connection_worker` (`connection.rs:106`) calls
  `thread::spawn(move || ...)` (`connection.rs:121`) and wraps the
  session body in `std::panic::catch_unwind` (`connection.rs:127`)
  so a faulting session never tears down the daemon - the documented
  thread-equivalent of upstream's per-connection fork
  (`accept_loop.rs:1-10`).

Per-connection shared state - `Arc<Vec<ModuleRuntime>>`, MOTD lines,
optional `Arc<SharedLogSink>`, parsed `Arc<Vec<SocketOption>>`,
bandwidth / burst / reverse-lookup / proxy-protocol flags - is owned
by `AcceptLoopState` (`connection.rs:5`) and cloned into each worker
(`connection.rs:112-119`). All mutation lives on the accept thread.

Shutdown propagates through atomic flags on `SignalFlags`
(`crates/daemon/src/daemon/sections/signals.rs:8`):
`shutdown` (SIGTERM/SIGINT), `graceful_exit` (SIGUSR1),
`reload_config` (SIGHUP), `progress_dump` (SIGUSR2). The accept
loop polls these every iteration via `check_signals_and_maintain`
(`connection.rs:30`). Workers are reaped between accepts by
`reap_finished_workers`
(`crates/daemon/src/daemon/sections/server_runtime/workers.rs:7`)
and drained on exit by `drain_workers` (`workers.rs:23`).

The systemd integration is the
`ServiceNotifier::ready/status/stopping` API
(`crates/daemon/src/systemd.rs:43`, `:61`, `:75`), invoked from
`accept_loop.rs:255`, `:266`, `:303`, `:306`. It is a no-op when
`sd-notify` is disabled or `NOTIFY_SOCKET` is unset
(`systemd.rs:28-40`).

### Existing async-feature scaffold

A parallel tokio-based listener already lives in-tree behind the
existing `async` feature
(`crates/daemon/Cargo.toml:20`,
`async = ["dep:tokio", "core/async"]`). `AsyncDaemonListener::serve`
(`crates/daemon/src/daemon/async_session/listener.rs:180`) runs an
accept loop with `tokio::select!` over `listener.accept()` and a
`broadcast` shutdown channel (`listener.rs:184-255`), bounds
concurrency with a `tokio::sync::Semaphore` (`listener.rs:113`,
`DEFAULT_MAX_CONNECTIONS = 200` at `listener.rs:25`), and dispatches
each accepted stream into a fully async session handler via
`tokio::spawn` (`listener.rs:216`). Public re-exports are
`#[cfg(test)]`-gated (`async_session/mod.rs:34-35`) and the module
carries `#![allow(dead_code)]` (`async_session/mod.rs:28`); it is
not on a production path.

This RFC is *not* the same shape as that scaffold. The existing
scaffold runs the session body on the async runtime (`tokio::spawn`
+ `tokio::io` in `crates/daemon/src/daemon/async_session/session.rs:14`);
this RFC proposes tokio at the accept boundary only, with
`spawn_blocking` handing off to the existing synchronous
`handle_session` body unchanged. The two approaches can coexist
behind separate features while we measure.

## Motivation

Tasks #1933 (benchmark thread-per-connection at 100/1k/10k) and
#1935 (implement tokio async listener) flag the same scaling
concern: each idle accepted connection parks an OS thread on a
blocking read. For the typical rsync-daemon deployment (a handful
of long-lived bulk-copy sessions) this is fine and matches upstream
rsync 3.4.1's fork-per-connection model
(`target/interop/upstream-src/rsync-3.4.1/socket.c:599`). The case
this RFC future-proofs is hosts handling many concurrent thin
clients (e.g. fleets of backup agents polling for module listings),
where thousands of idle threads cost significant RSS and scheduler
overhead.

The async path is opt-in. The synchronous path stays the default:
it is parity-tested against upstream 3.4.1, carries no async
runtime in the data path, and `catch_unwind`
(`connection.rs:127`) gives the same crash isolation as
upstream's `fork(2)`. The async path is worth designing now because
the Cargo scaffold and skeleton already exist (no new workspace
dep), benchmarking (#1933) needs a credible alternative to measure
against, and the `spawn_blocking` boundary keeps the blast radius
of tokio strictly inside the daemon crate.

## Proposed Cargo feature

Add a new feature in `crates/daemon/Cargo.toml`:

```toml
async-daemon = ["dep:tokio", "core/async"]
```

This sits alongside the existing `async` feature
(`crates/daemon/Cargo.toml:20`). The two features can coexist; the
existing `async` scaffold becomes the "fully async session" track
(deferred), while `async-daemon` becomes the "async accept,
synchronous session" track. Both pull `tokio` from the workspace
declaration at `Cargo.toml:180`
(`tokio = { version = "1.45", features = ["rt-multi-thread",
"io-util", "net", "fs", "sync", "time", "process", "macros"] }`).

The feature name is open for review. If we decide that the existing
`async` feature should be repurposed for this design (and the fully
async session track shelved), `async-daemon` collapses into `async`
and we delete the unused per-session async code. That decision lives
in #1935, not here.

### tokio dependency-scope policy

Per the project's tokio dependency-scope policy (tasks #1818 and #1779),
tokio MUST NOT leak into other crates. The boundary held by this RFC:

- `crates/daemon/Cargo.toml` adds `tokio` only behind the
  `async-daemon` feature (already optional at `Cargo.toml:43`).
- No other crate gains a tokio dependency. `crates/core` already
  has an `async` feature
  (`async = ["dep:tokio", "core/async"]` shape, see
  `crates/daemon/Cargo.toml:20`); this RFC reuses it without
  changing core's feature surface.
- The async accept loop hands the accepted `std::net::TcpStream`
  (NOT `tokio::net::TcpStream`) to a `spawn_blocking` task that
  calls into the existing synchronous `handle_session` unchanged.
  All `tokio::io` types stay inside the accept-loop module.

The conversion from `tokio::net::TcpStream` to `std::net::TcpStream`
is via `into_std()`, which yields a non-blocking std stream; the
worker re-arms blocking mode before invoking `handle_session`,
matching the current sync path
(`crates/daemon/src/daemon/sections/server_runtime/connection.rs:232`).

## API sketch

The async accept loop exposes a single entry point. This is the
signature only - no implementation - to anchor reviewer feedback:

```rust
// crates/daemon/src/daemon/async_session/run.rs (new, feature-gated)
//
// Boundary: only the accept side runs on the tokio runtime.
// Per-session work runs on tokio's blocking pool via spawn_blocking,
// invoking the existing synchronous handle_session unchanged.

#[cfg(feature = "async-daemon")]
pub async fn run_async_listener(
    config: DaemonConfig,
    shutdown: ShutdownToken,
) -> std::io::Result<()> {
    // Sketch:
    // 1. Bind tokio listeners for each address in config.bind_addresses.
    //    Use tokio::net::TcpListener::bind(addr).
    // 2. Wire ShutdownToken to a tokio broadcast channel and tokio::signal
    //    handlers for SIGTERM/SIGINT/SIGHUP/SIGUSR1/SIGUSR2.
    // 3. Loop on tokio::select! { accept, shutdown_signal }.
    // 4. For each accepted (tokio_stream, peer_addr):
    //      let std_stream = tokio_stream.into_std()?;
    //      std_stream.set_nonblocking(false)?;
    //      let cfg = config.clone();  // or Arc-share
    //      tokio::task::spawn_blocking(move || {
    //          existing_session_handler(std_stream, peer_addr, cfg)
    //      });
    // 5. On shutdown: stop accepting, await outstanding spawn_blocking
    //    handles up to a drain timeout, then return.
    todo!("sketch only - implementation lands in #1935")
}
```

`ShutdownToken` is a thin wrapper around a `tokio::sync::broadcast`
sender plus the existing `SignalFlags`
(`crates/daemon/src/daemon/sections/signals.rs:8`); the wrapper lets
both the sync and async paths share one shutdown source. See
"Shutdown semantics" below.

`existing_session_handler` is `handle_session` as called today from
`spawn_connection_worker`
(`crates/daemon/src/daemon/sections/server_runtime/connection.rs:131`);
it is invoked unchanged. Per-session state (`SessionParams`,
`crates/daemon/src/daemon/sections/server_runtime/connection.rs:134`)
is constructed identically.

This is sketch level. The actual implementation in #1935 will need
to thread through the rest of `serve_connections`: `become_daemon`
(`accept_loop.rs:214`), `drop_privileges` (`accept_loop.rs:236`),
PID file (`accept_loop.rs:223`), syslog (`accept_loop.rs:71-84`),
log sink (`accept_loop.rs:62`), socket options
(`accept_loop.rs:178-206`), and proxy-protocol bits
(`accept_loop.rs:285`). Task #1676 in the prior audit already
tracks the parity matrix; this RFC does not duplicate it.

## Shutdown semantics

The synchronous accept loop polls `SignalFlags` every iteration
(`crates/daemon/src/daemon/sections/server_runtime/connection.rs:30-97`).
The async accept loop must consume the same signals without
introducing a second source of truth.

Approach:

- Keep `register_signal_handlers`
  (`crates/daemon/src/daemon/sections/signals.rs:52`) unchanged. The
  underlying `platform::signal::SignalFlags` is the canonical source.
- In `run_async_listener`, spawn a small tokio task that registers
  `tokio::signal::unix::signal(SignalKind::terminate())` and
  friends, and forwards each event into both the existing
  `AtomicBool` flag and a `tokio::sync::broadcast` channel. The
  broadcast lets the accept loop wake immediately
  (no 500 ms `SIGNAL_CHECK_INTERVAL` poll;
  `crates/daemon/src/daemon/sections/server_runtime/listener.rs:45`).
- `tokio::select!` in the accept loop branches on `accept`,
  `shutdown_rx.recv`, and `reload_rx.recv` (SIGHUP). On SIGTERM /
  SIGINT we stop accepting and drain. On SIGUSR1 we stop accepting
  and let outstanding `spawn_blocking` tasks finish, then return -
  matching `connection.rs:46-64`.
- systemd integration is unchanged: the same `ServiceNotifier`
  (`crates/daemon/src/systemd.rs:16`) is called from the async
  path, with `ready` after bind, `status` updates on each
  active-connection change, and `stopping` at exit
  (`accept_loop.rs:255`, `:266`, `:303`, `:306`). `sd_notify` is
  synchronous (a single `sendmsg` on a Unix socket) and safe to
  call from inside an async task without `spawn_blocking`.

Drain on shutdown: collect `spawn_blocking` `JoinHandle`s in a
`FuturesUnordered` and `select!` between drain completion and a
configurable drain deadline. On deadline, log and return without
forcing the kernel to reap straggling sessions; matches the
existing `drain_workers` semantics
(`crates/daemon/src/daemon/sections/server_runtime/workers.rs:23`).

## Open questions

Flagged, not resolved here.

- **Blocking-pool size.** Tokio's default `max_blocking_threads`
  is 512, adequate for typical deployments. For thousands of
  concurrent active transfers the pool becomes a bottleneck and
  should be tuned via
  `tokio::runtime::Builder::max_blocking_threads`. Policy deferred
  to #1935; one option is to derive it from `max_sessions` with a
  sane minimum (e.g. `max(max_sessions.unwrap_or(0), 64)`).
- **`max-connections` semantics.** The sync path enforces
  `max_sessions` by counting accepted connections in the loop
  (`connection.rs:264-270`). The async scaffold uses a
  `tokio::sync::Semaphore`
  (`crates/daemon/src/daemon/async_session/listener.rs:113`).
  Reusing the semaphore approach is the proposal; whether to
  expose a separate `--async-max-connections` knob or piggyback on
  `max_sessions` is open.
- **Bind directive mapping.** `RuntimeOptions::bind_address` and
  `address_family` (`accept_loop.rs:51`, `accept_loop.rs:107-120`)
  compose to a list of `IpAddr`s. `tokio::net::TcpListener::bind`
  takes the same `SocketAddr` shape as `std::net::TcpListener::bind`,
  so the mapping is mechanical; the open detail is mirroring the
  graceful dual-stack partial-bind fallback at
  `accept_loop.rs:152-160`.
- **Pre-bound listener injection.** The sync path accepts
  `pre_bound_listener: Option<TcpListener>`
  (`accept_loop.rs:14`, `accept_loop.rs:128-133`) to avoid bind /
  port-allocation TOCTOU in tests. The async path needs the same
  hook; `tokio::net::TcpListener::from_std` makes it a one-liner.
- **Proxy-protocol.** `proxy_protocol` (`accept_loop.rs:285`)
  parsing lives in the sync session handler. With `spawn_blocking`
  keeping that body unchanged this is free; a fully-async session
  would need a port. Out of scope here.
- **Windows.** Tokio on Windows uses IOCP and accept semantics
  differ from Unix epoll/kqueue. Prior audit task #1682 tracks
  cross-platform validation.

## Non-goals

Explicitly out of scope for this RFC and the Phase 2
implementation:

- Migrating the transfer pipeline (sender, receiver, generator,
  `core::session`) to async. The pipeline stays synchronous.
- Introducing tokio I/O on the data path. The accepted stream is
  converted to `std::net::TcpStream` before any session code runs.
- Changing wire format, protocol version, capability strings, or
  any observable on-the-wire behaviour. This is a daemon-internal
  scheduling change.
- Touching other crates' dependency graphs. `tokio` stays daemon-
  only and feature-gated. Per the project's tokio-scope policy, no
  new tokio leakage into `cli`, `core`, `transfer`, `protocol`,
  `engine`, or anywhere else.
- Replacing `catch_unwind`-based panic isolation. Tokio's
  `JoinHandle` returns `Err(JoinError)` on panic
  (`crates/daemon/src/daemon/async_session/listener.rs:211-215`);
  the implementation will log and continue, matching the existing
  semantics.
- Removing the synchronous accept loop. Both paths coexist; the
  default stays synchronous.

## Phasing

- **Phase 1 - this RFC (#1934).** Docs only. Establishes the
  design, the feature-gate boundary, and the open questions.
- **Phase 2 - feature-gated tokio listener (#1935).** Implements
  `run_async_listener` per the API sketch above and adds the
  `async-daemon` Cargo feature. Wires the parity matrix from
  prior audit task #1676 (signal handling, syslog, PID file,
  `become_daemon`, `drop_privileges`, systemd notifier, socket
  options, dual-stack bind, pre-bound listener, proxy-protocol).
  Per-session work delegates to `existing_session_handler` via
  `spawn_blocking`. Default-off.
- **Phase 3 - benchmark vs thread-per-connection (#1933).**
  Deferred until Phase 2. Measure RSS, thread count, accept
  latency, and SIGTERM-to-drain latency at 100 / 1k / 10k
  concurrent idle connections; compare sync path, `async-daemon`
  path, and upstream rsync 3.4.1. Numbers decide whether to
  promote `async-daemon` to a documented production path.
- **Phase 4 - tokio I/O on connection setup/teardown only
  (deferred).** Optionally extend tokio to the pre-handshake
  greeting / module-list / auth phase
  (`crates/daemon/src/daemon/sections/greeting.rs`,
  `crates/daemon/src/daemon/sections/module_access/`), keeping the
  bulk-transfer phase synchronous. Future option, not a commitment.

## Cross-references

- `docs/audits/daemon-event-loop-multiplexing.md` (#1675)
  evaluated `mio` (option a) and a fully-async-session track
  (option b). This RFC narrows option (b) into a concrete,
  feature-gated sketch with a `spawn_blocking` boundary.
- #1933: benchmark thread-per-connection at 100 / 1k / 10k.
  Phase 3.
- #1935: implement the listener proposed here. Phase 2.
- #1674: broader daemon process-model rationale; see
  `docs/DAEMON_PROCESS_MODEL.md`.
- #1367: related daemon-scaling input from the prior audit;
  informs the motivation.
- Tokio dependency-scope policy pair (#1818, #1779): policy enforced
  by the "tokio dependency-scope policy" section.

## References

- Sync accept loop:
  `crates/daemon/src/daemon/sections/server_runtime/{accept_loop,connection,listener,workers}.rs`.
- Async scaffold:
  `crates/daemon/src/daemon/async_session/{listener,session,shutdown,mod}.rs`.
- Signals: `crates/daemon/src/daemon/sections/signals.rs:8`,
  `:52`. systemd: `crates/daemon/src/systemd.rs:16`.
- Daemon Cargo features: `crates/daemon/Cargo.toml:16-28`.
  Workspace tokio pin: `Cargo.toml:180`.
- Upstream baseline (fork-per-connection):
  `target/interop/upstream-src/rsync-3.4.1/clientserver.c:1496`,
  `target/interop/upstream-src/rsync-3.4.1/socket.c:533`,
  `target/interop/upstream-src/rsync-3.4.1/socket.c:599`.
- Prior audit: `docs/audits/daemon-event-loop-multiplexing.md`.
- Tokio:
  <https://docs.rs/tokio/1/tokio/task/fn.spawn_blocking.html>,
  <https://docs.rs/tokio/1/tokio/runtime/struct.Builder.html#method.max_blocking_threads>.
