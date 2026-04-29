# macOS `dispatch_io` evaluation as an async I/O backend

Task: #1653. Branch: `docs/macos-dispatch-io-audit`. No code changes.

## Overview and decision question

oc-rsync ships two platform-specific async I/O backends in `fast_io`:
io_uring on Linux (`crates/fast_io/src/io_uring/`) and Windows IOCP
(`crates/fast_io/src/iocp/`). On macOS the receiver hot path falls back to
synchronous `BufReader` / `BufWriter` (`crates/fast_io/src/traits.rs:76-185`)
plus `clonefile` / `fcopyfile` for whole-file copies
(`crates/fast_io/src/platform_copy/dispatch.rs:62-211`). The async copier in
`crates/engine/src/async_io/copier.rs` is built on tokio's blocking-thread
pool and does not exercise any kernel async-I/O surface on macOS.

The decision question for #1653: should oc-rsync grow a third backend that
implements `FileReader` / `FileWriter` (`crates/fast_io/src/traits.rs:12-49`)
on top of Apple's Grand Central Dispatch (`libdispatch`) `dispatch_io`
channels, mirroring the IOCP and io_uring file/socket reader/writer layout?
This document collects the evidence needed to answer that and to plan the
work behind a `dispatch_io` cargo feature analogous to `iocp` and `io_uring`.

Upstream rsync 3.4.1 does not use `dispatch_io`. The only `dispatch`
references in `target/interop/upstream-src/rsync-3.4.1/` are SIMD compiler
"target attribute dispatching" hits in `simd-checksum-x86_64.cpp:50` and
`configure.ac:312`; both are unrelated to libdispatch. There is therefore no
upstream parity obligation - `dispatch_io` would be an oc-rsync optimization
in the same category as IOCP, FICLONE, and ReFS reflink.

## `dispatch_io` API summary

`dispatch_io` (declared in `<dispatch/io.h>`, framework
`/usr/include/dispatch/io.h` shipped by Xcode) is the async, queue-driven
streaming-I/O facility of libdispatch. The relevant surface, paraphrased
from Apple's `dispatch/io.h` header documentation:

- **Channels.** A `dispatch_io_t` wraps a file descriptor (regular file,
  pipe, or socket) plus a target dispatch queue. Two creation entry points
  exist: `dispatch_io_create(type, fd, queue, cleanup_handler)` for an
  already-open fd, and `dispatch_io_create_with_path(type, path, oflag, mode,
  queue, cleanup_handler)` for a path. The `type` is either
  `DISPATCH_IO_STREAM` (sequential, no random access; pipes, sockets, char
  devices) or `DISPATCH_IO_RANDOM` (regular files; offsets specified per
  read/write). The `cleanup_handler` runs on the channel's queue once the
  channel is closed and all pending I/O drained, receiving the final error
  code; it is the natural place to release the owning fd.

- **Read and write.** `dispatch_io_read(channel, offset, length, queue,
  io_handler)` and `dispatch_io_write(channel, offset, data, queue,
  io_handler)` enqueue an operation and return immediately. The `io_handler`
  block is invoked one or more times - the API explicitly delivers partial
  results as data becomes available - with `(bool done, dispatch_data_t
  data, int error)`. The `done` flag is set on the final delivery for a given
  call. `dispatch_data_t` is an immutable, ref-counted, possibly
  discontiguous buffer whose underlying storage may be backed by `mmap` or by
  zero-copy regions supplied by the kernel (see
  `dispatch_data_create_map`, `dispatch_data_apply`).

- **Back-pressure / sizing knobs.** `dispatch_io_set_high_water(channel,
  size)` and `dispatch_io_set_low_water(channel, size)` define the buffering
  envelope: handlers are invoked as soon as `low_water` bytes are available,
  and the channel will not buffer more than `high_water`. Set both to
  `SIZE_MAX` for "deliver everything in one shot"; set low to a small page
  multiple for streaming. `dispatch_io_set_interval(channel, interval, flags)`
  schedules periodic delivery callbacks irrespective of accumulated bytes,
  matching `IORING_SETUP_SQPOLL`-style "drain on a timer" semantics.

- **Cancellation.** `dispatch_io_close(channel, flags)` requests close;
  `flags=DISPATCH_IO_STOP` cancels in-flight operations and triggers the
  io_handler one last time with the error set to `ECANCELED`. There is no
  per-operation cancel handle, only channel-wide stop.

- **Memory pressure.** libdispatch publishes a global memory-pressure source
  (`DISPATCH_SOURCE_TYPE_MEMORYPRESSURE`) with `WARN`, `CRITICAL`, and
  `NORMAL` levels. `dispatch_io` channels honour the global pressure level
  internally - on `CRITICAL` the kernel may evict cached file-backed pages
  bound to outstanding `dispatch_data_t` regions, and the runtime defers
  read-ahead. Callers that hold `dispatch_data_t` regions for long lifetimes
  should subscribe to the source themselves and proactively `_map` and copy.

- **Barrier and queues.** `dispatch_io_barrier(channel, block)` serializes a
  block against all enqueued reads and writes, equivalent to a per-channel
  fence. The channel's target queue is supplied at creation time; oc-rsync
  would create a dedicated serial queue per channel to mirror IOCP's
  "per-writer completion port" pattern (`crates/fast_io/src/iocp/file_writer.rs:27-71`).

- **Error handling.** All async errors arrive in the io_handler's `error`
  argument as POSIX errnos (`EIO`, `ECANCELED`, `ENOSPC`, ...). Synchronous
  setup errors are returned by `dispatch_io_create_with_path` as a `NULL`
  channel and the cleanup_handler is invoked once with the errno. This maps
  directly to `io::Error::from_raw_os_error`.

`dispatch_io` is implemented on top of `aio(4)` and the kernel's
`vnode_pageout` paths; on APFS it uses the unified buffer cache rather than
`O_DIRECT`. It is therefore not a true zero-copy mechanism in the
io_uring / `splice` sense - it is closer in spirit to IOCP overlapped I/O:
async submission, partial-completion delivery, kernel-managed buffering.

## Mapping to oc-rsync's I/O patterns

The receiver-side hot path that the planned `AsyncFileWriter` trait
(referenced in #1655) targets sits in
`crates/engine/src/local_copy/executor/file/copy/transfer/execute.rs` and
the standalone async copier in `crates/engine/src/async_io/copier.rs:91-213`.
Today the receiver writes via `BufWriter<File>` (`StdFileWriter`,
`crates/fast_io/src/traits.rs:120-185`) on macOS; the IOCP
`IocpWriter` (`crates/fast_io/src/iocp/file_writer.rs:27-300`) and the
io_uring `IoUringWriter` are the two existing trait implementations that
bypass `BufWriter`.

A `DispatchIoWriter` that implements `FileWriter`
(`crates/fast_io/src/traits.rs:38-49`) would map onto `dispatch_io` as
follows:

| oc-rsync operation | `dispatch_io` mapping |
|---|---|
| `FileWriterFactory::create(path)` | `dispatch_io_create_with_path(DISPATCH_IO_RANDOM, path, O_WRONLY \| O_CREAT \| O_TRUNC, 0o644, channel_queue, cleanup_handler)` |
| `FileWriter::preallocate(size)` | `ftruncate(fd, size)` inside the cleanup-handler-owned fd via a `dispatch_io_barrier` (no first-class preallocate in `dispatch_io`); F_PREALLOCATE on APFS via `fcntl` |
| Sequential `Write::write(buf)` | accumulate into a per-writer `Vec<u8>`; on flush wrap in a `dispatch_data_t` via `dispatch_data_create(buf.as_ptr(), buf.len(), queue, DISPATCH_DATA_DESTRUCTOR_DEFAULT)` and call `dispatch_io_write(channel, offset, data, queue, handler)` |
| `Write::flush` | `dispatch_io_barrier` + wait on a `dispatch_semaphore_t` signalled in the io_handler when `done==true` |
| `FileWriter::sync` | `dispatch_io_barrier` + `fcntl(fd, F_FULLFSYNC)` (Apple's documented "actually flush to platter" call; matches the `fsync` policy oc-rsync already implements elsewhere) |
| Drop / cancel | `dispatch_io_close(channel, DISPATCH_IO_STOP)` and wait on the cleanup handler |

Symmetric mappings exist for read (`DISPATCH_IO_RANDOM` for files,
`DISPATCH_IO_STREAM` for the remote-shell stdio path used by
`crates/transport`). The chunked-delivery semantics of the io_handler block
match the way `crates/engine/src/async_io/copier.rs:141-171` already loops
over `read` / `write_all` until `n == 0`.

The buffer-pool integration story is the most subtle part. `BufferPool`
currently hands out `Vec<u8>` regions to the synchronous `BufWriter`. Wrapping
those in `dispatch_data_t` requires either (a) copying into a libdispatch-
allocated region via `dispatch_data_create_concat` or (b) using
`dispatch_data_create(_, _, queue, ^{ pool.release(buf); })` so that the
ref-counted destructor returns the buffer to the pool when libdispatch
finishes with it. Option (b) is the only path that preserves the buffer-pool
zero-copy property and matches how IOCP's `OverlappedOp` pins the buffer for
the lifetime of the kernel call (`crates/fast_io/src/iocp/file_writer.rs:108-158`).

## Comparison vs io_uring (Linux) and IOCP (Windows)

| Property | io_uring (Linux 5.6+) | IOCP (Windows) | `dispatch_io` (macOS) |
|---|---|---|---|
| Submission model | Submission Queue ring of SQEs, batched via `io_uring_enter` | Overlapped op + per-handle completion port; one syscall per submit | Block-based, queued onto a `dispatch_queue_t`; libdispatch coalesces submissions |
| Completion model | Completion Queue ring; one syscall to drain N CQEs | `GetQueuedCompletionStatus` blocks until N events ready | io_handler block invoked on the channel queue, possibly multiple partial deliveries per call |
| Kernel support | Linux 5.6+ probed at runtime in `crates/fast_io/src/io_uring/config.rs` | Vista+ probed in `crates/fast_io/src/iocp/config.rs` | macOS 10.7+; libdispatch is part of every supported macOS release - no runtime probe needed |
| Zero-copy | Yes, via fixed buffers / fixed files / `IORING_OP_SPLICE` | No (overlapped buffer is copied through the cache) | No (regular files go through unified buffer cache); `dispatch_data_t` can be backed by `mmap` |
| Cancellation | `IORING_OP_ASYNC_CANCEL` per SQE | `CancelIoEx` per overlapped op | Channel-wide `dispatch_io_close(DISPATCH_IO_STOP)` only |
| Back-pressure | Application-managed via SQ depth | Application-managed via queued-op count | Built-in via high-water / low-water marks |
| Memory pressure | Application-managed | Application-managed | First-class via `DISPATCH_SOURCE_TYPE_MEMORYPRESSURE` |
| Socket support | Yes (`crates/fast_io/src/io_uring/socket_*.rs`) | Yes (overlapped socket I/O) | Yes (`DISPATCH_IO_STREAM` over socket fd) |
| Best-fit oc-rsync workload | Linux receiver (delta apply, network->disk) | Windows receiver | macOS receiver, particularly the SSH-stdio path in `crates/transport` |

The single largest difference for oc-rsync is the cancellation granularity:
io_uring and IOCP both support per-operation cancellation, which is what
`crates/transfer` already relies on for timeouts. `dispatch_io` only supports
channel-wide cancel, so a per-file timeout has to translate to "close the
channel for that file", which is acceptable because oc-rsync never multiplexes
multiple file transfers onto a single fd.

## FFI / binding strategy

oc-rsync currently binds Apple-only APIs through `libc`
(`crates/fast_io/src/platform_copy/dispatch.rs:152-211` for `clonefile` /
`fcopyfile`) and `nix` (`crates/apple-fs/Cargo.toml:11`). `libdispatch`
symbols (`dispatch_io_create`, `dispatch_io_read`, `dispatch_io_write`,
`dispatch_io_close`, `dispatch_data_create`, `dispatch_queue_create`,
`dispatch_release`, `dispatch_semaphore_create`, `dispatch_semaphore_wait`,
`dispatch_semaphore_signal`) are not exposed by `libc` 0.2 today.

Three viable FFI paths, in increasing order of invasiveness:

1. **Hand-written `extern "C"` declarations.** Add roughly twenty
   `extern "C" { fn dispatch_io_* }` declarations under
   `crates/fast_io/src/dispatch_io/ffi.rs` linked against the system
   `System` framework (which transitively includes `libdispatch`; no extra
   `#[link]` directive is required because the dynamic loader picks it up
   from `/usr/lib/system/libdispatch.dylib`). This is the same approach used
   for `clonefile` / `fcopyfile` today and is the lowest-friction path. The
   block-pointer parameters (`^{ ... }` ObjC blocks) are the awkward part:
   they are an ABI extension, not a Rust-supported calling convention. The
   workable pattern is to hand-roll a `BlockLiteral<F>` repr-C struct and
   pass `&BlockLiteral as *const c_void` cast to the block parameter. This
   has been done by the `block` and `block2` crates and works on AArch64 and
   x86_64 macOS, but it is `unsafe` and version-fragile.

2. **`block2` + `objc2` ecosystem.** The `block2` crate (re-exported by
   `objc2`) provides `Block<dyn Fn(...)>` types with a stable Rust-side API.
   Apple has migrated their public Rust SDKs onto `objc2`, and the crate is
   actively maintained. Using `block2::Block` for the io_handler and
   cleanup_handler eliminates the hand-rolled block ABI risk. Cost: pulls
   `block2` (~3 KLoC) and `objc2-encode` into the workspace; `fast_io` would
   gain its first ObjC-adjacent dependency. Per project policy, all unsafe
   stays in `fast_io`.

3. **`bindgen` against `<dispatch/dispatch.h>`.** Possible but not
   recommended: libdispatch's headers rely heavily on Clang block syntax,
   `__OSX_AVAILABLE_STARTING` macros, and inline functions, which bindgen
   handles inconsistently. The hand-written + `block2` combo is more
   predictable and matches how the rest of the workspace handles Apple FFI.

**Recommended FFI path:** hand-written `extern "C"` declarations for the
`dispatch_io_*` and `dispatch_data_*` C entry points (which take regular
function pointers for everything except blocks), combined with `block2` for
the io_handler / cleanup_handler block-callback parameters. No `bindgen`,
no `objc2-foundation`. The `block2` crate is a single new dependency under
`[target.'cfg(target_os = "macos")'.dependencies]` in
`crates/fast_io/Cargo.toml`, alongside `libc`.

The `dispatch_io` symbols are part of `libSystem.B.dylib` on every macOS
since 10.7, so there is no runtime availability probe to write - unlike
io_uring's `is_io_uring_available()` (`crates/fast_io/src/io_uring/config.rs`)
or IOCP's `is_iocp_available()` (`crates/fast_io/src/iocp/config.rs`). A
compile-time `#[cfg(all(target_os = "macos", feature = "dispatch_io"))]`
gate is sufficient, with a stub module mirroring
`crates/fast_io/src/iocp_stub.rs` for non-macOS or feature-disabled builds.

## Integration sketch and phase plan

The intended end state mirrors the existing IOCP layout exactly:

```
crates/fast_io/src/
  dispatch_io/
    mod.rs              # public re-exports, parallels iocp/mod.rs
    config.rs           # DispatchIoConfig, DEFAULT_HIGH_WATER, etc.
    channel.rs          # safe wrapper around dispatch_io_t lifetime
    file_writer.rs      # DispatchIoWriter: FileWriter
    file_reader.rs      # DispatchIoReader: FileReader
    file_factory.rs     # DispatchIoOrStdReader/Writer enum + factory
    socket_reader.rs    # optional, parallels io_uring/socket_reader.rs
    socket_writer.rs    # optional
    ffi.rs              # extern "C" + block2 imports
  dispatch_io_stub.rs   # non-macOS / feature-disabled stub
```

`crates/fast_io/src/lib.rs:111-127` already shows the cfg pattern for
io_uring and IOCP; `dispatch_io` would slot in alongside, gated on
`cfg(all(target_os = "macos", feature = "dispatch_io"))`. The capability
list in `platform_io_capabilities()` (`crates/fast_io/src/lib.rs:327-365`)
gains a `dispatch_io` entry under the macOS arm.

The `IoUringPolicy` / `IocpPolicy` enums in `crates/fast_io/src/lib.rs:397-447`
are the model for a `DispatchIoPolicy` (Auto / Enabled / Disabled). CLI flag
naming would be `--dispatch-io` / `--no-dispatch-io`, threading through
`crates/cli` the same way the existing two policies do.

Phase plan, sized to land one PR each:

1. **FFI floor.** Land `crates/fast_io/src/dispatch_io/ffi.rs` plus a
   no-public-API `Channel` wrapper that owns a `dispatch_io_t`, exposes
   safe `read_all` / `write_all` methods, and unit-tests against
   `tempfile`-backed regular files. New dependency: `block2 = "0.5"` under
   `[target.'cfg(target_os = "macos")'.dependencies]`. `fast_io` already has
   `#[allow(unsafe_code)]` on specific functions (`fast_io` is one of the
   five crates listed in CLAUDE.md's "Unsafe Code Policy" as permitted to
   contain unsafe), so no policy change is required.
2. **`FileWriter` impl.** Add `DispatchIoWriter` and the `DispatchIoOrStdWriter`
   factory enum, mirroring `IocpOrStdWriter`. Wire into the `FileWriterFactory`
   trait. Add the `dispatch_io` cargo feature (default-on, like `iocp` and
   `io_uring`).
3. **`FileReader` impl.** Symmetric `DispatchIoReader` for the receiver
   read-side of local-copy.
4. **Engine wiring.** Switch the macOS branch of
   `crates/engine/src/local_copy/executor/file/copy/transfer/execute.rs` to
   prefer `DispatchIoWriter` over `BufWriter` for files above a sentinel
   size (mirror `IOCP_MIN_FILE_SIZE = 64 * 1024` from
   `crates/fast_io/src/iocp/config.rs:23`). The clonefile fast path stays
   first - `dispatch_io` is for the data-copy fallback, never the CoW path.
5. **`AsyncFileWriter` trait integration.** Once the trait from #1655
   stabilizes in `crates/engine/src/async_io/`, add a feature-gated
   `DispatchIoAsyncWriter` impl alongside the tokio-based one in
   `crates/engine/src/async_io/copier.rs`.
6. **Socket path (optional).** Implement `DispatchIoSocketReader/Writer` for
   the SSH-stdio path used by `crates/transport`, paralleling
   `crates/fast_io/src/io_uring/socket_*.rs`. This is the highest-leverage
   piece of the audit - macOS today has no async-network fast path, and
   `DISPATCH_IO_STREAM` over a socket fd is the natural macOS analogue of
   `IORING_OP_SEND` / `IORING_OP_RECV`.

Each phase is independently mergeable and adds zero risk to non-macOS
builds because of the cfg gate.

## Blockers and open questions

- **Block ABI surface area.** The block-pointer ABI for io_handler and
  cleanup_handler is the only `unsafe` surface that does not already exist
  in the workspace. `block2` is well-maintained but adds the workspace's
  first transitive ObjC dependency on macOS. Open question: is the
  maintenance cost of pulling `block2` into `fast_io` acceptable, or does
  the team prefer a hand-rolled `BlockLiteral` (smaller surface, more risk)?
  CLAUDE.md's "Standard library first" rule weighs against `block2`; the
  "no deprecated APIs" and unsafe-confined-to-`fast_io` rules weigh for it.
- **`dispatch_data_t` lifetime vs `BufferPool`.** The most attractive design
  hands `dispatch_io_write` a `dispatch_data_t` whose destructor returns the
  buffer to oc-rsync's `BufferPool`. The `BufferPool` is a
  `Mutex<Vec<Vec<u8>>>` (per CLAUDE.md "Buffer pool contention" note), and
  the destructor block runs on a libdispatch-managed queue, not on the
  receiver thread. Open question: does the destructor's queue reentrancy
  interact poorly with the existing pool's `Mutex`? Worth a microbenchmark
  before phase 2 lands.
- **Cancellation granularity.** `dispatch_io_close(channel, DISPATCH_IO_STOP)`
  is channel-wide; oc-rsync's per-file timeouts would translate to
  channel-per-file. Open question: is the cost of one libdispatch channel per
  open file (a serial queue + a few KB of accounting) acceptable for very
  large flists (>10k files)? io_uring and IOCP both share a single completion
  port across many fds, which is cheaper.
- **Memory-pressure handler ownership.** Subscribing to
  `DISPATCH_SOURCE_TYPE_MEMORYPRESSURE` is process-wide; only one component
  should own it. Today nobody does. Open question: should the memory-pressure
  source live in `fast_io::dispatch_io` (where the consumers are) or in a
  cross-cutting place like `crates/core`?
- **F_FULLFSYNC semantics.** The receiver's `--fsync` flag currently calls
  `File::sync_all`, which is `fsync(2)` on Apple - and Apple `fsync(2)` does
  not flush to platter. Switching to `F_FULLFSYNC` is the right thing on
  APFS but it is an order of magnitude slower. Open question: does the
  `DispatchIoWriter::sync` path adopt `F_FULLFSYNC`, or stay aligned with
  the existing `sync_all` behaviour to avoid a perf regression?
- **`AsyncFileWriter` (#1655) is not yet landed.** Phase 5 is blocked on the
  trait shape from #1655. Phases 1-4 and 6 can land before #1655.
- **Benchmark gap.** The benchmark scripts (`scripts/benchmark.sh`,
  `scripts/benchmark_hyperfine.sh`) only run inside the
  `localhost/oc-rsync-bench:latest` Linux container per CLAUDE.md
  "Containers (Podman)". A macOS-host benchmark harness will need to be
  added in `xtask` to validate that `dispatch_io` actually wins against
  `BufWriter` on APFS - an unbenched optimization is not one we should land.

## Recommendation

Pursue phases 1-4 behind a default-on `dispatch_io` cargo feature, with
runtime fallback to `StdFileWriter` when libdispatch returns an error
(symmetric with how `IocpOrStdWriter` falls back today). Use hand-written
`extern "C"` declarations plus the `block2` crate for the two block
parameters; do not introduce `bindgen` or `objc2-foundation`. Defer phase 5
until #1655 lands the `AsyncFileWriter` trait. Phase 6 (the socket path) is
the highest-value follow-up because oc-rsync has no macOS async-network
fast path today, but it should land only after phases 1-4 have been
validated against an APFS benchmark harness.

The audit's strongest single finding: the FFI cost is bounded
(approximately 200 lines of `extern "C"` plus one new dependency), the
cfg-gated stub pattern is already in place for IOCP and io_uring, and the
existing macOS clonefile fast path is unaffected. There is no architectural
reason to decline the work - the only open questions are about polish
(block crate choice, F_FULLFSYNC, memory-pressure ownership), not about
viability.
