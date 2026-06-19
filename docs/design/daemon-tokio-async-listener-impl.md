# Daemon Tokio Async Listener - Implementation Plan

Status: Design (TODO #1935)
Audience: daemon maintainers, transfer-pipeline maintainers, release engineering
Scope: concrete implementation steps for replacing the synchronous accept loop in
`crates/daemon` with a Tokio-driven listener that bridges to the existing sync
transfer worker via `spawn_blocking`.

> Feature flag: the implementation described here lands behind the existing
> `async-daemon` Cargo feature on `crates/daemon` (see `crates/daemon/Cargo.toml`).
> The long-form design rationale lives in
> `docs/design/daemon-async-accept-sync-workers.md`.

## 1. Background

- Issue #1934 captured the RFC for a Tokio-based async accept loop. The RFC has
  been accepted: the daemon will host an asynchronous listener on top of
  `tokio::net::TcpListener` while keeping the sync transfer state machine
  intact behind `tokio::task::spawn_blocking`.
- Issue #1751 tracks the broader async migration roadmap. This implementation
  is the first concrete deliverable on that roadmap and intentionally limits
  itself to the accept-and-dispatch boundary.
- The hybrid model is documented in `daemon-async-accept-sync-workers.md`. This
  document is the implementation companion: it does not relitigate design
  trade-offs.

## 2. Goals and Non-Goals

Goals:

- Replace the per-connection `std::thread::spawn` accept path with a Tokio
  runtime that drives `TcpListener::accept` and spawns one task per
  connection.
- Preserve the existing synchronous transfer worker; the worker keeps owning
  the wire protocol, filters, signature, and engine pipelines unchanged.
- Gate the new code path behind a `async-daemon` Cargo feature so the legacy
  loop remains the default until the feature graduates.
- Cite #1935 in the changelog and PR for traceability with #1751.

Non-goals:

- No change to the wire protocol, the auth layer, or `oc-rsyncd.conf` parsing.
- No async rewrite of the transfer engine, signature, filters, or metadata
  crates. Those stay sync and run on the blocking pool.
- No change to the SSH or `rsync://` client paths.

## 3. Implementation Plan

1. Feature gate.
   - Add `async-daemon = ["dep:tokio"]` to `crates/daemon/Cargo.toml`.
   - Add `tokio = { version = "1", features = ["rt-multi-thread", "net", "macros", "signal"], optional = true }`.
   - Mirror the gate in the workspace `Cargo.toml` so CI can build both paths.
2. Runtime construction.
   - New module `crates/daemon/src/daemon/async_session/runtime.rs`.
   - Build a `tokio::runtime::Builder::new_multi_thread()` runtime with a
     bounded worker count (default `available_parallelism()`, capped at 8).
   - The runtime is owned by the daemon process for its lifetime; shutdown is
     driven by the existing `Ctrl-C` and SIGTERM handlers via `tokio::signal`.
3. Async accept loop.
   - New module `crates/daemon/src/daemon/async_session/listener.rs`.
   - Bind via `tokio::net::TcpListener::bind` using the same address resolution
     helper used today.
   - Loop on `listener.accept().await`; on each accepted `(TcpStream, SocketAddr)`,
     spawn a per-connection task with `tokio::spawn`.
   - Apply the existing connection-limit semaphore before spawning so the
     async path observes the same `max-connections` semantics.
4. Bridge to the sync transfer worker.
   - The per-connection task converts the `tokio::net::TcpStream` into a
     `std::net::TcpStream` via `into_std()` and sets it back to blocking mode.
   - The task then calls `tokio::task::spawn_blocking(move || run_sync_worker(stream, ...))`
     and awaits the join handle.
   - `run_sync_worker` is the existing entry point factored out of
     `connection.rs`; no behavioural change inside the worker.
   - Panics in the blocking task are caught by `JoinHandle`; the listener
     logs and continues, matching the current `catch_unwind` behaviour.
5. Wiring and selection.
   - In `daemon/sections/server_runtime/connection.rs`, branch on the
     `async-daemon` feature: when enabled, dispatch to the async listener;
     when disabled, retain the current `std::thread::spawn` loop verbatim.
   - No public API change; the daemon binary picks the path at compile time.
6. Tests.
   - Unit test in `listener.rs` asserts that accepting a TCP connection on a
     loopback port enqueues a blocking task and that the task runs to
     completion under a `current_thread` runtime.
   - Integration test in `crates/daemon/tests/` boots the async daemon on an
     ephemeral port, runs three concurrent module listings, and asserts all
     three complete successfully.
   - Both tests are gated by `#[cfg(feature = "async-daemon")]`.
7. Observability.
   - Reuse the existing `tracing` spans; the async listener emits an
     `accept` span per connection and the blocking task emits the existing
     `worker` span. No new metrics in this slice.

## 4. Compatibility

- Feature off (default): zero behavioural change. CI keeps testing the legacy
  loop on every platform matrix.
- Feature on: same wire behaviour, same `oc-rsyncd.conf`, same auth, same
  exit codes. Only the accept-and-spawn primitive changes.
- Cross-platform: Tokio supports Linux, macOS, and Windows. Signal handling
  differs per OS but is encapsulated in `tokio::signal`.

## 5. Rollout

- Land behind `async-daemon` feature, off by default.
- Add a CI job `daemon-async` that builds and runs the gated tests.
- After two release cycles of green CI plus interop runs, flip the default
  in a separate PR tracked under #1751.

## 6. Risks

- Blocking-pool starvation if `spawn_blocking` queue saturates. Mitigation:
  the connection-limit semaphore caps in-flight tasks; the blocking pool
  size is set to `max_connections + small slack`.
- Stream conversion cost via `into_std()` plus `set_nonblocking(false)`.
  Mitigation: measured once per connection, negligible against handshake cost.
- Tokio dependency surface. Mitigation: feature-gated; default builds remain
  Tokio-free.

## 7. References

- #1935 - this implementation issue.
- #1934 - accepted RFC for Tokio-based listener.
- #1751 - parent async migration roadmap.
- `docs/design/daemon-async-accept-sync-workers.md` - hybrid model rationale.
- `docs/design/async-migration-plan.md` - long-term async direction.
