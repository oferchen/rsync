# macOS kqueue-based fast I/O path (#1385)

Tracking issue: oc-rsync task #1385.

Design only. No code lands with this note. The intent is to map kqueue
onto the existing `AsyncFileWriter` trait shape (#1655) and to record
where it does and does not displace the libdispatch-based `dispatch_io`
evaluation (#1653) and the Linux io_uring path.

## 1. Current macOS I/O paths in `crates/fast_io/src/`

The macOS surface today is whole-file copy plus synchronous fall-throughs
for everything else. There is no asynchronous disk path on macOS.

- **`platform_copy/dispatch.rs`**. Whole-file copy fan-out for the
  local-copy executor:
  - `clonefile_impl` (line 151) wraps `libc::clonefile(src, dst, 0)`
    for instant APFS CoW clones. Used by the local-copy executor and by
    `try_clonefile`. Fails on cross-device, HFS+, and existing
    destinations.
  - `fcopyfile_impl` (line 186) wraps
    `libc::fcopyfile(src_fd, dst_fd, NULL, COPYFILE_DATA)` for
    kernel-accelerated data-only copy. Fallback when `clonefile` is not
    applicable.
  - `platform_copy_impl` (line 63) chains them as
    `clonefile -> fcopyfile -> std::fs::copy`.
- **`sendfile.rs`**. `send_file_to_fd` (lines 146-160) is Linux-only
  via `try_sendfile`; the `#[cfg(all(unix, not(target_os = "linux")))]`
  arm at line 157 redirects macOS straight into the synchronous
  `read`/`write` loop in `copy_via_fd_write`. There is no macOS
  equivalent fast path for socket egress today.
- **`copy_file_range.rs`** and **`splice.rs`**. Linux-only by surface;
  on macOS they are not compiled in.
- **`io_uring/`** and **`io_uring_stub.rs`**. Linux-only batched disk
  writer. macOS uses the stub, which exposes `try_new -> None`.
- **`iocp/`** and **`iocp_stub.rs`**. Windows-only batched disk writer;
  macOS uses the stub.
- **Disk-commit on macOS**. `crates/transfer/src/disk_commit/writer.rs`
  routes to the `Buffered` arm: a 256 KB reusable buffer over
  `std::fs::File`, mirroring upstream's `wf_writeBuf`. Every chunk
  blocks the disk-commit thread inside `write(2)`.
- **`mmap_reader.rs`**. Cross-platform mmap-based reader; orthogonal to
  the write path and excluded from the asynchronous data path by the
  basis-file policy (`docs/design/basis-file-io-policy.md`, F2).

The receiver-side delta-apply hot path therefore has no macOS-specific
acceleration. The only macOS fast paths in tree are whole-file local
copy (`clonefile`, `fcopyfile`).

## 2. kqueue scope

`kqueue(2)` plus `kevent(2)` is macOS's native event-notification
primitive. Relevant filters:

- **`EVFILT_READ`** / **`EVFILT_WRITE`**. Readiness for file
  descriptors (regular files, pipes, sockets, terminals).
  `EV_CLEAR` makes the event edge-triggered, matching `EPOLLET` on
  Linux. `EV_ONESHOT` removes the registration after one fire.
- **`EVFILT_VNODE`**. File-system change notifications:
  `NOTE_DELETE`, `NOTE_WRITE`, `NOTE_EXTEND`, `NOTE_ATTRIB`,
  `NOTE_RENAME`. Useful for watch-style consumers, not for the rsync
  data path.
- **`EVFILT_TIMER`**. One-shot or periodic timer events with
  millisecond, microsecond, or nanosecond resolution. Replaces ad-hoc
  `select` timeouts.
- **`EVFILT_SIGNAL`**, **`EVFILT_PROC`**, **`EVFILT_USER`**. Signal
  delivery, child-process state, and user-triggered wakeups.
  `EVFILT_USER` doubles as a cross-thread wakeup channel.

Critical property for this design: **kqueue is a notification surface,
not an I/O submission surface**. Userspace still calls `pwrite`,
`writev`, `read`, `recv`, etc.; kqueue only tells you when an fd is
ready. Submissions remain syscalls. There is no kqueue analogue to
io_uring's SQE batch submission or `IORING_OP_WRITE_FIXED`.

## 3. Where kqueue could replace polling

Three sites in oc-rsync currently rely on synchronous waits or coarse
polls and could plausibly use kqueue.

### 3.1 Disk-commit thread (primary target)

The disk-commit thread is the primary motivation for #1385. Today it
calls blocking `write(2)` on the buffered backend. Replacing the
buffered arm with a `KqueueDiskBatch` lets the thread:

1. Open the destination fd with `O_NONBLOCK`.
2. Register the fd with kqueue under `EVFILT_WRITE | EV_CLEAR`.
3. Issue `pwrite(2)` per chunk; if the kernel writeback queue is full,
   `pwrite` returns `EAGAIN`, the chunk stays pending, and the thread
   parks on `kevent(2)` until the fd reports ready.

This overlaps page-cache absorption with the next chunk's preparation
on the rayon producer side. It does **not** eliminate the per-chunk
syscall (kqueue is not io_uring).

### 3.2 Daemon socket I/O

`crates/daemon/src/listener.rs` accepts connections on a TCP listener
and hands each off to a worker. The accept loop today uses a blocking
`accept(2)`. Multiplexing several listeners or interleaving accepts
with periodic config-reload checks would benefit from kqueue with
`EVFILT_READ` on the listener fd plus `EVFILT_TIMER` for the reload
poll. This matches the
`feedback_multiplex_preference.md` user direction (prefer multiplex
I/O for concurrency where the protocol allows).

Per-connection sockets, however, run inside the existing transport
worker with synchronous reads on the rayon-managed thread. There is
no win from kqueue-multiplexing them: each worker handles exactly one
connection, and the read syscall is unavoidable.

### 3.3 Directory walk

Generator-side directory traversal currently uses synchronous
`readdir(3)`. kqueue does not help here: `readdir` is a single
syscall against a directory fd, not a poll on readiness. The
`EVFILT_VNODE` filter would only matter for a watch-style consumer
(file-system change monitor), which oc-rsync does not implement and
does not need for transfer correctness. **Conclusion**: directory walk
is out of scope; mention only to record the rejection.

### 3.4 SSH stdio

The SSH transport's stdio pipe path was audited in
`docs/audits/ssh-socketpair-vs-pipes.md` (#1938) and stays on
synchronous reads. kqueue does not change that calculus.

## 4. Comparison to Linux io_uring

kqueue and io_uring solve different problems. They are not feature
parallels.

| Axis | Linux io_uring | macOS kqueue |
|------|---------------|---------------|
| Surface | I/O submission and completion | Event notification only |
| Submission cost | Single `io_uring_enter` for N SQEs (or 0 with SQPOLL) | One syscall per `pwrite`/`writev`/`recv` |
| Completion cost | One reap for N CQEs | One `kevent` reap for N readiness events |
| Zero-copy | `IORING_OP_WRITE_FIXED` over registered buffers | None; user-to-kernel copy on every `pwrite` |
| Polled mode | `IORING_SETUP_SQPOLL` keeps a kernel thread submitting | No equivalent |
| Fixed buffers | `IORING_REGISTER_BUFFERS` removes per-SQE pinning | No equivalent |
| Linked operations | `IOSQE_IO_LINK` chains SQEs | None; userspace chains on its own |
| Batched writes from one fd | N writes in one syscall | N writes is N syscalls |

Practical implication for oc-rsync: on Linux the headline win is
syscall elimination on submission. On macOS the headline win is
**writeback overlap** - the `pwrite` returns when the page cache
accepts the data, not when the device drains it - plus syscall
reduction on the readiness path. For chunks larger than the page
granule this amortizes well; for delta-heavy workloads with many
small chunks the ratio is less favourable.

## 5. Recommendation: kqueue vs. dispatch_io for the AsyncFileWriter trait

`AsyncFileWriter` in this project is the `FileWriter` trait at
`crates/fast_io/src/traits.rs:38-49`: `Write` plus `bytes_written`,
`sync`, and an advisory `preallocate`. Implementations slot in via
`FileWriterFactory::create` / `create_with_size`. Task #1655
(completed) settled this shape; the question is which macOS backend
fits it.

### 5.1 Where kqueue fits

kqueue is the right backend for `AsyncFileWriter` on macOS. It maps
cleanly onto the trait without surrendering ownership of the
disk-commit thread:

- `Write::write` buffers internally and flushes via `pwrite` plus a
  kqueue readiness wait when the buffer fills. Mirrors the
  `IoUringDiskBatch::write_data` pattern at
  `crates/fast_io/src/io_uring/disk_batch.rs`.
- `Write::flush` drains pending `pwrite` chunks, including any
  `EAGAIN` resubmits.
- `FileWriter::sync` calls `fcntl(fd, F_FULLFSYNC)` (or `fsync` if
  `F_FULLFSYNC` returns `ENOTSUP`, e.g. on tmpfs). `F_FULLFSYNC` is
  the only macOS primitive that guarantees data hits stable storage
  on Apple SSDs.
- `FileWriter::preallocate` uses `fcntl(fd, F_PREALLOCATE)` with
  `F_ALLOCATEALL | F_ALLOCATECONTIG`, falling back to `ftruncate`.
- `FileWriterFactory::create` and `create_with_size` return a
  `KqueueOrStdWriter` whose variant is decided by a one-time
  `OnceLock` probe (open a kqueue, register an EOF event on a
  self-pipe, close), matching `is_io_uring_available()` at
  `crates/fast_io/src/io_uring/mod.rs`.

The kqueue backend keeps a single owner (the disk-commit thread)
and a single fd lifetime, exactly like `IoUringDiskBatch` and
`IocpDiskBatch`. Rayon workers do not interact with it. This
preserves the composition invariant from
`docs/design/io-uring-rayon-composition.md`.

### 5.2 Where dispatch_io does not fit

`dispatch_io` was evaluated in #1653 and rejected. The conclusion
stands and applies to the trait shape:

- `dispatch_io` owns the I/O lifecycle, the queue topology, and the
  buffer copies. Wrapping it in `AsyncFileWriter` requires either
  ceding ownership of the disk-commit thread to libdispatch
  (incompatible with the rayon composition rules) or maintaining a
  shadow buffer that mirrors libdispatch's internal state
  (doubles the bookkeeping for no measurable throughput win in the
  evaluation).
- Per-chunk `dispatch_io_write` invocations queue against a serial
  dispatch queue. The completion handler runs on a libdispatch
  worker thread, which forces a hand-back to the disk-commit thread
  through a channel - exactly the sort of round-trip the trait was
  designed to avoid.
- libdispatch insists on its own buffer ownership model
  (`dispatch_data_t`). The buffer-pool integration in
  `crates/fast_io/src/parallel.rs` and the
  `BufferPool` ownership chain do not compose with that.

`dispatch_io` would only fit if the project adopted libdispatch as
the runtime for the disk-commit phase. That is a much larger change
and is not on the roadmap.

### 5.3 Recommendation

Implement the macOS arm of `AsyncFileWriter` as `KqueueDiskBatch`
sitting behind a `KqueueWriterFactory`. Treat `dispatch_io` as
permanently rejected for this trait per #1653. Treat `F_NOCACHE`
plus `writev` (#1657) as a complement to kqueue - the same fd opens
with `O_NONBLOCK | F_NOCACHE`, registers the same kqueue event, and
uses the same `pwrite` / `kevent` loop - not as an alternative.

Phasing follows the existing IOCP and io_uring playbooks: add a
`KqueuePolicy { Auto, Enabled, Disabled }` enum next to
`IoUringPolicy` and `IocpPolicy`; cascade `try_create_*_batch` in
`disk_thread_main`; keep the buffered writer as the universal
fallback. CLI surface is `--kqueue` / `--no-kqueue`, gated to macOS.

## 6. References

- `crates/fast_io/src/traits.rs:38-49` - `FileWriter` trait surface.
- `crates/fast_io/src/platform_copy/dispatch.rs:63` - macOS
  whole-file copy chain (`platform_copy_impl`).
- `crates/fast_io/src/sendfile.rs:157-160` - macOS sendfile fallback.
- `crates/fast_io/src/io_uring/disk_batch.rs` - Linux batched writer
  whose public shape the kqueue backend mirrors.
- `crates/fast_io/src/iocp/disk_batch.rs` - Windows batched writer,
  same shape.
- `crates/transfer/src/disk_commit/writer.rs` - dispatch site for
  the per-platform writer variants.
- `docs/design/io-uring-rayon-composition.md` - rayon composition
  invariants (single owner per asynchronous backend).
- `docs/design/basis-file-io-policy.md` - mmap exclusion rule.
- `docs/design/macos-fnocache-writev-fallback.md` - `F_NOCACHE`
  complement (#1657).
- `docs/audits/ssh-socketpair-vs-pipes.md` (#1938) - SSH stdio
  rationale; kqueue does not apply.
