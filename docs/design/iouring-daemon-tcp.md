# io_uring Socket I/O for Daemon TCP (#1876)

## Summary

The daemon accept loop in `crates/daemon/src/daemon.rs` and the
async-mode session in `crates/daemon/src/daemon/async_session/` drive
TCP I/O through `std::net::TcpStream` and `tokio::net::TcpStream`
respectively. Both paths take a kernel copy on every send and a syscall
per recv. This design replaces those reader/writer halves with the
io_uring socket adapters in `crates/fast_io/src/io_uring/socket_*` once
the shared-ring foundation lands, so the daemon shares one ring per
session and submits `IORING_OP_RECV` / `IORING_OP_SEND` SQEs against
poll-add'd accepted fds.

## 1. Scope and ordering

This work strictly follows #1874 (shared session ring with `poll_add`
multiplexing). Submissions for accepted TCP fds reuse the shared ring
already plumbed for file I/O; the daemon does not build private rings
per connection. The wider zero-copy migration to `IORING_OP_SEND_ZC` is
tracked under #1832 (see `docs/design/iouring-send-zc.md`) and is
orthogonal to this change.

Companion docs:

- `docs/design/shared-io-uring-instance.md` - ring-shape contract this
  design composes with.
- `docs/design/iouring-session-ring-pool.md` (#1936) and the per-session
  pool work tracked under #1937 - per-session ring lifetime that the
  daemon listener leases from.
- `docs/design/iouring-send-zc.md` (#1832) - zero-copy follow-on.

## 2. Current state

Sync path:

- `crates/daemon/src/daemon.rs:19` imports `std::net::{TcpListener,
  TcpStream}`. The accept loop in `serve_connections` spawns one OS
  thread per connection and hands each thread a blocking `TcpStream`
  with a configured read/write timeout.
- The session reads the `@RSYNCD:` greeting, negotiates auth, then
  drives the multiplex stream via blocking `Read` / `Write` on the
  same `TcpStream`. Every `write_all` issues a `send(2)` syscall; every
  multiplex frame body costs a kernel copy.

Async path:

- `crates/daemon/src/daemon/async_session/session.rs:14-16` wraps
  `tokio::net::TcpStream` with `BufReader` and `BufWriter`. Tokio's
  default reactor is epoll-driven on Linux. Submission and completion
  go through tokio's per-thread runtime, not io_uring.

Neither path leverages the existing socket adapters in `fast_io`:

- `crates/fast_io/src/io_uring/socket_reader.rs:32` -
  `IoUringSocketReader::from_raw_fd` builds a private ring per socket
  today.
- `crates/fast_io/src/io_uring/socket_writer.rs:32` -
  `IoUringSocketWriter::from_raw_fd` mirror image. The `POLLOUT`
  linked-timeout fix from #1872 lives in `batching.rs:155-254`; that
  fix is load-bearing for the daemon path because daemon writes block
  on slow clients far more often than file writes block on disk.

## 3. Design

### 3.1 Listener stays std

`TcpListener::accept` keeps using the std API. io_uring's
`IORING_OP_ACCEPT` would buy nothing: accept is rare, the kernel-side
work dominates, and a blocking accept thread composes cleanly with the
existing per-connection worker model.

### 3.2 Per-connection adapter swap

After `accept` returns the raw fd, the worker:

1. Sets `TCP_NODELAY` (already set today) and clears `O_NONBLOCK` on
   the fd. io_uring drives readiness via SQEs; we do not want EAGAIN
   leaking through to the multiplex layer.
2. Leases a ring from the per-session `RingPool` (#1936/#1937). The
   pool is created at daemon start with `count = max(num_cpus, 4)` and
   re-leased per accepted connection. Idle connections do not hold a
   lease; the lease is reacquired on demand inside the read/write
   helpers.
3. Calls `register_fd_in_lease(fd)` to add the accepted fd to the
   ring's fixed-fd table. Frees the slot when the connection closes.
4. Wraps the fd in `IoUringSocketReader::with_shared_ring` and
   `IoUringSocketWriter::with_shared_ring` (new constructors that take
   an `Arc<SharedInstance>` instead of building a private ring).

The result is a `(impl Read, impl Write)` pair that drops in to both
the sync and async session paths. Sync workers call `read`/`write_all`
as they do today. The async path replaces tokio's `TcpStream` halves
with a thin `tokio::io::AsyncRead`/`AsyncWrite` adapter that delegates
to the io_uring ops via a oneshot waker per outstanding SQE.

### 3.3 Submission flow

For each `Read::read(buf)` call:

- Reserve an `op_id` via `shared_ring::next_op_id(OpTag::SocketRead)`.
- Push `IORING_OP_RECV` SQE with `fd_index`, the caller buffer, and
  `user_data = pack(OpTag::SocketRead, op_id)`.
- Park on the per-op completion slot. The shared ring's reaper thread
  routes the CQE back via `op_id` demux.

For `Write::write_all(buf)`:

- Same flow with `IORING_OP_SEND`. The poll-add+linked-timeout pattern
  from #1872 (`batching.rs:155-254`) applies to daemon writes too;
  without it a slow client stalls the worker indefinitely. The fix
  must be reused, not reimplemented.

### 3.4 Backpressure and timeouts

The existing `read_timeout` / `write_timeout` setsockopts become
io_uring `IORING_OP_LINK_TIMEOUT` SQEs chained to each RECV/SEND. This
matches upstream's `io_timeout` semantics (compat with `--timeout`)
and removes the syscall-level timeout knobs.

### 3.5 Fallback

If `RingPool::try_lease()` returns `None` (kernel pre-5.6, EMFILE,
disabled by policy), the worker falls through to the existing std/
tokio TCP path unchanged. This preserves correctness on every kernel
the binary already runs on; io_uring is purely additive.

## 4. Pitfalls

- **Buffer lifetime.** `IORING_OP_RECV` requires the receive buffer to
  outlive the SQE. Using caller-owned slices is fine for sync workers
  (the worker blocks on the CQE before returning), but the async
  adapter must hold the buffer in the future and not drop on cancel
  until the CQE arrives. Cancellation issues `IORING_OP_ASYNC_CANCEL`
  and waits for both the original CQE and the cancel CQE.
- **Fd-slot pressure.** Fixed-fd tables default to 8 slots; the daemon
  lifts that to `max_connections + spare` at ring construction. If
  registration fails the worker falls back to raw-fd SQEs (slower but
  correct).
- **CQE storms.** Many idle keep-alive connections all parked on RECV
  burn CQ depth. Cap concurrent parked RECVs at `cq_entries / 2`; over
  the cap, the worker switches to short blocking reads on the raw fd
  for that connection.
- **Auth handshake timing.** `@RSYNCD:` greeting + auth must complete
  inside the existing `IDLE_HANDSHAKE_TIMEOUT`. Linked-timeout SQEs
  enforce this without a separate timer thread.

## 5. Implementation steps

1. Add `IoUringSocketReader::with_shared_ring` /
   `IoUringSocketWriter::with_shared_ring` constructors taking
   `Arc<SharedInstance>`. Migrate the existing private-ring path to
   call them with a single-instance pool.
2. Plumb a `RingPool` into `crates/daemon/src/daemon.rs` behind a
   `--io-uring-listener` flag (default off during stabilisation).
3. Replace the sync worker's `TcpStream` with the adapter pair when
   the flag is on; otherwise keep std.
4. Add a tokio `AsyncRead`/`AsyncWrite` shim and wire it into
   `daemon/async_session/session.rs:14-16`.
5. Promote the flag to default-on after two consecutive nightly runs
   of `tools/ci/run_interop.sh` plus the daemon TPC benchmark
   (`docs/design/daemon-tpc-benchmark-plan.md`) report green and a CPU
   reduction at >= 1 GiB/s sustained.

## 6. Test plan

- Unit: adapter construction succeeds against a `socketpair(2)` fd;
  RECV / SEND parity vs std `TcpStream` for 1 KiB, 64 KiB, 1 MiB
  payloads.
- Unit: linked-timeout SQE fires after `read_timeout`; verify the
  worker surfaces `ErrorKind::TimedOut`, matching the std path.
- Integration: `tools/ci/run_interop.sh` daemon push and pull against
  upstream 3.0.9, 3.1.3, 3.4.1 with the listener flag on and off.
- Bench: daemon TPC plan delta vs main on the rsync-profile container.
- Negative: kernel pre-5.6 path falls back cleanly; `EMFILE` at
  registration falls back; tokio-only build (`--no-default-features
  --features async`) compiles and runs.
