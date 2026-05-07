# Daemon Thread-per-Connection Scalability Audit

Tracking: oc-rsync task #1673. Static analysis only - no code changes.

## 1. Current accept loop and connection lifecycle

The daemon binds one or more `TcpListener` sockets and runs an accept
loop in `crates/daemon/src/daemon/sections/server_runtime/accept_loop.rs`.
`serve_connections` (lines 11-319) sets up signal handlers, opens log
sinks, builds the bound listeners, then dispatches to either
`run_single_listener_loop` or `run_dual_stack_loop` (lines 288-294)
defined in `connection.rs`.

Single-listener path (`connection.rs:216-274`): non-blocking
`listener.accept()` polled with `SIGNAL_CHECK_INTERVAL` sleep on
`WouldBlock`. On success, `spawn_connection_worker` (lines 106-170)
pushes a fresh `std::thread::spawn` worker into `state.workers`. Each
worker increments `ConnectionCounter` (RAII guard,
`connection_counter.rs:38-44`), wraps the session in
`std::panic::catch_unwind` (lines 127-145), and runs `handle_session`
to completion synchronously - greeting, auth, module dispatch, full
delta transfer.

Dual-stack path (`connection.rs:281-385`): one acceptor thread per
listener forwards `(TcpStream, SocketAddr)` over an MPSC channel; the
main loop drains the channel with `recv_timeout(100ms)` and spawns
the same worker. Finished workers are reaped each iteration via
`reap_finished_workers` (`workers.rs:7-20`); `drain_workers` (lines
23-28) joins survivors at shutdown.

## 2. Scalability ceiling

One OS thread per connection. Default Rust thread stack is 2 MiB of
reserved virtual address space; ~500 live connections per GiB of
committed thread stacks, ~30 000 ceiling on Linux from default
`RLIMIT_NPROC` (`ulimit -u`). `RLIMIT_NOFILE` defaults to 1024 and
becomes the first practical limit.

Per-connection working-set estimate at peak transfer:

- `BufferPool` borrowed buffers (`crates/engine/src/local_copy/buffer_pool/pool.rs`):
  one ~256 KiB block + temporary scratch -> ~512 KiB resident.
- `ReorderBuffer` for concurrent delta (`crates/engine/src/concurrent_delta/`):
  bounded ring of pending `DeltaWork` items, ~1-4 MiB depending on
  `reorder_capacity`.
- File list (sender flist + receiver mirror): ~120 bytes/entry; a
  10 k-file transfer pins ~1.2 MiB on each side.
- 2 MiB thread stack reservation (mostly virtual, ~64 KiB committed).

Aggregate: ~5-8 MiB resident per active session, dominated by
`ReorderBuffer` and flist. 10 000 active transfers project to
50-80 GiB RSS plus 20 GiB stack address space - far past commit
limits before the thread cap bites.

## 3. Comparison with upstream rsyncd

Upstream `clientserver.c:daemon_main()` forks a child per accepted
connection: full COW process, separate page tables, ~few MiB private
RSS once the child writes pages. Crash isolation is the OS process
boundary; signal disposition and resource accounting are per-child.

oc-rsync uses `thread::spawn` + `catch_unwind`
(`connection.rs:121-145`, `workers.rs:38-58`). Shared address space
removes fork/exec overhead and the COW page-table cost, gives lower
per-connection RSS, and lets the global `BufferPool` amortise
allocations across sessions. The trade-off: panics that escape
`catch_unwind` (FFI, stack overflow) take the daemon down where
upstream loses only the child; module config and bandwidth limiters
become shared mutable state requiring synchronisation.

## 4. Failure modes

- **Thread exhaustion under SYN flood / connect storm**: every
  successful accept allocates a `JoinHandle` and 2 MiB stack. With
  no admission control before `thread::spawn`, an attacker hits
  `RLIMIT_NPROC` or commits thread stacks into the OOM killer's
  range. `ConnectionCounter::active()`
  (`connection_counter.rs:32-35`) is wired but unread.
- **Slowloris on auth-stalled connections**: `handle_session` runs
  greeting and auth synchronously on the worker thread. A peer that
  never finishes `@RSYNCD:` negotiation pins one OS thread
  indefinitely; there is no pre-handshake timeout in the accept
  path.
- **Lock contention on shared state**: `state.modules`
  (`Arc<Vec<ModuleRuntime>>`, `connection.rs:15`) is hot-cloned on
  every accept; `ModuleRuntime` holds per-module lock files and the
  `ConnectionLimiter`. SIGHUP reload swaps the `Arc` under the
  acceptor only (`accept_loop.rs:267-286`), but per-module
  `active_connections` counters serialise every connect/disconnect.
- **Worker reap latency**: `reap_finished_workers` is O(n) over
  `state.workers` each accept; a 10 k-deep vector adds measurable
  jitter to accept latency under load.

## 5. Mitigation paths

- **Async tokio accept loop** (#1934, `daemon-async-listener-rfc.md`)
  + async pre-handshake greeting (#1935): move accept, greeting, and
  auth onto a tokio runtime, hand established sessions to a sync
  worker pool only for the bulk transfer phase. Eliminates the
  thread-per-connection cost during the slow auth window where
  slowloris bites.
- **Daemon-level `max connections`**: enforce
  `ConnectionCounter::active() >= limit` before `spawn_connection_worker`,
  returning `@ERROR: max connections (N) reached` like upstream
  `clientserver.c:start_daemon()`. The atomic counter is already in
  place; only the gate and config wiring are missing.
- **Connection rate limit**: token-bucket on accept keyed by peer
  `IpAddr`, dropping connections beyond the configured per-source
  rate. Pairs with `reverse_lookup` already plumbed through
  `AcceptLoopState`.
- **Bounded worker pool**: replace ad-hoc `Vec<JoinHandle>` with a
  fixed-size pool plus a backlog queue; reject (or 503-equivalent)
  past the high-water mark instead of unbounded `thread::spawn`.
- **Reduced thread stack**: `Builder::stack_size(512 * 1024)` for
  session workers cuts reserved address space 4x; transfer code
  avoids deep recursion so 512 KiB is sufficient.
