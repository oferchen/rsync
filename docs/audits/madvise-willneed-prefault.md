# MADV_WILLNEED prefault audit for mmap'd basis files

Task: #1662. Branch: `docs/madvise-willneed-prefault`.

## Summary

oc-rsync maps basis files into memory via `MmapReader` and hands the resulting
slices to delta-apply, checksum, and (prospectively) io_uring read paths. The
mapped pages are demand-faulted; under SQPOLL the kernel polling thread cannot
service those faults, leaving SQEs blocked on the submitter context. This audit
recommends calling `posix_madvise(MADV_WILLNEED)` on the regions we know we are
about to feed to io_uring, gated by a size threshold, and aligned with the
`MAP_POPULATE` decision recorded in PR #3442.

## Current state

`MmapReader` already exposes three advice helpers but no caller uses
`advise_willneed`. The relevant declarations are
`crates/fast_io/src/mmap_reader.rs:125-128` (`advise_sequential` calling
`memmap2::Advice::Sequential`), `crates/fast_io/src/mmap_reader.rs:132-135`
(`advise_random` calling `memmap2::Advice::Random`), and
`crates/fast_io/src/mmap_reader.rs:139-143` (`advise_willneed` calling
`advise_range(memmap2::Advice::WillNeed, offset, len)`). The non-Unix stubs
live at `crates/fast_io/src/mmap_reader_stub.rs:71-87` and return `Ok(())` on
Windows.

`crates/transfer/src/map_file/mmap.rs` currently has zero advice calls. The
file opens the basis via `MmapReader::open()` at line 38, exposes
`as_slice()` at line 44, and serves slices to `MapStrategy::map_ptr()` at
lines 50-66 with no kernel hint in between. Every basis byte that delta-apply
or io_uring touches is therefore demand-faulted on first access. Today's only
`advise_*` consumers are `crates/checksums/src/parallel/files.rs:43,238,341`
which call `advise_sequential` for whole-file rolling-checksum scans;
`crates/engine/src/local_copy/prefetch.rs:23-37` uses `posix_fadvise` (file
descriptor, not mapping) for the local-copy fast path.

## Why MADV_WILLNEED matters with io_uring

The standard io_uring submission path tolerates page faults: the kernel
worker that drains the SQ runs in the submitter's address space and can take
the fault, populate the PTE, and proceed. SQPOLL changes that contract. With
`IORING_SETUP_SQPOLL` the dedicated kernel thread runs without a userspace
mm context for the duration of an SQE; if the target user buffer is not
resident, the read either stalls until the submitter touches the page or
fails the SQE outright with `-EFAULT` on older kernels. Registered buffers
(`io_uring_register(IORING_REGISTER_BUFFERS)`) make the problem explicit:
registration calls `get_user_pages` which faults each page in immediately,
then pins it for the lifetime of the registration, so an unfaulted mmap
region either forces a synchronous prefault inside `register` or returns
`-EFAULT` if registration is attempted on a hole. Issuing
`posix_madvise(MADV_WILLNEED)` on the basis range before submission lets the
kernel start readahead asynchronously, so by the time the SQE is ready (or
registration runs) the pages are likely resident and the SQPOLL thread does
not park.

## Cost model

On Linux `posix_madvise(MADV_WILLNEED)` is a hint, not a guarantee. The
implementation queues asynchronous readahead on the file backing the mapping,
walking the requested range in `force_page_cache_readahead` (mm/madvise.c).
The call returns once the readahead has been scheduled, not once it
completes; under memory pressure the kernel may drop the request entirely or
satisfy only a partial prefix. Cost on the submitter is bounded - one
syscall plus PTE walk - but there is no hard guarantee that the pages will
be resident when io_uring submission runs. We therefore treat `WILLNEED` as
a best-effort optimization, never as a precondition: code paths must still
tolerate demand faulting and registration-time prefault.

On macOS and the BSDs `posix_madvise` (and the legacy `madvise` shim) is
substantially stronger. Darwin maps `MADV_WILLNEED` onto the unified buffer
cache prefetch path, which initiates synchronous readahead and blocks until
the requested extents are queued; the call also nudges the cluster I/O
heuristic to treat the range as sequential. FreeBSD's vm_object behaves
similarly. Because oc-rsync's io_uring path is Linux-only, the macOS cost is
mostly a CI consideration: the hint is cheap enough to leave unconditional
for parity tests, and the local-copy delta path benefits from the warmer
page cache.

## Recommendation

Call `MmapReader::advise_willneed(offset, len)` from
`crates/transfer/src/map_file/mmap.rs::map_ptr` whenever the mapping is
larger than a threshold and the requested span exceeds one or two pages.
The threshold should match the `MAP_POPULATE` size gate landed in PR #3442
so the two prefault strategies do not double-pay: if `MAP_POPULATE` already
faulted the entire mapping at `mmap` time, `WILLNEED` is redundant and we
skip it; if the mapping is below the populate threshold but a single SQE
will read more than 64 KiB, `WILLNEED` on that exact span is the cheap
middle ground. Skip the call entirely for basis files smaller than 64 KiB
(one readahead window already covers them) and for `Random`-advised
windows where speculative readahead is counter-productive. Errors from
`posix_madvise` must be ignored with `let _ =` because the hint is
advisory and any failure (`EBADF`, `EINVAL` on holes) is non-fatal. The
io_uring submission site should additionally call `WILLNEED` on the exact
SQE range immediately before `io_uring_submit`, mirroring the
`registered_buffers` pin path so SQPOLL never trips a fault.

Telemetry: add a `WILLNEED` counter and reuse the existing `IO3` debug trace
so flame graphs distinguish the hint from actual page-fault stalls. Gated by
`#[cfg(unix)]`; Windows continues to no-op via `mmap_reader_stub.rs:82-87`.

## References

- `crates/fast_io/src/mmap_reader.rs:125-143` - existing advice helpers,
  including the unused `advise_willneed` we plan to wire up.
- `crates/fast_io/src/mmap_reader_stub.rs:71-87` - Windows no-op stubs.
- `crates/transfer/src/map_file/mmap.rs:30-77` - basis-file mapping site
  with no advice calls today.
- `crates/checksums/src/parallel/files.rs:43,238,341` - existing
  `advise_sequential` consumers we mirror.
- `crates/engine/src/local_copy/prefetch.rs:23-79` - `posix_fadvise`
  precedent for fd-based hints.
- `crates/fast_io/src/debug_io.rs:574-593` - `trace_mmap_advise` IO3
  debug hook for telemetry.
- `docs/audits/mmap-iouring-co-usage.md` - companion audit (#1660) listing
  every mmap-to-io_uring crossing.
- `docs/audits/mmap-map-populate-evaluation.md` (PR #3442 / task #1663) - companion `MAP_POPULATE` evaluation.
- Man pages: Linux `posix_madvise(3)`, `madvise(2)` (`MADV_WILLNEED`),
  `io_uring_setup(2)` (`IORING_SETUP_SQPOLL`), `io_uring_register(2)`
  (`IORING_REGISTER_BUFFERS`); Darwin `posix_madvise(2)` for stronger semantics.
