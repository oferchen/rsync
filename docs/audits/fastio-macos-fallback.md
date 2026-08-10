# fast_io fallback path on macOS vs Linux (#1652)

Tracking issue: oc-rsync task #1652. Static, source-grounded audit of the
macOS fast_io fallback path. No runtime traces or benchmark numbers were
collected for this document; every quantitative claim is derived from
syscall semantics and source-level behaviour and is tagged inferred.

Scope: identify the macOS-specific code paths in `crates/fast_io/src/`,
contrast them against the Linux fast paths used by the Linux build,
quantify the structural performance gap, point at where async machinery
would close that gap, and recommend cross-platform micro-benchmarks.

Companion docs (do not duplicate, this audit references):

- `docs/audits/fast-io-fallback-macos-vs-linux.md` - longer source
  walk and per-syscall budget for the same gap.
- `docs/audits/macos-dispatch-io.md` - #1653, dispatch_io evaluation
  that decided against landing that surface.
- `docs/design/macos-kqueue-fast-io.md` - #1385, the kqueue backend
  that would close the receiver disk-commit gap.

## 1. macOS code paths in `crates/fast_io/src/`

### 1.1 Whole-file copy (#1388, shipped)

Single source file owns the macOS dispatch:
`crates/fast_io/src/platform_copy/dispatch.rs`.

- `platform_copy_impl` for macOS at line 62 dispatches in priority order
  `clonefile_impl` (line 149) -> `fcopyfile_impl` (line 184) ->
  `std::fs::copy`.
- `clonefile_impl` (line 149) wraps `clonefile(2)` for APFS copy-on-write.
  Returns `Ok(())` for instant CoW or `EXDEV`/`ENOTSUP` to fall through.
- `fcopyfile_impl` (line 184) wraps `fcopyfile(3)` with `COPYFILE_DATA`
  for kernel-side data copy on HFS+, NFS, SMB. Single syscall per file
  with internal kernel chunking.
- Non-macOS arms (lines 212, 220) return synchronous `io::Error` so the
  caller hits `std::fs::copy`.

This path is healthy. Both `clonefile` and `fcopyfile` complete inside
the kernel; the local-copy executor does not see user-space chunking.

### 1.2 Receiver disk-commit chunk writes

`crates/transfer/src/disk_commit/writer.rs:141` defines the only
`Writer` enum used during transfer:

- `Writer::Buffered` - always available, wraps `ReusableBufWriter` over
  the disk thread's permanent 256 KiB scratch buffer.
- `Writer::IoUring { batch }` - gated `cfg(all(target_os = "linux",
  feature = "io_uring"))`.
- `Writer::Iocp { batch }` - gated `cfg(all(target_os = "windows",
  feature = "iocp"))`.

There is no `Writer::Kqueue` or `Writer::DispatchIo`. On macOS the only
branch that fires for delta-apply chunk writes is `Writer::Buffered`.
Every chunk handed to `write_chunk` becomes a synchronous `pwrite(2)`
when `BufWriter` flushes; the disk-commit thread blocks for each
syscall.

### 1.3 Network paths

- `crates/fast_io/src/sendfile.rs:47-244` - all Linux-gated. The macOS
  arm at line 157 (`cfg(all(unix, not(target_os = "linux")))`) falls
  through to `copy_via_fd_write`, a `read(2)` + `write(2)` loop. Apple's
  `sendfile(2)` (different signature from Linux) is not invoked.
- `crates/fast_io/src/splice.rs` - all Linux-gated; macOS has no
  `splice(2)`. The non-Linux arm returns `Unsupported`.

### 1.4 Stubs that gate out the async backends on macOS

- `crates/fast_io/src/io_uring_stub.rs:25` -
  `is_io_uring_available()` always returns `false`.
- `crates/fast_io/src/iocp_stub.rs:24` - keeps `IOCP_MIN_FILE_SIZE`
  for ABI symmetry; nothing constructs an `IocpDiskBatch`.

### 1.5 Capability advertisement

`crates/fast_io/src/lib.rs:355` (`platform_io_capabilities`) advertises
exactly two macOS capabilities: `clonefile` and `fcopyfile`. There is
no `kqueue`, `dispatch_io`, `aio`, or `F_NOCACHE` entry today.

## 2. Linux fast paths and their macOS analogues

| Linux fast path | Source | macOS analogue today | Status on macOS |
|---|---|---|---|
| `io_uring` disk-commit batch | `crates/fast_io/src/io_uring/disk_batch.rs` | none | absent |
| `IORING_REGISTER_BUFFERS` | `crates/fast_io/src/io_uring/registered_buffers.rs` | none | absent |
| `IORING_REGISTER_FILES` | `crates/fast_io/src/io_uring/file_factory.rs` | none | absent |
| Provided buffer ring (`IORING_REGISTER_PBUF_RING`) | `crates/fast_io/src/io_uring/buffer_ring.rs` | none | absent |
| `splice(2)` socket -> file | `crates/fast_io/src/splice.rs:162` | none | absent (no `splice` on Darwin) |
| `sendfile(2)` file -> socket | `crates/fast_io/src/sendfile.rs:147` | Apple `sendfile(2)` (different ABI) | not wired |
| `copy_file_range(2)` same-fs copy | `crates/fast_io/src/copy_file_range.rs:86` | `clonefile(2)` (CoW) + `fcopyfile(3)` | shipped (#1388) |
| `FICLONE` / Btrfs reflink | `dispatch.rs:501` | `clonefile(2)` | shipped (#1388) |
| `O_TMPFILE` -> `linkat` atomic commit | `crates/fast_io/src/o_tmpfile/`, `io_uring/linkat.rs` | rename of `tempfile`-named sibling | functional, not zero-link |
| `copy_file_range` cross-fs fallback | `dispatch.rs:36` (CFR_THRESHOLD = 64 KiB) | `fcopyfile(3)` | shipped |

Whole-file copy reaches structural parity through `clonefile` /
`fcopyfile`. The receiver disk-commit chunk write and the network read
path do not.

## 3. Performance gap evidence

All numbers below are inferred from syscall semantics and source.
Actual baselines are tracked under #1659 (M2 Mac mini) and #1386
(cross-architecture comparison parked).

### 3.1 Per-file syscall budget, 1 MiB file

Linux io_uring with `register_files = true`, `register_buffers = true`,
`sq_entries = 32`, `DEFAULT_BUFFER_CAPACITY = 256 KiB`
(`io_uring/disk_batch.rs:32`):

- 0 syscalls for ring setup (per-thread, amortised).
- 1 `IORING_REGISTER_FILES_UPDATE` per file rotation in `begin_file`.
- ceil(1 MiB / 256 KiB) = 4 SQEs carrying `IORING_OP_WRITE_FIXED`,
  packed into one `io_uring_enter(2)`. With `SQPOLL` enabled, drops to
  zero submission syscalls.
- 0-1 `fsync`.

Budget: 2-4 syscalls per 1 MiB file without `SQPOLL`; 1-2 with
`SQPOLL`.

macOS `Writer::Buffered`:

- 4 `pwrite(2)` calls when the 256 KiB `BufWriter` flushes; potentially
  more when the destination's cluster size shears the chunk.
- 0-1 `fcntl(F_FULLFSYNC)` for `--fsync`.

Budget: 4-5 syscalls per 1 MiB file, none batched, all blocking the
disk-commit thread.

### 3.2 Per-file syscall budget, 4 KiB file

The small-file mix is the worst case. Each transfer is `open` + one
`pwrite` + `close`.

- Linux io_uring: the `pwrite` becomes one slot in a batched SQE
  submission. `submit_and_wait(N)` reaps N completions in a single
  syscall, and `IORING_OP_OPENAT` / `IORING_OP_CLOSE` further amortise
  open and close. A 1000-file 4 KiB transfer fits in roughly 32 ring
  drains.
- macOS: 1000 individual `open`, 1000 `pwrite`, 1000 `close` syscalls
  with no overlap.

Wall-clock impact (inferred): the macOS path leaves throughput on the
table on small-file workloads (every chunk is a syscall, no userspace
pipelining of the writeback queue) and is competitive on large-file
workloads where the syscall amortises over the bytes copied.

### 3.3 Why `BufWriter` is not "free"

`std::io::BufWriter` coalesces small writes but does not overlap them
with the device. Each `BufWriter::flush` is a synchronous `write` on a
single fd. The disk-commit thread is therefore latency-bound on the
kernel's writeback acceptance, not throughput-bound on NVMe. Linux
io_uring's batched submission lets the kernel walk the SQ and queue
several writes into the writeback path in one transition; that overlap
is missing on macOS.

### 3.4 Windows reference point

The Windows side closed exactly this gap under PR #3698 (issue #1868).
`Writer::Iocp { batch }` keeps `concurrent_ops = 4` overlapped writes in
flight per file and reaps completions through
`GetQueuedCompletionStatusEx` in batches of 64. The shape is identical
to `IoUringDiskBatch` on Linux, with overlapped IOCP playing the role
of io_uring. macOS today is the only target without this structure.

## 4. Where async would close the gap

`docs/audits/macos-dispatch-io.md` (#1653) evaluated `dispatch_io` and
recommended pursuing it behind a default-on cargo feature with runtime
fallback to `StdFileWriter`. Three concrete observations from that
audit map directly onto the gap above:

### 4.1 Disk-commit chunk writes

`DispatchIoWriter` would mirror `IocpWriter` and `IoUringWriter`:
sequential `Write::write` accumulates into a per-writer buffer; on
flush wrap in `dispatch_data_t` via `dispatch_data_create` and call
`dispatch_io_write(channel, offset, data, queue, handler)`. This
removes the per-chunk synchronous `pwrite`. Several writes can be
in-flight against one channel; the io_handler block delivers partial
completions on the channel queue.

Closing the structural gap from section 3.1: where macOS today emits
4 blocking `pwrite` calls per 1 MiB file, `dispatch_io` would emit one
queue submission and reap completions asynchronously. The 4 KiB-file
mix benefits even more because the syscall-per-chunk overhead vanishes.

### 4.2 Socket read path

The dispatch_io audit's phase 6 (highest-leverage follow-up) wires
`DISPATCH_IO_STREAM` over a socket fd as the macOS analogue of
`IORING_OP_RECV`. Today the receiver socket read path on macOS is
`read(2)` in a loop, identical to the disk write path.

### 4.3 Buffer-pool integration

The dispatch_io audit flags one open question (line 295): the
`dispatch_data_t` destructor block runs on a libdispatch-managed queue,
not the receiver thread, and `BufferPool` is `Mutex<Vec<Vec<u8>>>`.
Reentrancy interaction is unmeasured; section 5.5 below proposes a
microbenchmark.

### 4.4 What async cannot fix

Neither `dispatch_io` nor a kqueue backend gives macOS true zero-copy.
On Linux, `IORING_OP_SPLICE` and `splice(2)` move bytes from socket to
file without crossing user space; macOS has no equivalent primitive.
The async win is removing per-chunk blocking and overlapping writeback,
not eliminating the user-space copy.

The kqueue design at `docs/design/macos-kqueue-fast-io.md` projects
"close most of the 5-8% gap by removing per-chunk blocking, leaving
2-3% behind Linux io_uring after accounting for the F_FULLFSYNC barrier
on commit."

## 5. Recommended cross-platform micro-benchmarks

Place under `crates/fast_io/benches/` (criterion-based) and
`xtask::bench` for harness integration. Each benchmark must run on
Linux, macOS, and Windows so the cross-platform delta is comparable.

### 5.1 Single-file disk write throughput vs file size

- File sizes: 4 KiB, 64 KiB, 256 KiB, 1 MiB, 16 MiB, 256 MiB.
- Backends per platform: `Writer::Buffered` (always), `Writer::IoUring`
  (Linux), `Writer::Iocp` (Windows). Future: `Writer::DispatchIo`,
  `Writer::Kqueue`.
- Metric: bytes/sec, syscalls/sec (`dtruss -c -t pwrite` on macOS,
  `strace -c -e write` on Linux).
- Goal: prove the per-chunk syscall claim in section 3.1; locate the
  sweet-spot chunk size on APFS NVMe.

### 5.2 N-file 4 KiB-file mix throughput

- N = 1000, 10000, 100000.
- Backends: same as 5.1.
- Metric: total wall-clock, syscalls/file, peak RSS.
- Goal: quantify the small-file penalty on macOS vs Linux io_uring's
  batched SQE submission.

### 5.3 Fsync barrier cost

- Variants: no fsync, `fsync(2)`, `fcntl(F_FULLFSYNC)` on macOS.
- File size: 1 MiB, 256 MiB.
- Backend: `Writer::Buffered`.
- Goal: measure the `F_FULLFSYNC` premium so the kqueue design's
  projection in section 4.4 has a concrete number.

### 5.4 `pwritev(2)` vs N x `pwrite(2)`

- 4 iovecs of 64 KiB each.
- macOS only.
- Goal: quantify the #1657 "writev batching" mitigation's payoff in
  isolation from any async backend.

### 5.5 Buffer-pool destructor reentrancy

- `dispatch_data_create` with destructor returning the buffer to
  `BufferPool::Mutex<Vec<Vec<u8>>>`.
- Concurrency: 1, 4, 16 in-flight writes.
- Metric: contention on the pool mutex (perf counters or
  `pthread_mutex_lock` wait time).
- Goal: answer the dispatch_io audit's open question 4.3 before phase 2
  of #1653 lands.

### 5.6 Cross-platform parity baseline

- Identical 100 MiB tarball-of-source-tree workload on Linux x86_64,
  macOS aarch64 (M2), Windows x86_64.
- Tools: `hyperfine` driving `oc-rsync local /src /dst` with each
  backend.
- Goal: establish the headline cross-platform delta that #1386 will
  consume; provide a regression target before and after #1657 / #1385
  land.

Each benchmark must (per project policy): pre-check resource
availability and degrade gracefully; never sleep without a syscall
counter check; emit JSON output for CI consumption; live behind a
`bench-io` xtask command so the disk-commit thread is not measured
alongside the protocol stack.

## 6. Summary

macOS fast_io today consists of a healthy whole-file copy path
(`clonefile` + `fcopyfile`, #1388) and a synchronous fallback for
everything else: receiver disk-commit chunk writes hit
`Writer::Buffered` with one blocking `pwrite` per chunk; network paths
hit `read`/`write` loops because `splice` and Linux `sendfile` do not
exist on Darwin. Linux fast paths that have no current macOS analogue
include `io_uring` batched submission, registered buffers, the
provided buffer ring, fixed-file table, and `splice`. Whole-file copy
is the only category at parity.

The Windows side closed the equivalent gap under PR #3698 by adding
`Writer::Iocp { batch }`. The macOS side has two pending designs that
together would close most of it: #1657 (`F_NOCACHE` + `pwritev`) for
the cheap small-file win and #1385 (kqueue backend) for structural
parity with IOCP. The dispatch_io evaluation under #1653 is the
alternative async surface that was scoped but deferred to phases.

The micro-benchmarks in section 5 are what the project needs before
landing either #1657 or #1385: an apples-to-apples disk-write harness
that runs on all three target platforms so the gap and the closing of
it are measured, not inferred.
