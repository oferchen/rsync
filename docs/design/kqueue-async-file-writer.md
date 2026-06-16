# KQ-2: kqueue-driven async file writer for the receiver disk-commit path

Tracking issue: KQ parent (KQ-3 ships the implementation; KQ-7 benches it;
KQ-8 decides default-on). Audit sibling: `docs/design/kqueue-pipeline-audit.md`.
Foundational primitive: `crates/fast_io/src/kqueue/mod.rs::KqueueLoop`.
Historical context: `docs/design/macos-kqueue-fast-io.md`.

This document specifies the macOS-only `KqueueAsyncFileWriter` that
replaces the synchronous `ReusableBufWriter` arm of `disk_commit::writer::Writer`
when the `kqueue-disk-writer` feature flag is on. Upstream rsync uses
synchronous `write(2)` on macOS; this proposal does not change wire bytes,
only the syscall pattern on the receiver disk-commit thread.

## Goals

- Park on `EVFILT_WRITE` instead of blocking the disk-commit thread when
  the kernel reports backpressure (`EAGAIN` from `pwrite(2)` on the
  nonblocking fd).
- Reuse the existing `BufferPool` ownership model so the writer never
  allocates per-chunk.
- Match `ReusableBufWriter`'s byte-for-byte output (same vectored
  flush ordering, same final-fsync semantics from `DiskCommitConfig`).

Non-goals: pipelining writes across multiple files, dispatch_io
integration (tracked by GCD-3), data-path correctness changes.

## Public API

Lives in `crates/fast_io/src/kqueue/async_writer.rs` behind
`#[cfg(target_os = "macos")]` plus the `kqueue-disk-writer` feature.

```rust
use std::fs::File;
use std::io;
use std::sync::Arc;

use crate::buffer_pool::PooledBuffer;
use crate::kqueue::KqueueLoop;

/// Async file writer backed by `KqueueLoop` `EVFILT_WRITE` parking.
///
/// One instance per disk-commit thread; not `Sync`. The `KqueueLoop`
/// must be the same instance that owns the bandwidth `EVFILT_TIMER`
/// registration if both are active on the same thread.
pub struct KqueueAsyncFileWriter {
    file: File,                 // O_NONBLOCK set at construction
    kq: Arc<KqueueLoop>,        // shared with peer subsystems on the same thread
    buf: PooledBuffer,          // leased from the BufferPool; never reallocated
    offset: u64,                // next pwrite offset
    registered: bool,           // EVFILT_WRITE currently armed
    user_data: u64,             // tag delivered back via KEvent::user_data
}

impl KqueueAsyncFileWriter {
    /// Construct a writer over an open file. Sets `O_NONBLOCK` on the fd
    /// and leases a buffer from the pool. The `kq` loop is reused for
    /// readiness waits.
    ///
    /// # Errors
    /// Propagates `fcntl(F_SETFL)` failures and pool exhaustion.
    pub fn new(file: File, kq: Arc<KqueueLoop>, user_data: u64) -> io::Result<Self>;

    /// Total bytes successfully written so far. Lets the disk-commit
    /// thread book-keep without re-statting.
    #[must_use]
    pub fn bytes_written(&self) -> u64;

    /// Force a buffered-data flush. Returns once the buffer is empty.
    ///
    /// # Errors
    /// Propagates underlying I/O errors. `WouldBlock` is converted into
    /// a `kevent()` park and retried; the caller never observes EAGAIN.
    pub fn flush(&mut self) -> io::Result<()>;

    /// Finalize: flush any pending bytes, deregister the write event,
    /// and surrender the buffer back to the pool. Returns the underlying
    /// `File` for the disk-commit thread to fsync/rename.
    ///
    /// # Errors
    /// Propagates flush errors. The buffer is always returned to the pool
    /// regardless of error.
    pub fn into_inner(self) -> io::Result<File>;
}

impl io::Write for KqueueAsyncFileWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize>;
    fn flush(&mut self) -> io::Result<()>;
}
```

Disk-commit consumers see only `io::Write`; the existing `Writer` enum in
`crates/transfer/src/disk_commit/writer.rs` gains a `KqueueAsync` arm
gated by the same feature flag.

## Behavior

### Buffer pool integration

`PooledBuffer` (RAII handle from `fast_io::buffer_pool`) holds a 256 KB
slab matching `ReusableBufWriter`'s working set. Construction leases it;
`into_inner` drops it; `Drop` on the writer surrenders it on the panic
path. There is no per-write allocation. The pool size cap is the binding
constraint on concurrent writers (already enforced by BufferPool
tests under `EnvGuard`).

### Submission and parking on EAGAIN

`write` appends into the buffer until full, then issues `pwrite(2)` at
`self.offset` with the buffer contents. The fd is `O_NONBLOCK`; outcomes:

1. `Ok(n)` with `n == buffer.len()`: advance `self.offset`, clear the
   buffer, return.
2. `Ok(n)` with `0 < n < len`: advance `self.offset`, retain the
   unflushed tail, loop without parking.
3. `Err(EAGAIN)` or `Err(EWOULDBLOCK)`: arm `EVFILT_WRITE` on the file fd
   via `kq.submit_write(self.file.as_raw_fd(), self.user_data)`, call
   `kq.wait(timeout)` with the disk-commit thread's configured timeout
   (`DiskCommitConfig::write_timeout`), then retry the `pwrite`. Treat
   `EV_EOF` or `EV_ERROR` as fatal.
4. `Err(EINTR)`: retry the syscall without consulting `kq` (matches
   `KqueueLoop::wait`'s own EINTR handling).
5. Other `Err`: bubble up unchanged, matching `ReusableBufWriter` error
   discipline.

Vectored flushes (basis-data plus delta-tail) reuse the same loop with
two `IoSlice` instances and `pwritev(2)`. The retry path is identical.

### Completion and re-arm

`EVFILT_WRITE` with `EV_CLEAR` is edge-triggered (mirrors `EPOLLET`).
`KqueueLoop::submit_write` already passes the `EV_CLEAR` flag. After a
parked write succeeds the registration stays armed for the next EAGAIN;
`into_inner` calls `kq.remove(fd, KEventFilter::Write)` (idempotent on
`ENOENT`) before returning the file.

### Fsync and partial-write recovery

`DiskCommitConfig::fsync` runs after `into_inner`, on the synchronous
file handle returned from the writer. The async path never calls
`fsync(2)` itself - it would defeat the parking model. Partial writes
are recovered by the retry loop; the disk-commit thread sees only
"complete" or "error", matching the existing `Writer::Buffered`
contract.

## Concurrency rules

- One `KqueueAsyncFileWriter` per disk-commit worker thread. The
  `KqueueLoop` carries `Send + !Sync`; sharing requires external
  synchronization, which is incompatible with the hot path.
- The same loop may co-host `EVFILT_TIMER` (bandwidth) registrations
  per KQ-S.4 / PR #5818, distinguished by `user_data` tags.
- `user_data` allocation: callers pass a unique `u64` per writer so
  multiple writers on the same loop can be demultiplexed. Disk-commit
  uses `file_index << 1 | role_bit` to stay within the tag space.

## Wire-byte parity

The kqueue path issues identical syscalls in identical order to the
synchronous path, except for `kevent(2)` parks on EAGAIN. Output bytes
on disk are unchanged. A nextest parity cell (KQ-3 follow-up) hashes
the post-transfer file against the synchronous baseline.

## Test plan

- Unit: pipe-pair test parallel to `kqueue/mod.rs::read_event_fires_on_pipe_write`
  that proves an EAGAIN-induced park wakes when the reader drains.
- Unit: `into_inner` returns the buffer to the pool even when `flush`
  errors (panic-safety via explicit `Drop` test).
- Property: random write sizes (1 B to 1 MB) round-trip byte-identical
  to `ReusableBufWriter`.
- Integration: 1 GB single-file receive transfer under the
  `kqueue-disk-writer` feature, hashed against the synchronous path.
- Bench cell (KQ-7): disk-commit throughput on APFS, 100 K-file and
  10 GB workloads.

## Reference

- `kqueue(2)` / `kevent(2)`: Apple Open Source `xnu/bsd/sys/event.h`,
  available locally as `man 2 kqueue` on macOS. Documents the
  `EVFILT_WRITE` semantics, `EV_CLEAR` edge-trigger model, `EV_EOF` /
  `EV_ERROR` handling, and `timespec` timeout contract that this design
  relies on.
- Upstream rsync 3.4.1 macOS write path: `target/interop/upstream-src/rsync-3.4.1/io.c`,
  function `writefd_unbuffered()` and its `pwrite` siblings - confirms
  upstream uses synchronous blocking writes on macOS with no kqueue
  involvement. Wire-protocol fidelity is preserved by definition.
