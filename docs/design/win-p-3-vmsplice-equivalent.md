# WIN-P.3 - Windows scatter-gather audit (vmsplice equivalent)

**Date:** 2026-06-16
**Status:** DECIDED - **PERMANENT GAP**
**Scope:** Evaluate Win32 scatter-gather and overlapped-vectored primitives
(`WriteFileGather`, `ReadFileScatter`, Registered I/O `RIO_BUF`) as Windows
analogues to Linux `vmsplice(2)` for the disk-commit hot path. Feeds
WIN-P.6 (`docs/design/win-p-6-windows-stub-decision-matrix.md`) with a
per-stub verdict for `Writer::Vmsplice`.

Companion audit: `docs/audits/win-p-3-vmsplice-windows-equivalent.md`
(WIN-P.3 inventory and candidate-API matrix). Companion design doc:
`docs/design/windows-splice-vmsplice-equivalents.md` (WIN-S.4 throughput
quantification). This document is the design-side ship-or-gap decision
write-up.

## 1. Linux semantics being matched

`vmsplice(2)` (Linux 2.6.17+) inserts a userspace buffer page into a
kernel pipe's `pipe_buffer` ring **by reference**, holding a
`get_user_pages()` reference until the downstream `splice(pipe_read_fd,
file_fd)` consumes it. The full Linux path is:

```
userspace_buffer -> vmsplice -> kernel pipe -> splice -> file_fd
```

The optimisation is bounded by memcpy bandwidth: each gather operation
saves one userspace-to-kernel buffer copy on the disk-write side. Three
hard requirements:

1. **Page granularity.** Every `iovec` entry must point to a 4 KiB-aligned
   userspace page; sub-page buffers are silently copied.
2. **Pipe-mediated transfer.** The destination is the anonymous pipe, not
   the destination file directly. The actual move to the file is a
   second `splice(2)` syscall.
3. **Cooperative consumer.** The downstream consumer must drain pipe
   buffers before the userspace pages are reused, or the kernel
   transparently degrades to copy semantics.

Caller in oc-rsync: `Writer::Vmsplice` variant in
`crates/transfer/src/disk_commit/writer.rs:162`
(`#[cfg(all(target_os = "linux", feature = "vmsplice"))]`), driven by
`VmspliceFileWriter` in
`crates/fast_io/src/vmsplice_writer.rs:82`. The selection gate
`should_vmsplice()` (`vmsplice_writer.rs:191`) requires chunk size
`>= 64 KiB` and 4 KiB-aligned buffer pointer.

## 2. Windows scatter-gather primitives

### 2.1 `WriteFileGather`

`WriteFileGather` (Win32 kernel32.dll; Windows 2000+) writes from N
single-page buffers into one file handle in a single overlapped
operation. Each `FILE_SEGMENT_ELEMENT` entry points to exactly one
memory page; the array is null-terminated.

**Constraints (load-bearing):**

| Constraint | Detail |
|---|---|
| Memory-page alignment | Each buffer must be aligned on, and a multiple of, the volume sector size **and** the system page size (typically 4 KiB on x86_64, 16 KiB on aarch64 Windows on ARM). Allocate via `VirtualAlloc` with `MEM_COMMIT \| MEM_RESERVE`. |
| Sector-aligned file offset | `OVERLAPPED::Offset` / `OffsetHigh` must be a multiple of the volume sector size. |
| File-handle flags | Handle must be opened with `FILE_FLAG_OVERLAPPED \| FILE_FLAG_NO_BUFFERING`. `FILE_FLAG_NO_BUFFERING` bypasses the system cache; subsequent buffered reads of the same file miss the cache. |
| Buffer pinning | The kernel takes a temporary memory-descriptor-list (MDL) reference for the DMA. Userspace can reuse the buffer only after the OVERLAPPED completes. Unlike `vmsplice`, the kernel does **not** keep a reference past completion. |
| Total length | Implicit; the write size equals the number of segment entries times the page size. Arbitrary chunk sizes require the caller to pad to a page boundary. |
| Maximum gather length | `nNumberOfBytesToWrite` field stays an `ULONG`; max single op is bounded by sector-aligned page count fitting in 32 bits. |

MSDN reference:
<https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-writefilegather>.

### 2.2 `ReadFileScatter`

`ReadFileScatter` is the symmetric read primitive: reads from one file
handle into N single-page buffers in one overlapped operation. Same
sector + page alignment, `FILE_FLAG_OVERLAPPED \| FILE_FLAG_NO_BUFFERING`
requirement, and per-element page granularity.

MSDN reference:
<https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-readfilescatter>.

Direction: file -> userspace pages. Wrong direction for the
`Writer::Vmsplice` hot path (which is userspace pages -> file). Listed
here because the task asked for both APIs, but it does not produce a
new gap-closure candidate beyond what `WriteFileGather` offers.

### 2.3 Registered I/O `RIO_BUF`

Registered I/O (Winsock; Windows 8+) lets the application pre-register a
buffer region with `RIORegisterBuffer`, then issue scatter-gather sends
via `RIOSend` / `RIOSendEx` using `RIO_BUF` descriptors that index into
the registered region. The kernel keeps the buffer region pinned for the
lifetime of the registration, eliminating the per-call
`MmProbeAndLockPages` cost.

**Constraints:** sockets only. No file-write path. Already shipped on
the socket-vectored output side by WPG-9
(`docs/design/wpg-9-registered-buffer-windows-equivalent.md`). Listed
here for completeness; it does not close the file-write gap.

MSDN reference:
<https://learn.microsoft.com/en-us/windows/win32/api/mswsock/nf-mswsock-rioregisterbuffer>.

## 3. Linux vs Windows: constraint delta

| Property | Linux vmsplice | Windows `WriteFileGather` |
|---|---|---|
| Target | Pipe (`pipe_buffer` ring) | File handle directly |
| Granularity | Page (4 KiB on x86_64) | Page **and** sector |
| Open-flag requirements | None on the source side | `FILE_FLAG_OVERLAPPED \| FILE_FLAG_NO_BUFFERING` on the file handle |
| Buffer pinning duration | Until downstream `splice` consumes | Until OVERLAPPED op completes |
| Zero-copy semantic | Page-reference (no copy at all) | DMA from userspace (one kernel-bounce avoided on the cached path; not bounced if `FILE_FLAG_NO_BUFFERING`) |
| Sub-page handling | Silently degrades to copy | Caller must pad; otherwise op fails with `ERROR_INVALID_PARAMETER` |
| Cache interaction | Pages remain in the page cache after the splice | Bypasses cache; subsequent buffered reads of the same file miss |
| Per-chunk syscalls | 2 (vmsplice + splice) | 1 (overlapped WriteFileGather) |

The key asymmetry: vmsplice on Linux is **pipe-only on the source side**
(write via vmsplice goes into a pipe, not directly into a file).
`WriteFileGather` is **file-only**, and the alignment regime is far
stricter (page + sector + open-flag) than vmsplice's page-only
requirement.

## 4. Map to oc-rsync use sites

| Site | Linux behaviour | Windows fit for `WriteFileGather` |
|---|---|---|
| `crates/fast_io/src/vmsplice_writer.rs:82` `VmspliceFileWriter::write_chunk` | 64 KiB+ literal token chunks; pipe -> file via `splice`. | **No fit.** Literal token chunks come from the delta token stream with arbitrary byte alignment. Padding to page + sector boundaries would re-introduce the memcpy that vmsplice exists to avoid. |
| `crates/transfer/src/disk_commit/process.rs:449` `make_writer` selection | Falls back to `Writer::Iocp` -> `Writer::Buffered` when neither io_uring nor the IOCP path claimed the file. | Already routes through `Writer::Iocp` on Windows; `Writer::Vmsplice` is `#[cfg(target_os = "linux")]` and absent from the Windows dispatch table. |
| `crates/transfer/src/disk_commit/writer.rs:162` `Writer::Vmsplice` enum variant | Linux-only enum arm. | Compiled out on Windows. A Windows `Writer::WriteFileGather` variant would need to be a sibling, not a replacement, and would require the alignment shim above. |
| `crates/fast_io/src/splice/syscalls.rs:211` `try_vmsplice_to_file` | libc wrapper. | Not reachable on Windows. |

The Windows daemon receive path is already covered by `Writer::Iocp`
(IOCP-batched `WriteFile` with `GetQueuedCompletionStatusEx` dequeue),
which is the structural equivalent of io_uring + `WRITE_FIXED` on Linux.
Per the WIN-P.6 matrix row for io_uring full surface, IOCP is the
production Windows path for every io_uring-equivalent code path; the
io_uring path on Linux supplants `Writer::Vmsplice` from kernel 5.6+
before `make_writer` even considers it.

**No call site benefits from `WriteFileGather`** given the
literal-token-stream alignment regime. The only call sites that could
theoretically benefit (very large sector-aligned writes from a
sector-aligned source buffer) are exactly the call sites IOCP already
handles efficiently via the system-cache write-behind path.

## 5. Verdict

**PERMANENT GAP.** Three converging reasons:

1. **Alignment cost dominates the win.** `WriteFileGather` requires
   sector-aligned offsets, page-aligned buffers, and
   `FILE_FLAG_NO_BUFFERING`. The rsync literal-token stream is
   byte-arbitrary; padding to alignment in userspace reintroduces the
   memcpy vmsplice exists to avoid. The wall-clock win on Linux is
   bounded at ~3% for large transfers (per WIN-S.4 §"Throughput
   quantification"); after subtracting the alignment-shim memcpy on
   Windows, the win is negative.
2. **No production reach.** `Writer::Vmsplice` is cfg-gated out on
   Windows entirely (`#[cfg(all(target_os = "linux", feature =
   "vmsplice"))]`). The Windows write path dispatches to `Writer::Iocp`
   which already covers the syscall-amortisation envelope. Shipping
   `WriteFileGather` would add a third Windows writer variant with no
   observable advantage.
3. **Cache interaction is harmful.** `FILE_FLAG_NO_BUFFERING` bypasses
   the system cache. Subsequent buffered reads (rsync's checksum
   verification pass and any post-transfer access) miss the cache and
   take a second disk round-trip. The cache-bypass cost outweighs the
   memcpy save on every realistic workload.

This verdict aligns with the WIN-P.6 decision matrix row for vmsplice
(**PERMANENT GAP**), with the WIN-P.3 inventory audit
(`docs/audits/win-p-3-vmsplice-windows-equivalent.md` §4), and with
WIN-S.4's earlier conclusion documented in
`docs/design/windows-splice-vmsplice-equivalents.md`.

`ReadFileScatter` is included in the candidate set but is the wrong
direction for the `Writer::Vmsplice` hot path; the same alignment
cost analysis applies to any future read-side scatter use. `RIO_BUF`
is socket-only and already shipped under WPG-9.

## 6. Feed-forward

- **WIN-P.6 (#3687)** decision matrix: vmsplice row records
  `verdict = PERMANENT GAP`, `windows-reach = zero`,
  `linux-win = bounded < 3%`, with this design doc as the
  ship-or-gap reference.
- **WIN-P.8 (#3689)** "Implement vmsplice Windows equivalent" closes
  with **no implementation**. Reference this audit, WIN-P.3 (audits),
  and WIN-S.4.
- **WIN-TIER2.5** Tier-2 caveat lists vmsplice as a documented
  permanent gap, not a missing feature.

## 7. References

- `docs/audits/win-p-3-vmsplice-windows-equivalent.md` - companion
  inventory audit.
- `docs/design/windows-splice-vmsplice-equivalents.md` (WIN-S.3 / WIN-S.4)
  - prior design write-up with throughput quantification.
- `docs/design/splice-vmsplice-zero-copy.md` - Linux-side design.
- `docs/design/win-p-6-windows-stub-decision-matrix.md` - WIN-P.6 matrix
  that consumes this verdict.
- `docs/design/wpg-9-registered-buffer-windows-equivalent.md` - WPG-9
  socket-vectored RIO_BUF design.
- MSDN `WriteFileGather`:
  <https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-writefilegather>.
- MSDN `ReadFileScatter`:
  <https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-readfilescatter>.
- MSDN `FILE_SEGMENT_ELEMENT`:
  <https://learn.microsoft.com/en-us/windows/win32/api/winnt/ns-winnt-file_segment_element>.
- MSDN `CreateFileW` (FILE_FLAG_NO_BUFFERING semantics):
  <https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-createfilew>.
- MSDN File Buffering (alignment requirements):
  <https://learn.microsoft.com/en-us/windows/win32/fileio/file-buffering>.
- MSDN `RIORegisterBuffer`:
  <https://learn.microsoft.com/en-us/windows/win32/api/mswsock/nf-mswsock-rioregisterbuffer>.
- Linux man page `vmsplice(2)`:
  <https://man7.org/linux/man-pages/man2/vmsplice.2.html>.

## 8. Tracking

- Parent: **WIN-P** (#3681).
- This document: design-side companion to **WIN-P.3** (#3684).
- Closes: WIN-P.3 task with verdict **PERMANENT GAP**.
- Feeds: **WIN-P.6** (#3687), **WIN-P.8** (#3689) no-implementation
  closure.
