# Daemon Process Model: Fork vs Thread

This document describes how oc-rsync's daemon differs from upstream rsync's
process model and the implications for operators and contributors.

**Last verified:** 2026-02-22

---

## Overview

Upstream rsync and oc-rsync both listen on TCP port 873 and serve the
`@RSYNCD:` protocol, but they use fundamentally different concurrency
strategies:

| Aspect | Upstream rsync | oc-rsync (sync) | oc-rsync (async) |
|--------|---------------|-----------------|------------------|
| Concurrency unit | `fork()` child process | `std::thread::spawn` | `tokio::spawn` task |
| Address space | Separate per connection | Shared across all connections | Shared across all connections |
| Panic/crash isolation | OS process boundary | `catch_unwind` + log | Tokio `JoinHandle` error |
| File descriptor table | Inherited copy | Shared process-wide | Shared process-wide |
| Memory overhead | Full COW page tables | Thread stack only (~8 MB default) | Task frame only (~few KB) |
| Signal handling | Per-process | Process-wide flags (`AtomicBool`) | Process-wide (`broadcast` channel) |

---

## Upstream rsync: Fork-per-Connection

Upstream rsync (C implementation) calls `fork()` for every accepted TCP
connection.  Each child process receives a copy of the parent's address space
via copy-on-write page tables.

**Key properties:**

- **Crash isolation:** If a child segfaults or hits an assertion failure, only
  that child dies. The parent continues accepting connections. The OS reclaims
  all resources (file descriptors, memory, locks) automatically.

- **Memory isolation:** Each child has its own heap. A buffer overrun or use-
  after-free in one connection cannot corrupt another connection's data.

- **Resource inheritance:** The child inherits a *copy* of open file
  descriptors. Closing a descriptor in the child does not affect the parent,
  and vice versa.

- **Reference:** `main.c` -- the daemon's accept loop calls `fork()` and the
  child calls `start_daemon()`.

---

## oc-rsync: Thread-per-Connection (Sync Mode)

The sync daemon (production path) spawns a `std::thread` for each accepted
TCP connection in `serve_connections()`.

**Reference:** `crates/daemon/src/daemon/sections/server_runtime.rs`

### Panic Isolation via `catch_unwind`

Because all connections share a single process, a panic in one thread would
normally abort the entire daemon.  To match upstream rsync's crash-isolation
semantics, every session handler is wrapped in `std::panic::catch_unwind`:

```rust
// server_runtime.rs (simplified)
let handle = thread::spawn(move || {
    let result = std::panic::catch_unwind(
        std::panic::AssertUnwindSafe(|| {
            handle_session(stream, peer_addr, params)
        }),
    );
    match result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err((Some(peer_addr), error)),
        Err(payload) => {
            // Log the panic and continue -- the daemon stays alive.
            let description = describe_panic_payload(payload);
            if let Some(log) = log_sink.as_ref() {
                let text = format!(
                    "connection handler for {peer_addr} panicked: {description}"
                );
                log_message(log, &rsync_error!(SOCKET_IO_EXIT_CODE, text));
            }
            Ok(())
        }
    }
});
```

The `join_worker` function provides a second defense layer: if a panic somehow
escapes `catch_unwind`, the `JoinHandle::join()` returns `Err(payload)` which
is logged and swallowed rather than propagated.

**Why `AssertUnwindSafe`?**  The session closure captures `Arc`-wrapped shared
state (module list, MOTD lines, log sink).  These types are not inherently
`UnwindSafe`, but the closure does not observe torn state because each session
operates on its own `TcpStream` and `Arc::clone` snapshots.  A panic in one
session leaves the `Arc`-wrapped data consistent for other sessions.

### Shared Resources

Unlike forked processes, threads share:

- **The process file descriptor table.** All threads see the same set of open
  file descriptors.  The daemon mitigates this by ensuring each session works
  only with its own accepted `TcpStream` and locally opened files.

- **Global process state.** Signal handlers, environment variables, and the
  current working directory are process-wide. The daemon installs signal
  handlers once before entering the accept loop and uses `AtomicBool` flags
  (`signal_flags.shutdown`, `signal_flags.reload_config`) for safe
  cross-thread signaling.

- **Heap memory.** All sessions share one allocator.  A session that corrupts
  memory through unsafe code could affect other sessions.  oc-rsync mitigates
  this with `#![deny(unsafe_code)]` on the daemon crate.

---

## oc-rsync: Tokio Tasks (Async Mode)

The async daemon (`crates/daemon/src/daemon/async_session.rs`, gated behind
the `async` feature) spawns a `tokio::spawn` task per connection instead of
an OS thread.

**Reference:** `crates/daemon/src/daemon/async_session.rs`

### Panic Handling

Tokio automatically catches panics inside spawned tasks.  A panic causes the
task's `JoinHandle` to resolve to `Err(JoinError)` rather than aborting the
runtime. The daemon logs the error and continues serving other connections,
providing the same isolation guarantee as the sync mode's `catch_unwind`.

### Connection Limiting

The async listener uses a `tokio::sync::Semaphore` to enforce the maximum
connection limit.  When the semaphore has no permits, new connections are
dropped immediately. This is analogous to upstream rsync's `max connections`
directive.

---

## Behavioral Differences for Users

### Identical Behavior

From a client's perspective, the daemon protocol is identical regardless of
the process model.  Clients connect to port 873, exchange `@RSYNCD:` greetings,
authenticate, list modules, and transfer files using the same wire protocol.

### Observable Differences

| Scenario | Upstream (fork) | oc-rsync (thread) |
|----------|----------------|-------------------|
| Session crash | Only that transfer fails; other connections unaffected | Same: `catch_unwind` isolates the panic; daemon continues |
| Memory corruption | Cannot propagate across processes | Could theoretically propagate across threads (mitigated by `deny(unsafe_code)`) |
| `max connections` | Each child is a process; OS `ulimit` applies | Threads are lighter; semaphore or `max_sessions` config enforces the limit |
| PID in logs | Each session has its own PID | All sessions share the daemon PID; session identity is the peer address |
| `kill <session>` | `kill <child-pid>` terminates one session | No per-session PID; the daemon must handle cancellation internally |
| Signal delivery | SIGTERM to child kills only that child | SIGTERM to the daemon process triggers graceful shutdown of all sessions |
| Core dumps | Per-child core file on crash | Single core file for the whole daemon (if a panic escapes `catch_unwind`) |
| Resource cleanup | OS cleans up on child exit (fds, memory, locks) | Rust `Drop` impls handle cleanup; panics unwind the thread stack |

### Operational Recommendations

1. **Monitor daemon logs.** Panics in connection handlers are logged to the
   daemon log file. Watch for `panicked:` entries which indicate bugs that
   need investigation.

2. **Resource limits.** Since all sessions share one process, set appropriate
   `ulimit -n` (open files) values to accommodate the maximum expected
   concurrent connections. Each session needs at least one fd for the socket
   plus fds for transferred files.

3. **Graceful shutdown.** Sending SIGTERM or SIGINT triggers a clean shutdown:
   the daemon stops accepting new connections and drains active sessions before
   exiting.

4. **Session limits.** Use `max connections` in `oc-rsyncd.conf` (or
   `--max-sessions` on the command line) to cap concurrent sessions, matching
   upstream rsync's per-module `max connections` directive.

---

## Design Rationale

The thread-based model was chosen over fork for several reasons:

1. **Cross-platform portability.** `fork()` is Unix-only. oc-rsync targets
   Linux, macOS, and Windows. Threads work uniformly across all three.

2. **Lower overhead.** Thread creation avoids the cost of duplicating page
   tables and is significantly faster than `fork()` on modern systems.

3. **Shared state efficiency.** The module configuration, MOTD, and log sink
   are shared via `Arc` without serialization or IPC overhead.

4. **Rust safety guarantees.** Rust's ownership model, `Send`/`Sync` bounds,
   and `deny(unsafe_code)` eliminate most classes of memory corruption that
   make fork's address-space isolation valuable in C.

The `catch_unwind` wrapper was added in PR #2413 to close the remaining
isolation gap: ensuring that a panic in one session handler does not tear
down the daemon process and kill all active transfers.

---

## Source References

| File | Role |
|------|------|
| `crates/daemon/src/daemon/sections/server_runtime.rs` | Sync accept loop, thread spawn, `catch_unwind` |
| `crates/daemon/src/daemon/async_session.rs` | Async accept loop, `tokio::spawn` |
| `crates/daemon/src/daemon/sections/session_runtime.rs` | Per-connection session handler |
| `crates/daemon/src/daemon/sections/signals.rs` | Signal flag registration |
| Upstream `main.c` | Fork-per-connection model |
