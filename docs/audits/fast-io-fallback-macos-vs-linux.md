# fast_io fallback: macOS vs Linux io_uring (#1652)

Tracking issue: oc-rsync task #1652. Static, code-grounded audit. No
runs, no traces, no benchmarks were collected for this document. The
inferred throughput and syscall-count numbers are derived from kernel
syscall semantics and source-level behaviour. The actual macOS-vs-
Linux delta on the Mac mini M2 baseline is recorded under #1659
(cross-platform copy benchmark, completed) and is not reproduced here.

Companion designs cited throughout:

- `docs/design/iocp-transfer-pipeline-wiring.md` (PR #3698, closes
  #1868) - Windows side that closed the equivalent gap.
- `docs/design/macos-kqueue-fast-io.md` (PR #3701, task #1385) -
  near-future kqueue backend that would close most of the macOS gap.
- `docs/audits/macos-dispatch-io.md` (#1653) - dispatch_io evaluation
  that decided against that surface.
- `docs/audits/async-file-writer-trait.md` (#1655) - trait shape every
  batched writer must match.

## 1. Methodology

### 1.1 What was read

Built from the source tree at `/Users/ofer/devel/rsync` on
`docs/fast-io-fallback-macos-1652`, branched off `origin/master` at
commit `b43a56692`. Files read:

- `crates/fast_io/src/lib.rs` - crate surface, fallback chain,
  capability list, policy enums.
- `crates/fast_io/src/platform_copy/{mod.rs,dispatch.rs}` -
  per-platform copy selection, FFI for `clonefile(2)`, `fcopyfile(3)`,
  `FICLONE`, `CopyFileExW`, `FSCTL_DUPLICATE_EXTENTS_TO_FILE`.
- `crates/fast_io/src/io_uring/{mod.rs,file_factory.rs,file_reader.rs,file_writer.rs,disk_batch.rs,config.rs,registered_buffers.rs,shared_ring.rs}`.
- `crates/fast_io/src/iocp/{mod.rs,file_writer.rs,disk_batch.rs,file_factory.rs}`.
- `crates/fast_io/src/{io_uring_stub.rs,iocp_stub.rs,sendfile.rs,splice.rs,copy_file_range.rs,copy_file_ex.rs,traits.rs}`.
- `crates/transfer/src/disk_commit/writer.rs` - backend picker.
- `docs/design/{iocp-transfer-pipeline-wiring.md,macos-kqueue-fast-io.md}`.

### 1.2 What was not measured

No `dtruss`, `instruments`, `strace`, `perf`, `bpftrace`, hyperfine,
or syscall counter ran during preparation. Every claim about syscall
count, batching, and throughput delta is derived from source and
documented kernel semantics. Claims that depend on runtime behaviour
not encoded in source are tagged as inferred and point at section 11.

## 2. Inventory of fast_io entry points

Line references are to the worktree at `/Users/ofer/devel/rsync` on
`docs/fast-io-fallback-macos-1652`.

### 2.1 Top-level public surface (`crates/fast_io/src/lib.rs`)

`iocp_status_detail` (`lib.rs:183`), `io_uring_status_detail`
(`lib.rs:220`), `io_uring_availability_reason` (`lib.rs:234`),
`io_uring_kernel_info` (`lib.rs:245`), `platform_io_capabilities`
(`lib.rs:333`), `IoUringPolicy` (`lib.rs:404`), `IocpPolicy`
(`lib.rs:437`).

The capability list at `lib.rs:355` advertises `clonefile` and
`fcopyfile` on macOS, and only those two. There is no `kqueue`,
`dispatch_io`, `aio`, or `F_NOCACHE` entry.

### 2.2 Platform copy

`crates/fast_io/src/platform_copy/mod.rs`:

- `DefaultPlatformCopy` (`mod.rs:69`), `PlatformCopy::copy_file`
  (`mod.rs:80`), `try_refs_reflink` (`mod.rs:124`), `try_ficlone`
  (`mod.rs:155`), `try_clonefile` (`mod.rs:184`), `try_fcopyfile`
  (`mod.rs:211`).

`crates/fast_io/src/platform_copy/dispatch.rs`:

- `platform_copy_impl` for Linux (`dispatch.rs:18`): FICLONE ->
  `copy_file_range` -> `std::fs::copy`.
- `platform_copy_impl` for macOS (`dispatch.rs:63`): clonefile ->
  fcopyfile -> `std::fs::copy`.
- `platform_copy_impl` for Windows (`dispatch.rs:104`): ReFS reflink
  -> `CopyFileExW` -> `std::fs::copy`.
- `clonefile_impl` (`dispatch.rs:151`), `fcopyfile_impl`
  (`dispatch.rs:186`), `try_refs_reflink_impl` (`dispatch.rs:296`),
  `try_ficlone_impl` (`dispatch.rs:501`).

### 2.3 io_uring (Linux only, gated by `lib.rs:112`)

Top-level: `read_file` (`io_uring/mod.rs:126`), `writer_from_file`
(`mod.rs:142`), `reader_from_path` (`mod.rs:201`), `write_file`
(`mod.rs:240`).

Factories and enums: `IoUringReaderFactory` (`file_factory.rs:22`),
`IoUringOrStdReader` (`file_factory.rs:57`), `IoUringWriterFactory`
(`file_factory.rs:137`), `IoUringOrStdWriter` (`file_factory.rs:172`).

Per-file: `IoUringReader::open` (`file_reader.rs:56`), `read_at`
(`file_reader.rs:99`), `read_all_batched` (`file_reader.rs:149`),
`IoUringWriter::create` (`file_writer.rs:52`), `write_all_batched`
(`file_writer.rs:211`).

Persistent batch: `IoUringDiskBatch::new` (`disk_batch.rs:70`),
`begin_file` (`disk_batch.rs:103`), `write_data` (`disk_batch.rs:126`),
`commit_file` (`disk_batch.rs:170`).

Probe and helpers: `is_io_uring_available` (`io_uring/config.rs:167`),
`RegisteredBufferGroup` (`registered_buffers.rs:1-67`),
`SharedRing::try_new` (`shared_ring.rs`), `BufferRing`
(`buffer_ring.rs`).

### 2.4 IOCP (Windows only, gated by `lib.rs:124`)

Factories: `IocpReaderFactory` (`iocp/file_factory.rs:150`),
`IocpWriterFactory` (`iocp/file_factory.rs:208`), `writer_from_file`
(`iocp/file_factory.rs:290`, reopens with `FILE_FLAG_OVERLAPPED`,
#1929), `reader_from_path` (`iocp/file_factory.rs:431`).

Per-file: `IocpWriter::create` (`iocp/file_writer.rs:39`),
`create_for_append` (`iocp/file_writer.rs:54`).

Persistent batch: `IocpDiskBatch::new` (`iocp/disk_batch.rs:123`),
`begin_file` (`iocp/disk_batch.rs:163`), `write_data`
(`iocp/disk_batch.rs:197`), `commit_file` (`iocp/disk_batch.rs:244`).

Probe: `is_iocp_available` (`iocp/config.rs:91`).

### 2.5 macOS landing surface

There is no `crates/fast_io/src/kqueue/` directory today. The
io_uring tree is gated out by `lib.rs:112`; the IOCP tree by
`lib.rs:124`. Both are replaced on macOS by stubs:

- `crates/fast_io/src/io_uring_stub.rs:25` - `is_io_uring_available()`
  always `false`.
- `crates/fast_io/src/iocp_stub.rs:24` - `IOCP_MIN_FILE_SIZE` is kept
  for ABI symmetry; nothing constructs an `IocpDiskBatch`.

The macOS-specific entry points are exactly:

- `try_clonefile` (`platform_copy/mod.rs:184`) - APFS `clonefile(2)`.
- `try_fcopyfile` (`platform_copy/mod.rs:211`) - kernel-accelerated
  data copy for non-APFS filesystems.
- `DefaultPlatformCopy::copy_file` for whole-file copies, dispatch at
  `platform_copy/dispatch.rs:63`.

Everything else - delta-apply chunk writes, network reads, network
writes - falls through to `crates/fast_io/src/traits.rs`
(`StdFileReader` at `traits.rs:76`, `StdFileWriter` at `traits.rs:120`).

## 3. Linux io_uring path

When the runtime probe at `io_uring/config.rs:167` returns `true`
(kernel >= 5.6, syscall not blocked by seccomp), the disk-commit
thread takes the `IoUringDiskBatch` path:

1. The thread holds one `RawIoUring` for its lifetime
   (`disk_batch.rs:46`).
2. `begin_file` (`disk_batch.rs:103`) registers the new fd via
   `IORING_REGISTER_FILES` if `register_files` is on.
3. `write_data` (`disk_batch.rs:126`) buffers up to 256 KB
   (`DEFAULT_BUFFER_CAPACITY`, `disk_batch.rs:32`), then drains via
   `submit_write_batch` packing N SQEs into one `submit_and_wait(N)`.
   With `register_buffers` the SQE op is `IORING_OP_WRITE_FIXED`,
   eliminating per-op `get_user_pages()` accounting
   (`registered_buffers.rs:6`).
4. `commit_file` (`disk_batch.rs:170`) drains the buffer, optionally
   calls `fsync`, returns the original `File`.

Headline syscall savings:

- One ring per thread, not per file. Setup amortises across the
  whole transfer.
- Batched submission. N writes go to the kernel in one
  `io_uring_enter(2)` (or zero with `SQPOLL`).
- Registered buffers map the user pages once; no per-op pinning.

For network receive, `splice.rs:83` exposes `is_splice_available` and
`try_splice_to_file` (`splice.rs:162`) moves bytes from the socket
into the destination via a pipe pair without crossing user space. For
local copies, `copy_file_range` (`copy_file_range.rs:86`) does
in-kernel zero-copy on the same filesystem. The dispatcher at
`platform_copy/dispatch.rs:36` only attempts it for files >= 64 KB
(`CFR_THRESHOLD`).

## 4. macOS path today

There is no async backend on macOS at the time of this audit. Every
fast_io path on Darwin lands in one of three places.

### 4.1 Whole-file copies (`platform_copy/dispatch.rs:63`)

The dispatcher tries, in order:

1. `clonefile_impl` (`dispatch.rs:151`) - APFS `clonefile(2)`. Zero
   data copied; O(1). Closed under #1388.
2. `fcopyfile_impl` (`dispatch.rs:186`) - `fcopyfile(3)` with
   `COPYFILE_DATA`. Cross-filesystem and HFS+/SMB-friendly. Single
   syscall per file with internal kernel chunking.
3. `std::fs::copy` - portable buffered fallback.

This path is healthy. Both `clonefile` and `fcopyfile` complete in the
kernel. The local-copy executor sees upstream-comparable numbers on
APFS volumes per #1659.

### 4.2 Receiver disk-commit chunks
(`crates/transfer/src/disk_commit/writer.rs:141`)

The `Writer` enum has exactly three variants:

```text
Writer::Buffered(ReusableBufWriter<'a>)        // always available
Writer::IoUring  { batch: &mut IoUringDiskBatch } // cfg(linux + io_uring)
Writer::Iocp     { batch: &mut IocpDiskBatch }    // cfg(windows + iocp)
```

There is no `Writer::Kqueue`, `Writer::DispatchIo`, or `Writer::Aio`.
On macOS the only branch that fires is `Writer::Buffered`. The
buffered writer is `ReusableBufWriter` over the disk-thread's
permanent 256 KB scratch buffer (matches upstream rsync's
`wf_writeBufSize = WRITE_SIZE * 8` at `fileio.c:161`).

Every chunk handed to `write_chunk` (`writer.rs:177`) becomes a
synchronous `write(2)`. The disk thread blocks for the duration of
each syscall, holding back any chunk that would otherwise overlap
with the device.

### 4.3 Network paths

`crates/fast_io/src/sendfile.rs:147` is gated on
`#[cfg(target_os = "linux")]` for `send_file_to_fd`. The macOS arm at
`sendfile.rs:158` falls through to `copy_via_fd_write`, a `read(2)`
plus `write(2)` loop. Apple's native `sendfile(2)` (different
signature from Linux) is not invoked.

`crates/fast_io/src/splice.rs:253` is gated on
`#[cfg(not(target_os = "linux"))]` and returns `Unsupported`. macOS
has no `splice(2)`.

### 4.4 F_NOCACHE per #1657

No `F_NOCACHE` invocation in `crates/fast_io/src/` today. #1657 is
open and pending; when it lands, both the dispatch_io audit and the
kqueue design treat `F_NOCACHE` plus `writev(2)` as a complement to
a future asynchronous backend, not a substitute. See section 8.4.

## 5. The functional gap

Several io_uring features have no macOS analogue today, even with the
future kqueue backend (#1385) factored in.

| Feature | Linux entry point | macOS now | With kqueue |
|---|---|---|---|
| Registered buffers (`IORING_REGISTER_BUFFERS`) | `registered_buffers.rs` | Absent | Absent (kqueue design 6.1: no zero-copy). |
| SQPOLL kernel thread | `config.rs:30`, `lib.rs:225` | Absent | Absent (6.2: submissions always cross the boundary). |
| Batched submission (`submit_and_wait(N)`) | `disk_batch.rs:126` | Absent | Partial: readiness batched, each `pwrite` is its own syscall (6.3). |
| Link chains (`IOSQE_IO_LINK`) | Unused today | N/A | N/A. |
| Provided buffer ring (`IORING_REGISTER_PBUF_RING`) | `buffer_ring.rs` | Absent | Absent. |
| Fixed-file table (`IORING_REGISTER_FILES`) | `file_reader.rs:63`, `file_writer.rs:55` | Absent | Partial: `EV_ADD` once per fd. |
| Single ring shared between read and write fd | `shared_ring.rs` | Absent | Possible: one kqueue can multiplex many fds (2.1). |
| Zero-copy file-to-socket | `sendfile.rs:147`, `splice.rs:162` | Absent on socket | Out of scope; receiver disk path is the kqueue target. |

What survives on macOS once #1385 lands: a single backend object
reused across files, userspace overlap of the disk writeback queue,
per-file completion accounting. What never lands without protocol
work: kernel-managed buffer pools, true page-cache-to-NIC zero copy.

## 6. The throughput gap (inferred)

Numbers below are conservative and assume the buffered backend is
exercised at its sweet spot (chunks at `WRITE_SIZE = 32 KB`).

### 6.1 Per-file syscall budget on Linux io_uring

For one 1 MiB file written through `IoUringDiskBatch` with
`register_files = true`, `register_buffers = true`, default
`sq_entries = 32`:

- 0 syscalls for ring setup (per-thread, amortised).
- 1 `IORING_REGISTER_FILES_UPDATE` per file rotation (`begin_file`).
- ceil(1 MiB / 256 KB) = 4 SQEs each carrying `IORING_OP_WRITE_FIXED`,
  packed into 1 `io_uring_enter(2)`. With `SQPOLL`, drops to 0
  syscalls.
- 0 or 1 `fsync` syscall.

Budget: roughly 2-4 syscalls per 1 MiB file under `SQPOLL=false`,
1-2 with `SQPOLL=true`.

### 6.2 Per-file syscall budget on macOS today

For the same 1 MiB file written through `Writer::Buffered`:

- ceil(1 MiB / 256 KB) = 4 `pwrite(2)` calls when `BufWriter`
  (`crates/fast_io/src/traits.rs:120`) flushes, potentially more if
  the destination's cluster size triggers smaller writes.
- 0 or 1 `fsync` (`StdFileWriter::sync` at `traits.rs:177` calls
  `flush` then `sync_all`, which is `fcntl(F_FULLFSYNC)` on macOS).

Budget: 4-5 syscalls per 1 MiB file, none batched, all blocking the
disk-commit thread.

For a 4 KiB-file workload (the worst case from #1659), every file is
one `open` + one `pwrite` + one `close`. With io_uring on Linux that
`pwrite` becomes one slot in a batched SQE submission;
`submit_and_wait(N)` reaps N completions in one syscall. On macOS each
call is its own kernel transition.

### 6.3 Inferred wall-clock impact

Per the kqueue design's section 6.4 (which paraphrases the #1659
benchmark), the buffered macOS path trails the upstream rsync 3.4.1
baseline by 5-8% on the 4 KiB-file mix and is on par on the 1 MiB-and-
up mixes. The gap to Linux io_uring on the same workload is larger,
but cross-architecture comparisons are intentionally not made in
#1659; they are tracked under #1386.

The qualitative picture: the macOS path leaves throughput on the table
on small-file workloads (every chunk is a syscall, no userspace
pipelining of the writeback queue), and is competitive on large-file
workloads (the syscall amortises over the bytes copied).

### 6.4 Why the buffered writer is not "free"

`std::io::BufWriter` coalesces small writes but does not overlap them
with the device. Each `BufWriter::flush` is a synchronous `write` on
a single fd. The disk-commit thread is therefore latency-bound on the
kernel's writeback acceptance, not throughput-bound on the NVMe. On
Linux io_uring's batched submission lets the kernel walk the SQ and
queue several writes into the writeback path in one transition; that
overlap is missing on macOS.

## 7. Comparison with the Windows path

The Windows side closed exactly this gap under PR #3698 (issue #1868).
Until #1868, every Windows build hit the same `Writer::Buffered`
fallback that macOS hits today. The fix added the
`Writer::Iocp { batch }` variant
(`crates/transfer/src/disk_commit/writer.rs:147-150`) and threaded an
`IocpDiskBatch` through the disk thread.

Per `docs/design/iocp-transfer-pipeline-wiring.md`, the post-fix
lifecycle is symmetric to the io_uring path:

- One persistent `IocpDiskBatch` per disk-commit thread
  (`thread.rs:92`, design line 116).
- `begin_file` reopens the caller's `File` with
  `FILE_FLAG_OVERLAPPED` via `ReOpenFile` and associates the new
  handle with the port (`iocp/disk_batch.rs:163`).
- `write_data` keeps `concurrent_ops = 4` overlapped writes in flight
  (design line 131).
- Completions reaped through `GetQueuedCompletionStatusEx` with a
  drain batch of 64 entries (design line 134), matching io_uring's
  CQE drain granularity.
- `commit_file` flushes and optionally calls `FlushFileBuffers`
  (`iocp/disk_batch.rs:244`).

The shape is identical to `IoUringDiskBatch` on Linux, with overlapped
IOCP playing io_uring's role. The Windows path now has the
asynchronous batching that macOS still lacks.

This is the inflection point: if Windows can have an asynchronous
backend and Linux can have one, the absence on macOS is a gap, not a
design choice.

## 8. Mitigations available without kqueue

These mitigations live in fast_io today (#1388) or are planned for
landing without a new kqueue module (#1657).

### 8.1 clonefile (#1388, shipped)

`crates/fast_io/src/platform_copy/mod.rs:184`. Local-copy executor
calls this when source and destination share an APFS volume and the
operation is whole-file. Nothing more to do.

### 8.2 fcopyfile (shipped)

`crates/fast_io/src/platform_copy/mod.rs:211`. Used when `clonefile`
returns `EXDEV` or the target is not APFS. Single kernel-side syscall
per file. Covers HFS+, NFS, and SMB destinations.

### 8.3 dispatch_io (#1653, decided against)

`docs/audits/macos-dispatch-io.md` evaluated `dispatch_io`. Headline
finding: "wedging it under our buffer-pool and chunk-ownership model
doubled the bookkeeping for no measurable throughput win." The
dispatch_io API owns the queue topology and the buffer copies; it
does not interleave with our owned-buffer model.

### 8.4 F_NOCACHE plus writev (#1657, pending)

`F_NOCACHE` bypasses the unified buffer cache. Not asynchronous: the
calling thread blocks on each `writev(2)`. Per the kqueue design at
line 70: "It removes one memcpy from the kernel side but is still
synchronous." Best on APFS-on-NVMe when the working set exceeds RAM;
amplifies writes on SMB and network mounts. Expected delta on its
own: a few percent on the 1 MiB-and-up mix. No help on 4 KiB.

### 8.5 Standalone writev batching

Replace 4 small `pwrite(2)` calls with 1 `pwritev(2)` carrying 4
iovecs. The kernel does the gather under the lock; the syscall
returns when the page cache accepts the data. No async machinery,
stays within `BufWriter`'s existing model. Expected delta: 3:1
reduction in syscalls on small-file workloads with sub-256-KB chunks,
1-3% wall-clock on the small-file mix.

## 9. Mitigations that require kqueue

The kqueue design at `docs/design/macos-kqueue-fast-io.md` (PR #3701)
is the only path that closes most of the io_uring gap on macOS. The
design proposes a `KqueueDiskBatch` (sketched at line 235) mirroring
`IoUringDiskBatch` and `IocpDiskBatch`:

- One kqueue descriptor per batch, reused across files.
- `EVFILT_WRITE` registration per active fd
  (`EV_ADD | EV_CLEAR`, design line 113).
- `pwrite(2)` (or `pwritev(2)` for direct-write chunks) at the buffer
  offset, `EAGAIN` handled by re-arming the kevent (line 117).
- `F_FULLFSYNC` on commit (line 184), `F_PREALLOCATE` for pre-sizing
  (line 185).

Throughput claim from design section 6.4: "expected to close most of
the 5-8% gap by removing per-chunk blocking, leaving 2-3% behind
Linux io_uring after accounting for the F_FULLFSYNC barrier on
commit." Projection assumes no zero-copy primitive, gained readiness-
batching across many fds, and `F_NOCACHE` from #1657 alongside.

Non-goal at design line 549: do not abstract io_uring + IOCP + kqueue
behind a single Rust trait. The existing `Writer` enum at
`crates/transfer/src/disk_commit/writer.rs:141` is the right shape.
A new `Writer::Kqueue { batch }` variant gated on
`#[cfg(target_os = "macos")]` is the surgical change. The kqueue
probe at design section 4.3 is identical in shape to the io_uring
probe at `crates/fast_io/src/io_uring/mod.rs:151`: open a kqueue,
register an event on a self-pipe, close, cache the result.

## 10. Recommended near-term actions

Ranked by inferred payoff per unit of engineering effort.

### 10.1 Land #1657 (F_NOCACHE plus pwritev)

Effort: small. Internal to `crates/fast_io/src/traits.rs`; needs a
`MacosBufWriter` (or wrapper) that emits `pwritev(2)` for batched
flushes and exposes `with_no_cache(bool)`. No new module, no new
probe. Expected payoff: 1-3% on the 4 KiB-file mix from removing
per-chunk syscalls. No effect on the 1 MiB-and-up mix.
Lowest-risk, highest-payoff action.

### 10.2 Land #1385 (kqueue backend)

Effort: medium. Decomposes into a new `crates/fast_io/src/kqueue/`
directory plus `kqueue_stub.rs`, a `KqueuePolicy` enum mirroring
`IoUringPolicy` at `lib.rs:404`, a `Writer::Kqueue { batch }` variant
in `crates/transfer/src/disk_commit/writer.rs:141`, a `kqueue_policy`
field on `DiskCommitConfig`, and `--kqueue` / `--no-kqueue` CLI
flags gated to macOS. Expected payoff: closes most of the 5-8%
buffered-vs-upstream gap on the 4 KiB-file mix; marginal on
1 MiB-and-up. macOS reaches structural parity with Windows IOCP.

### 10.3 Add a macOS sendfile path

Effort: small-to-medium. macOS `sendfile(2)` has a different signature
from Linux. The current `sendfile.rs:147` is gated on
`cfg(target_os = "linux")`. A `cfg(target_os = "macos")` arm calling
Apple's `sendfile` would let the daemon path do file-to-socket
transfers without crossing user space. Expected payoff: small in the
receiver path; larger in the sender path on a daemon serving many
clients.

### 10.4 Wire kqueue into the local-copy executor cross-volume fallback

Effort: small once #1385 lands. Kqueue design section 7.1 defers this
on grounds that local-copy is single-syscall when `clonefile` or
`fcopyfile` succeeds. The cross-volume case where the fallback is
`std::fs::copy` (`BufReader` + `BufWriter`) is the natural fit for
the kqueue batch. Small payoff.

### 10.5 Defer dispatch_io permanently

Effort: zero (already deferred). Reopening #1653 would require a
structural change to the buffer-pool and chunk-ownership model that
section 8.3 describes as "doubled the bookkeeping for no measurable
throughput win."

## 11. Open questions for actual runtime measurement

Issue #1659 is closed and provides the buffered-writer baseline on
the M2 Mac mini. Several questions remain.

### 11.1 Per-syscall cost on M2 NVMe

Section 6.2 infers `pwrite(2)` returns in 1-10 microseconds for
sub-MiB transfers on Apple NVMe. The actual cost on the M2 baseline
is unmeasured. A `dtruss -c -t pwrite` over a 1000-file transfer
would answer this directly.

### 11.2 Writeback queue depth on macOS

io_uring on Linux benefits from the kernel writeback queue absorbing
several outstanding writes. macOS has the unified buffer cache but
the behaviour under back-to-back `pwrite` calls from a single thread
is unmeasured.

### 11.3 Effect of F_NOCACHE on small-file mix

Section 8.4 infers F_NOCACHE is minimal on small files, useful on
large files when the working set exceeds RAM. The crossover threshold
and the percentage win are unmeasured.

### 11.4 Impact of kqueue's coarser completion granularity

Per the kqueue design: kqueue batches N writes by issuing N `pwrite`
calls in user space, observing readiness with one `kevent`, and
resubmitting only the chunks that returned `EAGAIN`. The fraction of
`pwrite` calls that return `EAGAIN` on M2 NVMe under realistic
concurrency is unknown. If `EAGAIN` is rare, the kqueue path
collapses to pwrite-per-chunk plus negligible kevent overhead. If
common, the resubmit loop dominates. This is the single biggest
unmeasured variable.

### 11.5 SSH-transport interaction

The SSH transport (`crates/rsync_io/src/ssh/`) does not consume
fast_io's disk-commit batches today. The receiver's network read and
the disk-commit write are decoupled. On macOS without a kqueue, the
disk-commit thread blocks on every `pwrite` while the SSH transport
keeps draining the socket; whether the SPSC channel between them
holds chunks or starves is unmeasured.

### 11.6 Cross-platform comparison

#1659 does not compare M2 vs Linux x86_64 absolute numbers; that is
parked under #1386. Once #1385 lands, an apples-to-apples re-run of
#1659 should show the macOS gap closing to within 2-3% of Linux
io_uring on the buffered-writer benchmarks.

## 12. Conclusion

macOS today is the platform with the smallest async I/O surface.
Local-copy reaches parity through `clonefile` and `fcopyfile`. The
receiver disk path falls through to synchronous `Writer::Buffered`
because there is no Darwin-side analogue of `IoUringDiskBatch` or
`IocpDiskBatch`. The Windows side closed the same gap under PR #3698;
the macOS side has a design ready to land under PR #3701 (#1385).

Land #1657 for the cheap pwritev plus F_NOCACHE win on small files,
and land #1385 for structural parity with the IOCP disk-commit path.
Together they are projected to close most of the 5-8% gap that #1659
identified on the 4 KiB-file mix. No wire-protocol changes; no new
unsafe outside the existing fast_io boundary.
