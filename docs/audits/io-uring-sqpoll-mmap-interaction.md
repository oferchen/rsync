# io_uring SQPOLL + mmap page-fault interaction audit

Tracking: oc-rsync follow-up to PR #3493. Re-verifies the SQPOLL kernel-thread
page-fault hazard against the current code base.

Companion documents (already in tree):

- `docs/audits/mmap-page-fault-iouring-sqpoll.md` (PR #3493, task #1661) -
  the document this audit re-verifies.
- `docs/audits/mmap-iouring-co-usage.md` (task #1660) - call-site inventory.
- `docs/audits/io_uring_sqpoll_mmap_pagefault.md` - long-form follow-up.
- `docs/audits/mmap-iouring-sqpoll-pagefaults.md` - kernel-thread mechanics.
- `docs/audits/iouring-sqpoll-bench-plan.md` (task #1626) - SQPOLL benchmark
  plan, pending.
- `docs/audits/iouring-socket-sqpoll-defer-taskrun.md` (task #1267) - daemon
  socket choice that explicitly rules SQPOLL out.
- `docs/design/basis-file-io-policy.md` (task #1666) - selector rule that
  forbids `MmapStrategy` whenever an io_uring writer is active.
- `docs/design/io-uring-ring-pool.md` (task #1936, PR #4014) - per-session
  ring-pool design that multiplies the number of SQPOLL kthreads should
  SQPOLL ever flip on.

## Verdict

**OK-with-caveat.**

There is no live wired path where a `MmapReader` /
`MmapStrategy`-backed buffer can be referenced from an SQPOLL'd io_uring SQE
today. The hazard is closed by three independent layers:

1. SQPOLL is off in every preset and never flipped on by a production caller
   (`crates/fast_io/src/io_uring/config.rs:374, 392, 408`).
2. `IoUringDiskBatch` writes from its own owned heap `Vec<u8>`
   (`crates/fast_io/src/io_uring/disk_batch.rs:51, 76`); `RegisteredBufferGroup`
   allocates page-aligned heap buffers via `alloc::alloc_zeroed`
   (`crates/fast_io/src/io_uring/registered_buffers.rs:283-301`).
3. `DeltaApplicator` forces `BufferedMap` for the basis file when its writer
   is io_uring-backed (`crates/transfer/src/delta_apply/applicator.rs:161-176`).

The **caveat** is that none of these layers is defended by a runtime check
that refuses SQPOLL when an `MmapReader` or `MmapStrategy` is in scope. The
invariant is held by convention at the call sites. The opt-in path
(`IoUringConfig::sqpoll = true`) builds the ring without consulting any
mmap state, and `IoUringWriter::write` still has a zero-copy bypass that
submits the caller's `&[u8]` directly when `len >= buffer_size`
(`file_writer.rs:330`, see audit #3440 F3). One future caller pairing
SQPOLL with `MmapStrategy` would reopen the hazard.

## Evidence table

Citations are repository-relative.

| Scenario | Code citation | Status | Note |
|---|---|---|---|
| SQPOLL gating in `IoUringConfig` | `crates/fast_io/src/io_uring/config.rs:330-339` | **OK** | `sqpoll: bool` is opt-in; doc-string at `:330-335` records `CAP_SYS_NICE` requirement and silent fallback. |
| SQPOLL defaults across presets | `crates/fast_io/src/io_uring/config.rs:374, 392, 408` | **OK** | `Default`, `for_large_files`, `for_small_files` all set `sqpoll: false`. No preset turns it on. |
| Stub `IoUringConfig` on non-Linux / no `io_uring` feature | `crates/fast_io/src/io_uring_stub.rs:84, 102, 118` | **OK** | Stub mirrors the same defaults so a recompile of code referencing the field compiles without surprise. |
| SQPOLL build + fallback | `crates/fast_io/src/io_uring/config.rs:436-451` | **OK** | `build_ring` attempts `setup_sqpoll(self.sqpoll_idle_ms)`; on any failure records `SQPOLL_FALLBACK` (`:30, :45`) and degrades to a plain ring. No mmap check here - intentional, the invariant lives at the caller. |
| Production callers that set `sqpoll: true` | none | **OK** | grep across `crates/` returns matches only inside `config.rs` tests (`:783, :801`). Wired paths (`crates/transfer/src/disk_commit/thread.rs:78`, `crates/transfer/src/pipeline/receiver.rs:79`, socket factory `crates/fast_io/src/io_uring/socket_factory.rs:66, 117`) take `IoUringConfig::default()` which is `sqpoll: false`. |
| `IoUringDiskBatch` SQE source buffer | `crates/fast_io/src/io_uring/disk_batch.rs:46-78` | **OK** | Owns its own `Vec<u8>` (line 51) initialised to `config.buffer_size.max(256 KiB)` (line 76). Never holds an mmap. |
| `RegisteredBufferGroup` SQE source buffer | `crates/fast_io/src/io_uring/registered_buffers.rs:283-307` | **OK (heap-only)** | `alloc::alloc_zeroed` with page-aligned layout (line 285) followed by `IORING_REGISTER_BUFFERS` (line 307). Kernel pins the heap pages via `get_user_pages_fast`; no file mapping participates. |
| `BufferRing` mmap (kernel-managed, not file-backed) | `crates/fast_io/src/io_uring/buffer_ring.rs:555-565, 824-829` | **OK** | The mmap target here is `IORING_OFF_PBUF_RING` on the io_uring fd itself - the kernel-owned provided-buffer descriptor region. Not a file mapping, not subject to the user-page-fault path. |
| `MmapReader::open` - the only file-backed mmap producer in `fast_io` | `crates/fast_io/src/mmap_reader.rs:77-91` | **OK** (consumers all non-uring) | `MmapOptions::new().map(&file)` without `MAP_POPULATE`, without `madvise` on the constructor path. |
| `MmapReader` consumers - parallel checksum digest | `crates/checksums/src/parallel/files.rs:42, 237, 340` | **OK** | Digest pass reads the slice in userspace; no io_uring SQE references it. |
| `MmapReader` consumers - `MmapStrategy` basis mapper | `crates/transfer/src/map_file/mmap.rs:27, 38` | **OK** (gated by selector) | Wrapped by `AdaptiveMapStrategy`; the receiver's `DeltaApplicator` downgrades to `BufferedMap` whenever the writer is io_uring-backed. |
| `DeltaApplicator` basis-vs-uring selector | `crates/transfer/src/delta_apply/applicator.rs:161-176` | **OK** | `config.writer_kind.is_io_uring()` -> `MapFile::open_adaptive_buffered`; else `MapFile::open_adaptive`. Only call-site that could put `MmapStrategy` on an io_uring writer's data path. |
| `BasisWriterKind::IoUring` is the documented hard gate | `crates/transfer/src/delta_apply/applicator.rs:50-71` | **OK** | Enum doc-strings explicitly cite SQPOLL stall and `SIGBUS` rationale. |
| `IoUringDiskBatch` source buffer never aliases mmap | `crates/transfer/src/disk_commit/process.rs` consumers feeding `IoUringDiskBatch::write_chunk` | **OK** | Disk-commit thread feeds heap-owned literal data from the wire; mapper is on the network thread (audit F4). |
| `IoUringWriter::write` zero-copy bypass at `len >= buffer_size` | `crates/fast_io/src/io_uring/file_writer.rs:211, 330` | **GAP (latent)** | Hands the caller's slice straight to `submit_write_batch` (no copy) when the buffer is large. If a future caller passes an mmap-backed `&[u8]` here while SQPOLL is on, the hazard is live. Audit #3440 finding F3 already calls this out; nothing in the current code base reaches this seam with an mmap, but there is no compile-time or runtime guard. |
| Mmap-pressure integration test | `crates/fast_io/tests/io_uring_mmap_pressure.rs:106` | **OK (non-SQPOLL)** | The existing integration test (`io_uring_reads_tolerate_mmap_madv_dontneed`) is helpful coverage for `MADV_DONTNEED` under a regular ring but explicitly does NOT enable SQPOLL: it uses `SharedRingConfig::default()`. The SQPOLL variant is the planned `#1664` follow-up. |
| Cross-platform stub | `crates/fast_io/src/io_uring_stub.rs:50` | **OK** | `sqpoll_fell_back()` returns `false` on non-Linux; mmap hazards on macOS/Windows are unrelated to SQPOLL. |

## Why this is OK-with-caveat rather than OK

The audit chain (#1660, #1661, #1666, #1906) successfully moved the
load-bearing invariant - "no mmap pointer in an io_uring SQE" - into the
`DeltaApplicator` constructor. That constructor enforces it
unconditionally when `BasisWriterKind::IoUring` is set. Every wired io_uring
consumer today is either `IoUringDiskBatch` (heap-only) or a registered
buffer group (heap-only). The current code base has no path that touches
mmap'd memory from an io_uring SQE - SQPOLL or otherwise. The verdict on
the present tree is therefore **OK**.

The remaining gap is forward-looking:

- The `IoUringConfig::build_ring` call (`config.rs:436-451`) does not
  consult any mmap state. A caller that flips `sqpoll = true` is taken at
  its word.
- The opt-in `MapFile<MmapStrategy>` constructor (`wrapper.rs:48-58`) and
  the public `MmapReader::open` (`mmap_reader.rs:77`) both produce
  file-backed mapped slices that are `Send`, `Sync`, and look like a normal
  `&[u8]` at the API surface.
- `IoUringWriter::write` bypasses its internal copy buffer at
  `len >= buffer_size` (default 256 KiB; `file_writer.rs:330`). The bypass
  submits the caller's pointer directly to `IORING_OP_WRITE`. The audit
  #3440 finding F3 already flags this seam; the present audit re-confirms
  it has no compile-time or runtime guard.

Any patch that (a) turns on SQPOLL in a production caller and (b) hands a
`MapFile<MmapStrategy>` slice (or a `MmapReader::as_slice()` slice) through
`IoUringWriter::write` >= 256 KiB or through any iovec-taking submit would
land the hazard described in PR #3493: a cold-page fault inside the SQPOLL
kthread stalls the entire SQ, or a third-party truncation surfaces as
`-EFAULT` (or `SIGBUS` from kernel context, with no userspace handler).
This is the residual-risk class that promotes the verdict to
"with caveat".

## Recommendation (single-PR fix to close the caveat)

Refuse SQPOLL at ring construction unless the caller has opted into the
mmap-incompatible path. Concretely, change `IoUringConfig::build_ring`
(`crates/fast_io/src/io_uring/config.rs:436-451`) to require an explicit
`allow_mmap_basis: bool` invariant on the config, defaulting to `false`,
and skip `setup_sqpoll` when the invariant cannot be guaranteed. Tie the
invariant to the same writer-kind signal that already drives the
`DeltaApplicator` selector:

- `BasisWriterKind::Standard` -> SQPOLL request honoured, but no io_uring
  writer is wired so no SQE is in flight.
- `BasisWriterKind::IoUring` -> SQPOLL request honoured, basis is forced
  to `BufferedMap` by the existing selector, no mmap can reach the ring.
- Any future "io_uring writer with mmap basis" caller -> the config
  builder refuses to set `setup_sqpoll`, returns `Err`, and the operator
  sees a clear "SQPOLL + mmap basis not supported" diagnostic instead of
  a silent kernel-thread stall.

The alternative - forcing `MAP_POPULATE` or `mlock` on every
`MmapReader::open` - was already evaluated and rejected
(`docs/audits/mmap-map-populate-evaluation.md`, task #1663). The basis
file is typically larger than the workload reads, so pre-faulting wastes
RAM and does not close the truncate-`SIGBUS` failure mode. The refuse-at-
construction approach is the single-PR mitigation that survives all three
failure modes (stall, task-work fallback, EFAULT/SIGBUS) without
per-call-site hardening.

This is a follow-up improvement, not a current bug fix: nothing today
constructs an SQPOLL ring on a wired path. The change defends against the
next contributor pairing SQPOLL with an mmap basis.

## References

- `crates/fast_io/src/io_uring/config.rs:25-46, 309-451` - SQPOLL config
  surface and ring-build fallback.
- `crates/fast_io/src/io_uring/file_writer.rs:211, 330` - zero-copy
  bypass seam.
- `crates/fast_io/src/io_uring/disk_batch.rs:46-78` - heap-only batch
  writer.
- `crates/fast_io/src/io_uring/registered_buffers.rs:283-307` -
  heap-only registered buffers.
- `crates/fast_io/src/io_uring/buffer_ring.rs:555-565` - kernel-managed
  PBUF_RING mmap (not a file mapping).
- `crates/fast_io/src/mmap_reader.rs:77-91, 124-143` - the sole
  file-backed mmap producer in `fast_io`.
- `crates/transfer/src/map_file/wrapper.rs:48-58, 102-110` -
  `MapFile::open` / `open_adaptive_buffered` entry points.
- `crates/transfer/src/delta_apply/applicator.rs:50-71, 154-194` -
  `BasisWriterKind` gate enforcing buffered basis on io_uring writers.
- `crates/checksums/src/parallel/files.rs:42, 237, 340` - parallel
  digest paths that own `MmapReader` without ever touching io_uring.
- `crates/fast_io/tests/io_uring_mmap_pressure.rs:54-155` - existing
  non-SQPOLL mmap pressure test; SQPOLL variant remains task #1664.
- PR #3493 - origin audit that documented this hazard.
- Task #1626 - SQPOLL benchmark plan, pending.
- Task #1936 / PR #4014 - per-session ring-pool design that multiplies
  the SQPOLL footprint once enabled.
