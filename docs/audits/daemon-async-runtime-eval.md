# Daemon async runtime evaluation

Tracking task: #1367. Companion RFC: #1934.

This document evaluates whether the rsync daemon should adopt a tokio-based async
listener for concurrent connection handling, replacing or supplementing the
current thread-per-connection model. The evaluation focuses on resource cost,
migration risk, and decision criteria that hold across the supported operating
matrix (Linux, macOS, Windows).

## 1. Current model: thread-per-connection

Source: `crates/daemon/src/daemon/sections/server_runtime/`.

Entry point `serve_connections` (`accept_loop.rs`) binds one or two
`std::net::TcpListener` sockets, optionally daemonises, drops privileges, then
hands control to either `run_single_listener_loop` or `run_dual_stack_loop`
depending on whether dual-stack IPv4 + IPv6 is requested. Both loops use
non-blocking accept with periodic signal-flag polling so SIGHUP, SIGTERM,
SIGUSR1, and SIGUSR2 are honored without leaking accept blocking.

For each accepted socket, `spawn_connection_worker` (`connection.rs`) calls
`std::thread::spawn`. Inside the thread, `std::panic::catch_unwind` isolates
panics so a bad session cannot tear down the daemon. The pattern mirrors
upstream rsync's fork-per-connection model in `clientserver.c`, with
`catch_unwind` standing in for the address-space isolation that fork provides.

Other characteristics of the current model:

- Worker handles accumulate in `AcceptLoopState::workers`. `reap_finished_workers`
  walks the vector each tick, joining finished threads. `drain_workers` blocks
  shutdown until every session ends.
- `ConnectionLimiter` (file-locked counter, `module_state.rs`) enforces the
  `max connections` directive per module across cooperating daemons.
- `BandwidthLimiter` is per-session, sleeping the worker thread on its own clock.
- All transfer I/O is fully blocking: `std::net::TcpStream`, `std::fs::File`,
  buffered reads, and bandwidth-driven `thread::sleep`. The engine, protocol,
  and core crates are all sync.
- `concurrent-sessions` feature already adds a `SessionRegistry` (DashMap) plus a
  `ConnectionPool` for shared bookkeeping; both are sync-friendly and reused by
  the async sketch below.

The model is simple, predictable, and easy to debug. Each connection has a
private stack and a stable thread id, which is helpful for `perf`, `pstack`,
and Windows ETW tooling. The downsides surface only at scale: every connection
carries an OS thread (default 8 MiB virtual / 80-128 KiB committed on Linux,
1 MiB committed on Windows by default), accept latency rises with the worker
sweep cost, and shutdown drains serially.

## 2. Proposed tokio listener (RFC #1934)

Sketch already lives at `crates/daemon/src/daemon/async_session/` behind the
`async` feature. Key types:

- `ListenerConfig` - builder with bind address, max connections, connect/read
  timeouts, TCP keepalive.
- `AsyncDaemonListener::bind` - constructs a `tokio::net::TcpListener`, a
  `Semaphore` sized to `max_connections`, and a `broadcast::Sender<()>` for
  graceful shutdown.
- `serve` - `tokio::select!` between `listener.accept()` and the shutdown
  receiver; each accept spawns a task that holds an owned permit until the
  session ends.
- `AsyncSession` and `handle_async_session` - per-connection async handler with
  `AsyncRateLimiter` (token-bucket) replacing `BandwidthLimiter::sleep`.

Benefits scale with concurrency:

| Concurrent sessions | Thread model footprint                | Tokio model footprint                  |
|---------------------|----------------------------------------|----------------------------------------|
| 100                 | ~10-12 MiB committed thread stacks     | ~0.5 MiB task state + worker threads   |
| 1 000               | ~100-128 MiB stacks, scheduler busy    | ~5 MiB task state, accept under 1 ms   |
| 10 000              | exceeds default `nproc`/ulimit on most distros | feasible on a 4-8 worker runtime |

Latency effects beyond memory:

- Accept-to-handler latency: thread spawn is ~30-80 us on Linux, ~150-400 us on
  Windows. `tokio::spawn` is ~1-3 us once the runtime is hot.
- Graceful shutdown: broadcast wakes every task at once; the synchronous loop
  waits on `JoinHandle::join` per worker, so a single slow session blocks the
  whole drain.
- Idle connections (e.g., daemons fronted by long-lived `--keep-alive` clients
  or proxy_protocol probes) cost a thread today; in the async model they cost a
  parked future plus a TCP fd.

Workloads that today fit comfortably in the thread model (a handful of large
transfers from trusted hosts) gain little. Workloads that lean on many small,
bursty connections - mirror operators, container build farms, fan-in backup -
gain materially.

## 3. Resource cost of tokio adoption

Runtime overhead.

- A multi-thread runtime with `worker_threads = num_cpus` adds N worker threads
  plus the timer driver and the I/O driver thread. On a quad-core daemon host,
  baseline RSS goes up by roughly 4-6 MiB versus the current zero-runtime cost.
- The `current_thread` flavor avoids worker-thread overhead but loses
  parallelism for crypto and compression. The daemon already pins those to
  blocking work, so `current_thread` is viable when the host has only one
  exposed core.
- Tokio binary size: enabling `net`, `io-util`, `sync`, `rt`, `time` adds about
  450 KiB to the daemon binary in release mode.

Task scheduling.

- Per-task overhead is roughly 64-128 bytes of state plus the future size.
  Compared with an 80 KiB committed thread stack, this is the dominant win at
  high connection counts.
- Cooperative scheduling means a single CPU-heavy session can starve peers if
  it never awaits. The transfer hot loop calls `read`/`write` frequently, so
  natural yield points exist, but compression and checksum kernels need
  explicit `tokio::task::yield_now` or relocation to `spawn_blocking`.

Blocking-task cost for transfer workers.

- The engine, protocol, checksums, and compress crates are sync and will stay
  sync; there is no plan to async-ify the file-system pipeline. The async
  listener therefore must dispatch real transfer work via
  `tokio::task::spawn_blocking`.
- `spawn_blocking` uses a separate, dynamically-sized pool (default cap 512).
  Each blocking task still consumes an OS thread for its duration. At 10k
  concurrent transfers we are back to thread-per-session for the actual I/O,
  with the async layer adding overhead, not removing it.
- The win, then, is concentrated in connections that are *not* actively
  transferring: handshake, auth retry loops, idle keepalive, and rate-limited
  bursts. For long-running bulk transfers the async model is roughly cost-
  neutral and may be slightly slower due to channel hops and cross-thread
  wakeups.
- Mitigation: cap concurrent transfers via the existing `Semaphore`, queue
  excess connections at the protocol-error stage, and let `spawn_blocking`
  handle the bulk path. Document the cap clearly so operators can size it.

## 4. Migration plan

Feature flag.

- The existing `async` cargo feature in `crates/daemon/Cargo.toml` already
  gates the tokio dependency and the `async_session` module. We rename the
  user-facing flag to `async-daemon` for clarity and keep `async` as an alias
  during one release cycle. Default remains off.
- A runtime CLI/config switch (`--async-listener` plus `use async = yes` in
  `oc-rsyncd.conf`) selects the async path at startup. Without the build
  feature the switch returns a clear error explaining the binary was compiled
  without async support.

Hybrid sync-worker model.

- Listener and accept loop run on tokio. Per-connection handshake, auth, and
  protocol negotiation also run on tokio so we get cheap timeouts and shutdown.
- Transfer execution moves to `spawn_blocking`, reusing the existing
  `core::session()` entry point unchanged. The blocking handle owns a sync
  `TcpStream` extracted from the tokio socket via `TcpStream::into_std`.
- Bandwidth limiting: the async path uses `AsyncRateLimiter` for the handshake
  phase, then hands the existing `BandwidthLimiter` to the blocking transfer.
  A small adapter unifies the metrics so logs and statistics remain identical.
- Session registry, connection pool, and the per-module `ConnectionLimiter`
  stay shared between the two paths. `concurrent-sessions` becomes a transitive
  dependency of `async-daemon`.

Compatibility.

- Wire protocol: unchanged. Async only affects the listener and process model;
  upstream rsync clients see identical greetings, multiplex frames, and
  daemon-mode responses.
- Signals: the sync loop reads `signal_flags` each iteration. The async loop
  uses `tokio::signal::unix::signal` on Unix and `tokio::signal::windows::ctrl_c`
  on Windows, then forwards into the same `SignalFlags` so the rest of the
  daemon code is unchanged.
- Systemd readiness, PID file, privilege drop, and pre-bound listener injection
  all happen before the async runtime starts, matching the current order so
  test infrastructure that relies on injected listeners keeps working.
- Tests: `concurrent_tests.rs` already exercises both paths through the shared
  registry. We extend the matrix so each parallel-session scenario runs once
  per process model.
- Rollback: the sync path stays in the codebase and is the default. Operators
  who hit issues set the runtime switch back to sync without rebuilding.

## 5. Decision criteria

Adopt the async listener when all of the following hold:

1. Benchmark on a representative deployment shows accept-to-handshake p99
   latency improving by at least 2x at 1k concurrent connections, or memory
   footprint dropping below 30 percent of the sync model at the same load.
2. Bulk-transfer throughput on a single connection regresses by no more than 2
   percent versus sync (measured against upstream interop and our nightly
   throughput suite).
3. Shutdown drain time at 1k active sessions stays under 5 seconds with
   `--graceful-exit`, matching or beating the sync model.
4. CI gains a tokio job that exercises the async listener under the existing
   protocol interop matrix (3.0.9, 3.1.3, 3.4.1) without flake.
5. A documented operator playbook covers tuning `worker_threads`, the
   blocking-pool cap, and the connection semaphore.

Defer when any of the following hold:

- Typical deployments stay below 100 concurrent connections; the win is too
  small to justify the second code path.
- The blocking-pool fan-out for `spawn_blocking` regresses long-running
  transfer throughput more than the 2 percent budget.
- Windows accept-task latency or ctrl-c handling lags the sync path; the
  IOCP-backed tokio reactor must reach parity before the flag flips on by
  default.

Reject outright if a future profile shows that protocol changes (varint
framing, capability strings) become harder to validate because the async path
diverges in subtle ordering. Wire compatibility with upstream is non-negotiable.

## Appendix: file map

- Sync listener: `crates/daemon/src/daemon/sections/server_runtime/`
  - `accept_loop.rs`, `connection.rs`, `workers.rs`, `listener.rs`,
    `socket_options.rs`, `pid_file.rs`, `connection_counter.rs`, `reload.rs`.
- Async sketch: `crates/daemon/src/daemon/async_session/`
  - `mod.rs`, `listener.rs`, `session.rs`, `shutdown.rs`.
- Shared concurrency primitives: `session_registry.rs`, `connection_pool/`.
- Cargo features: `crates/daemon/Cargo.toml` (`async`, `concurrent-sessions`).

Next steps tracked in #1367 cover benchmark harness, signal-handling parity,
and the operator playbook.
