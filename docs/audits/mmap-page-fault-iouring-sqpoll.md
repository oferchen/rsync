# mmap page-fault impact on io_uring SQPOLL

Task: #1661. Branch: `docs/mmap-page-fault-sqpoll`. Companion to the
mmap + io_uring co-usage audit landed as #3440
(`docs/audits/mmap-iouring-co-usage.md`).

## Summary

When `IORING_SETUP_SQPOLL` is enabled, a kernel SQ-poller kthread submits
SQEs on our behalf. If an SQE references a mmap'd basis-file page that is
not yet resident, the kthread context cannot service a major page fault
the way a userspace task can. The result on production kernels is either
a stall, a fall-back to task-work that erases the SQPOLL latency win, or
an `-EFAULT` completion. This audit records the hazard so that whoever
wires SQPOLL in oc-rsync prefaults or avoids mmap on the basis side first.

## The hazard

`io_uring` SQPOLL (man `io_uring_setup(2)`) starts a kthread that polls
our submission queue and parks after `sqpoll_idle_ms` of inactivity. The
kthread runs in kernel context with the submitter's address space pinned,
giving it immediate read-access to anonymous heap pages (already faulted
or zero-filled on first kernel touch). mmap'd file pages are demand-faulted:
a major fault from a kthread maps to `-EAGAIN` retry-via-task-work plus a
queued worker hop, not the `do_page_fault` path a userspace thread takes.

Three failure modes follow when the SQ kthread dereferences a non-resident
mmap'd page during `IORING_OP_READ` / `WRITE` / `READ_FIXED` /
`WRITE_FIXED`:

1. Stall - the kthread blocks on the readahead path, parking every other
   SQE behind it; the property SQPOLL was enabled for evaporates.
2. Task-work fallback - newer kernels punt the faulting SQE to an `io-wq`
   worker that re-runs the op with the submitter's mm; latency becomes
   equivalent to a non-SQPOLL ring plus a context switch.
3. Short or `-EFAULT` completion - on older kernels, or when the mapping
   is invalidated mid-op (truncate, hole-punch), the CQE returns short or
   `-EFAULT`. Writers retry shorts but `-EFAULT` propagates and kills
   the transfer.

The truncate variant is the `SIGBUS` hazard upstream avoids by not using
`mmap(2)` for basis files (`fileio.c:214-217`); under SQPOLL the fault is
in-kernel and not recoverable from a userspace signal handler.

## Affected oc-rsync call sites

Citations are repository-relative. None is wired to a mmap'd basis on a
live transfer path today (audit #3440 verified this), but each is one
careless caller away from being so.

- Buffer registration:
  `crates/fast_io/src/io_uring/registered_buffers.rs:238` allocates
  page-aligned buffers via `alloc::alloc_zeroed`; `:260` registers them
  through `submitter.register_buffers(&iovecs)`. Heap-only by
  construction, so the fixed-buffer path is SQPOLL-safe.
- Fixed-buffer submit:
  `crates/fast_io/src/io_uring/registered_buffers.rs:425`
  (`submit_read_fixed_batch`) and `:544` (`submit_write_fixed_batch`) -
  SQEs reference registered heap buffers.
- Read submit (non-fixed):
  `crates/fast_io/src/io_uring/file_reader.rs:99` (`read_at`),
  `:149` (`read_all_batched`), `:173` (in-loop `submit_read_fixed_batch`).
  Destination is a caller-owned `&mut [u8]`; an mmap-backed `MmapMut`
  destination would expose this site.
- Write submit (non-fixed) bypass branch:
  `crates/fast_io/src/io_uring/file_writer.rs:211, 330` -
  `write_all_batched` and `Write::write` submit the caller's pointer
  directly when `len >= buffer_size` (default 256 KiB). Highest-risk
  seam; called out as #3440 Finding F3.
- SQPOLL setup itself:
  `crates/fast_io/src/io_uring/config.rs:382, 384` - `setup_sqpoll` is
  opt-in via `IoUringConfig::sqpoll` and falls back to a plain ring on
  `EPERM` (`config.rs:388-390`). Default keeps SQPOLL off (`:336`).

## Mitigations

- `MAP_POPULATE` at mmap time (#1663, PR #3442) - prefaults the mapping
  during `mmap(2)`. Fixes failure modes 1 and 2; does not fix mode 3
  (truncate still yields `-EFAULT` / `SIGBUS`).
- `madvise(MADV_WILLNEED)` (#1662 audit) - hints the kernel to start
  readahead but does not block. Useful for very large basis files where
  `MAP_POPULATE` would burn RAM; not a guarantee pages are resident at
  SQE service.
- Explicit prefault loop, one byte per page (#1665) - portable fallback,
  pairs with `MADV_SEQUENTIAL`. Costs one fault per page on the
  submitter, which is what we want: the userspace task services its own
  faults without the kthread.
- Don't combine SQPOLL with mmap'd basis - keep the wired path on
  `MapFile<BufferedMap>` (heap sliding-window via `pread(2)`), as
  audit #3440 records for the live transfer; SQPOLL then only sees heap.

## Recommendation

Adopt the last option as a design invariant: when SQPOLL is enabled,
basis files MUST be served by `BufferedMap` (or another heap-owned
buffer), never by `MmapStrategy` / `AdaptiveMapStrategy`. This is the
only mitigation that survives all three call-site classes (read, write,
fixed) and all three failure modes (stall, task-work, EFAULT/SIGBUS)
without per-site hardening. As a second line, gate
`IoUringWriter::write`'s zero-copy bypass (`file_writer.rs:330`) behind
an explicit caller opt-in - matching #3440 F3 - so no future contributor
routes a `MapFile<MmapStrategy>` slice through SQPOLL at the 256 KiB
threshold. `MAP_POPULATE` / `MADV_WILLNEED` remain opt-in hints for
non-SQPOLL rings; they are not a substitute for this invariant.

## References

- `crates/fast_io/src/io_uring/config.rs:305-314, 382-390` - SQPOLL config
  and fall-back path.
- `crates/fast_io/src/io_uring/file_reader.rs:99, 149, 173` - read submits.
- `crates/fast_io/src/io_uring/file_writer.rs:211, 330` - write/bypass.
- `crates/fast_io/src/io_uring/registered_buffers.rs:238, 260, 425, 544` - registration and fixed-buffer submits.
- `docs/audits/mmap-iouring-co-usage.md` (#3440) - companion audit;
  Findings F1, F3 are the load-bearing call sites here.
- `man io_uring_setup(2)` - `IORING_SETUP_SQPOLL` and `CAP_SYS_NICE`.
- Linux kernel `Documentation/io_uring.rst` and `fs/io_uring.c` -
  `io_sq_thread()` (kthread loop), `io_issue_sqe()` (submission path that
  faults on user pages).
- `target/interop/upstream-src/rsync-3.4.1/fileio.c:214-217` - upstream
  rationale for not using `mmap(2)` on basis files.
