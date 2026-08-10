# io_uring SQPOLL meets mmap'd basis files: page-fault hazard audit

Tracking issue: oc-rsync task #1661.

Companion audits and design notes:

- `docs/audits/mmap-iouring-co-usage.md` (#1660) - inventory of every site
  where mmap and io_uring meet today.
- `docs/audits/mmap-page-fault-iouring-sqpoll.md` (#1661 sibling) - the
  three-failure-mode summary on the same hazard, written first; this
  audit is the long-form follow-up that the task brief asks for.
- `docs/audits/madvise-willneed-prefault.md` (#1662) - why
  `MADV_WILLNEED` is the prefault we keep, not the one we drop.
- `docs/audits/mmap-map-populate-evaluation.md` (#1663) - why
  `MAP_POPULATE` was rejected.
- `docs/design/basis-file-io-policy.md` (#1666) - selector rule that
  forbids mmap whenever io_uring is active on a transfer.
- `docs/design/iouring-session-ring-pool.md` (#1409) - session-level
  ring-pool design that, if SQPOLL is ever turned on, multiplies the
  number of poller kthreads and therefore the number of independent
  failure points covered by this audit.

## Methodology

The audit was conducted by reading every io_uring source file under
`crates/fast_io/src/io_uring/`, every basis-file mapper under
`crates/transfer/src/map_file/`, and every consumer of either path
under `crates/transfer/src/`, `crates/checksums/src/parallel/`, and
`crates/engine/src/local_copy/`. The four crates named in the task
brief - `fast_io`, `transfer/map_file`, `signature`, and
`engine/local_copy/dir_merge/parse` - were each grep-checked for
`mmap`, `memmap2`, `MmapReader`, `MapFile`, and io_uring opcodes that
take a user pointer (`Read`, `Write`, `ReadFixed`, `WriteFixed`).
Findings were cross-referenced against upstream Linux: the SQPOLL
kthread loop (`io_sq_thread()` in `io_uring/sqpoll.c` on modern
kernels, formerly `fs/io_uring.c`), the SQE dispatch
(`io_issue_sqe()`), and the page-fault entry a kthread must take when
it touches a non-resident user page (`handle_mm_fault()`, invoked
with `current->mm` borrowed via `kthread_use_mm()`).

Upstream rsync 3.4.1 was inspected at
`target/interop/upstream-src/rsync-3.4.1/fileio.c:214-217` for the
`SIGBUS`-on-truncate rationale that motivates `BufferedMap`. The
`signature` crate and `engine/local_copy/dir_merge/parse/` contain no
mmap or io_uring usage; the check is recorded here, the rest of the
audit treats them as out of scope.

## Where mmap meets io_uring in oc-rsync

Each site below was visited and its current behaviour verified against
the source. File:LINE citations are repository-relative.

### A. mmap producers

- `crates/fast_io/src/mmap_reader.rs:84` - the only place oc-rsync
  calls `MmapOptions::new().map(&file)`. No `populate`, no madvise on
  the constructor path. Returns an `MmapReader` whose `as_slice()`
  (line 97) yields a `&[u8]` over kernel-faulted file pages.
- `crates/fast_io/src/mmap_reader.rs:139-143` - `advise_willneed` over
  a sub-range. Defined, gated `#[cfg(unix)]`, currently uncalled from
  any io_uring submission site.
- `crates/transfer/src/map_file/mmap.rs:36-46` - `MmapStrategy::open`
  wraps `MmapReader::open`; `as_slice` and `map_ptr` (lines 50-66)
  return slices into the mapping with no advice or prefault.
- `crates/transfer/src/map_file/adaptive.rs:36-54` -
  `AdaptiveMapStrategy::open` selects `MmapStrategy` whenever
  `size >= MMAP_THRESHOLD` (1 MiB).
- `crates/transfer/src/map_file/adaptive.rs:68-72` - `open_buffered`
  forces the buffered variant; this is the io_uring-safe entry point
  used by `DeltaApplicator` when its writer is io_uring-backed.
- `crates/checksums/src/parallel/files.rs:42, 237, 340` - parallel
  digest paths open `MmapReader` for whole-file hashing. Never crosses
  into io_uring.

### B. io_uring submission sites that take a user pointer

- `crates/fast_io/src/io_uring/file_reader.rs:99-138` - `read_at`
  builds an `IORING_OP_READ` SQE pointing at a caller-owned
  `&mut [u8]`.
- `crates/fast_io/src/io_uring/file_reader.rs:149-184` -
  `read_all_batched`; the `READ_FIXED` branch (lines 158-184) submits
  SQEs against `RegisteredBufferGroup` heap buffers, the fallback
  branch (line 188+) submits regular `Read` SQEs into the caller's
  destination.
- `crates/fast_io/src/io_uring/file_writer.rs:211-265` -
  `write_all_batched`; the `WRITE_FIXED` branch references registered
  heap buffers, the fallback branch hands `data: &[u8]` to
  `submit_write_batch`.
- `crates/fast_io/src/io_uring/file_writer.rs:330-350` - `Write::write`
  copies small writes into the writer's internal buffer but bypasses
  the copy when `buf.len() >= self.buffer_size` (default 256 KiB),
  submitting the caller's slice directly. Highest-risk seam if a
  future caller hands it an mmap-backed slice.
- `crates/fast_io/src/io_uring/registered_buffers.rs:251-315` -
  `RegisteredBufferGroup::new` allocates page-aligned heap buffers via
  `alloc::alloc_zeroed` (line 285) and pins them with
  `IORING_REGISTER_BUFFERS` (line 307). Registration calls
  `get_user_pages_fast` in the kernel; the buffers are heap-only by
  construction so registration never touches a file mapping.
- `crates/fast_io/src/io_uring/registered_buffers.rs::submit_read_fixed_batch`
  / `submit_write_fixed_batch` - submit `READ_FIXED` / `WRITE_FIXED`
  SQEs that name a registered-buffer index, not a user pointer.

### C. Where A meets B in production today

The cross-reference table from `docs/audits/mmap-iouring-co-usage.md`
holds: nowhere on the wired transfer path does an mmap-derived
pointer reach an io_uring SQE. The receiver (`transfer_ops/response.rs`,
`transfer_ops/streaming.rs`, `transfer_ops/token_loop.rs`) opens basis
files via `MapFile::open` (`BufferedMap`), and `DeltaApplicator`
(`crates/transfer/src/delta_apply/applicator.rs:154-184`) selects
`MapFile::open_adaptive_buffered` when `BasisWriterKind::IoUring` is
in effect, so the io_uring-paired applicator never sees an
`MmapStrategy`.

### D. SQPOLL configuration surface

- `crates/fast_io/src/io_uring/config.rs:305-314` - `IoUringConfig`
  exposes `sqpoll: bool` and `sqpoll_idle_ms: u32`.
- `crates/fast_io/src/io_uring/config.rs:336, 353, 368` - all three
  builtin presets (`Default`, `for_large_files`, `for_small_files`)
  set `sqpoll: false`. SQPOLL is opt-in at every layer.
- `crates/fast_io/src/io_uring/config.rs:381-396` - `build_ring`
  attempts `setup_sqpoll(self.sqpoll_idle_ms)` first; on any failure
  (typically `EPERM` for missing `CAP_SYS_NICE`) it sets the
  process-wide `SQPOLL_FALLBACK` atomic and falls back to a regular
  ring. The fallback path is the only one currently exercised by CI.
- `crates/fast_io/src/io_uring/mod.rs:56-59, 63-70` - module-level
  documentation of SQPOLL's privilege requirements and the
  ring-creation fallback chain.

No production caller flips `sqpoll` to `true`; every test in
`io_uring/tests.rs:374-794` sets `sqpoll: false`. The hazard below is
therefore forward-looking - the contract that any future caller
enabling SQPOLL must honour.

## SQPOLL kernel-thread page-fault behaviour

`IORING_SETUP_SQPOLL` (man `io_uring_setup(2)`) starts a kernel
thread that polls the submission queue head and submits SQEs on
behalf of the userspace owner. The kthread runs with the submitter's
`mm` borrowed (`kthread_use_mm()` on modern kernels, `use_mm()` on
older ones) so that user-virtual pointers in an SQE resolve through
the right page tables. What it does *not* run with is the userspace
task's signal-delivery context, fault-handling stack, or scheduler
priority class. That asymmetry is the hazard.

When the SQPOLL kthread services an SQE that references a non-resident
user page, three things can happen depending on kernel version, the
opcode, and the mapping type. The relevant kernel entry points are
`io_issue_sqe()` (the SQE dispatch) and `handle_mm_fault()` (the page
fault that the dispatch implicitly triggers when it copies bytes to
or from the user pointer):

1. **Synchronous fault inside the kthread.** The kthread runs
   `handle_mm_fault()` itself. For an anonymous page or a populated
   file page this completes in microseconds. For a cold file page the
   fault blocks on filesystem I/O. While the kthread is blocked, the
   submission queue stops draining; every other ring fed by the same
   kthread (post-5.11 the kthread can be shared across rings if the
   `wq_fd` parameter ties them together) stalls behind it. SQPOLL was
   enabled to remove `io_uring_enter` syscalls; a fault stall converts
   that latency win into the same syscall path you would have had
   without SQPOLL, plus a context switch.

2. **Punt to `io-wq` task-work.** If the kernel detects that the
   submission cannot complete inline - for example the opcode is
   marked `IO_WQ_WORK_*` and the page is missing - it queues the
   request to an `io-wq` worker that re-runs the op with the
   submitter's `mm` and can take the fault on its own stack. The
   request now costs one SQ-poll wakeup, one work-queue dispatch, and
   one CQE; latency becomes equal to (or worse than) a non-SQPOLL ring.

3. **Short or `-EFAULT` completion.** On older kernels (pre-5.12-ish
   for the `IORING_OP_READ`/`WRITE` family) and on opcodes that do
   not punt, the kthread fails the op outright. The CQE returns a
   short result or `-EFAULT`. Writers retry on short, but `-EFAULT`
   is fatal: it surfaces as `io::Error` from `submit_and_wait`, and
   the transfer aborts. The truncate variant of this is the
   `SIGBUS`-on-mid-transfer-truncate case that upstream rsync
   sidesteps by never using `mmap(2)` for basis files
   (`fileio.c:214-217`); under SQPOLL the fault is delivered in
   kernel context, so the userspace `SIGBUS` handler upstream relies
   on cannot run.

The `IORING_REGISTER_BUFFERS` path is different. Registration calls
`get_user_pages_fast()` synchronously, faulting each page in and
pinning it for the registration's lifetime. If the registered buffers
were ever backed by an mmap, registration either eats the fault cost
up front (turning a deferred stall into an open-time stall) or
returns `-EFAULT` for a hole. After successful registration the
SQPOLL kthread cannot fault on those pages because they are pinned.
Today oc-rsync only registers heap allocations
(`registered_buffers.rs:285`), so the basis pointer never becomes the
registered iovec on either the `READ_FIXED` (basis is the source,
heap buffer is the destination) or `WRITE_FIXED` (basis is read into
the heap buffer first) path.

## Current mitigations

These are the layered defences oc-rsync has accumulated, in the order
they apply at runtime.

1. **`MAP_POPULATE` was evaluated and rejected (#1663,
   `docs/audits/mmap-map-populate-evaluation.md`).** The audit
   concludes that `MAP_POPULATE` is the wrong instrument: it pre-faults
   the entire mapping at `mmap` time proportional to file size, while
   delta transfer typically reads only 30-60% of the basis. The
   synchronous open-time cost is paid for bytes the worker never
   touches. The audit also notes that even a fully populated mapping
   is still vulnerable to truncate-`SIGBUS`, so `MAP_POPULATE` does
   not close the failure-mode-3 hole. Decision: do not use
   `MAP_POPULATE`; document the choice so it is not re-litigated.

2. **`MADV_WILLNEED` per-range prefault is the kept primitive
   (#1662, `docs/audits/madvise-willneed-prefault.md`).** Issued from
   the basis-file consumer immediately before the io_uring SQE that
   will dereference the slice, `posix_madvise(MADV_WILLNEED)` queues
   asynchronous readahead so the pages are likely resident by the
   time the kthread services the op. The hook
   (`MmapReader::advise_willneed`,
   `crates/fast_io/src/mmap_reader.rs:139-143`) exists and is gated
   `#[cfg(unix)]`. Errors are deliberately ignored: the hint is
   advisory and any failure (`EBADF`, `EINVAL` on holes) is
   non-fatal. Telemetry is via the existing IO3 debug trace
   (`crates/fast_io/src/debug_io.rs:574-593`).

3. **Pre-fault loop for io_uring registered buffers backed by mmap
   (#1665, in progress).** When registered-buffer setup is changed
   to accept caller-owned pages (a future optimisation, not the
   current code), the registration call site must walk the buffer
   one byte per page to force the userspace task to take the faults
   itself, before `IORING_REGISTER_BUFFERS` calls
   `get_user_pages_fast`. This keeps faults on the submitter rather
   than the SQPOLL kthread and turns deferred `-EFAULT` outcomes
   into eager `EFAULT` from the registration syscall, which is
   recoverable. The loop is portable, costs one minor fault per page
   (typically 1-3 microseconds), and is paired with `MADV_SEQUENTIAL`
   so the kernel readahead window absorbs most of the per-page cost.

4. **mmap-vs-buffered policy with io_uring as a hard gate (#1666,
   `docs/design/basis-file-io-policy.md`).** The decision matrix
   in that document treats `io_uring_active` as a forcing column:
   when it is `true`, the basis-file strategy is `BufferedMap`
   regardless of file size, sparse likelihood, or any other input.
   The implementation entry point is
   `AdaptiveMapStrategy::open_buffered`
   (`crates/transfer/src/map_file/adaptive.rs:70-72`), reachable from
   `MapFile::open_adaptive_buffered` and wired through
   `DeltaApplyConfig::writer_kind`
   (`crates/transfer/src/delta_apply/applicator.rs:73-87, 154-184`).
   This is the only mitigation that survives all three failure modes
   without per-site hardening: an mmap pointer cannot reach the ring
   if `MmapStrategy` is never constructed.

The four compose: (1) and (2) are workload optimisations, (3) is
correctness for a not-yet-wired registered-buffer path, (4) is the
load-bearing invariant. (4) alone makes the others optional for the
basis-file path; (1)+(2)+(3) without (4) is strictly weaker, because
failure mode 3 (truncate-`SIGBUS` in kernel context) is not closed by
any prefault primitive.

## Gaps

Concrete failure modes that survive all four mitigations and that
this audit explicitly flags as residual risk:

- **Transparent-hugepage NUMA migrations.** Even after `MADV_WILLNEED`
  succeeds and the basis pages are resident, a kernel with
  `transparent_hugepage=always` and active `khugepaged` may collapse
  4 KiB pages into 2 MiB hugepages or migrate them across NUMA nodes
  while the SQE is in flight. The migration invalidates the kernel's
  PTE for the range, and the SQPOLL kthread re-faults during dispatch.
  This is invisible from userspace and not reported in the CQE; it
  manifests as latency jitter, not error. Mitigation (4) avoids this
  for the basis path entirely; mitigation (3) does not, because the
  pre-fault loop runs before registration but cannot prevent later
  migration. There is no userspace API on Linux today that disables
  `khugepaged` migration for a specific mapping; `MADV_NOHUGEPAGE` is
  prophylactic only. Recommended forward action: if any future code
  registers mmap-backed buffers, pair the pre-fault loop with
  `MADV_NOHUGEPAGE` on the same range.

- **MMU notifier teardown during shutdown.** When the basis file is
  closed (process exit, panic, or explicit `Drop` of `MmapReader`),
  the kernel runs MMU notifiers that invalidate any pinned references
  in the io_uring ring. If the ring's SQ contains in-flight SQEs that
  reference the mapping, the kthread sees stale PTEs and either
  completes them with `-EFAULT` or returns short. The current
  `IoUringWriter` field-drop order
  (`docs/audits/mmap-iouring-co-usage.md` finding F6) ensures the
  ring fd is closed before any registered-buffer allocation is freed,
  but it does *not* govern the order of `MmapReader::Drop` against
  ring teardown - because today no caller holds both. If a future
  refactor introduces an `IoUringWriter` that carries an `MmapReader`
  field, the field order must be ring-then-mapper. Document this
  invariant before that pattern lands.

- **Basis-file truncation race.** Mitigation (4) closes this for
  basis files on the io_uring path, but it does not close it for the
  parallel-checksum digest path
  (`crates/checksums/src/parallel/files.rs:42, 237, 340`). That path
  uses `MmapReader` without io_uring; a concurrent truncation by
  another process raises `SIGBUS` synchronously on the digest
  thread's userspace stack. The default panic-on-`SIGBUS` aborts the
  process. The risk window is the digest pass, which can be tens of
  seconds on multi-GiB basis files. There is no current handler.

- **Direct I/O alignment under SQPOLL.** `IoUringConfig::direct_io`
  (`crates/fast_io/src/io_uring/config.rs:298`) is currently false in
  every preset, but a future caller that enables both `direct_io` and
  `sqpoll` would face a third constraint: O_DIRECT requires
  block-aligned user buffers, and an mmap-backed pointer satisfies
  that only if the mapping was created with a hugepage or with
  filesystem-block alignment. Misalignment under O_DIRECT returns
  `-EINVAL` from the kthread, which the SQPOLL retry path does not
  recover from in the same way it recovers from `-EAGAIN`.
  Mitigation (4) again closes this for the basis path, but the gap
  is recorded so that if `direct_io` is ever turned on, the
  `BufferedMap` invariant is doubly load-bearing.

- **macOS / non-Linux semantics.** The whole audit is Linux-only:
  `io_uring` does not exist on Darwin, FreeBSD, or Windows, and the
  stub at `crates/fast_io/src/io_uring_stub.rs` returns
  `is_io_uring_available() == false` on those platforms. The mmap
  hazards (truncate-`SIGBUS` in particular) still apply on macOS, but
  there is no SQPOLL kthread there. Cross-platform code must continue
  to gate SQPOLL-specific mitigations behind `#[cfg(target_os = "linux")]`
  blocks; mitigation (4) is portable because `BufferedMap` is the
  only basis strategy on non-Unix anyway.

## Recommendation

When an io_uring writer is in play on a transfer, the basis file MUST
be served by `BufferedMap` regardless of size, sparseness, inplace
flags, or filesystem; this is the policy in `basis-file-io-policy.md`
and the only mitigation that survives kernel-context faults and
mid-transfer truncation. Keep `MAP_POPULATE` off; keep `MADV_WILLNEED`
and the pre-fault loop available for any future code path that
deliberately ships mmap'd pages to the kernel (signature scans,
checksum-only modes, future registered-buffer optimisations) but do
not rely on them as a substitute for the buffered-basis invariant.
Disable SQPOLL in any future caller that cannot guarantee both the
buffered-basis invariant and a stable, single-node, non-O_DIRECT page
backing for every SQE - which today means: leave SQPOLL off, full
stop, until the ring-pool design (#1409) and the registered-buffer
mmap path (#1665) have landed and been benchmarked.

## Test surface

The existing test seam is the empty-set baseline: every io_uring test
in `crates/fast_io/src/io_uring/tests.rs` builds a non-SQPOLL ring,
and every basis-file test in `crates/transfer/src/map_file/tests.rs`
exercises `BufferedMap` or `MmapStrategy` without io_uring, so neither
file currently exercises the hazard.

Task #1664 (mmap + io_uring memory-pressure test) is the planned
coverage: it would build a SQPOLL ring on a CI runner with
`CAP_SYS_NICE`, allocate a multi-GiB sparse basis file, and submit
`READ` SQEs against an `MmapReader::as_slice()` window under
artificial memory pressure (`memory.high` cgroup limit on Linux 5.13+).
The expected outcome with mitigation (4) in place is that the test
is unreachable: the production code path never constructs an
`MmapStrategy` while `io_uring_active` is true, so the test must
manually bypass the policy to exercise the hazard, and that bypass is
the load-bearing assertion.

Additional checks not covered by #1664 that this audit recommends:

- **NUMA-migration reproducer.** Run a transfer with
  `numactl --cpunodebind=0 --membind=1` and `transparent_hugepage=always`
  on a two-node host, with a basis file pre-warmed on node 0 and the
  receiver pinned to node 1. Assert that the buffered-basis invariant
  holds and that no `khugepaged` migration of basis pages can be
  observed via `/proc/<pid>/numa_maps`. This protects gap 1.

- **Drop-order regression test.** Construct a hypothetical
  `IoUringWriter` carrying an `MmapReader` field (gated behind a
  `#[cfg(test)]` builder), submit a long-running SQE, panic from
  another thread, and assert no `-EFAULT` in the CQE. The test cannot
  be written today because no production type couples the two; the
  scaffolding belongs in the same file as the F6 drop-order invariant
  so the regression test exists the moment the coupling lands.
  Protects gap 2.

- **Truncate race for the parallel-checksum digest path.** Open an
  `MmapReader` on a 256 MiB file, truncate from another thread
  mid-digest, and assert the process either aborts cleanly or returns
  an `io::Error` rather than a silently-wrong digest. Current code
  aborts; the test pins that behaviour. Protects gap 3.

- **SQPOLL fallback assertion in CI.** Existing
  `build_ring_with_sqpoll_falls_back_gracefully`
  (`crates/fast_io/src/io_uring/config.rs:714-728`) verifies
  `build_ring` succeeds when `CAP_SYS_NICE` is missing. Add a sibling
  asserting `sqpoll_fell_back() == true` on unprivileged runners, so
  a kernel that quietly grants SQPOLL without `CAP_SYS_NICE` cannot
  flip production into a real SQPOLL ring unnoticed.

- **Static check via `clippy::disallowed_methods`.** Refuse calls to
  `MapFile::open_adaptive` from any module that constructs an
  `IoUringWriter`. Pins mitigation (4) against future refactors.

No code changes are made by this audit. The remediation work
(implementing the recommended tests, adding the static lint, wiring
the registered-buffer pre-fault loop) is tracked under #1664, #1665,
and follow-up issues out of scope for #1661.

## References

- `crates/fast_io/src/io_uring/config.rs:305-314, 336, 353, 368, 381-396` -
  SQPOLL config, defaults, and fallback path.
- `crates/fast_io/src/io_uring/file_reader.rs:99, 149, 173` - read
  submission sites.
- `crates/fast_io/src/io_uring/file_writer.rs:211, 330` - write and
  bypass-branch submission sites.
- `crates/fast_io/src/io_uring/registered_buffers.rs:251-315, 425, 544` -
  buffer registration and fixed-buffer submits.
- `crates/fast_io/src/io_uring/mod.rs:56-79` - SQPOLL privilege table
  and ring-creation fallback chain.
- `crates/fast_io/src/mmap_reader.rs:84, 124-143` - the only mmap
  call site, the unused advice helpers.
- `crates/transfer/src/map_file/mmap.rs:36-77` - `MmapStrategy::open`
  and `map_ptr`.
- `crates/transfer/src/map_file/adaptive.rs:36-85` -
  `AdaptiveMapStrategy::open` and `open_buffered`.
- `crates/transfer/src/delta_apply/applicator.rs:60-87, 154-184` -
  `BasisWriterKind` selector and io_uring-aware basis open.
- `crates/checksums/src/parallel/files.rs:42, 237, 340` - mmap
  consumers on the digest path that do not cross into io_uring.
- `target/interop/upstream-src/rsync-3.4.1/fileio.c:214-217` -
  upstream's documented rationale for not using `mmap(2)` on basis
  files.
- Linux kernel: `io_uring/sqpoll.c::io_sq_thread()` (kthread loop),
  `io_uring/io_uring.c::io_issue_sqe()` (SQE dispatch),
  `mm/memory.c::handle_mm_fault()` (page-fault entry), `kthread.c::kthread_use_mm()`
  (mm borrowing). On older kernels (< 5.10) these all live in
  `fs/io_uring.c`.
- man pages: `io_uring_setup(2)` (`IORING_SETUP_SQPOLL`),
  `io_uring_register(2)` (`IORING_REGISTER_BUFFERS`),
  `madvise(2)` (`MADV_WILLNEED`, `MADV_NOHUGEPAGE`),
  `mmap(2)` (`MAP_POPULATE`).
