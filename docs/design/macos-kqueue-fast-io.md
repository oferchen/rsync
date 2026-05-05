# macOS kqueue fast-I/O backend (#1385)

Tracking issue: oc-rsync task #1385.

Related design notes and prior evaluations:

- `docs/design/iouring-session-ring-pool.md` (#1409) - the Linux io_uring
  ring-pool model whose lease/return shape this design reuses.
- `docs/design/io-uring-rayon-composition.md` (#1283) - the rayon vs.
  asynchronous-I/O composition rules; this design adopts the same
  invariants for macOS.
- `docs/design/basis-file-io-policy.md` (#1666) - keeps mmap pointers off
  the asynchronous data path; the same rule applies here.
- Prior macOS evaluation work referenced from #1385: dispatch_io
  feasibility (#1653, completed), AsyncFileWriter trait shape (#1655,
  completed), platform fast-path catalogue (#1390, completed), APFS
  clonefile interaction (#1388, completed), and the cross-platform copy
  benchmark (#1659, completed). The pending task #1657 (`F_NOCACHE` plus
  `writev` fallback) is treated below as a complement, not a substitute.

This document is design-only. No code lands in this PR; the entry points,
type stubs, and dispatch sites named here are sketches that follow the
existing `IoUringDiskBatch` and `IocpDiskBatch` shape so the disk-commit
thread keeps a single calling convention across platforms.

## 1. Motivation

oc-rsync ships three batched-write backends today:

- Linux: `IoUringDiskBatch`
  (`crates/fast_io/src/io_uring/disk_batch.rs:45`), gated by
  `#[cfg(all(target_os = "linux", feature = "io_uring"))]`.
- Windows: `IocpDiskBatch`
  (`crates/fast_io/src/iocp/disk_batch.rs:87`), gated by
  `#[cfg(all(target_os = "windows", feature = "iocp"))]`. The wiring gap
  between the type and the disk-commit thread is tracked separately as
  #1868 and is out of scope for this design.
- Everything else, including macOS: the `Buffered` arm of
  `disk_commit::writer::Writer`
  (`crates/transfer/src/disk_commit/writer.rs:141-151`), which is plain
  synchronous `std::fs::File` plus a 256 KB reusable buffer that mirrors
  upstream's `wf_writeBuf` (fileio.c:161).

macOS therefore has no asynchronous disk path. Every chunk handed to the
disk-commit thread crosses a `write(2)` boundary, blocking the thread
for the duration of the transfer to disk. On a Mac mini M2 with NVMe the
median copy throughput trails the upstream rsync 3.4.1 baseline by 5-8%
on the 4 KiB-file mix from the cross-platform benchmark (#1659), where
the kernel's writeback queue could absorb several outstanding writes if
we let it.

### 1.1 Why kqueue and not the alternatives

`kqueue(2)` plus `EVFILT_READ`/`EVFILT_WRITE` registered against a
non-blocking file descriptor is the closest macOS equivalent to
`epoll(7)`. A single kqueue descriptor can multiplex many fds and
delivers level-triggered (or one-shot) readiness events through
`kevent(2)`. The dispatcher thread reaps events, schedules a `pwrite(2)`
or `writev(2)` for each ready fd, and frees the buffer once the syscall
returns. Submission stays in user space; only the readiness signal
crosses the kernel boundary.

The other macOS asynchronous-I/O surfaces were evaluated and rejected:

- **dispatch_io (#1653)**. Provides asynchronous queues at the libdispatch
  level, but it owns the I/O lifecycle, the queue topology, and the
  buffer copies. Wedging it under our buffer-pool and chunk-ownership
  model doubled the bookkeeping for no measurable throughput win in the
  evaluation. The matching PR closed #1653 with "no path forward without
  ceding ownership of the disk-commit thread."
- **`F_NOCACHE` + `writev(2)` (#1657, pending)**. Disables UBC caching for
  the open fd. It removes one memcpy from the kernel side but is still
  synchronous: the dispatching thread blocks on each `writev`. It pairs
  cleanly with kqueue (the fd remains the same), so this design treats
  #1657 as a complement, not an alternative. See section 7.
- **`fcopyfile(3)` (#1659)**. Already wired through
  `crates/fast_io/src/platform_copy/mod.rs:188` for whole-file copies
  where source and destination are both regular files. It does not help
  the delta-apply hot path because the receiver writes assembled chunks,
  not a verbatim source file.
- **APFS `clonefile(2)` (#1388, completed)**. Already wired through
  `try_clonefile` in `platform_copy/mod.rs:159`. Same scope limitation
  as `fcopyfile`: it serves the local-copy executor, not the network
  receiver. Clonefile interaction with the kqueue path is covered in
  section 7.

The conclusion from all three: kqueue is the only surface that maps
naturally onto our existing "submit a chunk, get a completion later"
model without forcing oc-rsync to surrender control of the disk-commit
thread.

## 2. Per-fd kqueue model

### 2.1 Single kqueue, many fds

The macOS backend creates exactly one kqueue descriptor per
`KqueueDiskBatch`, mirroring `IocpDiskBatch`'s single completion port
(`crates/fast_io/src/iocp/disk_batch.rs:88`) and `IoUringDiskBatch`'s
single ring (`crates/fast_io/src/io_uring/disk_batch.rs:46`). The kqueue
fd lives for the entire lifetime of the batch and is reused as files
rotate in via `begin_file`.

For each active output file the batch:

1. Reopens the caller's `File` with `O_NONBLOCK` so `pwrite(2)` returns
   `EAGAIN` instead of blocking when the writeback queue is full. The
   original `File` is held to preserve its lifetime and is returned by
   `commit_file` so the disk-commit thread can rename and finalize it,
   matching `IocpDiskBatch::commit_file`
   (`crates/fast_io/src/iocp/disk_batch.rs:244`).
2. Registers the non-blocking fd with the kqueue using a `kevent` whose
   `filter` is `EVFILT_WRITE`, `flags` is `EV_ADD | EV_CLEAR`
   (one-shot edge-triggered), `udata` is the per-file completion key.
   `EV_CLEAR` is critical: without it, the fd would remain readable on
   every kevent dequeue until userspace explicitly drained it, which is
   the macOS equivalent of ignoring `EPOLLET`.
3. Writes via `pwrite(2)` (or `pwritev(2)` for direct-write chunks) at
   the buffer offset, draining `EAGAIN` returns by re-arming the kevent
   and waiting on the next readiness signal.

### 2.2 Granularity vs. io_uring

kqueue's granularity is coarser than io_uring's by design. io_uring
delivers a CQE (completion queue entry) per submitted SQE, with the
exact bytes transferred and a per-SQE error code
(`crates/fast_io/src/io_uring/disk_batch.rs:255`). kqueue delivers a
single readiness event per `(fd, filter)` pair until that filter is
re-armed. The implication for the batched writer:

- io_uring batches N writes, then `submit_and_wait(N)` reaps N
  completions in one syscall.
- kqueue batches N writes by issuing N `pwrite` calls in user space,
  observing readiness with one `kevent` syscall, and resubmitting only
  the chunks that returned `EAGAIN`.

The expected win on macOS is therefore syscall-count reduction on the
*readiness* path (one kevent serving multiple chunks) plus the kernel
writeback overlap (`pwrite` returns when the page cache accepts the
data, not when the write hits the device). It is not zero-copy and it
does not eliminate the per-chunk syscall, which is io_uring's headline
feature.

### 2.3 Coexistence with rayon

The kqueue dispatcher runs on the existing single disk-commit thread
(`crates/transfer/src/disk_commit/thread.rs:53`). Rayon workers do not
interact with the kqueue fd: they pass `FileMessage::Chunk` items
through the SPSC channel, exactly as today. This preserves the
"composition by ownership" invariant from #1283: the asynchronous
backend has one owner, and it is the disk-commit thread.

The kqueue fd is `Send` but not `Sync`. The batch type is therefore
`!Sync` and `Send` only when no `current_file` is registered, matching
`IoUringDiskBatch` and `IocpDiskBatch`. Trying to share a kqueue across
the rayon pool would re-introduce the
"every worker has its own ring" pathology called out in #1283 section
2; we explicitly do not do that.

## 3. Mapping to the AsyncFileWriter trait (#1655)

Task #1655 settled the trait shape for asynchronous file writers. The
implemented surface lives in `crates/fast_io/src/traits.rs`:

- `FileWriter::bytes_written` (`traits.rs:40`),
- `FileWriter::sync` (`traits.rs:43`),
- `FileWriter::preallocate` (`traits.rs:46`),
- the standard `Write::write`/`Write::flush` methods,
- `FileWriterFactory::create` and `create_with_size` for construction
  (`traits.rs:62-69`).

`KqueueDiskBatch` implements the `Write` half of the trait via the
existing pattern used by `IoUringDiskBatch::write`
(`crates/fast_io/src/io_uring/disk_batch.rs:287`) and
`IocpDiskBatch::write` (`crates/fast_io/src/iocp/disk_batch.rs:337`):
each `Write::write` call delegates to a `write_data` helper that
buffers up to the configured chunk size and flushes via the kqueue
dispatcher when full.

| Trait method | kqueue implementation |
|---|---|
| `Write::write` | Buffers internally; flushes via `pwrite` + kevent when the buffer fills, mirroring `IoUringDiskBatch::write_data`. |
| `Write::flush` | Drains all pending `pwrite` chunks for the current fd, including any `EAGAIN` re-arms. |
| `FileWriter::bytes_written` | Returns the cumulative drained byte count; pending buffer bytes are exposed via `bytes_written_with_pending` for parity with the io_uring and IOCP backends. |
| `FileWriter::sync` | Calls `fcntl(fd, F_FULLFSYNC)` (or `fsync(fd)` if `F_FULLFSYNC` returns `ENOTSUP`, e.g. on tmpfs) after draining. `F_FULLFSYNC` is the macOS equivalent of `fdatasync` plus a barrier, and is the only way to guarantee data hits stable storage on Apple SSDs. |
| `FileWriter::preallocate` | Uses `fcntl(fd, F_PREALLOCATE)` with `F_ALLOCATEALL | F_ALLOCATECONTIG`, falling back to `ftruncate` when contiguous allocation is unavailable. |

The factory side is straightforward: a `KqueueWriterFactory` returns
`KqueueOrStdWriter` from `create` and `create_with_size`. When the
runtime probe (section 4.3) reports kqueue unavailable - e.g. inside a
sandbox or on a non-macOS target - the factory returns the `Std` variant
just like `IocpReaderFactory` does today
(`crates/fast_io/src/iocp/file_factory.rs`).

## 4. API surface

The public types live in `crates/fast_io/src/kqueue/disk_batch.rs`,
under a new sibling module to the io_uring and IOCP trees.

### 4.1 Module layout

```text
crates/fast_io/src/kqueue/
    mod.rs                  -- re-exports + availability probe
    config.rs               -- KqueueConfig (chunk_size, max_in_flight, ...)
    completion_loop.rs      -- kevent dispatcher
    disk_batch.rs           -- KqueueDiskBatch (this design)
    file_writer.rs          -- KqueueWriter (analog to IoUringWriter)
    file_factory.rs         -- KqueueWriterFactory
```

Stub layout for non-macOS targets:

```text
crates/fast_io/src/kqueue_stub.rs   -- KqueueDiskBatch::try_new -> None
                                       on every other platform
```

`crates/fast_io/src/lib.rs:112-128` already demonstrates the
`#[cfg]`-gated module re-export pattern this follows.

### 4.2 Public type sketch

```rust
// Sketch only - not implemented in this PR.

/// Batched kqueue disk writer for the disk-commit phase on macOS.
///
/// Owns a single kqueue descriptor reused across every file processed
/// by the batch. Mirrors the public surface of [`IoUringDiskBatch`] and
/// [`IocpDiskBatch`] so the disk-commit thread uses one calling
/// convention on all platforms.
///
/// Not `Sync`; designed for single-threaded use on the dedicated disk
/// commit thread.
pub struct KqueueDiskBatch {
    kq_fd: OwnedFd,
    config: KqueueConfig,
    current_file: Option<ActiveKqFile>,
    buffer: Vec<u8>,
    buffer_pos: usize,
    next_completion_key: usize,
}

impl KqueueDiskBatch {
    pub fn new(config: &KqueueConfig) -> io::Result<Self>;

    #[must_use]
    pub fn try_new(config: &KqueueConfig) -> Option<Self>;

    pub fn begin_file(&mut self, file: File) -> io::Result<()>;
    pub fn write_data(&mut self, data: &[u8]) -> io::Result<()>;
    pub fn flush(&mut self) -> io::Result<()>;
    pub fn commit_file(&mut self, do_fsync: bool) -> io::Result<(File, u64)>;

    #[must_use] pub fn bytes_written(&self) -> u64;
    #[must_use] pub fn bytes_written_with_pending(&self) -> u64;
}

impl Write for KqueueDiskBatch { ... }
impl Drop for KqueueDiskBatch { ... }   // best-effort flush + close
```

Each method's contract is identical to the io_uring and IOCP analogues:

- `begin_file` flushes the previous file, deregisters its fd from the
  kqueue (`EV_DELETE`), reopens the new file with `O_NONBLOCK`, and
  registers it with `EV_ADD | EV_CLEAR`. Mirrors
  `IocpDiskBatch::begin_file` (`crates/fast_io/src/iocp/disk_batch.rs:163`).
- `commit_file(do_fsync)` flushes pending data, optionally calls
  `F_FULLFSYNC`, deregisters the fd, and returns the original `File`
  handle so the caller can rename it. Mirrors
  `IocpDiskBatch::commit_file`
  (`crates/fast_io/src/iocp/disk_batch.rs:244`).
- `try_new` returns `None` if the kqueue probe (section 4.3) fails or
  on any non-macOS target. Production callers must treat `None` as
  "fall back to the buffered writer."

### 4.3 Runtime probe

A `kqueue::is_kqueue_available()` function performs a one-time
`OnceLock` probe identical to `is_io_uring_available()`
(`crates/fast_io/src/io_uring/mod.rs:151`) and `is_iocp_available()`:

1. Compile-time gate: `#[cfg(target_os = "macos")]`. Other targets
   return `false` from the stub.
2. Runtime gate: open a kqueue with `kqueue()`, register an EOF event
   on a self-pipe to confirm `kevent(2)` is reachable, close the
   descriptor. Cache the result.

The probe runs once per process, exactly like the io_uring probe at
`crates/fast_io/src/io_uring/config.rs:381`.

## 5. Wiring into transfer/disk_commit

The disk-commit thread already supports two batched backends behind
mutually-exclusive policy fields:

- `DiskCommitConfig::io_uring_policy`
  (`crates/transfer/src/disk_commit/config.rs:80`),
- `DiskCommitConfig::iocp_policy`
  (`crates/transfer/src/disk_commit/config.rs:89`).

A third policy field is added for kqueue:

```rust
// Sketch only.
pub kqueue_policy: fast_io::KqueuePolicy,
```

`KqueuePolicy` matches `IoUringPolicy` and `IocpPolicy` exactly
(`crates/fast_io/src/lib.rs:404,437`): `Auto` (default) /
`Enabled` / `Disabled`. The dispatch lives in
`disk_thread_main`
(`crates/transfer/src/disk_commit/thread.rs:164`):

```rust
// Sketch only. Mirrors the existing io_uring + iocp dispatch at
// thread.rs:171-179.
let mut disk_batch = try_create_disk_batch(config.io_uring_policy);
let mut iocp_batch = if disk_batch.is_none() {
    try_create_iocp_batch(config.iocp_policy)
} else {
    None
};
let mut kqueue_batch = if disk_batch.is_none() && iocp_batch.is_none() {
    try_create_kqueue_batch(config.kqueue_policy)
} else {
    None
};
```

The three backends are mutually exclusive by platform: io_uring is
Linux-only, IOCP is Windows-only, kqueue is macOS-only. The cascade
above keeps the invariant explicit and exposes the same fall-through
"none of the above, use buffered" path that
`disk_thread_main` already implements.

`process_file`/`process_whole_file` grow a third optional batch
parameter (`crates/transfer/src/disk_commit/process.rs:38-39`),
and `make_writer` adds a `Writer::Kqueue { batch }` variant alongside
`Writer::IoUring` and `Writer::Iocp` in
`crates/transfer/src/disk_commit/writer.rs:141-151`. The variant is
gated on `#[cfg(target_os = "macos")]`. As with the io_uring and IOCP
variants, sparse mode is steered onto the buffered writer because the
kqueue batch does not implement `Seek` (sparse-write hole punching
requires `seek(SeekFrom::Current(n))`, which conflicts with the
asynchronous offset bookkeeping). The existing
`Writer::buffered_for_sparse` accessor
(`crates/transfer/src/disk_commit/writer.rs:160`) gains the
corresponding cfg arm.

CLI flag plumbing reuses the policy precedent set by `--io-uring` /
`--no-io-uring`. The user-facing flag would be `--kqueue` /
`--no-kqueue`, gated to macOS. The default is `Auto`, which means "use
kqueue if the probe succeeds." Disabled is for benchmarking and for
sandboxes that block `kqueue(2)`.

## 6. Limitations vs. io_uring

kqueue is structurally weaker than io_uring on three axes that matter
for the disk-commit hot path. Acknowledging them up front prevents the
backend from being marketed as parity with the Linux path.

### 6.1 No zero-copy

io_uring offers `IORING_OP_WRITE_FIXED` over registered buffers
(`crates/fast_io/src/io_uring/file_writer.rs:56-64`), eliminating
per-SQE `get_user_pages` accounting. kqueue has no equivalent: every
`pwrite` copies from user space into the page cache. The only
mitigations on macOS are `F_NOCACHE` (write-through, bypassing the
unified buffer cache) and `mmap` plus `msync`; neither is a true
zero-copy primitive and `mmap` is excluded by the basis-file policy
(`docs/design/basis-file-io-policy.md`, F2).

### 6.2 No SQPOLL equivalent

io_uring's `IORING_SETUP_SQPOLL` lets a kernel thread poll the
submission queue, removing the `io_uring_enter` syscall from the
submission path entirely. kqueue submissions always cross the syscall
boundary. The expected delta on a busy disk-commit thread is roughly
one `pwrite` syscall per chunk plus one `kevent` syscall per readiness
batch.

### 6.3 Less batched-submission efficiency

On Linux, `submit_and_wait(N)` issues N writes in a single syscall and
reaps N completions in another single syscall. On macOS, N writes are
N `pwrite` syscalls; only the *readiness* polling is batched. For
chunks larger than the page-cache granule (typical literal tokens are
32 KB or more) the syscall overhead amortizes well, but for
delta-heavy workloads with many small chunks the ratio is less
favourable.

### 6.4 Expected throughput delta

The cross-platform copy benchmark (#1659, completed) measured
`oc-rsync` vs upstream rsync 3.4.1 on local copies. With the buffered
backend, macOS lagged Linux by 5-8% on the 4 KiB-file mix and was on
par on the 1 MiB-and-up mixes. Modelling the kqueue path (one extra
syscall amortized over 64 chunks, no zero-copy):

- 4 KiB-file mix: expected to close most of the 5-8% gap by removing
  per-chunk blocking, leaving 2-3% behind Linux io_uring after
  accounting for the F_FULLFSYNC barrier on commit.
- Larger files: marginal. The buffered backend already saturates the
  NVMe write bandwidth for 1 MiB chunks.

These numbers are projections, not measurements. They are validated
against the same benchmark hardware used in #1659; section 8 covers
the reproduction plan. Any benchmark comparison against Linux io_uring
must explicitly call out hardware differences (M2 Mac mini vs Linux
x86_64 CI runner): the absolute numbers are not directly comparable.

## 7. Open questions

### 7.1 APFS clonefile interaction (#1388, completed)

The local-copy executor calls `try_clonefile`
(`crates/fast_io/src/platform_copy/mod.rs:159`) for whole-file copies
on APFS. The receiver, which is the kqueue path's only consumer, never
touches `clonefile`: it always opens a fresh temporary file and writes
delta-applied chunks. The two paths therefore do not interact at the
file level, but they share `try_create_kqueue_batch` lifecycle
ownership of the disk-commit thread, so the open question is whether
the local-copy fast paths should *also* drain through the kqueue
batch when the destination filesystem is non-APFS. The answer is "not
in this design": local-copy is single-syscall per file when clonefile
or fcopyfile succeeds, and falling back to a kqueue-batched
`std::fs::copy` adds bookkeeping for no measurable win. We revisit
this only if profiling on a non-APFS macOS volume (e.g. external
NTFS) shows local-copy as the bottleneck.

### 7.2 F_NOCACHE + writev fallback (#1657, pending)

`F_NOCACHE` instructs the kernel to bypass the unified buffer cache
on subsequent writes. It is **complementary** to kqueue, not an
alternative: a fd opened with `O_NONBLOCK | F_NOCACHE` and registered
with the kqueue still receives readiness signals through `kevent`,
and `pwrite` returns once the kernel has issued the write to the
device rather than once the page cache accepts it. The combination
trades latency-per-syscall for throughput when the working set
exceeds the cache.

The decision rule is the same as
`docs/design/basis-file-io-policy.md` uses for mmap: turn on
`F_NOCACHE` only when the destination filesystem can absorb
unbuffered writes without amplification (APFS on NVMe: yes; SMB
mounts: no). The probe lives in #1657 and lands independently. When
both #1657 and this design are in tree, `KqueueConfig` gains a
`f_nocache: bool` field that defaults to `Auto` (probe-driven).

### 7.3 SIGPIPE on file descriptors

Unlike sockets, kqueue file descriptors do not raise `SIGPIPE` on
write failure, but the disk-commit thread already runs with `SIGPIPE`
masked via the standard library's signal-handling guarantees on
Unix. No additional masking is required, but the test harness
(section 8) includes a regression test that validates this remains
true under `kqueue` plus `F_FULLFSYNC`.

### 7.4 Sandboxes that block kqueue

Some macOS sandbox profiles block `kqueue(2)` outright. The runtime
probe (section 4.3) catches this and returns `Disabled`, falling
through to the buffered writer. Open question: do we also surface a
log line at `-vv` saying "kqueue: unavailable (sandbox)"? The
io_uring path does this via
`fast_io::io_uring_availability_reason()`
(`crates/fast_io/src/lib.rs:234`); the kqueue path adopts the same
pattern.

## 8. Test strategy

### 8.1 CI matrix coverage

Existing required CI checks (per project policy): fmt+clippy, nextest
(stable), Windows (stable), macOS (stable), Linux musl (stable). The
macOS leg of the matrix is the only one that exercises the kqueue
path. Coverage targets:

- macOS stable: full kqueue test suite, including `F_FULLFSYNC`
  paths and probe-failure fallback.
- Linux musl, Windows stable: stub-only coverage. The stub returns
  `None` from `try_new` and the integration tests assert that
  `make_writer` selects the buffered or platform-native backend.

The same `#[cfg(target_os = "macos")]` gating used by the existing
fast_io modules
(`crates/fast_io/src/lib.rs:112,124`) keeps Linux and Windows builds
warning-free. Imports that exist only for the macOS arms must be
cfg-gated to avoid the unused-import warnings called out in the
project's Cross-Platform Compilation pitfall list.

### 8.2 Reused test harness

The existing fast_io test harness (`crates/fast_io/src/io_uring/disk_batch.rs:305`
and `crates/fast_io/src/iocp/disk_batch.rs:632`) provides a template
for the kqueue test module. Each test gets a `tempdir`, opens a
writable target file, exercises one of: single-file write/commit,
multi-file sequential rotation, large-write-exceeds-buffer, fsync
path, drop-without-commit, and the `Write` trait round-trip.

The kqueue test module reuses the same scenarios verbatim, with one
addition: a `kqueue_eagain_resubmits_pending_chunks` test that
forces `pwrite` to return `EAGAIN` (by capping the write buffer
in `KqueueConfig`) and asserts that all bytes land before the
file is committed.

### 8.3 Integration tests

Reuse the disk-commit integration tests in
`crates/transfer/src/disk_commit/tests.rs`. The tests already
parameterize on backend selection through `DiskCommitConfig`; the
kqueue arm becomes the macOS default once `kqueue_policy` is added.

The interop matrix (`tools/ci/run_interop.sh`) does not need new
fixtures: the kqueue path is invisible to the wire protocol. We add
a single sanity test that runs a 100 MB transfer with
`kqueue_policy = Auto` and asserts the bytes match.

### 8.4 Performance benchmarks

`scripts/benchmark.sh` and `scripts/benchmark_hyperfine.sh` already
cover the local-copy and SSH paths. Add a macOS-specific run that:

1. Captures buffered-writer baseline (today's behaviour).
2. Captures kqueue-writer numbers with `kqueue_policy = Enabled`.
3. Reports the delta against upstream rsync 3.4.1 on the same host.

Hardware is the M2 Mac mini already used for #1659. The benchmark
report explicitly notes that the io_uring numbers from the Linux CI
runner are *not* directly comparable: comparing across hardware is
an ongoing project tracked under #1386 (Linux/macOS/Windows perf gap
benchmark).

### 8.5 Property tests

A property test in
`crates/fast_io/src/kqueue/disk_batch.rs::tests` exercises the
buffer-fill and `EAGAIN`-resubmit paths with random chunk sizes,
asserting that the on-disk byte sequence equals the input
concatenation. Same shape as the io_uring `large_write_exceeds_buffer`
test (`crates/fast_io/src/io_uring/disk_batch.rs:407`) generalised
to a quickcheck-style harness.

## 9. Non-goals

This design intentionally does **not**:

- Replace `dispatch_io`. The dispatch_io evaluation (#1653) closed
  with "no path forward"; this design does not revisit that decision
  and does not duplicate libdispatch's semantics.
- Provide a cross-platform asynchronous-I/O abstraction. The
  trait-shaping work was settled in #1655; this design implements
  that trait on macOS rather than introducing a new one.
- Modify the wire protocol. The kqueue backend is a host-side I/O
  optimisation. Wire-format compatibility with upstream rsync 3.4.1
  is unchanged.
- Wire IOCP into the disk-commit thread. The `IocpDiskBatch` wiring
  gap is tracked separately as #1868 and is independent of macOS
  work.
- Cover the SSH transport's stdio-pipe path. The audit in
  `docs/audits/ssh-socketpair-vs-pipes.md` (#1938) keeps that path
  on synchronous reads; kqueue does not change the calculus there.
- Add CLI flags for kqueue tuning beyond `--kqueue` / `--no-kqueue`.
  Buffer sizes, in-flight depth, and `F_NOCACHE` follow the
  `IocpConfig` pattern of compile-time defaults that can be
  overridden through `KqueueConfig` from a future configuration
  file, not via CLI.

## 10. Migration sequence

The phased plan keeps the macOS path runnable at every step.

**Phase 1 - Trait scaffolding (no behavioural change).** Add
`KqueuePolicy` to `fast_io::lib.rs` next to `IoUringPolicy` and
`IocpPolicy`. Add `kqueue_policy: KqueuePolicy` to
`DiskCommitConfig`. Add `try_create_kqueue_batch` to
`disk_commit::thread.rs` returning `None` from a `kqueue_stub.rs`.
This phase is a pure refactor - no kqueue syscalls yet - so it can
ship before any of the dependent tasks complete.

**Phase 2 - F_NOCACHE plus writev fallback (#1657).** Lands the
unbuffered-write probe in `fast_io`. The result is exposed as
`fast_io::macos::is_f_nocache_available()`. No kqueue interaction
yet; `KqueueConfig::f_nocache` lives but defaults to `Disabled`.

**Phase 3 - AsyncFileWriter trait re-confirmation (#1655,
completed).** Validate the trait shape against the kqueue
implementation sketches in section 3. If a new method is required
(e.g. a cancellation hook), file an addendum task and resolve it
before phase 4. The trait is already in tree; this phase confirms
the kqueue implementation slots into it without modification.

**Phase 4 - KqueueDiskBatch core.** Implement
`crates/fast_io/src/kqueue/disk_batch.rs` per section 4. Land the
runtime probe and stub. Add the test module from section 8.2. This
phase is gated behind `--kqueue` (Default `Disabled`) so it ships
without changing default behaviour.

**Phase 5 - Default `Auto` and benchmark (#1659, #1386).** Flip
`KqueuePolicy::default()` to `Auto`. Run the benchmark suite from
section 8.4 on the M2 Mac mini, capture the delta, and update the
release notes. Cross-reference #1386 for the cross-platform gap
analysis once both kqueue (this task) and IOCP wiring (#1868) are
default-on.

**Phase 6 - Tuning.** Once production telemetry is available
(internal use, then opt-in user reports), revisit `KqueueConfig`
defaults: chunk size, in-flight depth, and `F_NOCACHE` activation
threshold. This phase is open-ended and lives outside the closing
of #1385.

## 11. References

- `crates/fast_io/src/io_uring/disk_batch.rs:45` -
  `IoUringDiskBatch` struct, public surface this design mirrors.
- `crates/fast_io/src/iocp/disk_batch.rs:87` - `IocpDiskBatch`
  struct, the Windows analogue.
- `crates/fast_io/src/traits.rs:38-49` - `FileWriter` trait the
  kqueue backend implements.
- `crates/fast_io/src/lib.rs:112-128` - module gating template for
  io_uring and IOCP, reused for kqueue.
- `crates/fast_io/src/lib.rs:404,437` - `IoUringPolicy` and
  `IocpPolicy`, template for `KqueuePolicy`.
- `crates/transfer/src/disk_commit/thread.rs:164` -
  `disk_thread_main`, the dispatch site.
- `crates/transfer/src/disk_commit/config.rs:80,89` -
  `io_uring_policy` and `iocp_policy`, template for
  `kqueue_policy`.
- `crates/transfer/src/disk_commit/writer.rs:141-151` - `Writer`
  enum, gains a `Kqueue` variant on macOS.
- `crates/transfer/src/disk_commit/process.rs:38-39` -
  `process_file` parameter list, gains an optional kqueue batch.
- `crates/fast_io/src/platform_copy/mod.rs:159,188` - existing
  macOS fast paths (`clonefile`, `fcopyfile`) for the local-copy
  executor; orthogonal to the kqueue receiver path.
- `docs/design/iouring-session-ring-pool.md` - ring-pool model
  whose lease/return shape this design generalises to kqueue.
- `docs/design/io-uring-rayon-composition.md` - rayon composition
  invariants reused here.
- `docs/design/basis-file-io-policy.md` - mmap exclusion rule that
  applies equally to the kqueue path.
