# Platform-agnostic `AsyncFileWriter` trait design

Tracker: #1655. Branch: `docs/async-file-writer-trait-1655`. No code changes.

## Scope

This document designs the unified `AsyncFileWriter` trait that hides the three
async file-write backends oc-rsync ships or plans to ship:

- Linux **io_uring** (already implemented in
  `crates/fast_io/src/io_uring/`).
- Windows **IOCP** (already implemented in
  `crates/fast_io/src/iocp/`, tracker #1656).
- macOS **`dispatch_io`** plus **`writev`** / `F_NOCACHE` fallback
  (designed in `docs/audits/macos-dispatch-io.md` for #1653, with #1657
  pending the optimized fallback).

A portable `std::fs::File`-backed implementation already exists as
`StdFileWriter` (`crates/fast_io/src/traits.rs:120-185`) and is the universal
fallback when no async backend is available.

The companion audits feeding into this trait are:

- `docs/audits/macos-dispatch-io.md` (#1653)
- `docs/audits/bsd-aio.md` (#1654)
- `docs/audits/windows-iocp-benchmark.md` (#1899)
- `docs/audits/disk-commit-iouring-batching.md` (#1086)

## 1. Why a unified trait?

Today the receiver hot path threads two distinct enum-wrapped writer types
through engine and transfer code: `IoUringOrStdWriter` on Linux and
`IocpOrStdWriter` on Windows. Both are dispatch enums hand-written per
platform, and both exist because the `FileWriter` trait
(`crates/fast_io/src/traits.rs:38-49`) cannot express the platform-specific
async submission semantics. macOS gets neither; the receiver simply uses the
sync `StdFileWriter`.

### Concrete `cfg`-branched call sites

These five sites are the engine/transfer code that today picks a writer based
on `cfg(target_os)` or feature gates, and that the unified trait removes:

1. **`crates/transfer/src/disk_commit/writer.rs:139-145`** -
   the disk-commit `Writer` enum is gated on
   `cfg(all(target_os = "linux", feature = "io_uring"))`:

   ```rust
   pub(super) enum Writer<'a> {
       Buffered(ReusableBufWriter<'a>),
       #[cfg(all(target_os = "linux", feature = "io_uring"))]
       IoUring {
           batch: &'a mut fast_io::IoUringDiskBatch,
       },
   }
   ```

   Each method (`write_chunk`, `flush_and_sync`, `finish`,
   `buffered_for_sparse`) carries a parallel `#[cfg]` arm
   (`crates/transfer/src/disk_commit/writer.rs:157`,
   `:169`, `:192`, `:210`).

2. **`crates/transfer/src/disk_commit/process.rs:263-281`** -
   `make_writer` selects between buffered and io_uring backends with a
   `#[cfg(all(target_os = "linux", feature = "io_uring"))]` block plus an
   `#[allow(unused_variables)]` attribute so non-Linux builds compile.

3. **`crates/transfer/src/transfer_ops/response.rs:108`** -
   the receiver-side write path explicitly calls
   `fast_io::writer_from_file(file, writer_capacity, ctx.config.io_uring_policy)`
   and gets back an `IoUringOrStdWriter`. There is no IOCP equivalent on the
   Windows hot path; Windows builds fall through to the `Std` arm.

4. **`crates/transfer/src/generator/mod.rs:728`** -
   the sender read side calls `fast_io::reader_from_path(path, policy)` for
   basis-file scanning. The matching write side per platform is a
   read/dispatch asymmetry: io_uring exposes both reader and writer factories,
   IOCP exposes both, but only the reader is wired into the generator.

5. **`crates/fast_io/src/lib.rs:111-127`** -
   the crate itself selects the async backend with stacked `cfg` lines:

   ```rust
   #[cfg(all(target_os = "linux", feature = "io_uring"))]
   pub mod io_uring;
   #[cfg(not(all(target_os = "linux", feature = "io_uring")))]
   #[path = "io_uring_stub.rs"]
   pub mod io_uring;

   #[cfg(all(target_os = "windows", feature = "iocp"))]
   pub mod iocp;
   #[cfg(not(all(target_os = "windows", feature = "iocp")))]
   #[path = "iocp_stub.rs"]
   pub mod iocp;
   ```

   Every platform gets a stub for the others, doubling the public-API surface
   that has to stay in lockstep across the matrix.

### Cost of the current shape

- **Dead code on every platform.** The Linux build pulls in the entire
  `iocp_stub.rs`; the Windows build pulls in `io_uring_stub.rs`. Each stub
  has to mirror the full `IocpOrStdWriter` / `IoUringOrStdWriter` surface
  (factories, enum variants, `Send` bounds) so callers compile uniformly.
- **Divergent error handling.** `IocpWriter::write_at`
  (`crates/fast_io/src/iocp/file_writer.rs:103-157`) handles
  `ERROR_IO_PENDING` (Win32 errno 997) inline by calling
  `GetQueuedCompletionStatus`, while
  `IoUringWriter::flush` (`crates/fast_io/src/io_uring/file_writer.rs`)
  handles `EAGAIN`/short-write scenarios via `submit_write_batch`. There is no
  unified error vocabulary that says "the backend would block - retry".
- **Harder testing.** Every test that wants to exercise the async write path
  must guard with `#[cfg(target_os = "linux", feature = "io_uring")]` (see the
  64 tests in `crates/fast_io/src/io_uring/tests.rs`) or
  `#[cfg(target_os = "windows", feature = "iocp")]` (see
  `crates/fast_io/src/iocp/file_writer.rs:307-410`). A trait-level test
  written against `dyn AsyncFileWriter` runs on every platform and is a
  single source of truth for invariants such as "after `flush`, the file
  contains every byte previously passed to `write_at`".
- **Harder benchmarking.** The benchmark suite in
  `crates/fast_io/benches/io_optimizations.rs` and the IOCP benchmark plan
  (#1899) cannot share a single harness: the input type is different on
  every platform.
- **Receiver-side asymmetry.** Today the disk-commit thread has an io_uring
  fast path (`crates/transfer/src/disk_commit/thread.rs:69-82`) but no IOCP
  fast path on Windows and no `dispatch_io` fast path on macOS. A unified
  trait makes adding the missing platforms a one-place change at the
  factory call site rather than five places.

### Goal

Hide the platform difference behind a single async-friendly trait so that
`crates/transfer` and `crates/engine` are platform-neutral - a
`Box<dyn AsyncFileWriter>` (or static-dispatch generic) on every OS, with
the only platform-specific code living inside `fast_io`.

## 2. Current state per platform

### 2.1 Linux io_uring

Public API surface (`crates/fast_io/src/io_uring/mod.rs:81-117`):

- `IoUringWriter` - per-file writer (`file_writer.rs:30-42`):

  ```rust
  pub struct IoUringWriter {
      ring: RawIoUring,
      file: File,
      bytes_written: u64,
      buffer: Vec<u8>,
      buffer_pos: usize,
      buffer_size: usize,
      sq_entries: u32,
      fixed_fd_slot: i32,
      registered_buffers: Option<RegisteredBufferGroup>,
  }
  ```

  Each writer owns its own ring. Submission entry points are
  `IoUringWriter::create` (`file_writer.rs:52`) and
  `IoUringWriter::from_file` (`file_writer.rs:80`). Internal flush goes via
  `submit_write_batch` (`crates/fast_io/src/io_uring/batching.rs`) for plain
  `IORING_OP_WRITE` SQEs and `submit_write_fixed_batch`
  (`crates/fast_io/src/io_uring/registered_buffers.rs`) when registered
  buffers are available, taking the `IORING_OP_WRITE_FIXED` path.

- `IoUringDiskBatch` - shared-ring writer for the disk-commit thread
  (`crates/fast_io/src/io_uring/disk_batch.rs:45-79`). Owns a single
  `RawIoUring` and reuses it across files via
  `begin_file(file: File)` (`disk_batch.rs:103`),
  `write_data(&[u8])` (`:126`),
  `flush(&mut self)` (`:158`),
  `commit_file(do_fsync: bool)` (`:170`).
  This is the writer used in production today by
  `crates/transfer/src/disk_commit/`.

- Factory: `IoUringWriterFactory` returns `IoUringOrStdWriter`
  (`crates/fast_io/src/io_uring/file_factory.rs`), an enum with `IoUring`
  and `Std` variants. The free function
  `fast_io::writer_from_file(file, capacity, policy)` is the
  `IoUringPolicy`-aware helper used at
  `crates/transfer/src/transfer_ops/response.rs:108`.

- Completion handling: `submit_and_wait` is hidden inside the batched
  helpers; callers never touch CQEs directly.
  `man 2 io_uring_enter`,
  `man 2 io_uring_setup`, and
  `man 7 io_uring` describe the kernel ABI.

Kernel and runtime gates:

- Kernel >= 5.6, probed once and cached
  (`crates/fast_io/src/io_uring/config.rs::is_io_uring_available`,
  exposed via `fast_io::is_io_uring_available()`).
- Optional `IORING_REGISTER_FILES` (saves ~50 ns / SQE) and
  `IORING_SETUP_SQPOLL` (no syscalls per submission, requires
  `CAP_SYS_NICE`) - both summarized in the privilege table at
  `crates/fast_io/src/io_uring/mod.rs:60-71`.

### 2.2 Windows IOCP

Public API surface (`crates/fast_io/src/iocp/mod.rs:28-44`):

- `IocpWriter` (`crates/fast_io/src/iocp/file_writer.rs:27-34`):

  ```rust
  pub struct IocpWriter {
      handle: HANDLE,
      port: CompletionPort,
      config: IocpConfig,
      buffer: Vec<u8>,
      file_offset: u64,
      bytes_written: u64,
  }
  ```

  Construction: `IocpWriter::create` (`:38`) opens the file with
  `FILE_FLAG_OVERLAPPED` and associates a per-writer `CompletionPort`
  (`man WriteFile`,
  `man CreateIoCompletionPort`,
  `man GetQueuedCompletionStatus`).

- Submission: `IocpWriter::write_at` (`file_writer.rs:103-157`) submits a
  single `OverlappedOp` via `WriteFile`, then drains it with
  `GetQueuedCompletionStatus` if the call returned `ERROR_IO_PENDING`
  (Win32 errno 997). The `OverlappedOp` (`iocp/overlapped.rs:11-25`) holds a
  pinned `OVERLAPPED` plus its own `Vec<u8>`, ensuring the buffer remains at
  a stable address for the lifetime of the kernel call.

- Trait conformance: `IocpWriter` implements `Write`, `Seek`, and the
  internal `FileWriter` trait
  (`crates/fast_io/src/iocp/file_writer.rs:186-287`). `bytes_written`,
  `sync` (calls `FlushFileBuffers`), and `preallocate`
  (`SetFilePointerEx` + `SetEndOfFile`) are wired through.

- Factory: `IocpWriterFactory` returns `IocpOrStdWriter`
  (`crates/fast_io/src/iocp/file_factory.rs:201-262`) with the same
  `Auto`/`Enabled`/`Disabled` policy contract as io_uring
  (`crates/fast_io/src/lib.rs:430-447`).

Runtime gating: `is_iocp_available()` performs a one-time check via
`CreateIoCompletionPort` and caches the result; small files (< 64 KB,
`IOCP_MIN_FILE_SIZE`) bypass IOCP because the per-op overhead exceeds the
async benefit (`crates/fast_io/src/iocp/mod.rs:23-26`).

### 2.3 macOS dispatch_io / writev / F_NOCACHE

There is **no implementation today**. The receiver write path on macOS
falls through to `StdFileWriter` (`crates/fast_io/src/traits.rs:120-185`)
because the io_uring and IOCP modules are stubbed
(`crates/fast_io/src/io_uring_stub.rs`,
`crates/fast_io/src/iocp_stub.rs`). The audit at
`docs/audits/macos-dispatch-io.md` (#1653) describes the planned mapping:

| oc-rsync operation | `dispatch_io` mapping |
|---|---|
| `create(path)` | `dispatch_io_create_with_path(DISPATCH_IO_RANDOM, path, O_WRONLY|O_CREAT|O_TRUNC, 0o644, queue, cleanup)` (`man dispatch_io_create_with_path`) |
| `preallocate(size)` | `fcntl(fd, F_PREALLOCATE, ...)` on APFS via a `dispatch_io_barrier` |
| `write_at(buf, offset)` | wrap in `dispatch_data_t` then `dispatch_io_write(channel, offset, data, queue, handler)` |
| `flush` | `dispatch_io_barrier` + `dispatch_semaphore_wait` |
| `sync` | `dispatch_io_barrier` + `fcntl(fd, F_FULLFSYNC)` |
| `close` | `dispatch_io_close(channel, DISPATCH_IO_STOP)` and wait on cleanup handler |

The macOS-specific fallback chain (#1657) is `dispatch_io` ->
`writev`-with-`F_NOCACHE` for large transfers (`man 2 fcntl`,
`F_NOCACHE` to bypass the unified buffer cache for the duration of the
write) -> plain `BufWriter<File>`. Today none of these layers are
implemented; the trait below is the prerequisite for landing them.

### 2.4 Std fallback

`StdFileWriter` (`crates/fast_io/src/traits.rs:120-185`) wraps
`BufWriter<File>` with a default 8 KB capacity and is the trait's universal
fallback. It is what every platform receives when:

- io_uring/IOCP are compiled out (`default-features = false`).
- The runtime probe fails (kernel too old, seccomp, CreateIoCompletionPort
  failure).
- The caller passes `Disabled` policy.

`StdFileWriter` is also the test oracle for the trait property tests
described in section 7.

## 3. Trait design proposal

The trait sits in `crates/fast_io/src/traits.rs` next to the existing
`FileWriter` and adopts return-position-impl-trait (RPITIT) for async, which
is stable on the workspace's toolchain (Rust 1.88.0, see
`rust-toolchain.toml`). This is the same mechanism used by the standard
library's `IntoFuture` and avoids the `async_trait` macro's heap allocation
per call.

```rust
use std::future::Future;
use std::io;
use std::path::Path;

/// Capability bitset describing what an async writer backend supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriterCapabilities {
    /// Backend can register the buffer with the kernel for zero-copy I/O
    /// (`IORING_REGISTER_BUFFERS`, none on IOCP, none on dispatch_io).
    pub registered_buffers: bool,
    /// Backend supports per-submission positional offsets without a Seek
    /// (io_uring `IORING_OP_WRITE`, IOCP OVERLAPPED.Offset, dispatch_io
    /// random-access channel).
    pub positional_writes: bool,
    /// Backend supports an async fsync operation
    /// (io_uring `IORING_OP_FSYNC`, IOCP `FlushFileBuffers` is sync,
    /// dispatch_io `dispatch_io_barrier` + `F_FULLFSYNC`).
    pub async_fsync: bool,
    /// Backend supports per-operation cancellation
    /// (io_uring `IORING_OP_ASYNC_CANCEL`, IOCP `CancelIoEx`,
    /// dispatch_io: only channel-wide `DISPATCH_IO_STOP`).
    pub per_op_cancel: bool,
}

/// Platform-agnostic async file writer.
///
/// Hides io_uring (Linux), IOCP (Windows), dispatch_io (macOS), and the
/// portable `BufWriter<File>` fallback behind a single submit/await
/// surface. Implementations live in `crates/fast_io/src/{io_uring,iocp,
/// dispatch_io}` and the universal `crates/fast_io/src/std_async.rs`.
pub trait AsyncFileWriter: Send {
    /// Writes `buf` at the given byte offset. Returns the number of bytes
    /// the kernel committed to disk (which may be less than `buf.len()` on
    /// short writes; callers must loop on `EINTR`-equivalents).
    ///
    /// The backend takes a borrow of `buf` for the lifetime of the future.
    /// For io_uring this means the SQE pins the slice through the kernel
    /// callback; for IOCP the buffer is `Vec<u8>`-cloned into the
    /// `OverlappedOp` so the original borrow is released as soon as the
    /// future is polled to completion. The contract is "the caller may
    /// drop `buf` after `write_at(...).await` returns".
    fn write_at<'a>(
        &'a mut self,
        buf: &'a [u8],
        offset: u64,
    ) -> impl Future<Output = io::Result<usize>> + Send + 'a;

    /// Submits all queued SQEs / overlapped ops and waits for them.
    /// On io_uring this is `io_uring_enter` with `min_complete=N`; on IOCP
    /// it drains every outstanding `GetQueuedCompletionStatus`; on
    /// dispatch_io it issues `dispatch_io_barrier` and waits.
    fn flush(&mut self) -> impl Future<Output = io::Result<()>> + Send + '_;

    /// Persists buffered data and metadata to the storage device. Maps to
    /// `IORING_OP_FSYNC` (Linux), `FlushFileBuffers` (Windows; sync), and
    /// `fcntl(F_FULLFSYNC)` after a barrier (macOS).
    fn sync(&mut self) -> impl Future<Output = io::Result<()>> + Send + '_;

    /// Pre-allocates `size` bytes at the destination. Default is no-op so
    /// backends without pre-allocation (vanilla `BufWriter`) compile.
    fn preallocate(
        &mut self,
        _size: u64,
    ) -> impl Future<Output = io::Result<()>> + Send + '_ {
        async { Ok(()) }
    }

    /// Closes the writer and releases the kernel resources. Distinct from
    /// `Drop` so callers can observe close errors. The `Drop` impl of every
    /// backend must call the equivalent close path on a best-effort basis
    /// for panic-safety.
    fn close(self) -> impl Future<Output = io::Result<()>>
    where
        Self: Sized;

    /// Bytes the writer has accepted (not necessarily flushed to disk).
    /// Mirrors `FileWriter::bytes_written` semantics.
    fn bytes_accepted(&self) -> u64;

    /// Capability descriptor for the backend.
    fn capabilities(&self) -> WriterCapabilities;
}

/// Factory analogous to `FileWriterFactory` but producing async writers.
pub trait AsyncFileWriterFactory: Send + Sync {
    type Writer: AsyncFileWriter;
    fn create<P: AsRef<Path>>(
        &self,
        path: P,
    ) -> impl Future<Output = io::Result<Self::Writer>> + Send + '_;
    fn create_with_size<P: AsRef<Path>>(
        &self,
        path: P,
        size: u64,
    ) -> impl Future<Output = io::Result<Self::Writer>> + Send + '_;
}
```

### 3.1 async-trait vs RPITIT

oc-rsync pins Rust 1.88.0 (`rust-toolchain.toml`). RPITIT for traits has
been stable since 1.75 (December 2023), so the trait above is buildable
without the `async_trait` proc-macro crate. RPITIT avoids the
`Box<dyn Future>` allocation per call that `#[async_trait]` introduces and
keeps the trait object-safe via auxiliary `dyn`-friendly wrappers when
needed (the trait above is **not** object-safe because of the `Sized`
bound on `close`; callers that need `Box<dyn AsyncFileWriter>` can use a
simpler `AsyncFileWriterDyn` adapter that owns the writer inside an
`Option<T>` and produces `Pin<Box<dyn Future>>` futures via
`async move` blocks). This mirrors the strategy the `embedded-io-async`
ecosystem follows, which compiles on the same MSRV.

### 3.2 Buffer ownership contract

This is the core correctness question. Each backend has a different rule
for how long the bytes must remain valid:

- **io_uring**: the buffer pointer in the SQE is read by the kernel after
  the calling thread has returned from `io_uring_enter`. The buffer must
  outlive the in-flight operation, which on the Linux backend is enforced
  by holding the borrow inside the `IoUringWriter`'s internal `Vec<u8>`
  (`crates/fast_io/src/io_uring/file_writer.rs:34`,
  `:70` for the buffer field). Registered buffers
  (`IORING_REGISTER_BUFFERS`) tie buffer lifetime to the ring itself, which
  is owned by the writer (`registered_buffers.rs`).

- **IOCP**: `WriteFile` with an OVERLAPPED requires the buffer to remain at
  a stable address until the I/O completes. The current implementation
  copies the caller's data into a per-op `Vec<u8>` inside `OverlappedOp`
  (`crates/fast_io/src/iocp/file_writer.rs:108-110`):

  ```rust
  let mut op = OverlappedOp::new_write(offset, data);
  let overlapped_ptr = op.as_overlapped_ptr();
  ```

  `OverlappedOp::new_write` takes `&[u8]` and stores `data.to_vec()`
  (`crates/fast_io/src/iocp/overlapped.rs:40-48`), so the trait-level
  `write_at` borrow is released at await-point regardless of when the
  kernel completes. This is the safest contract and the trait codifies it.

- **dispatch_io**: `dispatch_io_write` consumes a `dispatch_data_t`. The
  `dispatch_data_create(buf, len, queue, destructor)` call hands the bytes
  to libdispatch with a destructor block invoked when the data is fully
  consumed (`docs/audits/macos-dispatch-io.md:130-138`). The trait
  contract above ("borrow released at await-point") therefore requires the
  macOS backend to either copy into a libdispatch-allocated region or use
  a destructor that drops the borrow at the await-point. The first is
  simpler and matches the IOCP contract; the second preserves zero-copy
  but requires unsafe stable-pointer reasoning.

The trait specifies the **safer, copy-when-needed** contract:
*the future borrows `buf` only until it is awaited*. Backends that want
the zero-copy fast path (io_uring registered buffers) expose it through
the future variant `write_at_owned(Vec<u8>) -> Vec<u8>` that returns the
buffer back to the caller after completion - the same pattern
`monoio` and `glommio` use for their owned-buffer APIs. That additional
method is **not** in the v1 trait surface; it can be added in a follow-up
once benchmarks justify it (#1410 covers this).

### 3.3 Error model

All three platform errors fall into `std::io::Error` cleanly:

- io_uring: CQE `res` field is a negative errno; converted to
  `io::Error::from_raw_os_error(-res)`.
- IOCP: `GetLastError()` is a Win32 error; converted via
  `io::Error::last_os_error()` (the existing `IocpWriter::write_at`
  already does this at `crates/fast_io/src/iocp/file_writer.rs:129`,
  `:153`).
- dispatch_io: the `io_handler` block delivers a POSIX `errno`; converted
  via `io::Error::from_raw_os_error`.

There is no `crates/fast_io/src/error.rs`; the trait keeps `io::Result`
to match the existing `FileWriter` and `FileReader` traits and avoid a
separate error type. Backends that need richer information (e.g.,
"transient EAGAIN, retry") use `io::ErrorKind::WouldBlock` or
`io::ErrorKind::Interrupted` per std conventions. This matches how
`StdFileWriter` already surfaces errors and avoids leaking an
`AsyncWriterError` enum into engine/transfer.

## 4. Async runtime question

The user-memory feedback at `feedback_no_wire_protocol_features.md` and
the verified rule "no second async runtime (async-std, smol)" (#1780) cap
the runtime choices: tokio is permitted but scoped to the SSH transport
(`crates/rsync_io/src/ssh/embedded/`, see #1779/#1818); there is no
project-wide tokio adoption.

**The `AsyncFileWriter` trait must therefore be runtime-agnostic.** The
RPITIT design above produces opaque `impl Future` types that the caller
can drive with any executor or with `futures::executor::block_on`. The
trait does not require `tokio::spawn`, `tokio::io`, or any tokio types.

Backends drive their own completion machinery synchronously inside the
returned future:

- **io_uring**: the future polls the CQ via `submit_and_wait(min_complete=1)`
  inside `Future::poll`. This is the same pattern `tokio-uring` uses but
  without the tokio reactor - the future directly issues the syscall and
  parks on the CQE rather than registering with epoll. Acceptable here
  because the disk-commit thread is already a dedicated thread
  (`crates/transfer/src/disk_commit/thread.rs:51-54`); blocking the
  syscall blocks only that thread.
- **IOCP**: `GetQueuedCompletionStatus` is a blocking syscall driven from
  inside the returned future. Identical pattern to today's
  `IocpWriter::write_at` (`crates/fast_io/src/iocp/file_writer.rs:141-150`),
  just wrapped in `async`.
- **dispatch_io**: the future bridges libdispatch's callback-based delivery
  to the Rust future via a `Waker` stored in the io_handler block. This is
  the only backend that needs cross-thread waker coordination, and the
  audit at `docs/audits/macos-dispatch-io.md:153-159` already describes
  the channel-wide cancel semantics that the future model has to honour.

The unit test harness uses `futures_executor::block_on` (zero runtime
weight) so tests compile without pulling tokio into `fast_io`'s
`[dev-dependencies]`. This is the same approach
`crates/rsync_io/src/ssh/embedded/` uses for its tokio-internal tests
without leaking tokio out of the embedded-ssh feature.

## 5. Integration points

The trait makes the per-platform `cfg` arms in engine and transfer
collapse to a single trait object or generic parameter at three call
sites:

1. **`crates/transfer/src/disk_commit/writer.rs:139-219`** -
   replace the `Writer<'a>` enum with `Box<dyn AsyncFileWriter>` (or a
   generic `W: AsyncFileWriter`). The current `Writer::write_chunk`
   (`:166`), `Writer::flush_and_sync` (`:179`), and `Writer::finish`
   (`:207`) all become trait method calls. The
   `#[cfg(all(target_os = "linux", feature = "io_uring"))]` guards are
   deleted.

2. **`crates/transfer/src/disk_commit/process.rs:263-281`** -
   `make_writer` shrinks to a one-line factory call:

   ```rust
   ctx.async_writer_factory.create(begin.file_path).await
   ```

   The factory is supplied by `DiskCommitConfig` and selected once at
   process start by the policy struct (`fast_io::IoUringPolicy` plus a
   new `IocpPolicy` and `DispatchIoPolicy` that already exist for io_uring
   at `crates/fast_io/src/lib.rs:397-447`).

3. **`crates/transfer/src/transfer_ops/response.rs:108`** -
   the call

   ```rust
   let mut output = fast_io::writer_from_file(file, writer_capacity, ctx.config.io_uring_policy)?;
   ```

   becomes

   ```rust
   let mut output = ctx.config.async_writer_factory.from_file(file, writer_capacity).await?;
   ```

   with `async_writer_factory` an `Arc<dyn AsyncFileWriterFactory>`
   constructed once in `crates/core` and threaded through `CoreConfig`.

4. **`crates/engine/src/local_copy/executor/file/copy/transfer/execute.rs`** -
   the receiver write path opens its destination via
   `open_destination_writer`
   (`crates/engine/src/local_copy/executor/file/copy/transfer/write_strategy.rs:114-225`)
   and currently returns a raw `fs::File`. Wrapping that file in an async
   writer becomes the responsibility of the disk-commit thread, so this
   site is unchanged - the trait integrates one layer up.

5. **`crates/fast_io/src/lib.rs:111-127`** -
   the per-OS `pub mod` lines stay; the new addition is a single
   `pub mod async_writer;` with three feature-gated submodules
   (`io_uring`, `iocp`, `dispatch_io`) plus the always-compiled
   `std_async` fallback. Each submodule declares its `AsyncFileWriter`
   impl. The top-level `default_async_writer_factory()` returns the best
   available backend based on the policy enums.

### 5.1 Receiver-side flow after integration

```
read network    receiver thread    AsyncFileWriter            kernel
  socket    ->    spsc::Sender      Box<dyn ...>               io_uring/IOCP/
  (sync)         -> FileMessage   .write_at(buf, offset).await dispatch_io
                                 .flush().await
                                 .sync().await    (only if --fsync)
                                 .close().await
```

The disk-commit thread (`crates/transfer/src/disk_commit/thread.rs`) owns
the writer and the executor (`block_on`); the network thread continues to
push chunks through the SPSC channel as it does today.

## 6. Fallback chain

The runtime decision tree, mirrored across the three platforms:

### Linux

```
AsyncFileWriterFactory::create
  -> IoUringPolicy::Auto:
        if is_io_uring_available() && config.build_ring().is_ok()
            -> IoUringAsyncWriter (via IoUringDiskBatch when batched)
        else
            -> StdAsyncWriter (BufWriter<File>)
  -> IoUringPolicy::Enabled:
        if !is_io_uring_available()
            -> Err(Unsupported)
        else
            -> IoUringAsyncWriter
  -> IoUringPolicy::Disabled:
        -> StdAsyncWriter
```

This is exactly what `fast_io::writer_from_file` does today
(`crates/fast_io/src/io_uring/mod.rs:140-188`) - the trait does not
introduce new fallback states.

### Windows

```
AsyncFileWriterFactory::create
  -> IocpPolicy::Auto:
        if is_iocp_available() && size >= IOCP_MIN_FILE_SIZE
            -> IocpAsyncWriter
        else
            -> StdAsyncWriter
  -> IocpPolicy::Enabled:
        if !is_iocp_available()
            -> Err(Unsupported)
        else
            -> IocpAsyncWriter
  -> IocpPolicy::Disabled:
        -> StdAsyncWriter
```

Mirrors `IocpWriterFactory::create`
(`crates/fast_io/src/iocp/file_factory.rs:240-262`).

### macOS

```
AsyncFileWriterFactory::create
  -> DispatchIoPolicy::Auto:
        if is_dispatch_io_available()         (always true on macOS 10.7+)
            -> DispatchIoAsyncWriter
        else
            -> WritevAsyncWriter (#1657, F_NOCACHE-aware large writes)
              fallback ->
            -> StdAsyncWriter
  -> DispatchIoPolicy::Enabled:
        -> DispatchIoAsyncWriter (always succeeds)
  -> DispatchIoPolicy::Disabled:
        -> WritevAsyncWriter or StdAsyncWriter
```

The `WritevAsyncWriter` is the optimized fallback in #1657 - vectored
`writev(2)` with `F_NOCACHE` set on the destination fd to bypass the unified
buffer cache for large transfers. See `man 2 writev` and
`man 2 fcntl` (`F_NOCACHE`).

### Cross-cutting

The audit at `docs/audits/iouring-pbuf-ring.md` (#1748) walks through the
Linux fallback chain in detail; the new trait does not change that chain,
only the call site that selects it. On any platform, the universal
`StdAsyncWriter` is the bottom of the fallback ladder and is the test
oracle in section 7.

## 7. Test plan

Three layers of tests live in `crates/fast_io/tests/async_writer/`
(new directory) and `crates/fast_io/src/{io_uring,iocp}/tests.rs`
(extended).

### 7.1 Round-trip equivalence

A single test, parameterised over each backend at compile time, asserts:

> Given an arbitrary sequence of `(buf, offset)` writes followed by `flush`
> and `close`, every backend produces a file whose bytes match the
> equivalent `StdAsyncWriter` output.

Implementation: a helper `fn run_writes<W: AsyncFileWriter>(w: W,
ops: &[Op]) -> Vec<u8>` is invoked once per backend the platform supports,
and the resulting byte streams are compared with `assert_eq!`. The Std
backend is always the reference. This is a property test over `Op =
{Write{offset, data}, Flush, Sync, Preallocate{size}}` driven by
`proptest`, which is already a workspace dev-dependency.

### 7.2 Concurrency / cancellation

For each backend that supports per-op cancel
(`capabilities().per_op_cancel == true`, currently io_uring + IOCP), a
test submits an in-flight write, drops the future before completion, and
asserts no kernel resources leak. For `dispatch_io` (channel-wide cancel
only), the test asserts that dropping the writer cancels every queued op.

### 7.3 Benchmarks

Three Criterion benches in
`crates/fast_io/benches/async_writer_bench.rs` (new):

- `async_writer_per_file_overhead`: measures the latency of
  `factory.create + write_at(64 KB) + flush + close` for each backend.
  Targets the question in #1410 (per-file io_uring vs shared ring).
- `async_writer_throughput_64k_chunks`: measures sustained write
  throughput streaming 1 GB in 64 KB chunks. Targets #1899
  (IOCP vs `std::fs::File`).
- `async_writer_crossplatform_paths`: runs the same workload on
  io_uring, IOCP, dispatch_io (when available), and Std. Targets #1659
  (cross-platform fast_io paths).

Existing benches in `crates/fast_io/benches/io_optimizations.rs` continue
to test the underlying syscall paths; the new bench specifically targets
the trait-level dispatch overhead.

## 8. Recommendation

Recommended landing sequence for #1655:

1. **Lift IOCP into the trait**. The `IocpWriter`
   (`crates/fast_io/src/iocp/file_writer.rs`) is the most complete
   implementation today and already encapsulates the buffer-pinning
   contract. Phase 1 introduces `AsyncFileWriter` in
   `crates/fast_io/src/traits.rs` and provides the `IocpAsyncWriter`
   wrapper that delegates to the existing `IocpWriter`. No call-site
   changes; `IocpOrStdWriter` continues to exist alongside the new
   trait. This validates the trait shape against a real backend without
   destabilising the disk-commit thread.

2. **Wrap io_uring under the trait**. Phase 2 produces
   `IoUringAsyncWriter` that delegates to either `IoUringWriter` (per-file
   ring) or `IoUringDiskBatch` (shared ring). The
   `crates/transfer/src/disk_commit/writer.rs:139-145` enum is
   converted to use the trait; `Writer<'a>` becomes a `Box<dyn
   AsyncFileWriter>` (or a generic) and the `#[cfg]` arms collapse.
   At this point the receiver still has Linux-only fast pathing but
   through the new trait surface.

3. **Add macOS impl per #1657**. Phase 3 introduces
   `DispatchIoAsyncWriter` and `WritevAsyncWriter` per
   `docs/audits/macos-dispatch-io.md`. The
   `crates/transfer/src/transfer_ops/response.rs:108` call site picks the
   right factory; macOS receivers stop using `BufWriter<File>` for hot
   paths. Phase 3 is the first to deliver user-visible perf on macOS.

4. **Replace per-platform call sites in engine/transfer**. Phase 4 removes
   `IoUringOrStdWriter` and `IocpOrStdWriter` once every consumer has
   migrated. The `cfg` block in `crates/fast_io/src/lib.rs:111-127` keeps
   the per-platform `pub mod` declarations (the unsafe code lives there)
   but the public re-exports now point only at the trait factories. The
   stub crates (`io_uring_stub.rs`, `iocp_stub.rs`) are deleted because
   `Std` is now the universal stub.

### Unsafe boundary

Per `feedback_unsafe_code_policy.md` and the project's unsafe-code
policy, the trait must stay in `fast_io` - the only
crate that may currently contain `unsafe`. Every backend that touches
io_uring, OVERLAPPED, libdispatch, or `writev` keeps its `#[allow(unsafe_code)]`
inside `crates/fast_io/src/`. Engine and transfer hold only the
`Box<dyn AsyncFileWriter>` and never see a raw fd, HANDLE, or
`dispatch_data_t`. The lib.rs preamble at
`crates/fast_io/src/lib.rs:6-32` already articulates this boundary; the
new trait is what makes the boundary practically enforceable.

### Long-term consolidation

The project's unsafe-code policy states: "Consolidate all unsafe code
into `fast_io` as the single crate permitted to contain unsafe code. New
unsafe code should go into `fast_io` and expose safe public APIs."
`AsyncFileWriter` is exactly
this pattern: every io_uring SQE, every OVERLAPPED, every
`dispatch_io_write` is wrapped behind a single safe trait method. After
phase 4, `crates/transfer` and `crates/engine` no longer reference any
platform-specific I/O type at all - the only knob they have is the policy
enum.

## Appendix: cross-references

- Tracker: #1655.
- Related (completed): #1656 IOCP, #1654 BSD aio, #1653 dispatch_io eval,
  #1136-#1139 PlatformCopy trait.
- Related (pending): #1657 macOS optimized fallback, #1821 IoBackend
  wiring.
- IOCP work: #1415, #1656, #1717-#1725, #1896-#1900.
- io_uring work: #1409, #1097, #1739, #1874, #1937.
- Audits: `docs/audits/macos-dispatch-io.md`, `docs/audits/bsd-aio.md`,
  `docs/audits/disk-commit-iouring-batching.md`,
  `docs/audits/windows-iocp-benchmark.md`,
  `docs/audits/iouring-pbuf-ring.md`.
- Memory: `feedback_unsafe_code_policy.md`,
  `feedback_no_wire_protocol_features.md`, `project_protocol_compat.md`.
