# DASYNC.1 - Daemon accept-loop and per-conn worker spawn inventory

Status: audit, no implementation.
Master snapshot: `e225a6d26` (current `master`).
Scope: enumerate every site where the rsync daemon accepts a TCP connection
and dispatches a per-session worker, catalogue the synchronous primitives
those sites depend on, and identify safe insertion points for a future
tokio-based replacement (DASYNC.2 design input).

The established threading model and the 10K-connection ceiling are taken as
prior context; this audit only inventories the surfaces that a runtime
migration must touch and the invariants those surfaces hold.

## 1. Accept-loop sites

The daemon has four distinct accept surfaces. Three are production paths,
one is an opt-in skeleton:

| # | Path | Listener | Entry | Status |
|---|---|---|---|---|
| 1 | Standalone TCP, single listener | `std::net::TcpListener` | `crates/daemon/src/daemon/sections/server_runtime/connection.rs:348` (`run_single_listener_loop`) | production default |
| 2 | Standalone TCP, dual-stack (v4 + v6) | `std::net::TcpListener` x 2 | `crates/daemon/src/daemon/sections/server_runtime/connection.rs:436` (`run_dual_stack_loop`) | production default when both families bind |
| 3 | Inetd / systemd socket-activation / `RSYNC_CONNECT_PROG` | inherited `stdin` (no accept) | `crates/daemon/src/daemon/sections/inetd.rs:49` (`serve_inetd_session`) | production, selected by `is_stdin_socket()` |
| 4 | Hybrid tokio accept + sync worker via `spawn_blocking` | `tokio::net::TcpListener` | `crates/daemon/src/async_listener.rs:95` (`accept_loop`); driver `crates/daemon/src/daemon.rs:441` (`run_async_daemon`) | skeleton, gated on `async-daemon` cargo feature; not wired by default |

Dispatch into one of these paths happens in `run_daemon`
(`crates/daemon/src/daemon.rs:233`). Order: `is_stdin_socket()` -> inetd
session; otherwise -> `serve_connections` (`accept_loop.rs:11`) which picks
single vs dual-stack based on `listeners.len()`
(`accept_loop.rs:342`-`348`). The async path is reached only through the
separately exposed `run_async_daemon` entrypoint
(`crates/daemon/src/lib.rs:226`), behind the `async-daemon` feature.

A fifth, internal-only async path lives behind the `async` feature at
`crates/daemon/src/daemon/async_session/listener.rs:180` (`AsyncDaemonListener::serve`).
It is `#[allow(dead_code)]`, never invoked from the production dispatcher,
and exists only for forward-compat tests. It is in scope for DASYNC.2
design because it is the second tokio-shaped skeleton already in the tree.

## 2. Per-conn worker spawn pattern

### 2.1 Sync path (paths 1 + 2)

The worker is always spawned through `spawn_connection_worker`
(`crates/daemon/src/daemon/sections/server_runtime/connection.rs:189`).
The single-listener loop calls it directly at `connection.rs:372`; the
dual-stack loop receives the accepted `(TcpStream, SocketAddr)` on an
`mpsc::channel` and calls the same function at `connection.rs:492`.

Spawn primitive (`connection.rs:204`):

```rust
thread::spawn(move || {
    let _conn_guard = conn_guard;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        handle_session(stream, peer_addr, SessionParams { /* ... */ })
    }));
    // panic + error mapping ...
})
```

Properties:

- One OS thread per accepted connection. No bounded pool, no reuse.
- `JoinHandle<WorkerResult>` is stashed in `AcceptLoopState::workers: Vec<...>`
  (`connection.rs:7`). Handles are reaped by `reap_finished_workers`
  (`workers.rs:7`) on every accept-loop iteration via
  `check_signals_and_maintain` (`connection.rs:44`). Reaping is O(n) over
  the live worker vec.
- Final drain happens through `drain_workers` (`workers.rs:23`) after the
  loop exits.
- A `ConnectionGuard` (`connection_counter.rs:69`) is moved into the
  worker closure. Drop on the worker thread decrements the global
  `AtomicUsize` consulted by `--max-connections`
  (`connection.rs:124` `refuse_if_at_capacity`).
- `catch_unwind` isolates panics so a crashing session does not bring down
  the daemon, replacing the upstream `fork()` crash domain.

### 2.2 Dual-stack acceptor threads

`run_dual_stack_loop` spawns one extra plain `thread::spawn` per listener
(`connection.rs:432`) whose only job is to call
`listener.set_nonblocking(true)` + `listener.accept()` in a loop and
forward `(TcpStream, SocketAddr)` results onto the shared
`std::sync::mpsc::channel` (`connection.rs:415`). These are *not* session
workers; they are accept fan-in threads. They terminate by polling the
shared `signal_flags.shutdown` / `graceful_exit` atomics.

### 2.3 Inetd path

`serve_inetd_session` (`inetd.rs:49`) does no spawn. It runs the single
session on the caller's thread via
`handle_legacy_session(DaemonStream::stdio(...))` and returns. The only
isolation primitive is the absence of a loop (the process is one-shot).

### 2.4 Hybrid async skeleton

`run_hybrid_listener` (`async_listener.rs:73`) builds a
`tokio::runtime::Builder::new_multi_thread()` with capped worker count and
runs `accept_loop` (`async_listener.rs:90`). Per accept it:

1. Polls `shutdown: Arc<AtomicBool>` between accepts.
2. `tokio::time::timeout(250ms, listener.accept())` so shutdown drains
   without blocking on a quiet listener.
3. Converts `tokio::net::TcpStream` -> `std::net::TcpStream` via
   `into_std()` + `set_nonblocking(false)` (`async_listener.rs:121`-`131`).
4. Dispatches the sync worker through
   `tokio::task::spawn_blocking(move || worker(std_stream, peer_addr))`
   (`async_listener.rs:133`).

Today the production wire-up (`run_async_daemon`,
`crates/daemon/src/daemon.rs:391`) installs a stub worker that just drops
the stream (`daemon.rs:432`). The real `SessionParams` plumbing is
explicitly deferred to a follow-up; the skeleton proves the runtime can
bind, accept, dispatch, and shut down.

### 2.5 `AsyncDaemonListener::serve`

The dead-code async path (`async_session/listener.rs:180`) uses pure
`tokio::spawn` (`listener.rs:212`) without `spawn_blocking`. The handler
is `handle_async_session` (`async_session/session.rs`), which is a parallel
implementation of the legacy `@RSYNCD:` handshake against `tokio::io`
primitives. Admission control is a `tokio::sync::Semaphore`
(`listener.rs:188`) with an owned permit dropped after the task body
returns. This path does not interoperate with the production
`handle_session` pipeline.

## 3. Concurrency primitive table

Surfaces a future tokio replacement must preserve or migrate cleanly.

| Primitive | Site | Role |
|---|---|---|
| `std::net::TcpListener` (blocking) | `accept_loop.rs:151`, `connection.rs:339`, `connection.rs:428` | Owned listener; set non-blocking inside the loop to allow signal polling |
| `std::net::TcpStream` (blocking) | accepted in `connection.rs:349`, `connection.rs:437` | Forced back to blocking before worker handoff (BSD propagates O_NONBLOCK on accept) |
| `std::thread::spawn` (worker) | `connection.rs:204` | One OS thread per accepted session; primary capacity ceiling |
| `std::thread::spawn` (acceptor) | `connection.rs:432` | Per-listener fan-in thread in dual-stack mode |
| `std::sync::mpsc::channel` | `connection.rs:415` | Dual-stack fan-in to the main loop; `recv_timeout(100ms)` for signal polling |
| `std::thread::JoinHandle<WorkerResult>` vec | `connection.rs:8`, `workers.rs:7`, `workers.rs:23` | Reaped per-iteration and drained on shutdown |
| `std::panic::catch_unwind` | `connection.rs:210` | Replaces upstream fork crash isolation |
| `Arc<AtomicUsize>` (`ConnectionCounter`) | `connection_counter.rs:17`, used at `connection.rs:198` `acquire`, `connection.rs:133` `active` | Daemon-level `--max-connections` admission cap |
| `Arc<AtomicBool>` (signal flags `shutdown`, `graceful_exit`, `reload_config`, `progress_dump`) | `connection.rs:46`, `connection.rs:58`, `connection.rs:77`, `connection.rs:89`; cloned into dual-stack acceptors at `connection.rs:423`-`424` | SIGTERM/SIGINT/SIGHUP/SIGUSR1/SIGUSR2 fan-out |
| `Arc<Vec<ModuleRuntime>>`, `Arc<Vec<String>>`, `Arc<Vec<SocketOption>>`, `Arc<ConnectionLimiter>`, `Arc<SharedLogSink>` | `connection.rs:195`-`197`, `accept_loop.rs:105`-`111` | Read-only shared state; cloned cheap into every worker closure |
| `tokio::runtime::Builder::new_multi_thread()` | `async_listener.rs:80` (skeleton); `async_session/listener.rs` (dead-code) | Optional runtimes; not active by default |
| `tokio::task::spawn_blocking` | `async_listener.rs:133` | The intended bridge from async accept to sync worker |
| `tokio::sync::Semaphore` | `async_session/listener.rs:113`, `listener.rs:188` | Admission control on the dead-code async path |
| `tokio::sync::broadcast::Sender<()>` | `async_session/listener.rs:114`, `listener.rs:181` | Shutdown fan-out on the dead-code async path |
| `socket2` | `listener.rs:126` `bind_with_backlog` | Explicit `listen(2)` backlog, `SO_REUSEADDR`, `IPV6_V6ONLY` |
| `rayon`, `crossbeam` | none in this crate's daemon path | `daemon` does not depend on either crate today; transfer-side concurrency lives downstream of `handle_session` in `engine`/`transfer`/`core` |

Cargo dependency facts (`crates/daemon/Cargo.toml`):

- `tokio` is optional, gated behind the `async` and `async-daemon` features.
- `dashmap` is optional, gated behind `concurrent-sessions` for the
  dead-code `SessionRegistry` / `ConnectionPool` types only.
- The default build is tokio-free. The CLI dispatcher
  (`crates/daemon/src/cli.rs`) routes to `run_daemon`, not
  `run_async_daemon`.

## 4. Sync I/O blocking points inside the connection handler

`handle_session` (`session_runtime.rs:44`) is the per-thread entry point.
Every I/O step from here is synchronous and assumes the underlying
`DaemonStream` is blocking. The handler dispatches to
`handle_legacy_session` (`session_runtime.rs:207`) because the daemon
protocol is always legacy `@RSYNCD:` (the comment at
`session_runtime.rs:59`-`63` explains why detection deadlocks).

Concrete blocking sites a runtime swap must account for:

1. **Stream configuration**: `configure_stream` (`listener.rs:162`) calls
   `set_read_timeout` / `set_write_timeout`, which are `SO_RCVTIMEO` /
   `SO_SNDTIMEO` on a blocking socket. Tokio sockets do not honour these;
   timeouts must be replaced with `tokio::time::timeout` wrappers.
2. **PROXY protocol read**: `parse_proxy_header` is called at
   `session_runtime.rs:71` directly on the stream and blocks until the
   header arrives.
3. **Reverse DNS**: `resolve_peer_hostname` (`session_runtime.rs:89`) is
   synchronous (`dns-lookup` crate). Should run on a blocking pool or be
   pre-resolved out of band.
4. **BufReader + `read_line` loop**: `session_runtime.rs:221` wraps the
   `DaemonStream` in `BufReader` and pumps `read_trimmed_line` in the
   `while let Some(line) = ...` loop at `session_runtime.rs:241`. Each
   iteration is a blocking `read`. The handler then calls
   `reader.get_mut().flush()` (`session_runtime.rs:321`) and similar to
   force protocol frames out.
5. **Early-input read**: `read_early_input` (`session_runtime.rs:366`)
   calls `reader.read_exact(&mut buf)` (`session_runtime.rs:390`).
6. **Binary negotiation**: `handle_binary_session_internal`
   (`session_runtime.rs:404`) calls `stream.read_exact(&mut client_bytes)`
   at `session_runtime.rs:413`. Currently unreachable from production
   (daemon is always legacy) but lives on the same hot path.
7. **Module-side transfer**: ultimately `execute_transfer`
   (`module_access/transfer.rs:533`) calls `run_server_with_handshake`
   from the `core` crate with `&mut dyn Read` / `&mut dyn Write` borrows
   over the blocking `DaemonStream`. The whole rsync sender/receiver
   pipeline (engine + transfer + filters + checksums + delta) runs on
   this thread.
8. **TLS handshake**: `wrap_accepted_stream` (`connection.rs:291`) calls
   `tls::wrap_stream(acceptor, tcp_stream)` synchronously on the accept
   loop thread. Today this blocks the entire accept loop for the duration
   of one TLS handshake (constraint C2 below).
9. **`refuse_if_at_capacity`**: writes the `@ERROR: max connections (N)
   reached -- try again later\n` payload via `stream.write_all` +
   `stream.flush` (`connection.rs:141`, `connection.rs:149`) on the
   accept-loop thread. Same blocking property as the TLS handshake.
10. **Inetd path**: `serve_inetd_session` (`inetd.rs:49`) runs
    `handle_legacy_session` directly on the caller's thread; there is no
    fan-out, so blocking is intentional and a tokio migration should leave
    this path alone (constraint C3).

## 5. Recommended insertion points for the tokio-based replacement

A DASYNC.2 design that keeps the synchronous worker pipeline intact has
five clean seams. Listed in increasing scope:

1. **Replace `run_single_listener_loop` + `run_dual_stack_loop` body with
   a tokio runtime hosted inside `serve_connections`.** Keep
   `AcceptLoopState`, `ConnectionCounter`, signal flags, module Arcs,
   socket-options Arc, log sink, and the `client_socket_options` plumbing
   intact. The new loop drives `tokio::net::TcpListener::accept().await`
   on the tokio runtime built inside `serve_connections`. This is the
   exact transform `run_hybrid_listener` (`async_listener.rs:73`) already
   implements at proof-of-concept scale.

   Outputs the seam currently produces: an accepted `TcpStream` plus
   peer `SocketAddr`. Substitute `tokio::net::TcpStream::into_std()` +
   `set_nonblocking(false)` so downstream `wrap_accepted_stream`,
   `apply_client_options`, `refuse_if_at_capacity`, and
   `spawn_connection_worker` keep their current signatures.

2. **Move TLS handshake off the accept thread.** Today
   `wrap_accepted_stream` blocks the accept thread for the full TLS
   handshake (`connection.rs:296`). Defer the rustls handshake into
   `tokio::task::spawn_blocking` (sync rustls already supports this) or
   a future async rustls wrapper. The accept thread enqueues the
   handshake task and immediately resumes accepting. Required so paths
   that already use `daemon-tls` do not regress; see constraint C2.

3. **Replace `std::thread::spawn` worker with `tokio::task::spawn_blocking`.**
   The worker body
   (`connection.rs:204`-`252`) already takes a `DaemonStream`, owned
   `SessionParams`-shaped state, and the `ConnectionGuard`. Moving it
   inside `spawn_blocking` preserves:
   - `catch_unwind` panic isolation,
   - `ConnectionGuard` Drop on the worker thread,
   - synchronous worker pipeline downstream of `handle_session`
     (engine, transfer, filters, daemon-tls, parallel-receive-delta,
     IOCP/io_uring metadata) - constraint C1.

   Replace `Vec<JoinHandle<WorkerResult>>` reaping with
   `tokio::task::JoinSet<WorkerResult>` so finished tasks are reaped
   automatically without the O(n) `is_finished()` scan in
   `reap_finished_workers` (`workers.rs:7`).

4. **Bound the blocking pool to the daemon's intended ceiling.** The
   tokio blocking pool sizes to 512 by default. The DASYNC line is to
   make the daemon serve more than the current ~10K-thread ceiling, so
   the blocking pool size and the `--max-connections` admission cap need
   to be reconciled in `run_hybrid_listener`
   (`async_listener.rs:79`) via
   `Builder::max_blocking_threads(...)`. The `ConnectionCounter` admission
   gate (`connection.rs:124`) remains the per-request enforcement point;
   the pool cap is only a backstop.

5. **Replace `mpsc::channel` dual-stack fan-in with `tokio::select!` over
   per-listener `accept().await`.** The two acceptor threads at
   `connection.rs:432` and the `recv_timeout(100ms)` poll loop at
   `connection.rs:479` collapse into a single `tokio::select!` that polls
   both listeners plus the shutdown future. Removes the 100ms shutdown
   latency floor and the need for non-blocking listeners.

Signal-flag wiring stays intact: `register_signal_handlers`
(`accept_loop.rs:22`) and the existing `Arc<AtomicBool>` flags can be
polled inside the tokio loop, exactly as `accept_loop`
(`async_listener.rs:97`) already does. No async signal-handling crate is
required.

Worker reaping in the single-runtime model is best handled by
`JoinSet<()>`: finished tasks are auto-removed; abnormal join errors are
delivered via `JoinSet::join_next().await`. The existing
`drain_workers` / `reap_finished_workers` functions
(`workers.rs:7`, `workers.rs:23`) become a single `JoinSet::shutdown()`
on accept-loop exit.

The dead-code `async_session::AsyncDaemonListener` path
(`async_session/listener.rs`) and its handler
(`async_session/session.rs`) should be flagged as **out of scope** for
DASYNC.2 in their current form: they reimplement the handshake against
`tokio::io` and would diverge from the legacy handler under maintenance
load. Recommend keeping them as a reference for a later phase that
unlocks fully-async I/O at the protocol layer, or deleting them once the
hybrid path lands.

## 6. Constraints (must not break)

The following live capabilities must continue to function across the
runtime swap. Each pins a specific seam.

- **C1. daemon-tls.** `wrap_accepted_stream` (`connection.rs:291`)
  wires `tls::wrap_stream` over the accepted TCP stream. The TLS
  acceptor is held in `AcceptLoopState::tls_acceptor`
  (`connection.rs:34`) and the underlying `rustls::StreamOwned` lives
  inside `DaemonStream::Tls` (`daemon_stream.rs:65`). The post-handshake
  `Read + Write` interface must remain blocking-shaped so the sync worker
  pipeline downstream sees no behavioural change. Migration moves the
  handshake to `spawn_blocking`; everything after that is unchanged.

- **C2. parallel-receive-delta, IOCP, io_uring.** These live below
  `handle_session` (engine + transfer + fast_io). They are invoked
  through `run_server_with_handshake` from the `core` crate at
  `module_access/transfer.rs:561`. They take blocking `&mut dyn Read /
  &mut dyn Write` borrows. Keeping the worker inside
  `tokio::task::spawn_blocking` preserves their assumptions verbatim and
  prevents accidentally driving a blocking syscall on a tokio worker
  thread.

- **C3. Inetd / stdio paths.** `serve_inetd_session` (`inetd.rs:49`) and
  `run_daemon_stdio` (`daemon.rs:270`) are one-shot synchronous sessions
  over inherited file descriptors. They must remain tokio-free; a
  systemd/inetd handoff into a tokio runtime would add latency for no
  benefit since the dispatch is already one-shot. The runtime split must
  be at the `is_stdin_socket()` branch in `run_daemon`
  (`daemon.rs:245`), not earlier.

- **C4. ConnectionCounter semantics.** The atomic admission counter
  (`connection_counter.rs`) and the `@ERROR: max connections (N)
  reached -- try again later\n` refusal wording
  (`connection.rs:140`) must continue to fire from the accept side
  *before* a worker is dispatched. The wording is mirrored byte-for-byte
  from upstream `clientserver.c:744-756`. Moving the worker into a tokio
  task does not change this seam, but the runtime swap must keep the
  admission check on the accept code path, not inside the worker
  closure.

- **C5. Signal semantics.** SIGHUP reload, SIGUSR1 graceful exit,
  SIGUSR2 progress dump, and SIGTERM/SIGINT shutdown must continue to
  drain in-flight workers before exit
  (`drain_workers`, `workers.rs:23`). The replacement loop must wait on
  the `JoinSet` shutdown future before returning from
  `serve_connections`.

- **C6. Branding crate untouched.** The audit lists no edits inside
  `crates/branding`. The user-feedback bar is explicit
  ("no impact to oc-rsync features or branding"); DASYNC.2 design is
  a daemon-runtime change only.

## 7. Open questions for DASYNC.2

These are not blocking for the inventory but are the design choices the
next document should resolve:

1. Single workspace tokio runtime vs per-daemon-process runtime. The
   transport (`crates/transport`) and SSH (`crates/ssh`) crates already
   build their own runtimes for client-side paths; sharing one is not
   currently feasible from the daemon side.
2. Whether to retire the dead-code `async_session::AsyncDaemonListener`
   path before or after DASYNC.2 lands.
3. Whether to keep `run_hybrid_listener` as the production entrypoint
   (renamed) or fold it back into `serve_connections` behind a feature
   gate.
4. Sizing relationship between `--max-connections` and
   `Builder::max_blocking_threads(...)` once the worker bridge is the
   blocking pool.
5. Windows behaviour: `set_nonblocking(false)` on tokio streams is
   documented but rarely exercised; verify it round-trips on the
   Windows CI matrix before DASYNC.3 implementation.

## 8. Citations

Primary call sites referenced above:

- `crates/daemon/src/daemon.rs:233` `run_daemon` dispatch
- `crates/daemon/src/daemon.rs:245` inetd branch
- `crates/daemon/src/daemon.rs:391` `run_async_daemon`
- `crates/daemon/src/daemon/sections/server_runtime/accept_loop.rs:11` `serve_connections`
- `crates/daemon/src/daemon/sections/server_runtime/accept_loop.rs:342` listener split
- `crates/daemon/src/daemon/sections/server_runtime/connection.rs:189` `spawn_connection_worker`
- `crates/daemon/src/daemon/sections/server_runtime/connection.rs:204` `thread::spawn` worker
- `crates/daemon/src/daemon/sections/server_runtime/connection.rs:334` `run_single_listener_loop`
- `crates/daemon/src/daemon/sections/server_runtime/connection.rs:408` `run_dual_stack_loop`
- `crates/daemon/src/daemon/sections/server_runtime/connection.rs:432` per-listener acceptor thread
- `crates/daemon/src/daemon/sections/server_runtime/connection.rs:124` `refuse_if_at_capacity`
- `crates/daemon/src/daemon/sections/server_runtime/connection.rs:291` `wrap_accepted_stream`
- `crates/daemon/src/daemon/sections/server_runtime/connection_counter.rs:17` `ConnectionCounter`
- `crates/daemon/src/daemon/sections/server_runtime/workers.rs:7` `reap_finished_workers`
- `crates/daemon/src/daemon/sections/server_runtime/workers.rs:23` `drain_workers`
- `crates/daemon/src/daemon/sections/server_runtime/listener.rs:126` `bind_with_backlog`
- `crates/daemon/src/daemon/sections/server_runtime/listener.rs:162` `configure_stream`
- `crates/daemon/src/daemon/sections/session_runtime.rs:44` `handle_session`
- `crates/daemon/src/daemon/sections/session_runtime.rs:207` `handle_legacy_session`
- `crates/daemon/src/daemon/sections/session_runtime.rs:404` `handle_binary_session_internal`
- `crates/daemon/src/daemon/sections/module_access/transfer.rs:533` `execute_transfer`
- `crates/daemon/src/daemon/sections/inetd.rs:49` `serve_inetd_session`
- `crates/daemon/src/async_listener.rs:73` `run_hybrid_listener`
- `crates/daemon/src/async_listener.rs:90` `accept_loop` async
- `crates/daemon/src/async_listener.rs:133` `spawn_blocking` worker bridge
- `crates/daemon/src/daemon/async_session/listener.rs:180` `AsyncDaemonListener::serve` (dead-code)

Related design notes consulted (not modified):

- `docs/design/daemon-async-runtime-choice.md`
- `docs/design/daemon-tokio-async-listener-impl.md`
- `docs/design/daemon-async-accept-sync-workers.md`
- `docs/DAEMON_PROCESS_MODEL.md`
