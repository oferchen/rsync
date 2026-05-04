# FreeBSD/NetBSD POSIX AIO (`aio_*`) audit

Tracking issue: oc-rsync task #1654. Branch: `docs/bsd-aio-audit`. Related task
#1655 (`AsyncFileWriter` trait).

## Overview & decision question

This audit evaluates whether POSIX asynchronous I/O (`aio_read(2)`,
`aio_write(2)`, `aio_suspend(2)`, `aio_return(2)`, `aio_error(2)`,
`aio_cancel(2)`) - in particular the FreeBSD `aio(4)` kernel implementation and
its NetBSD counterpart - is a viable async-I/O backend for oc-rsync's writer
hot path on BSD targets, alongside Linux io_uring
(`crates/fast_io/src/io_uring/`) and Windows IOCP
(`crates/fast_io/src/iocp/`).

The decision question: should oc-rsync introduce a `target_os = "freebsd"` (and
optionally `target_os = "netbsd"`) async writer backend behind a feature gate
that mirrors the existing `io_uring` and `iocp` integration patterns in
`crates/fast_io/src/lib.rs:111-127`?

## Upstream evidence

A recursive ripgrep for `\baio_(read|write|suspend|return|error|cancel)\b`
under `target/interop/upstream-src/rsync-3.4.1/` returns zero matches. The
single case-insensitive `aio` hit is the word `destintaion` (sic) in
`support/atomic-rsync:110`. Upstream rsync 3.4.1 therefore performs all file
I/O via blocking `read(2)`/`write(2)` and does not depend on any POSIX AIO
feature. A BSD AIO backend in oc-rsync is an additive performance
optimisation, not a wire-protocol concern.

## POSIX AIO API summary

The relevant POSIX.1-2017 surface (also documented in FreeBSD/NetBSD man pages
`aio(4)`, `aio_read(2)`, `aio_write(2)`, `aio_suspend(2)`, `aio_return(2)`,
`aio_error(2)`, `aio_cancel(2)`):

| Call | Purpose | Notes |
|------|---------|-------|
| `aio_read(struct aiocb *)` | Submit a positioned read | Operates on a registered control block; non-blocking submission. |
| `aio_write(struct aiocb *)` | Submit a positioned write | Same control-block convention as `aio_read`. |
| `aio_suspend(const struct aiocb *const list[], int n, const struct timespec *)` | Block until any of `n` ops completes or timeout expires | Useful for batch reaping similar to `io_uring_enter` waits. |
| `aio_return(struct aiocb *)` | Retrieve the byte count or `-1` after completion | Must be called exactly once per submitted op. |
| `aio_error(struct aiocb *)` | Poll error status (`EINPROGRESS` while pending) | Pairs with `aio_return` once the op is done. |
| `aio_cancel(int fd, struct aiocb *)` | Cancel pending ops on `fd` (or the specific op when non-NULL) | Returns `AIO_CANCELED`, `AIO_NOTCANCELED`, or `AIO_ALLDONE`. |
| `lio_listio(int mode, struct aiocb *const list[], int n, struct sigevent *)` | Submit a vector of mixed reads/writes | `LIO_WAIT` blocks; `LIO_NOWAIT` returns immediately and signals on completion. |

Completion notification is configured per `aiocb` via
`aiocb.aio_sigevent.sigev_notify`:

- `SIGEV_NONE` - silent completion; caller must poll with `aio_error` /
  `aio_suspend`.
- `SIGEV_SIGNAL` - kernel posts a real-time signal when the op completes. Hard
  to integrate with a Rust application that does not own the signal mask.
- `SIGEV_THREAD` - libc spawns a callback thread per completion. On glibc this
  is implemented via the same thread pool that emulates AIO; on FreeBSD it is
  a real kernel-driven mechanism but still allocates a thread.
- FreeBSD `SIGEV_KEVENT` (extension) - the kernel posts a `kevent(2)` to a
  kqueue; the caller drains completions via `kevent()` with filter
  `EVFILT_AIO`. This is the only mechanism that composes cleanly with a
  reactor without per-op thread or signal cost.

## FreeBSD/NetBSD extensions

Per `aio(4)` on FreeBSD 13/14 and NetBSD 9/10:

- `aio_readv(2)` and `aio_writev(2)` (FreeBSD-only) accept an `iovec`-style
  vector for true scatter/gather without coalescing on the caller's side.
  These are the closest equivalent to `IORING_OP_READV` / `IORING_OP_WRITEV`.
- `aio_mlock(2)` (FreeBSD-only) pins the destination buffer pages, similar in
  spirit to `IORING_REGISTER_BUFFERS`. Reduces per-op page-fault and pin
  overhead for very large transfers.
- `aio_fsync(int op, struct aiocb *)` (POSIX) submits an asynchronous fsync.
  `op` may be `O_SYNC` for full data + metadata fsync, or `O_DSYNC` (FreeBSD
  also accepts `O_FSYNC`) for data-only sync. Lets the writer overlap fsync
  with subsequent file work.
- `EVFILT_AIO` (FreeBSD `kqueue(2)`) attaches a `kevent` to a submitted
  `aiocb`. The corresponding `aio_sigevent.sigev_notify_kqueue` /
  `sigev_notify_kevent_flags` fields are filled in before submission. This is
  the FreeBSD-recommended completion model for high-throughput servers and
  the closest behavioural match to io_uring's CQE drain loop. NetBSD does not
  expose `EVFILT_AIO`; on NetBSD the only practical reactor-friendly option
  is `SIGEV_NONE` plus periodic `aio_suspend`.
- `aiocb.aio_offset` is a `off_t` (signed 64-bit on LP64 BSDs); `aio_nbytes` is
  `size_t`. The kernel preserves submission order per fd only when explicitly
  requested via `lio_listio` with `LIO_NOWAIT`; otherwise submitted ops may
  complete in any order, which oc-rsync's positioned writes already tolerate.

Tunables (sysctl `vfs.aio.*` on FreeBSD): `max_aio_queue`, `max_aio_per_proc`,
`max_aio_procs`, and `aio_unsafe_warningcnt`. NetBSD exposes
`kern.aio_listio_max`, `kern.aio_max`, and `kern.aio_per_process_max`. The
kernel can refuse submission with `EAGAIN` when limits are hit; the writer
must fall back to `pwrite(2)` in that case, mirroring how
`fast_io::io_uring` falls back to `StdFileWriter`.

## oc-rsync I/O hot paths and the `AsyncFileWriter` trait

The current writer dispatch is concentrated in two files:

- `crates/fast_io/src/lib.rs:111-127` selects io_uring on
  `target_os = "linux"` with `feature = "io_uring"` and IOCP on
  `target_os = "windows"` with `feature = "iocp"`. Each non-supported target
  falls through to the stub modules `io_uring_stub.rs` / `iocp_stub.rs`.
- `crates/fast_io/src/io_uring/mod.rs:140-187` (`writer_from_file`) and
  `crates/fast_io/src/io_uring/mod.rs:199-233` (`reader_from_path`) are the
  primary integration points used by:
  - `crates/transfer/src/transfer_ops/response.rs:108` for the receiver
    write path during whole-file transfer (open/seek/append, then bulk
    writes through `IoUringOrStdWriter`).
  - `crates/transfer/src/generator/mod.rs:728` for the sender read path
    during basis-file scanning (large source reads via `IoUringOrStdReader`).

The `FileWriter` trait (`crates/fast_io/src/traits.rs:38-49`) defines the
abstraction every backend must satisfy: `Write` super-trait, `bytes_written`,
`sync`, optional `preallocate`. Today there is no separate `AsyncFileWriter`
trait; #1655 contemplates introducing one to express overlapped submission
semantics that `Write::write` cannot express (submit-now-complete-later, batch
flush, completion barriers). A BSD AIO backend would slot in as a third
concrete implementor of that trait, exactly parallel to `IoUringWriter`
(`crates/fast_io/src/io_uring/file_writer.rs`) and `IocpWriter`
(`crates/fast_io/src/iocp/file_writer.rs`).

Code locations that would gain a BSD AIO backend (gated on
`target_os = "freebsd"` / `target_os = "netbsd"`):

- `crates/fast_io/src/lib.rs` - new `pub mod bsd_aio;` cfg-gated alongside
  `io_uring` and `iocp`, with a stub fallback module for other targets.
- `crates/fast_io/src/lib.rs::platform_io_capabilities` (lines ~327-365) -
  add a `"posix_aio"` capability entry on FreeBSD/NetBSD when the runtime
  probe succeeds.
- `crates/fast_io/src/lib.rs::IoUringPolicy` siblings - introduce
  `BsdAioPolicy { Auto, Enabled, Disabled }` with the same Auto-detect and
  fallback contract.
- `crates/transfer/src/transfer_ops/response.rs:108` - call the
  policy-aware factory rather than hardcoding `fast_io::writer_from_file`,
  so the dispatch picks the right backend per platform.
- `crates/transfer/src/generator/mod.rs:728` - same dispatch change for the
  read side.
- `crates/engine/src/async_io/mod.rs:1-35` - `async_io::AsyncFileCopier`
  could optionally back its tokio-based path with `aio_*` on BSD instead of
  the default thread-pool block_in_place pattern. This is secondary - the
  primary win is on the synchronous writer used by the receiver.

## Comparison vs io_uring / IOCP / dispatch_io

| Property | Linux io_uring | Windows IOCP | macOS dispatch_io | FreeBSD `aio(4)` + EVFILT_AIO | NetBSD `aio(4)` |
|----------|---------------|--------------|-------------------|-------------------------------|-----------------|
| Submission cost | Single shared SQ ring, batched `io_uring_enter` | Per-handle association, `WriteFile` with OVERLAPPED | Internal GCD queue | One syscall per `aio_write` (no batching equivalent to a ring) | Same as FreeBSD |
| Completion drain | CQE ring | `GetQueuedCompletionStatusEx` | Block-based callbacks | `kevent()` with `EVFILT_AIO` | `aio_suspend(2)` only |
| Vectored ops | `IORING_OP_READV` / `IORING_OP_WRITEV` | `WriteFileGather` / `ReadFileScatter` | `dispatch_io_write` with chained `dispatch_data_t` | `aio_writev(2)` / `aio_readv(2)` (FreeBSD only) | None (manual coalescing) |
| Buffer pinning | `IORING_REGISTER_BUFFERS` | None native (rely on memory locking) | Internal | `aio_mlock(2)` (FreeBSD only) | None |
| Cancellation | `IORING_OP_ASYNC_CANCEL` | `CancelIoEx` | `dispatch_io_close` w/ `DISPATCH_IO_STOP` | `aio_cancel(2)` | `aio_cancel(2)` |
| Async fsync | `IORING_OP_FSYNC` / `_FDATASYNC` | `FlushFileBuffers` (synchronous) | `dispatch_io_barrier` | `aio_fsync(2)` | `aio_fsync(2)` |
| Reactor integration | epoll-readable ring fd | Completion port | GCD owns the queue | kqueue `EVFILT_AIO` | None reactor-friendly |
| Per-op overhead at scale | ~50-200 ns | ~1-5 us | ~1 us | ~3-10 us (one syscall + signal/kevent post) | Similar |

io_uring remains the clear performance leader; FreeBSD AIO with `EVFILT_AIO`
is the next-best fit because it composes with kqueue (which oc-rsync already
references for socket-option lookups, see
`crates/core/src/client/module_list/socket_options/lookup.rs`). NetBSD's lack
of `EVFILT_AIO` makes the integration significantly less attractive there;
the only completion path is `aio_suspend(2)`, which forces either a dedicated
reaper thread or interleaved submit/wait calls that erase most of the async
benefit.

## macOS POSIX AIO status

macOS retains the `aio_*` symbols and they are documented in `aio(2)` /
`aio_read(2)` / `aio_write(2)` man pages, but Apple has not extended the
implementation in many releases:

- No `EVFILT_AIO` on macOS: `kqueue(2)` ignores AIO completion. Notification
  is restricted to `SIGEV_NONE` (poll) and `SIGEV_SIGNAL`.
- Apple's developer documentation steers all new code toward `dispatch_io_*`
  (Grand Central Dispatch I/O channels), which uses libdispatch's internal
  thread pool and is the only Apple-supported async I/O facility for new
  code. POSIX AIO on macOS is widely treated as deprecated in practice
  (still present, no longer evolving).
- Per-process AIO limits on macOS are conservative (`kern.aiomax = 90`,
  `kern.aioprocmax = 16`) and are not documented as raisable in production.

For the macOS path oc-rsync should evaluate `dispatch_io` separately rather
than try to make POSIX AIO portable to macOS. A future `dispatch-io.md` audit
should pick up that question; this audit treats macOS POSIX AIO as
out-of-scope. No `dispatch-io.md` audit currently exists under
`docs/audits/`.

## Integration sketch

A FreeBSD-first sketch that mirrors the existing io_uring integration:

```
crates/fast_io/src/
  bsd_aio/
    mod.rs            // pub use; cfg-gated
    config.rs         // BsdAioConfig (queue depth, kqueue handle, max in-flight)
    file_writer.rs    // BsdAioWriter implementing FileWriter
    file_reader.rs    // BsdAioReader implementing FileReader
    completion.rs     // EVFILT_AIO drain loop on FreeBSD;
                      // aio_suspend loop on NetBSD
    probe.rs          // runtime detection + fallback
  bsd_aio_stub.rs     // non-BSD stub mirroring io_uring_stub.rs
```

Submission flow for a write batch (FreeBSD):

1. Build `aiocb` for each chunk, populate `aio_fildes`, `aio_offset`,
   `aio_buf`, `aio_nbytes`, set `sigev_notify = SIGEV_KEVENT`,
   `sigev_notify_kqueue = self.kq_fd`, `sigev_value.sival_ptr` to the
   per-chunk completion token.
2. Submit each `aiocb` with `aio_write(2)`. If the queue rejects with
   `EAGAIN`, fall back to a synchronous `pwrite(2)` for that chunk.
3. Track in-flight `aiocb`s in a `SmallVec`-backed table keyed by
   `sival_ptr`.
4. Drain completions with `kevent(kq, NULL, 0, events, n, timeout)` filtered
   on `EVFILT_AIO`. For each completion, `aio_return(2)` retrieves the
   transfer count or error; release the buffer back to the pool.
5. On `flush()`, drain to empty, then optionally submit `aio_fsync(2)` and
   wait for it.

For NetBSD (no `EVFILT_AIO`), the completion module instead polls the
in-flight table with `aio_suspend(2)` on a per-batch basis. This is roughly
equivalent to the io_uring "submit then wait" model but without per-op
batching; throughput is correspondingly lower.

`Drop` for `BsdAioWriter` must call `aio_cancel(fd, NULL)` followed by
`aio_suspend` on any still-pending ops to avoid the kernel writing through a
freed buffer. This mirrors `IoUringWriter`'s drain-on-drop discipline.

## Blockers

1. **NetBSD reactor gap.** Without `EVFILT_AIO`, oc-rsync would have to
   either dedicate a kernel thread to `aio_suspend` (defeating much of the
   async benefit) or block at flush points. Recommendation: implement
   FreeBSD-only first; add NetBSD only if benchmarks show a measurable win
   over `pwrite`.
2. **macOS deprecation in practice.** As discussed above, POSIX AIO on
   macOS is not the right tool. Defer to a separate dispatch_io audit.
3. **glibc thread-pool emulation.** glibc on Linux implements POSIX AIO
   entirely in user space via a thread pool (`librt`); each "async" write
   becomes a `pwrite` on a worker thread. There is no Linux performance
   case for routing through `aio_*` when io_uring is available, and the
   thread pool is well-known to scale poorly. The BSD AIO backend must be
   gated `#[cfg(any(target_os = "freebsd", target_os = "netbsd"))]` and
   never compiled on Linux.
4. **Kernel/version requirements.**
   - FreeBSD: `aio(4)` is in the GENERIC kernel since 11.0; older releases
     required `aio_load="YES"` in `loader.conf`. The runtime probe should
     attempt a no-op `aio_error(2)` against an `EBADF` aiocb and fall back
     if the kernel returns `ENOSYS`.
   - NetBSD: `aio(4)` requires `options AIO` in the kernel config. NetBSD
     7+ ships GENERIC with AIO. Probe via `lio_listio(LIO_NOWAIT, NULL,
     0, NULL)` and check for `ENOSYS`.
5. **Signal-based completion is unsuitable.** `SIGEV_SIGNAL` would require
   oc-rsync to install a real-time signal handler and manage the signal
   mask across threads. This is incompatible with the rest of oc-rsync's
   threading (rayon worker pools, tokio runtimes when the `async` feature
   is on). The integration must use either `SIGEV_NONE`+poll or
   `SIGEV_KEVENT`.
6. **`aio_cancel` is best-effort.** `AIO_NOTCANCELED` may be returned, in
   which case the op is in flight in the kernel and the buffer must remain
   valid until the kernel completes it. The writer's `Drop` impl must
   `aio_suspend` until drain completes, even on early-return error paths.
7. **Per-process queue limits.** FreeBSD's `vfs.aio.max_aio_per_proc`
   defaults to 32 (older releases) or 256 (recent). Saturating the queue
   gives `EAGAIN`; the writer must implement back-pressure rather than
   spin-retrying. NetBSD has tighter defaults.
8. **No native vectored writes on NetBSD.** Without `aio_writev`, NetBSD
   submissions become per-chunk, doubling the syscall cost vs FreeBSD.
9. **Buffer ownership.** Submitted buffers must remain valid until
   completion (`AIO_ALLDONE` or `aio_return`). `BsdAioWriter` must own the
   buffers it submits (e.g., via the existing `BufferPool` from
   `crates/engine/src/local_copy/buffer_pool/`) and never expose them
   to user code while in flight. This rules out exposing `&mut [u8]`
   borrowed-buffer APIs on the AsyncFileWriter trait without an explicit
   completion barrier.

## Recommendation

**Defer with a documented design path.**

- **Out of scope today**: macOS POSIX AIO (covered by future dispatch_io
  audit), NetBSD POSIX AIO (insufficient async wins without `EVFILT_AIO`),
  Linux POSIX AIO (always inferior to io_uring).
- **Defer**: a FreeBSD-only `bsd_aio` backend in `crates/fast_io/src/`
  behind a `bsd_aio` cargo feature, gated `#[cfg(target_os = "freebsd")]`,
  using `EVFILT_AIO` for completion and `aio_writev` / `aio_readv` /
  `aio_mlock` / `aio_fsync` extensions where available. The expected
  benefit is moderate; the maintenance cost is comparable to the existing
  io_uring backend. Implementation should land only after #1655
  (`AsyncFileWriter` trait) is merged so the new backend can be added
  without churning the writer dispatch surface, and after a representative
  FreeBSD benchmark in `scripts/benchmark.sh` shows >= 15% throughput uplift
  over `pwrite(2)` on the receiver write path.
- **Support unconditionally**: nothing in this audit. Every AIO surface has
  a non-trivial pitfall.

## Follow-up tasks

- [ ] #1654-1 prototype `crates/fast_io/src/bsd_aio/` writer on FreeBSD 14
  using `aio_write` + `EVFILT_AIO`; measure vs `pwrite` baseline.
- [ ] #1654-2 add a FreeBSD CI lane (cross-compile via cirrus-ci or
  vmactions) that exercises the `bsd_aio` feature.
- [ ] #1654-3 produce `docs/audits/dispatch-io.md` for the macOS path.
- [ ] #1655 introduce `AsyncFileWriter` trait that abstracts io_uring,
  IOCP, and (future) `bsd_aio` behind a single submission/drain contract.

## References

- Upstream rsync 3.4.1 source: `target/interop/upstream-src/rsync-3.4.1/`
  (no `aio_*` usage; recursive ripgrep returns zero matches for the API).
- Existing async backends:
  - Linux io_uring: `crates/fast_io/src/io_uring/mod.rs:1-244`,
    `crates/fast_io/src/io_uring/file_writer.rs`,
    `crates/fast_io/src/io_uring/file_reader.rs`,
    `crates/fast_io/src/io_uring_stub.rs`.
  - Windows IOCP: `crates/fast_io/src/iocp/mod.rs:1-45`,
    `crates/fast_io/src/iocp/file_writer.rs`,
    `crates/fast_io/src/iocp_stub.rs`.
- Writer dispatch surface: `crates/fast_io/src/lib.rs:111-127`,
  `crates/fast_io/src/lib.rs:140-188`, `crates/fast_io/src/traits.rs:38-49`.
- Hot-path call sites:
  `crates/transfer/src/transfer_ops/response.rs:108`,
  `crates/transfer/src/generator/mod.rs:728`.
- Existing BSD platform gates:
  `crates/core/src/client/module_list/socket_options/lookup.rs`
  (`target_os = "freebsd"`, `target_os = "netbsd"`).
- POSIX.1-2017 specification of `aio_read`, `aio_write`, `aio_suspend`,
  `aio_return`, `aio_error`, `aio_cancel`, `aio_fsync`, `lio_listio`,
  and `<aio.h>`.
- FreeBSD man pages: `aio(4)`, `aio_read(2)`, `aio_write(2)`,
  `aio_suspend(2)`, `aio_return(2)`, `aio_error(2)`, `aio_cancel(2)`,
  `aio_readv(2)`, `aio_writev(2)`, `aio_mlock(2)`, `aio_fsync(2)`,
  `lio_listio(2)`, `kqueue(2)` (`EVFILT_AIO`), `sigevent(3)`.
- NetBSD man pages: `aio(4)`, `aio_read(2)`, `aio_write(2)`,
  `aio_suspend(2)`, `aio_cancel(2)`, `aio_fsync(2)`, `lio_listio(2)`.
- macOS man pages (for comparison only): `aio(2)`, `aio_read(2)`,
  `aio_write(2)` (no `EVFILT_AIO`).
