# Daemon Tokio Async Listener - Implementation Plan

Status: Implemented (accept path wired; opt-in, default-off)
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

## 6a. Wired implementation

The accept path is now wired to a real per-connection worker (previously a
skeleton that dropped each accepted socket):

- `crates/daemon/src/daemon/sections/server_runtime/connection_context.rs`
  introduces `ConnectionContext`, a cheaply-clonable bundle of the daemon-wide
  per-connection state (module table, MOTD, log sink, client socket options,
  bandwidth limits, reverse-lookup / PROXY toggles). Its `serve_session` core
  runs the full legacy `@RSYNCD:` session under `catch_unwind` and is shared by
  both accept engines. The synchronous `spawn_connection_worker` builds the same
  context and calls `serve_session`, so the extract is behaviour-preserving; the
  async worker calls `serve_one_connection`, which additionally applies the
  accepted-stream socket tuning and client socket options before delegating to
  `serve_session`. Only accept + task dispatch differ between the two paths; the
  wire behaviour is byte-identical.

- `run_async_daemon` builds one shared `ConnectionContext` and passes a
  `SyncWorker` closure to `run_hybrid_listener`.

### Runtime selection

The shipping binary still runs the synchronous accept loop by default. The TCP
(non-stdio) daemon dispatch selects the async path only when **both** hold:

- the binary is built with `--features async-daemon` (forwarded from
  `crates/cli` via its own `async-daemon` feature), and
- the `OC_RSYNC_ASYNC_DAEMON` environment variable is set at runtime.

When the variable is unset or the feature is off, the sync path is unchanged.
This gate lives in `crates/daemon/src/cli.rs` and only affects the TCP daemon;
the stdio (inetd / remote-shell) path is untouched. It exists to enable the
async-vs-sync daemon concurrency benchmark (ASY-4).

### Admission control

`run_async_daemon` enforces the daemon-level `max connections` cap with a
`tokio::sync::Semaphore` sized to the configured limit, in addition to any
per-module `ConnectionLimiter` the session handler already applies. On
exhaustion it writes the same `@ERROR: max connections (N) reached -- try again
later` refusal the sync accept loop emits, then drops the connection.

### Limitation: privileged modules unsupported

Privilege drop, chroot, and setuid/setgid are **not** plumbed through the async
accept path. To avoid a silent security regression, `run_async_daemon` fails
closed: if any module sets `uid`, `gid`, or `use chroot = true` (the upstream
default for `use chroot` is `true`), or a global daemon `uid`/`gid`/`chroot` is
configured, it returns a `DaemonError` with the message
`async-daemon does not support privileged (uid/gid/chroot) modules; use the
sync daemon`. Non-privileged modules (`use chroot = false`, no `uid`/`gid`) -
the benchmark case - run normally. Lifting this limitation requires threading
the chroot / setuid / setgid sequence through the async dispatch and is tracked
as a follow-up under #1751.

## 7. References

- #1935 - this implementation issue.
- #1934 - accepted RFC for Tokio-based listener.
- #1751 - parent async migration roadmap.
- `docs/design/daemon-async-accept-sync-workers.md` - hybrid model rationale.
- `docs/design/async-migration-plan.md` - long-term async direction.
