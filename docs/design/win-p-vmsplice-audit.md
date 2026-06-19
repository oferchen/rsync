# WIN-P.3 - vmsplice Windows-equivalent audit

**Date:** 2026-06-19
**Task:** WIN-P.3 (#3684) audit closure
**Status:** AUDIT COMPLETE - verdict **PERMANENT GAP**
**Feeds:** WIN-P.6 (#3687) decision matrix; WIN-P.8 (#3689) no-implementation closure.

This audit evaluates `WriteFileGather` and `ReadFileScatter` as Windows
equivalents for Linux `vmsplice(2)`. It is a scoped feed for WIN-P.6 and
sits alongside the prior WIN-P.3 design write-up
(`docs/design/win-p-3-vmsplice-equivalent.md`) and the WIN-P.3 inventory
audit (`docs/audits/win-p-3-vmsplice-windows-equivalent.md`).

## 1. Linux site under audit

The production caller is the `Writer::Vmsplice` arm of
`crates/transfer/src/disk_commit/writer.rs:162`
(`#[cfg(all(target_os = "linux", feature = "vmsplice"))]`), driven by
`VmspliceFileWriter` (`crates/fast_io/src/vmsplice_writer.rs:82`).
The gate `should_vmsplice()` at `vmsplice_writer.rs:191` requires
chunk size `>= 64 KiB` (`VMSPLICE_MIN_CHUNK`) and pointer alignment
to `ASSUMED_PAGE_SIZE = 4096` (`vmsplice_writer.rs:72`), plus
`is_splice_available()`. The libc wrapper is
`try_vmsplice_to_file` (`crates/fast_io/src/splice/syscalls.rs:211`);
the reused pipe pair is `SplicePipe::vmsplice_to_file`
(`crates/fast_io/src/splice/mod.rs:260`). `vmsplice` inserts userspace
pages into a kernel pipe ring **by reference**, and the downstream
`splice(pipe_rd, file_fd)` moves them into the page cache without a
userspace-to-kernel `memcpy`.

## 2. Windows candidates

### 2.1 `WriteFileGather`

Scatter-gather write from N single-page buffers into one file handle in
a single overlapped operation, completing through IOCP. The array is an
`FILE_SEGMENT_ELEMENT` ring terminated by a null entry. MSDN:
<https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-writefilegather>.

Load-bearing constraints:

| Constraint | Detail |
|---|---|
| Buffer alignment | Each segment must be aligned to **and** sized to the system page size **and** the volume sector size. Allocate via `VirtualAlloc(MEM_COMMIT \| MEM_RESERVE)`. Sub-page buffers fail with `ERROR_INVALID_PARAMETER`. |
| File-handle flags | `FILE_FLAG_OVERLAPPED \| FILE_FLAG_NO_BUFFERING` required. `FILE_FLAG_NO_BUFFERING` bypasses the system cache. |
| Offset alignment | `OVERLAPPED::Offset / OffsetHigh` must be sector-aligned. |
| Completion | Overlapped, dequeued via IOCP - composes with existing `fast_io::iocp` infrastructure. |
| Buffer pinning | Kernel MDL reference for the DMA, released at op completion. Unlike `vmsplice` the kernel keeps no post-completion reference. |
| FS support | NTFS / ReFS only; FAT32 and some network redirectors reject `FILE_FLAG_NO_BUFFERING`. |

### 2.2 `ReadFileScatter`

Symmetric read primitive - file -> N single-page buffers. Same
alignment + open-flag requirements. MSDN:
<https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-readfilescatter>.

Direction is wrong for the `Writer::Vmsplice` hot path; included only
because the task asked.

## 3. Linux vs Windows delta

| Property | Linux `vmsplice` | Windows `WriteFileGather` |
|---|---|---|
| Target | Pipe `pipe_buffer` ring | File handle directly |
| Granularity | Page (4 KiB on x86_64) | Page **and** sector |
| Source-side memcpy | None (page reference) | DMA from userspace; the bytes still cross the bus to disk |
| Subsequent buffered read | Hits page cache | Misses cache (`FILE_FLAG_NO_BUFFERING`) |
| Sub-page input | Silently copied | Fails with `ERROR_INVALID_PARAMETER` |
| Syscalls per chunk | 2 (`vmsplice + splice`) | 1 overlapped op |

**`WriteFileGather` is scatter-gather with copy, not zero-copy.** It
saves a kernel-bounce on the cached path but cannot replicate
`vmsplice`'s page-into-pipe-by-reference semantic; the closest Windows
primitive that pins userspace pages (`RIO_BUF`) targets sockets, not
files, and is already shipped under WPG-9.

## 4. Impact on the existing buffer pool

Literal-token chunks arrive at the writer with arbitrary byte alignment.
`fast_io::BufferPool` hands out reusable `Vec<u8>` sized to the
multiplex envelope, not to sector + page boundaries. Wiring
`WriteFileGather` requires one of (a) a parallel `VirtualAlloc`-backed
page-aligned pool, doubling resident buffer count and breaking the
single-pool dispatch in `make_writer`
(`crates/transfer/src/disk_commit/process.rs:449`); (b) padding every
chunk to the next page+sector boundary in userspace, reintroducing the
`memcpy` scatter-gather exists to avoid; or (c) a separate
`Writer::WriteFileGather` variant gated on sector-aligned chunks with
`Writer::Iocp` as fallback - a third Windows writer arm with no observable
advantage over the existing IOCP write-behind pipeline. None survives a
cost-benefit pass.

## 5. Recommendation for WIN-P.6

**Document permanent gap.** WIN-P.6's vmsplice row should record:

- `windows-candidate = WriteFileGather`
- `verdict = PERMANENT GAP`
- `reason = scatter-gather-with-copy, not zero-copy; cache-bypass is harmful; alignment shim defeats the win`
- `windows-production-reach = zero` (Windows already routes through `Writer::Iocp`; `Writer::Vmsplice` is `#[cfg(target_os = "linux")]` and unreachable on Windows)
- `WIN-P.8 = close with no implementation`

The honest assessment: Windows has no true `vmsplice` analogue.
`WriteFileGather` is the nearest API but is structurally a scatter-gather
write that still copies userspace data to disk. The Linux win is bounded
at ~3% on large transfers (per the throughput quantification in
`docs/design/windows-splice-vmsplice-equivalents.md`); after subtracting
the alignment shim and cache-bypass cost, the Windows projection is
negative.

## 6. Risk

If a future change wires `WriteFileGather` without a sector-aligned
buffer pool, every fall-through to the standard write path will pay an
extra userspace memcpy from the page-aligned segment back into a
normal buffer. The page-aligned buffer-pool requirement is the load-
bearing constraint and breaks the existing `BufferPool` dispatch unless
special-cased. Document this as a tripwire in WIN-P.6 and gate any
future revisit on a measured cached-vs-uncached read-after-write
benchmark on production Windows hardware.

## 7. References

- Companion decision doc: `docs/design/win-p-3-vmsplice-equivalent.md`.
- Companion inventory audit: `docs/audits/win-p-3-vmsplice-windows-equivalent.md`.
- WIN-S.4 throughput quantification: `docs/design/windows-splice-vmsplice-equivalents.md`.
- Linux design: `docs/design/splice-vmsplice-zero-copy.md`.
- MSDN `WriteFileGather`: <https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-writefilegather>.
- MSDN `ReadFileScatter`: <https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-readfilescatter>.
- MSDN `FILE_SEGMENT_ELEMENT`: <https://learn.microsoft.com/en-us/windows/win32/api/winnt/ns-winnt-file_segment_element>.
- MSDN File Buffering: <https://learn.microsoft.com/en-us/windows/win32/fileio/file-buffering>.
- Linux `vmsplice(2)`: <https://man7.org/linux/man-pages/man2/vmsplice.2.html>.

## 8. Tracking

Parent: **WIN-P** (#3681). Task: **WIN-P.3** (#3684).
Feeds: **WIN-P.6** (#3687), **WIN-P.8** (#3689).
