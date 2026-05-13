# Async Runtime Impact on io_uring Integration

Tracking issue: oc-rsync task #1595.

Related design notes:

- `docs/design/io-uring-rayon-composition.md` (#1283/#1284) - rayon-side
  composition policy for the native io_uring path.
- `docs/design/tokio-spawn-blocking-rayon.md` (#1751) - bridge between
  the async daemon and rayon CPU work.
- `docs/design/async-migration-plan.md`,
  `docs/design/daemon-tokio-async-listener-impl.md` (#1934/#1935) -
  async daemon runtime this evaluation must compose with.
- `docs/design/daemon-async-accept-sync-workers.md` (#1674) - hybrid
  async/sync execution model.
- `docs/design/iouring-session-ring-pool.md` (#1409) - session-level
  ring pool that io_uring submissions share.

## 1. Question

The oc-rsync codebase straddles two concurrency models: tokio for the
async daemon accept loop and rayon + dedicated threads for the synchronous
transfer pipeline. The `fast_io::io_uring` module provides high-performance
I/O on Linux 5.6+ using the raw `io_uring` crate (synchronous submission
via `submit_and_wait`). This document evaluates how the async tokio runtime
interacts with that synchronous io_uring integration across six dimensions
and recommends a composition strategy.

## 2. Current Architecture

### 2.1 io_uring in `fast_io`

The io_uring integration lives in `crates/fast_io/src/io_uring/` and
consists of:

- **`config.rs`** - `IoUringConfig` with tunable SQ depth (default 64),
  buffer size (64 KB), SQPOLL toggle, fd registration, and registered
  buffer count. Runtime availability check cached in process-wide
  atomics (`is_io_uring_available()`).
- **`file_writer.rs` / `file_reader.rs`** - per-file io_uring ring
  instances. Each writer/reader owns its own `RawIoUring`, registered
  buffers, and fixed-fd slot. Writes/reads buffer internally and flush
  via `submit_and_wait()` batches.
- **`disk_batch.rs`** - `IoUringDiskBatch` shares a single ring across
  multiple file write operations. Used by the disk-commit thread to
  amortize ring setup.
- **`shared_ring.rs`** - `SharedRing` co-locates a reader fd and a
  writer fd on one ring, halving syscall cost by servicing both
  directions with a single `submit_and_wait`. Demuxes CQEs via an
  8-bit `OpTag` in the high bits of `user_data`.
- **`registered_buffers.rs`** - `RegisteredBufferGroup` of page-aligned
  buffers pinned via `IORING_REGISTER_BUFFERS`. Lock-free atomic bitset
  for checkout/return. Telemetry counters feed adaptive sizing.
- **`buffer_ring.rs`** - PBUF_RING (Linux 5.19+) for completion-time
  buffer selection.
- **`batching.rs`** - `submit_write_batch` / `submit_send_batch` helper
  loops. Socket sends are gated by `IORING_OP_POLL_ADD(POLLOUT)` to
  prevent back-pressure deadlocks (issue #1872).
- **`socket_writer.rs` / `socket_reader.rs`** - per-socket io_uring
  instances for daemon TCP I/O.

All submission paths are synchronous: the caller calls
`ring.submit_and_wait(n)` and blocks until `n` CQEs are ready. There
is no integration with any event loop.

### 2.2 Tokio in the daemon

The async daemon (`crates/daemon/src/daemon/async_session/`) uses a
multi-threaded tokio runtime for the TCP accept loop and per-connection
handler tasks. `AsyncDaemonListener::serve()` calls
`tokio::select!` over `listener.accept()` and `shutdown_rx.recv()`,
spawning a `tokio::spawn` task per connection. Each connection task
runs the rsync protocol state machine.

The transfer pipeline itself - delta apply, checksum computation,
file-list building, disk writes - remains synchronous. It runs on
rayon workers and a dedicated disk-commit thread
(`crates/transfer/src/disk_commit/thread.rs`). When invoked from an
async daemon task, CPU-bound rayon work is bridged via
`tokio::task::spawn_blocking` per #1751.

### 2.3 Wiring: disk-commit thread and io_uring

The disk-commit thread creates a single `IoUringDiskBatch` at startup
(via `try_create_disk_batch()`) and threads it into every per-file
`process_file` / `process_whole_file` call. The `Writer` enum in
`disk_commit/writer.rs` dispatches between `Buffered` (standard I/O)
and `IoUring` (batch ring) variants. Sparse mode forces the buffered
path because io_uring does not support `Seek`.

This disk-commit thread is a plain `std::thread` - it never enters the
tokio runtime. Its io_uring submissions are entirely synchronous.

## 3. Interaction Analysis

### 3.1 Submission Thread Model

**Issue**: `submit_and_wait()` is a blocking syscall. If called from a
tokio async worker, it stalls that worker and starves other tasks. Tokio
workers use cooperative scheduling - they must yield regularly or the
entire runtime degrades.

**Current state**: All io_uring submission happens on non-tokio threads:

- The disk-commit thread (`std::thread`, name `disk-commit`).
- Rayon workers (via `IoUringReader` / `IoUringWriter` instances).
- The CLI path has no tokio runtime at all.

No io_uring submission currently occurs on tokio worker threads. The
`spawn_blocking` bridge from #1751 moves rayon entry points onto
tokio's blocking pool, which is sized for blocking work (default cap
512 threads). The blocking pool threads are permitted to call
`submit_and_wait` because they are not participating in tokio's
cooperative scheduling.

**Assessment**: No conflict. The current architecture already isolates
io_uring submission from tokio workers. If future daemon code needs
direct io_uring access (e.g., for socket I/O within an async handler),
it must go through `spawn_blocking` or use `tokio::task::block_in_place`
to temporarily opt out of cooperative scheduling. This pattern is
already established and documented.

### 3.2 Completion Polling

**Issue**: io_uring completions arrive on the CQ ring, which can be
polled via `io_uring_enter` (the `submit_and_wait` path) or monitored
via the io_uring fd itself (which is an epoll-compatible fd). In
principle, the io_uring fd could be registered with tokio's epoll
reactor via `tokio::io::unix::AsyncFd`, allowing completions to
integrate with tokio's event loop without blocking a thread.

**Analysis**: Registering the io_uring fd with `AsyncFd` would allow
an async task to `await` CQ readiness, then drain completions without
blocking. The sequence:

```text
let guard = async_fd.readable().await?;
ring.completion().for_each(|cqe| { ... });
guard.clear_ready();
```

This would eliminate the need for `submit_and_wait` and let io_uring
completions share tokio's epoll loop. However, this approach has
significant drawbacks:

1. **Submission still requires mutable ring access.** `ring.submission()`
   and `ring.submit()` are `&mut self`. The ring cannot be shared across
   tasks without synchronization, defeating the multi-threaded advantage.
2. **Buffer ownership becomes complex.** Buffers passed to SQEs must
   remain valid until the corresponding CQE is reaped. In async code,
   task cancellation can drop the owning future at any `.await` point,
   invalidating the buffer while the kernel still references it.
3. **No measurable benefit for disk I/O.** The disk-commit thread is
   already dedicated to I/O; integrating with tokio's event loop adds
   latency (epoll wakeup) without reducing it.
4. **Socket I/O could benefit.** For daemon TCP connections, integrating
   io_uring sends/recvs with tokio's event loop could reduce context
   switches. However, the daemon's current per-connection model already
   uses `TcpStream` through tokio's built-in epoll integration, which
   is well-optimized.

**Assessment**: Not worth pursuing for oc-rsync's workload. The disk
I/O hot path is on a dedicated thread where `submit_and_wait` is
natural. Socket I/O through the daemon is already served by tokio's
native epoll reactor. The `AsyncFd` bridge adds complexity and
cancellation-safety risks without measurable throughput gain.

### 3.3 Buffer Ownership and Async Cancellation

**Issue**: io_uring registered buffers (`IORING_REGISTER_BUFFERS`) and
provided buffer rings (`IORING_REGISTER_PBUF_RING`) pin memory that the
kernel may read from or write to at any time between SQE submission and
CQE completion. If a Rust future holding such a buffer is cancelled
(dropped at an `.await` point), the buffer's memory could be deallocated
while the kernel still has a pending operation referencing it.

**Current state**: The `RegisteredBufferGroup` in
`registered_buffers.rs` manages buffer lifetimes with RAII slots
(`RegisteredBufferSlot<'a>` borrows `&'a RegisteredBufferGroup`). The
group allocates page-aligned memory on construction and deallocates
only in `Drop`. The slot's `Drop` returns the slot index to the
atomic free-list. Crucially, the group outlives all slots by Rust's
borrow rules.

The buffer flow is:

1. `RegisteredBufferGroup` allocates and registers buffers with the ring.
2. Callers `checkout()` a slot, getting a `RegisteredBufferSlot<'_>`.
3. The slot's pointer is passed to an SQE (e.g., `ReadFixed`/`WriteFixed`).
4. `submit_and_wait()` blocks until the CQE arrives.
5. The caller processes the data and drops the slot.

Step 4 is synchronous - the buffer is guaranteed to outlive the kernel
operation because the thread does not proceed past `submit_and_wait`
until the CQE confirms completion.

**Async risk**: If this flow were made async (submit, `.await`
completion, process), cancellation between submit and completion would
drop the slot while the kernel still has the buffer. The kernel would
write into freed memory or a recycled buffer - a use-after-free.

**Assessment**: The synchronous `submit_and_wait` model avoids this
class of bugs entirely. Moving to async completion handling would
require either:

- Leak-on-cancel: do not deallocate buffers when a future is dropped,
  recover them on the next CQE drain. This is the `tokio-uring`
  approach but complicates memory management.
- Cancellation-safe wrappers: pin the buffer in an `Arc` that is only
  released when both the future and the kernel are done. Adds reference
  counting overhead to every I/O operation.

Neither is justified for oc-rsync's workload where the blocking model
works correctly and efficiently.

### 3.4 SQPOLL Thread and tokio Workers

**Issue**: When `IoUringConfig::sqpoll` is `true`, the kernel spawns a
per-ring polling thread (`io_sq_thread`) that continuously polls the
submission queue, eliminating the `io_uring_enter` syscall on submit.
This kernel thread runs at high priority and can pin a CPU core. If
tokio's worker threads are also competing for the same cores, SQPOLL
may interfere with cooperative scheduling.

**Current state**: SQPOLL is disabled by default
(`IoUringConfig::default().sqpoll == false`). It requires
`CAP_SYS_NICE` or root. `build_ring()` falls back transparently to a
regular ring on `EPERM`, recording the fallback in the
`SQPOLL_FALLBACK` atomic for diagnostics.

The SQPOLL idle timeout is 1000ms (`sqpoll_idle_ms`). After 1s of
inactivity, the kernel thread goes to sleep. It wakes on the next SQE
submission. This means SQPOLL cost is proportional to active I/O, not
to calendar time.

**Interaction with tokio**: If SQPOLL is active and the daemon's tokio
runtime is also running, the SQPOLL kernel thread and tokio's worker
threads compete for CPU time. On a system with N cores:

- Tokio spawns N worker threads (one per core).
- SQPOLL adds one kernel thread per active ring.
- The disk-commit thread occupies one more.

With SQPOLL enabled and a single ring (the session ring pool from
#1409), the total thread count is N + 2. This is modest oversubscription.
However, if multiple rings each enable SQPOLL (the per-file ring
pattern from `file_writer.rs`), oversubscription grows linearly.

**Assessment**: Minimal conflict in practice. SQPOLL is off by default,
requires elevated privileges, and is designed for latency-sensitive
workloads (NVMe, DPDK-style networking) where the ring is continuously
loaded. For rsync's batch-oriented disk I/O, the regular submission
path with batched SQEs (64-256 per `submit_and_wait`) already amortizes
syscall cost sufficiently. SQPOLL adds value only on high-IOPS NVMe
devices with sub-microsecond latency where even amortized
`io_uring_enter` cost is significant.

Recommendation: keep SQPOLL disabled by default. Document that
`--io-uring-sqpoll` should not be combined with high daemon concurrency
(many simultaneous transfers) on machines with few cores.

### 3.5 `tokio-uring` Alternative

**Issue**: Should oc-rsync adopt `tokio-uring` (crate version 0.5.x)
instead of the raw `io_uring` crate? `tokio-uring` provides a
tokio-native async interface to io_uring where futures yield on CQE
completion.

**Analysis**:

`tokio-uring` runs a dedicated single-threaded ring driver entered via
`tokio_uring::start()`. Key constraints:

1. **Single-threaded runtime.** `tokio-uring` creates its own
   `current_thread` tokio runtime. The multi-threaded `tokio::main`
   runtime that the async daemon uses cannot host it directly. Running
   `tokio_uring::start` on a dedicated OS thread recreates the
   `spawn_blocking` bridge pattern with additional ceremony.

2. **Incompatible with multi-threaded daemon.** Futures spawned inside
   `tokio_uring::start` are `!Send` because they hold references to
   thread-local ring state. They cannot be `await`ed from a
   multi-threaded tokio worker without `LocalSet` and manual driving.
   The daemon's `tokio::spawn` tasks are explicitly `Send`.

3. **Buffer ownership model.** `tokio-uring` uses `BufResult<T, B>`
   which returns the buffer alongside the result after each operation.
   Every call site must surrender and reclaim buffers around each
   `.await`. This would force a rewrite of every `fast_io::io_uring`
   consumer - `file_writer.rs`, `file_reader.rs`, `disk_batch.rs`,
   `shared_ring.rs`, `socket_writer.rs`, `socket_reader.rs`.

4. **Missing features.** `tokio-uring` (as of 0.5.x) does not expose:
   - `IORING_REGISTER_BUFFERS` - pre-registered buffer pools.
   - `IORING_SETUP_SQPOLL` - kernel-side SQ polling.
   - `IORING_REGISTER_PBUF_RING` - provided buffer rings (5.19+).
   - `IORING_REGISTER_FILES` - fixed-fd registration.
   - `IORING_OP_LINKAT` / `IORING_OP_RENAMEAT` - atomic rename/link.
   - `IORING_OP_SEND_ZC` - zero-copy socket send.

   oc-rsync's `fast_io::io_uring` already wires all of these through
   dedicated modules (`registered_buffers.rs`, `config.rs`,
   `buffer_ring.rs`, `linkat.rs`, `renameat2.rs`). Adopting
   `tokio-uring` would regress every one of these features.

5. **Maintenance and stability.** `tokio-uring` is not part of the main
   tokio workspace. Its API surface is experimental and has undergone
   breaking changes between minor versions. The raw `io_uring` crate
   (v0.7.x) mirrors the kernel UAPI directly and is more stable.

**Assessment**: `tokio-uring` is not suitable for oc-rsync. The
single-threaded constraint conflicts with the multi-threaded daemon.
The feature set is a strict subset of what `fast_io::io_uring` already
provides. The buffer ownership model would require extensive API
rewrites for no measurable benefit.

### 3.6 Migration Path

**Issue**: If async io_uring integration were desired in the future,
what incremental migration would preserve the existing synchronous path
while adding async capabilities?

**Phased approach** (for reference, not recommended):

**Phase 1: AsyncFd wrapper (low risk).** Wrap the io_uring ring fd in
`tokio::io::unix::AsyncFd` for completion notification. Submission
remains synchronous (`ring.submit()`), but the caller `await`s CQ
readiness instead of blocking on `submit_and_wait`. This avoids the
buffer ownership problem because submission and completion happen in
the same task, but requires careful cancellation handling.

```text
ring.submit()?;
loop {
    let guard = async_fd.readable().await?;
    if ring.completion().next().is_some() {
        // process CQE
        break;
    }
    guard.clear_ready();
}
```

**Phase 2: Async writer/reader wrappers.** Build async versions of
`IoUringWriter` and `IoUringReader` that wrap Phase 1. Expose them
behind a trait so callers can select sync or async at construction
time. The disk-commit thread continues using the sync path; daemon
socket I/O optionally uses the async path.

**Phase 3: Completion-driven pipeline.** Replace the synchronous
disk-commit thread with an async task that processes `FileMessage`
items from a tokio channel. The task submits io_uring SQEs and
`await`s completions through the `AsyncFd` wrapper. This would
unify the daemon and transfer I/O model but is a large refactor.

**Assessment**: None of these phases are necessary or beneficial for
oc-rsync's current architecture. The synchronous `submit_and_wait`
model on dedicated threads (disk-commit, rayon workers) is both
simpler and sufficient. The `spawn_blocking` bridge handles the
async-to-sync boundary cleanly. The migration path is documented here
for completeness.

## 4. Decision Matrix

| Criterion | Sync io_uring (current) | tokio-uring | Hybrid (AsyncFd) |
|-----------|------------------------|-------------|-------------------|
| **Daemon compat** | Via spawn_blocking | Single-threaded conflict | Compatible |
| **Buffer safety** | submit_and_wait guarantees | Leak-on-cancel | Cancellation risk |
| **Feature parity** | Full (reg bufs, SQPOLL, PBUF_RING, linkat, renameat2) | Subset only | Full |
| **Code churn** | Zero | Rewrite all fast_io consumers | Moderate |
| **CLI compat** | Direct call, no runtime | Needs tokio runtime | Needs tokio runtime |
| **Disk I/O perf** | Optimal (dedicated thread, batched SQEs) | No benefit over sync | Marginal |
| **Socket I/O perf** | Good (POLLOUT gating) | Theoretical improvement | Marginal |
| **Maintenance** | Stable API (io_uring crate) | Experimental API | Moderate |
| **Complexity** | Low | High | Medium |
| **Risk** | None (proven) | High (regression, feature loss) | Medium (cancellation) |

## 5. Recommendation

**Stay on native `fast_io::io_uring` with synchronous submission.**

Drive io_uring from sync code: the disk-commit thread owns an
`IoUringDiskBatch` for file writes, rayon workers use per-file
`IoUringReader`/`IoUringWriter` instances, and `SharedRing` co-locates
reader and writer fds on a single ring for bidirectional sessions. When
invoked from the async daemon, bridge through
`tokio::task::spawn_blocking` per the design in #1751.

**Rationale:**

1. **No conflict exists.** io_uring submission already runs on dedicated
   non-tokio threads. The `spawn_blocking` bridge cleanly separates
   async task scheduling from synchronous I/O submission.

2. **Feature preservation.** The current `fast_io::io_uring` module
   wires SQPOLL, registered buffers, PBUF_RING, fixed-fd registration,
   `IORING_OP_LINKAT`, `IORING_OP_RENAMEAT`, and zero-copy send. No
   alternative path preserves all of these without regression.

3. **Buffer safety.** Synchronous `submit_and_wait` guarantees that
   registered buffers outlive kernel operations. Async alternatives
   introduce cancellation-safety requirements that add complexity
   without benefit for batch-oriented disk I/O.

4. **SQPOLL isolation.** SQPOLL is disabled by default and only
   relevant for latency-sensitive NVMe workloads. The one-kernel-thread
   overhead does not conflict with tokio when rings are pooled per
   session (#1409).

5. **`tokio-uring` is unsuitable.** Its single-threaded runtime
   conflicts with the multi-threaded daemon, its feature set is a
   strict subset, and adoption would require rewriting all `fast_io`
   consumers for no measurable gain.

6. **CLI unaffected.** The CLI path has no tokio runtime and calls
   `fast_io::io_uring` directly from rayon workers. This decision
   preserves that zero-overhead path.

This decision is wire-compatible-neutral and platform-neutral: non-Linux
targets continue to use the synchronous `fast_io` fallbacks
(`io_uring_stub.rs`) unchanged. The IOCP path on Windows
(`crates/fast_io/src/iocp/`) follows the same sync-on-dedicated-thread
pattern and is unaffected.
