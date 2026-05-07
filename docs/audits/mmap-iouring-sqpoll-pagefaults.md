# mmap page-fault impact on the io_uring SQPOLL kernel thread

Tracking: oc-rsync task #1661.

This audit documents how demand-faulted pages in an `mmap(2)`-backed
basis file interact with an `IORING_SETUP_SQPOLL` kernel thread when
the SQE references those pages, why the resulting fault path is
fundamentally different from a userspace fault, what oc-rsync does
today to keep the two from meeting on the wired transfer path, and
which mitigations apply when a future caller deliberately ships
mmap'd pages through io_uring.

Companion documents:

- `docs/audits/mmap-iouring-co-usage.md` - inventory of every site
  where mmap and io_uring meet in the codebase today.
- `docs/audits/mmap-page-fault-iouring-sqpoll.md` - earlier
  three-failure-mode summary of the same hazard.
- `docs/audits/io_uring_sqpoll_mmap_pagefault.md` - long-form
  follow-up audit that catalogues SQPOLL configuration and the
  current mitigation stack.
- `docs/audits/madvise-willneed-prefault.md` - rationale for keeping
  `MADV_WILLNEED` over alternatives.
- `docs/audits/mmap-map-populate-evaluation.md` - rationale for
  rejecting `MAP_POPULATE` as a default.
- `docs/design/basis-file-io-policy.md` - selector rule that forbids
  `MmapStrategy` whenever an io_uring writer is active.

## 1. Why mmap'd basis files cause minor faults during io_uring reads

`mmap(2)` returns a virtual range whose page table entries are
populated lazily. The first read of a 4 KiB region issues a CPU page
fault, the kernel handler resolves the file-backed `vm_area_struct`,
and either the page-cache page is pinned into the PTE (a minor fault
when the page is already cached) or the kernel reads from disk (a
major fault). Subsequent reads of the same page hit the populated
PTE and never enter the fault handler.

For a freshly opened basis file none of the pages are mapped yet.
Every distinct page touched during delta application is at minimum
one minor fault; pages evicted under memory pressure or never read
into cache are major faults. A `MmapReader::open` followed by
`as_slice()` does not preempt this work: `memmap2::MmapOptions::new()
.map(&file)` does not pass `MAP_POPULATE`, so the mapping is
strictly demand-paged.

When a userspace task dereferences such a page the CPU traps to the
kernel, the scheduler may sleep the task while readahead runs, and
on return the access completes. The task owns the fault: its `mm`,
its signal mask, its priority, and its userspace stack are all
available to the fault handler.

When io_uring submits a `IORING_OP_READ` (or `WRITE`, or the
`READ_FIXED` / `WRITE_FIXED` variants) the kernel must `copy_to_user`
or `copy_from_user` against the user pointer named in the SQE. If
that pointer lands on a non-resident page the kernel takes the same
fault, but the *context* in which it takes the fault depends on who
issued the SQE. For a regular ring this is the userspace task that
called `io_uring_enter(2)`. For an SQPOLL ring this is a kernel
thread that has borrowed the userspace `mm` via `kthread_use_mm()`.

## 2. SQPOLL kernel thread and userspace page faults

`IORING_SETUP_SQPOLL` (man `io_uring_setup(2)`) starts a kthread per
ring (post-5.11 a kthread can be shared via `IORING_SETUP_ATTACH_WQ`)
that polls the SQ head, dispatches SQEs through `io_issue_sqe()`, and
parks after `sqpoll_idle_ms` of inactivity. The dispatch runs with
`current->mm` set to the submitter's address space so user-virtual
pointers in an SQE resolve through the right page tables. What it
does not run with is:

- A userspace task struct - the kthread has no `task_struct.mm` of
  its own; `mm` is borrowed for the dispatch and released after.
- A signal-delivery context - kernel threads do not receive signals,
  so a `SIGBUS` raised by a vanished file mapping cannot be caught
  by any userspace handler.
- A scheduler priority class compatible with cgroup I/O accounting
  for the submitter - the fault's I/O is charged to the kthread.
- A userspace stack on which to run signal handlers or backtraces.

When `io_issue_sqe()` invokes `copy_to_user` against an mmap'd page
that is not resident, `handle_mm_fault()` runs in the kthread. Three
behaviours can follow, depending on kernel version, the opcode, and
the mapping state:

1. **Synchronous fault inside the kthread.** The kthread runs the
   fault handler itself. For a cached file page this is microseconds.
   For a cold page it blocks on filesystem I/O. While the kthread
   blocks, the SQ stops draining; every other ring sharing that
   kthread (post-5.11 with `wq_fd`) stalls behind it. The latency
   property SQPOLL was enabled for - eliminating `io_uring_enter(2)`
   syscalls - degrades to the same wait the syscall path would have
   produced, plus one context switch back to the submitter when the
   kthread finally completes.

2. **Punt to `io-wq` task-work.** When the kernel detects the SQE
   cannot complete inline (the opcode is flagged `IO_WQ_WORK_*` and
   the page is missing) it queues the request to an `io-wq` worker
   that re-runs the op with the submitter's `mm`. The worker can
   take the fault on its own stack. The request now costs one SQ
   wakeup, one work-queue dispatch, and one CQE; latency reaches or
   exceeds a non-SQPOLL ring.

3. **Short result or `-EFAULT` completion.** On older kernels (the
   `IORING_OP_READ` / `WRITE` family before roughly 5.12) and on
   opcodes that do not punt, the kthread fails the op outright. The
   CQE returns either a short transfer or `-EFAULT`. Writers retry
   shorts; `-EFAULT` propagates up as `io::Error` from
   `submit_and_wait` and aborts the transfer. The truncate variant
   - the basis file shrinks while a SQE references its tail - is the
   `SIGBUS` case upstream rsync explicitly avoids by never using
   `mmap(2)` on basis files (`fileio.c:214-217`). Under SQPOLL the
   `SIGBUS` is delivered in kernel context, so the userspace handler
   upstream relies on cannot run.

The summary: SQPOLL kthreads can *take* a page fault on a borrowed
`mm`, but they take it on the kernel scheduling queue, not the
submitter's. Every fault either stalls SQ drain, forces a context
switch back to userspace via task-work, or fails the SQE.

`IORING_REGISTER_BUFFERS` is the special case. Registration calls
`get_user_pages_fast()` synchronously, faulting each page in and
pinning it for the registration's lifetime. The SQPOLL kthread can
never fault on a registered range because the pages are pinned.
This is why oc-rsync's `RegisteredBufferGroup` is the load-bearing
fixed-buffer primitive on the io_uring path.

## 3. Current mmap usage sites in oc-rsync

Repository-relative citations. Verified against `crates/fast_io/src/`,
`crates/transfer/src/map_file/`, `crates/checksums/src/parallel/`,
and `crates/engine/src/`.

### 3.1 mmap producers

- `crates/fast_io/src/mmap_reader.rs:84` - the only call to
  `MmapOptions::new().map(&file)` in the workspace. No
  `MAP_POPULATE`, no `madvise` on the constructor path. Returns an
  `MmapReader` whose `as_slice()` (line 97) yields a `&[u8]` over
  kernel-faulted file pages.
- `crates/fast_io/src/mmap_reader.rs:124-143` - `advise_sequential`,
  `advise_random`, and `advise_willneed`. Defined, gated
  `#[cfg(unix)]`, currently uncalled from any io_uring submission
  site.
- `crates/transfer/src/map_file/mmap.rs:36-40` - `MmapStrategy::open`
  wraps `MmapReader::open`. `as_slice` (44-46) and `map_ptr` (50-66)
  return slices into the mapping with no advice or prefault.
- `crates/transfer/src/map_file/adaptive.rs:36-54` -
  `AdaptiveMapStrategy::open` selects `MmapStrategy` whenever the
  file is at least `MMAP_THRESHOLD` (1 MiB).
- `crates/transfer/src/map_file/adaptive.rs:70-72` - `open_buffered`
  forces the buffered variant. This is the io_uring-safe entry
  point used by `DeltaApplicator` when its writer is io_uring-backed.
- `crates/checksums/src/parallel/files.rs:42, 237, 340` - parallel
  digest paths construct `MmapReader` for whole-file hashing. Never
  crosses into io_uring.

### 3.2 io_uring submission sites that take a user pointer

- `crates/fast_io/src/io_uring/file_reader.rs` - `read_at` and
  `read_all_batched` build `IORING_OP_READ` SQEs against
  caller-owned `&mut [u8]` destinations. The `READ_FIXED` branch
  submits against `RegisteredBufferGroup` heap buffers.
- `crates/fast_io/src/io_uring/file_writer.rs` - `write_all_batched`
  submits `WRITE_FIXED` against registered buffers in the fast
  branch and a fallback `Write` SQE that hands `data: &[u8]`
  directly to the kernel. `Write::write` bypasses internal
  buffering when `buf.len() >= self.buffer_size` (default 256 KiB),
  submitting the caller's slice directly. This is the highest-risk
  seam if a future caller hands it an mmap-backed slice.
- `crates/fast_io/src/io_uring/registered_buffers.rs` -
  `RegisteredBufferGroup::new` allocates page-aligned heap buffers
  via `alloc::alloc_zeroed` and pins them with
  `IORING_REGISTER_BUFFERS`. The buffers are heap-only by
  construction; registration never touches a file mapping.

### 3.3 Where the two paths could meet

Today none. The receiver opens basis files via `MapFile::open`
(`BufferedMap`), and `DeltaApplicator` selects
`MapFile::open_adaptive_buffered` when `BasisWriterKind::IoUring` is
in effect, so the io_uring-paired applicator never sees an
`MmapStrategy`. The cross-product of producer and submission sites
above is empty on the wired transfer path. The hazard is
forward-looking: any future caller that introduces an
`MmapStrategy` slice as an io_uring SQE pointer (intentionally or
through accidental refactor) re-opens it.

`crates/engine/src/` does not import `MmapReader` or `memmap2` at
all today; mmap usage in the engine path is mediated through the
transfer crate's `MapFile`. The engine's local-copy executor uses
buffered I/O for basis access on every path that pairs with io_uring
writes.

## 4. Mitigations

These four mitigations layer in the order they apply at runtime.

### 4.1 `MAP_POPULATE` (eager fault at mmap time)

`mmap(2)` with `MAP_POPULATE` walks the mapping at mmap time and
faults every page in synchronously. After the call returns, every
page is resident; an SQPOLL kthread that dispatches a SQE against
the range will not fault.

Cost: synchronous I/O proportional to file size at open time. For
basis files where delta transfer reads only 30-60% of the file, the
populate phase pays for bytes the worker never touches. The audit
in `docs/audits/mmap-map-populate-evaluation.md` rejects
`MAP_POPULATE` as the default for this reason. It also does not
close the truncate-`SIGBUS` failure mode: a populated mapping is
still vulnerable to mid-transfer truncation.

Use sparingly when prefault cost is acceptable and the entire
mapping will be read.

### 4.2 `madvise(MADV_WILLNEED)` (asynchronous hint)

`posix_madvise(MADV_WILLNEED, addr, len)` queues asynchronous
readahead for the named range. The call returns immediately; the
kernel populates the page cache in the background. Issued from the
basis-file consumer just before the io_uring SQE dereferences the
slice, the pages are likely (but not guaranteed) resident by the
time the kthread services the op.

Hook: `MmapReader::advise_willneed`
(`crates/fast_io/src/mmap_reader.rs:139-143`). Errors are
deliberately ignored; the hint is advisory and any failure
(`EBADF`, `EINVAL` on holes) is non-fatal.

This is the kept primitive for workloads where `MAP_POPULATE`'s
synchronous cost is unacceptable. It does not eliminate the fault
hazard - it reduces the probability of a fault landing on the
kthread - and it does not close the truncate-`SIGBUS` failure mode.

### 4.3 Pre-fault loop on registered buffers

When a future caller registers an mmap-backed range with
`IORING_REGISTER_BUFFERS` (oc-rsync does not do this today; all
registered buffers are heap-allocated), the registration site must
walk the buffer one byte per page before the
`io_uring_register(2)` call:

```text
for page in buffer.chunks(PAGE_SIZE) {
    std::hint::black_box(page[0]);
}
```

This forces the userspace task to take every fault itself, on its
own stack, with normal signal delivery. By the time
`get_user_pages_fast()` runs inside the kernel registration path,
the pages are resident and pinning succeeds without the kernel
having to fault. The loop is portable, costs one minor fault per
page (typically 1-3 microseconds), and is paired with
`MADV_SEQUENTIAL` so the kernel readahead window absorbs most of
the per-page cost.

The pre-fault loop also turns deferred `-EFAULT` outcomes into
eager `EFAULT` from the registration syscall, which is recoverable
from userspace; deferred `-EFAULT` from a kthread is not.

### 4.4 Hard gate: forbid `MmapStrategy` whenever io_uring is active

This is the only mitigation that survives all three failure modes
without per-site hardening. The selector in
`docs/design/basis-file-io-policy.md` treats `io_uring_active` as a
forcing column: when the writer is io_uring-backed,
`AdaptiveMapStrategy::open_buffered` is called instead of
`AdaptiveMapStrategy::open`, and the basis is served by
`BufferedMap` regardless of file size, sparseness, or any other
input. The implementation entry point is at
`crates/transfer/src/map_file/adaptive.rs:70-72`, reachable from
`MapFile::open_adaptive_buffered` and wired through
`DeltaApplyConfig::writer_kind`.

`BufferedMap` reads basis pages into a heap-owned sliding window via
`pread(2)`. The slice handed downstream is never an mmap pointer.
SQPOLL cannot fault on it because the pages are anonymous heap,
already faulted on first kernel touch. The truncate-`SIGBUS` window
collapses to the duration of one `pread` syscall instead of the
duration of an entire transfer.

## 5. Performance implications: SQPOLL throttling under fault pressure

SQPOLL is a latency optimisation: the userspace task hands work to
the SQ and continues without entering the kernel. When the kthread
faults, every clock cycle the kthread spends in `handle_mm_fault()`
or in an `io-wq` worker is a cycle the userspace task could have
been spending on application work. The throttling regimes:

- **Light fault pressure** (cached pages, occasional minor fault).
  Fault handler runs in microseconds. SQPOLL still wins versus a
  syscall-driven ring because the syscall path costs more than the
  fault. `MADV_WILLNEED` keeps this regime dominant on read-heavy
  workloads.

- **Major fault pressure** (basis pages cold, memory under pressure).
  Each fault blocks the kthread for milliseconds while readahead
  reads from disk. SQ drain stalls. Every other ring sharing the
  kthread stalls behind it. Effective SQ throughput drops below a
  syscall-driven ring because the kthread has been removed from the
  scheduler's I/O queue while running in `handle_mm_fault()`.
  `MAP_POPULATE` shifts this cost to open time but does not remove
  it; mitigation 4.4 removes it entirely from the basis path.

- **Punt regime** (kernel detects mid-fault and queues to `io-wq`).
  Each punted SQE costs a workqueue dispatch plus a CQE. Latency
  matches a non-SQPOLL ring; throughput is bounded by `io-wq`
  worker count, which on heavily-shared rings becomes the
  bottleneck. SQPOLL provides no benefit in this regime; the kthread
  becomes a router.

- **Failure regime** (older kernel, `-EFAULT` on the SQE, or
  truncate during a SQE). The transfer aborts. Mitigation 4.4 is
  the only defence: `BufferedMap` cannot raise `-EFAULT` on a
  read-side SQE because the SQE never references a file mapping.

`IoUringConfig::sqpoll` defaults to `false` in every preset
(`for_default`, `for_large_files`, `for_small_files`); SQPOLL is
opt-in at every layer; `build_ring` falls back to a non-SQPOLL ring
on `EPERM` (typically missing `CAP_SYS_NICE`). No production caller
flips `sqpoll` to `true` today. The hazard is therefore the contract
that any future caller enabling SQPOLL must honour: serve basis
files with `BufferedMap`, never with `MmapStrategy`, and gate the
zero-copy bypass at `Write::write`'s `buf.len() >= buffer_size`
threshold behind an explicit caller opt-in.

## 6. Recommendation

Keep SQPOLL off until the basis-file-io-policy invariant is wired
into a static lint (`clippy::disallowed_methods` against
`MapFile::open_adaptive` from any module that constructs an
`IoUringWriter`) and the registered-buffer pre-fault loop is in
place for any code path that intentionally registers mmap-backed
ranges. `MAP_POPULATE` and `MADV_WILLNEED` remain opt-in workload
optimisations; neither substitutes for the buffered-basis invariant.

## References

- `crates/fast_io/src/mmap_reader.rs:77-91, 124-143` - the only mmap
  call site, the unused advice helpers.
- `crates/transfer/src/map_file/mmap.rs:36-66` -
  `MmapStrategy::open` and `map_ptr`.
- `crates/transfer/src/map_file/adaptive.rs:36-72` -
  `AdaptiveMapStrategy::open` and `open_buffered`.
- `crates/transfer/src/delta_apply/applicator.rs` -
  `BasisWriterKind` selector and io_uring-aware basis open.
- `crates/checksums/src/parallel/files.rs:42, 237, 340` - mmap
  consumers on the digest path that do not cross into io_uring.
- `crates/fast_io/src/io_uring/file_reader.rs`,
  `crates/fast_io/src/io_uring/file_writer.rs`,
  `crates/fast_io/src/io_uring/registered_buffers.rs` - submission
  sites enumerated in section 3.2.
- `target/interop/upstream-src/rsync-3.4.1/fileio.c:214-217` -
  upstream's documented rationale for not using `mmap(2)` on basis
  files.
- Linux kernel: `io_uring/sqpoll.c::io_sq_thread()` (kthread loop),
  `io_uring/io_uring.c::io_issue_sqe()` (SQE dispatch),
  `mm/memory.c::handle_mm_fault()` (page-fault entry),
  `kthread.c::kthread_use_mm()` (mm borrowing). On older kernels
  (< 5.10) these are in `fs/io_uring.c`.
- man pages: `io_uring_setup(2)` (`IORING_SETUP_SQPOLL`),
  `io_uring_register(2)` (`IORING_REGISTER_BUFFERS`),
  `madvise(2)` (`MADV_WILLNEED`, `MADV_SEQUENTIAL`,
  `MADV_NOHUGEPAGE`), `mmap(2)` (`MAP_POPULATE`).
