# io_uring Socket I/O on Daemon TCP - Readiness Audit (#1876)

## 0. Purpose

#1876 wires the io_uring socket adapters (`IoUringSocketReader` /
`IoUringSocketWriter`) into the daemon TCP path so that each accepted
connection submits `IORING_OP_RECV` / `IORING_OP_SEND` against a leased
ring from the per-session pool. This document audits whether the
prerequisites have landed, lists what is still missing, and gives a
concrete wiring plan.

Companion docs:

- `docs/design/iouring-daemon-tcp.md` - the original architecture for
  #1876. This readiness audit refines its sequencing against the
  primitives that have since landed (#1874 / #3553, #1937 / #4275,
  #1935 / #4278).
- `docs/design/iouring-session-ring-pool.md` (#1937) - per-session pool
  contract.
- `docs/design/shared-io-uring-instance.md` (#1408) - shared-ring
  topology and demux scheme.
- `docs/design/daemon-tokio-async-listener-impl.md` (#1935) - hybrid
  tokio listener that hands accepted streams to sync workers.
- `docs/design/iouring-send-zc.md` (#1832) - zero-copy follow-on that
  layers on top of #1876.

## 1. Status of dependencies

| Task         | Subject                                              | Status                          | Evidence                                                                                                                            |
|--------------|------------------------------------------------------|---------------------------------|-------------------------------------------------------------------------------------------------------------------------------------|
| #1874        | Shared ring with `POLL_ADD` multiplexing             | landed                          | `crates/fast_io/src/io_uring/shared_ring.rs`; PR #3553; audit at `docs/audits/shared-iouring-session-instance.md:789` ("shipped").  |
| #1875        | No io_uring task with this id                        | not-found                       | Repo-wide grep yields no #1875 reference in any io_uring design, audit, or source. PR #1875 is a packaging change unrelated to this stack. |
| #1937 / #4275 | Per-session ring pool (`SessionRingPool`)            | landed                          | `crates/fast_io/src/io_uring/session_pool.rs`; PR #4275 (just merged).                                                              |
| #1935 / #4278 | Hybrid tokio listener with sync worker dispatch     | landed (skeleton)               | `crates/daemon/src/async_listener.rs`; `run_async_daemon` skeleton in `crates/daemon/src/daemon.rs:217`; PR #4278.                  |
| #1872        | `POLLOUT` linked-timeout fix for blocked sends       | landed                          | `crates/fast_io/src/io_uring/batching.rs:155-254`.                                                                                  |
| #1876        | Daemon TCP socket I/O wiring                          | design-only, code not started   | `docs/design/iouring-daemon-tcp.md`. No `with_shared_ring` constructor exists on the socket adapters; daemon still uses `std::net`. |

### What "#1875" most plausibly meant

The original #1876 task statement reads "after #1874+#1875 land". No
io_uring tracker uses #1875. The two prerequisites that did actually
need to land before #1876 became actionable are the **session ring
pool** (#1937 / PR #4275) and the **hybrid tokio listener** (#1935 /
PR #4278). Both merged immediately before this audit, which matches
the original spirit of the dependency note. We proceed treating
"#1874 + session-pool + tokio-listener" as the real prerequisite set.

## 2. What is present today

Socket adapters (`crates/fast_io/src/io_uring/`):

- `socket_reader.rs:16` - `IoUringSocketReader` with one constructor:
  `from_raw_fd(fd, &IoUringConfig)`. Builds a **private ring per
  socket** via `config.build_ring()`. No session-pool integration.
- `socket_writer.rs:23` - `IoUringSocketWriter` mirror; same private
  ring shape.
- `socket_factory.rs` - `IoUringOrStdSocketReader` / `Writer` enums
  that fall back to std on construction failure. Already consumed by
  `rsync_io` for SSH transports, but not by the daemon.

Shared ring (`shared_ring.rs`):

- `SharedRing::try_new(reader_fd, writer_fd, ...)` returns one ring
  hosting both directions with `IORING_OP_POLL_ADD` for write
  readiness and `OpTag`-tagged `user_data` for CQE demux. Used today
  by file batched I/O, not by the socket adapters.

Session ring pool (`session_pool.rs`):

- `SessionRingPool::new(SessionPoolConfig)` builds `N` rings up front
  (`N = min(available_parallelism(), 16)`).
- `acquire() -> Option<RingLease<'_>>` round-robins through the pool;
  `RingLease` derefs to `&mut RawIoUring`.
- No socket-specific integration; the pool is a primitive only.

Hybrid async listener (`async_listener.rs`):

- `run_hybrid_listener(bind_addr, worker_threads, shutdown, worker)`
  drives `tokio::net::TcpListener::accept` on a tokio multi-thread
  runtime, converts each `tokio::net::TcpStream` to a blocking
  `std::net::TcpStream`, and dispatches to `worker` via
  `tokio::task::spawn_blocking`.
- Default daemon still uses `serve_connections` (`daemon.rs:188`)
  with `std::net::TcpListener` and `std::thread::spawn`.
- `run_async_daemon` (`daemon.rs:217`) is gated behind the
  `async-daemon` feature and currently logs-and-closes; the worker
  callback is the integration seam for #1876.

Daemon async session (`crates/daemon/src/daemon/async_session/`):

- `session.rs` uses `tokio::net::TcpStream` wrapped in `BufReader` /
  `BufWriter`. Tokio's reactor is epoll-driven; no io_uring path
  exists here yet.

## 3. What is missing for #1876

1. **`with_shared_ring` / `with_session_pool` constructors.** Neither
   `IoUringSocketReader` nor `IoUringSocketWriter` has a constructor
   that accepts a leased `RingLease` (or an `Arc<SharedInstance>`).
   Today every adapter builds its own private ring, which defeats the
   whole point of leasing from `SessionRingPool`.
2. **Fixed-fd registration helper.** The socket adapters call
   `try_register_fd` against their private ring. The pooled path needs
   `register_fd_in_lease(&mut RingLease, fd) -> Option<i32>` so that
   the accepted fd's fixed-file slot is bound to the leased ring's
   table for the connection lifetime, and unregistered on drop.
3. **Daemon flag and runtime plumbing.** No `--io-uring-listener`
   flag, no `RuntimeOptions` field, no `SessionRingPool` constructed
   at daemon start. `run_daemon` and `run_async_daemon` both need a
   shared `Arc<SessionRingPool>` plumbed through to the per-connection
   worker.
4. **Sync worker swap.** `serve_connections` hands the worker a
   `TcpStream`. The worker needs an io_uring-or-std reader/writer
   pair instead; this is the call site that consumes the new
   constructor from (1).
5. **Async worker shim.** `async_session::session::handle_async_session`
   takes a `tokio::net::TcpStream`. A `tokio::io::AsyncRead` /
   `AsyncWrite` adapter that delegates to the io_uring SQEs via a
   oneshot waker per outstanding op is required. Without it the async
   path stays on tokio's epoll reactor and the wiring is sync-only.
6. **Cancellation / drop discipline.** `IORING_OP_RECV` requires the
   caller buffer to outlive the SQE. The async adapter must either
   own the buffer in the future and emit `IORING_OP_ASYNC_CANCEL` on
   drop, or use a bounded-lifetime borrow that the runtime enforces.
   No code path for this exists today.
7. **Linked timeouts.** `IORING_OP_LINK_TIMEOUT` is not yet used by
   the socket adapters; the existing `read_timeout` / `write_timeout`
   setsockopts have no analogue in the SQE flow. The fix at
   `batching.rs:155-254` is the template; #1876 needs the equivalent
   for socket RECV / SEND.

## 4. Recommendation: split #1876 into subtasks

The original #1876 plan in `iouring-daemon-tcp.md` lists five
implementation steps but treats them as one issue. Given the missing
surface enumerated in section 3, split #1876 into the following
actionable subtasks. Each is independently reviewable and shippable.

- **#1876-a "Pooled-ring socket constructors"** - add
  `IoUringSocketReader::with_pooled_ring(&mut RingLease, fd, &config)`
  and the writer mirror; migrate `from_raw_fd` to call the new path
  with a single-ring pool internally. No daemon changes.
- **#1876-b "Daemon `RingPool` plumbing"** - construct a
  `SessionRingPool` at daemon start when io_uring is available and the
  `--io-uring-listener` flag is set; thread an `Arc<SessionRingPool>`
  into `serve_connections` and `run_async_daemon`. Worker keeps using
  `TcpStream`; pool is wired but unused.
- **#1876-c "Sync worker swap"** - replace the sync worker's
  `TcpStream` with the pooled adapter pair from #1876-a. Wraps the
  existing handshake / multiplex code unchanged. This is the smallest
  observable shipping unit.
- **#1876-d "Async adapter shim"** - implement
  `tokio::io::AsyncRead` / `AsyncWrite` over the pooled SQEs and wire
  it into `async_session::session.rs:14-16`. This is the largest
  subtask; do it last.
- **#1876-e "Linked timeouts + cancel"** - add
  `IORING_OP_LINK_TIMEOUT` to socket RECV / SEND and the
  `IORING_OP_ASYNC_CANCEL` discipline for the async adapter. Replaces
  the setsockopt-based `read_timeout` / `write_timeout`.

The sequencing puts the sync path on io_uring first (a + b + c) so
the benchmark numbers and rollback story are settled before the
async adapter (d) is built. Linked timeouts (e) attach last because
they only matter once the adapters are real consumers.

## 5. Trigger conditions

Wiring is enabled at runtime only when **all** of the following hold:

- Kernel supports io_uring (`is_io_uring_available()`, kernel
  >= 5.6 in practice).
- Cargo feature `io_uring` is on (default for Linux builds).
- Cargo feature `async-daemon` is on **for the async path only**;
  the sync path requires no feature gate beyond `io_uring`.
- Operator opts in via `--io-uring-listener` (default off during
  stabilisation per section 3.2 of `iouring-daemon-tcp.md`).
- `SessionRingPool::try_new` succeeds. Any failure (kernel
  refusal, EMFILE, registered-file slot exhaustion) drops back to
  the std/tokio path without surfacing an error.

On non-Linux targets the adapters are stubbed (`io_uring_stub.rs`)
and every constructor returns the std fallback. No `#[cfg(linux)]`
gates are required in the daemon source.

## 6. Five-step implementation plan

1. **Add pooled-ring constructors** (`#1876-a`). In
   `crates/fast_io/src/io_uring/socket_reader.rs` and
   `socket_writer.rs`, add
   `with_pooled_ring(lease: &mut RingLease<'_>, fd: RawFd,
   config: &IoUringConfig) -> io::Result<Self>` that borrows the
   lease's ring instead of building a new one. Keep `from_raw_fd`
   for backwards compatibility, implemented via a one-shot pool.
   Add `register_fd_in_lease` in `session_pool.rs`. Unit tests:
   round-trip RECV / SEND against a `socketpair(2)` fd using a
   single-ring pool, with and without fixed-fd registration.

2. **Construct the pool and plumb it** (`#1876-b`). In
   `crates/daemon/src/daemon.rs`, add a `--io-uring-listener` CLI
   flag plus a `RuntimeOptions::io_uring_listener: bool` field. In
   `serve_connections` and `run_async_daemon`, build an
   `Arc<SessionRingPool>` from `SessionPoolConfig::default()` when
   the flag is set and io_uring is available; pass it into the
   per-connection worker via the existing closure capture. No
   behavioural change yet.

3. **Swap the sync worker** (`#1876-c`). Inside the sync per-
   connection worker (the closure passed to `thread::spawn` in
   `serve_connections`), call
   `pool.acquire()?.with(|lease| {
       let reader = IoUringSocketReader::with_pooled_ring(lease, fd, ...)?;
       let writer = IoUringSocketWriter::with_pooled_ring(lease, fd, ...)?;
       handle_session(reader, writer, ...)
   })`. Fall back to the std `TcpStream` path on any `None` /
   `Err`. The existing handshake, auth, and multiplex code consume
   `impl Read + Write` and need no changes. Add an interop run with
   the flag on against upstream 3.0.9 / 3.1.3 / 3.4.1.

4. **Add the async adapter shim** (`#1876-d`). Create
   `crates/fast_io/src/io_uring/async_socket.rs` exposing
   `IoUringAsyncSocket` implementing `tokio::io::AsyncRead` and
   `AsyncWrite`. Internally it submits SQEs via the leased ring and
   parks the task on a `tokio::sync::oneshot` keyed by `op_id`; a
   single reaper task per pool ring routes CQEs to the right
   waker. Replace the `tokio::net::TcpStream` halves in
   `async_session/session.rs:14-16` with this adapter when the
   pool is present. Cancellation: on adapter drop, submit
   `IORING_OP_ASYNC_CANCEL` and await both CQEs before releasing
   the owned receive buffer.

5. **Attach linked timeouts and promote the flag** (`#1876-e`).
   Replace `read_timeout` / `write_timeout` setsockopts with
   `IORING_OP_LINK_TIMEOUT` SQEs chained to every RECV / SEND,
   using the same pattern as `batching.rs:155-254`. Run two
   consecutive nightly cycles of `tools/ci/run_interop.sh` plus the
   daemon TPC benchmark with the flag on; promote
   `--io-uring-listener` to default-on once both report green and a
   measurable CPU reduction at >= 1 GiB/s sustained.

## 7. Test plan

- **Unit (cross-platform).** `with_pooled_ring` constructor returns
  `Err` cleanly on non-Linux / feature-off builds. `OpTag` round-trip
  parity with the existing `shared_ring` tests.
- **Unit (Linux).** Pooled-ring RECV / SEND parity vs std
  `TcpStream` across 1 KiB, 64 KiB, 1 MiB payloads against a
  `socketpair(2)` fd. Linked-timeout SQE surfaces
  `ErrorKind::TimedOut` matching the std path. Pool starvation
  (`acquire()` returns `None`) drops back cleanly.
- **Integration.** `tools/ci/run_interop.sh` daemon push and pull
  against upstream 3.0.9, 3.1.3, 3.4.1 with `--io-uring-listener`
  on and off. Concurrent-sessions stress test from
  `crates/daemon/src/daemon/concurrent_tests.rs` with the flag on.
- **Bench.** Daemon TPC plan delta vs master on the rsync-profile
  container; target a CPU reduction at >= 1 GiB/s sustained.
- **Negative.** Pre-5.6 kernel path falls through to std. EMFILE at
  pool construction falls through. `cargo build --no-default-features
  --features async-daemon` compiles without io_uring.

## 8. Rollback

The flag is off by default. A single revert of the
`run_daemon`/`run_async_daemon` plumbing in step 2 restores the std
path even with the adapters compiled in. The pooled-ring constructors
from step 1 stay - they are dormant without a caller.
