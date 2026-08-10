# WIN-P.3: vmsplice Windows equivalent

Status as of 2026-06-11. Scoped follow-up to WIN-P.1 (#3682,
`docs/audits/win-p-1-fast-io-stubs.md`) and WIN-S.4
(`docs/design/windows-splice-vmsplice-equivalents.md`, sections "WIN-S.4
- vmsplice Windows equivalent audit" through "Consolidated decision").

Audit question: would `WriteFileGather` and/or `ReadFileScatter` (or
any other Win32 vectored / overlapped / Registered I/O primitive) close
the gap that `vmsplice(2)` fills on Linux? Or is the Windows arm a
permanent gap?

Short answer: **permanent gap**. No Windows primitive provides the
user-page-into-kernel-pipe zero-copy semantics that `vmsplice` ships,
and the closest analogues (`WriteFileGather`, `ReadFileScatter`,
`WSASend` + `WSABUF`, `RIO_BUF`) either target a different direction
(file-write vectoring vs pipe-buffer page insertion) or impose
sector-alignment constraints that rsync's literal-delta token stream
cannot meet. The production Windows receive path already routes through
`Writer::Iocp` and never reaches `Writer::Vmsplice`; closing the gap
would not change a single byte of Windows-side wire output.

## 1. Linux semantic recap

`vmsplice(2)` (Linux 2.6.17+) inserts a userspace buffer page into a
kernel pipe's internal `struct pipe_buffer` ring **by reference** -
no memcpy. The kernel takes a `get_user_pages()` reference; the page
stays in the application's address space and ages out only after the
downstream `splice(pipe_read_fd, file_fd)` consumes it.

Three load-bearing invariants:

1. **Page-granularity addressing.** vmsplice's "zero-copy" property
   only holds when each `iovec` entry points to a 4 KiB-aligned
   userspace page. Sub-page buffers are silently copied by the kernel.
2. **Pipe-mediated transfer.** The destination is not a file or a
   socket - it is the anonymous pipe's `pipe_buffer` ring. The actual
   `pipe -> file` move is a subsequent `splice(2)` syscall.
3. **Cooperative consumer.** The downstream `splice(pipe_read_fd, ...)`
   must consume the pages before the user clobbers them, or the kernel
   silently degrades to a copy.

The full path is `userspace_buffer -> vmsplice -> pipe(kernel) -> splice
-> file_fd`. It is a two-syscall pair that avoids a single memcpy from
the userspace literal-token buffer into the page cache.

## 2. Caller inventory in oc-rsync

Single production caller: the disk-commit `Writer::Vmsplice` variant in
`crates/transfer/src/disk_commit/`.

| Site | Role |
|---|---|
| `crates/fast_io/src/vmsplice_writer.rs:82` | `VmspliceFileWriter` struct, owns the `SplicePipe` and dest fd. |
| `crates/fast_io/src/vmsplice_writer.rs:164` | `write_chunk()` - `should_vmsplice(chunk)` gate then `pipe.vmsplice_to_file(chunk, dest_fd)`; falls back to plain `write_all` otherwise. |
| `crates/fast_io/src/vmsplice_writer.rs:191` | `should_vmsplice()` - requires chunk `>= 64 KiB` and 4 KiB-aligned buffer pointer. |
| `crates/fast_io/src/splice/syscalls.rs:211` | `try_vmsplice_to_file()` - libc-level wrapper. |
| `crates/fast_io/src/splice/mod.rs:260` | `SplicePipe::vmsplice_to_file()` - reuses an existing pipe pair across many chunks. |
| `crates/transfer/src/disk_commit/process.rs:449` | Disk-commit `make_writer()` selects `Writer::Vmsplice` only on `target_os = "linux"` and `feature = "vmsplice"`, and only when neither io_uring nor IOCP claimed the file. |
| `crates/transfer/src/disk_commit/writer.rs:162` | `Writer::Vmsplice` enum variant; `#[cfg(all(target_os = "linux", feature = "vmsplice"))]`. |

Workloads accelerated on Linux:

- **Initial-sync literal-only transfers** where every delta token is a
  literal payload of 64 KiB or more. Per the WIN-S.4 estimate the win
  is one memcpy per chunk, bounded at ~0.3 ns/byte ~= ~300 ms saved
  per GB on modern DDR5.
- **Linux kernels 2.6.17 - 5.5** without io_uring. On 5.6+ the
  `IORING_OP_WRITE_FIXED` path supplants `Writer::Vmsplice` via the
  io_uring writer factory before `make_writer` even considers vmsplice.

Production reach on Windows: **zero**. The `Writer::Vmsplice` variant
is compiled out of Windows binaries entirely (`#[cfg(all(target_os =
"linux", feature = "vmsplice"))]`), and the Writer dispatch in
`writer.rs` falls through to `Writer::Iocp` before vmsplice is ever
considered.

## 3. Windows candidate-API matrix

| Candidate | What it does | Direction | Alignment requirement | Zero-copy? | Per-chunk syscall count | Matches vmsplice semantics? |
|---|---|---|---|---|---|---|
| `WriteFileGather` | Vectored write of N **single-page** buffers to one file/handle | userspace -> file (or pipe) | Each buffer exactly 1 page, sector-aligned offset, requires `FILE_FLAG_OVERLAPPED \| FILE_FLAG_NO_BUFFERING` | **No.** Pages are DMA'd from userspace; kernel does not hold a `get_user_pages` reference across the call. After the OVERLAPPED completes the userspace buffer can be reused. | 1 (overlapped) | **No.** No pipe-mediation; sector-alignment kills arbitrary literal-token use. |
| `ReadFileScatter` | Vectored read of N single-page buffers from one file/handle | file (or pipe) -> userspace | Same as WriteFileGather. | No | 1 (overlapped) | **No.** Wrong direction even ignoring alignment. |
| `WSASend` + `WSABUF[]` | Vectored send across N buffers to a socket | userspace -> socket | None on buffer alignment | No - kernel copies through socket buffer | 1 | **No.** Socket-only, no file-write path. Already exercised in network output, irrelevant to the file-write path vmsplice accelerates. |
| `WSARecv` + `WSABUF[]` | Vectored recv into N buffers from a socket | socket -> userspace | None | No | 1 | **No.** Wrong direction. |
| Registered I/O `RIO_BUF` + `RIOSend`/`RIOReceive` | Pre-registered userspace buffers serve as scatter/gather targets for socket I/O without per-call locking | userspace <-> socket | Registered buffer region | Closer - the buffer region is pinned at registration time, so subsequent ops avoid the per-call `MmProbeAndLockPages` cost. **But still no `get_user_pages`-into-pipe path; the registered buffer is the source, not a pipe page.** | 1 (RIO doorbell, no syscall for the I/O itself once registered) | **No.** Socket-only; pre-registration replaces the splice `pipe_buffer` ring concept with a fixed buffer table that does not interoperate with file I/O. Already covered separately by WPG-9 (#2669). |
| `WriteFileEx` overlapped + APC | Single-buffer async write with completion routine | userspace -> file (or pipe) | None | No | 1 | **No.** Just an async single-buffer write; no vectoring and no zero-copy. Functionally the existing IOCP path with a different completion model. |
| `TransmitFile` | Sendfile-style file -> socket | file -> socket | None | Partial - kernel reads the file page, sends to socket without userspace copy | 1 | **No.** Wrong direction (covered by WIN-S.2 / WPG-2). |
| `CreateFileMapping` + `MapViewOfFile` | Memory-map a file and treat it as a userspace pointer | n/a (mapping) | Allocation-granularity alignment | n/a | n/a | **No.** Sharing pages between processes, not piping userspace pages into a file descriptor. |
| Anonymous pipe (`CreatePipe`) + `WriteFile` + `ReadFile` chain | IPC byte stream | userspace -> pipe -> userspace | None | **No - two extra memcpys** vs a direct WriteFile | 2 | **No.** Adds copies rather than removing them. Same finding as WIN-S.3 for the splice case. |

None of the candidates provide both the **pipe-mediated routing** and
the **page-reference (not copy) transfer** that vmsplice does. The
closest single-property match is `WriteFileGather` (vectoring N pages
to one file handle in one syscall), but it imposes the
sector-alignment, page-granularity, and `FILE_FLAG_NO_BUFFERING`
constraints that the rsync literal-token stream cannot satisfy without
a copy-into-aligned-staging-buffer shim - which would re-introduce the
memcpy vmsplice is designed to avoid.

## 4. Verdict

**Permanent gap.** WIN-P.8 (#3689) should close with no
implementation. Three converging reasons:

1. **No analogue exists.** The Windows kernel I/O manager has no
   public API for inserting a userspace page into a pipe buffer ring
   by reference. Every available primitive either copies through a
   kernel buffer or requires DMA-aligned buffers backed by
   `FILE_FLAG_NO_BUFFERING`, which the rsync literal-token stream
   cannot meet.

2. **No production reach.** The `Writer::Vmsplice` variant is
   cfg-gated out of Windows entirely. The Windows receive path goes
   through `Writer::Iocp` (with `IOCP_BUFFERED` fallback to
   `Writer::Buffered` for sparse / append). Shipping any approximation
   would create a third Windows Writer variant with no observable
   advantage over IOCP's existing batched-completion model.

3. **The win is bounded by memcpy bandwidth, not syscall cost.**
   Per the WIN-S.4 audit (lines 222-272), the wall-clock vmsplice
   advantage on Linux is bounded at ~3% for large transfers, fully
   masked by network and disk I/O latency in profiled workloads.
   The IOCP path on Windows is architecturally equivalent to
   io_uring + `WRITE_FIXED` (which already supplants vmsplice on
   Linux 5.6+). On the Windows shrinking-target equivalent to
   "Linux without io_uring", the same memcpy compensation argument
   applies and the win does not exist.

The verdict matches WIN-S.4 explicitly:

> The stub is permanent. No Windows API can replicate vmsplice's
> zero-copy page-reference semantics.

WIN-S.4 already shipped this finding through
`docs/design/windows-splice-vmsplice-equivalents.md`. This audit
re-confirms it under the WIN-P.3 frame and aligns the per-stub
inventory in WIN-P.1 row "Class E - module absent" for
`vmsplice_writer::VmspliceFileWriter` (`#[cfg(all(target_os = "linux",
feature = "vmsplice"))]`).

## 5. Cross-references

- `docs/design/windows-splice-vmsplice-equivalents.md` (WIN-S.3 / WIN-S.4)
  - prior deep-dive that this audit cites and aligns with.
- `docs/design/splice-vmsplice-zero-copy.md` - existing design doc
  for the Linux vmsplice path, with the chunk-size + alignment gates
  used by `should_vmsplice()`.
- `docs/design/win-s8-windows-stub-priority-matrix.md` (WIN-S.8) -
  throughput-impact ranking placing vmsplice as a no-priority stub
  on Windows.
- `docs/audits/win-p-1-fast-io-stubs.md` (WIN-P.1, #3682) - parent
  inventory; classifies `VmspliceFileWriter` as Class E (module
  absent on Windows; enum variant compiled out).
- `docs/audits/win-tier2-stub-inventory.md` (WIN-TIER2.1) -
  Windows-transfer-path reachability classification matching this
  audit's "no production reach" finding.
- `docs/design/wpg-9-registered-buffer-windows-equivalent.md`
  (WPG-9, #2669) - Registered I/O on Windows; closes the *socket*
  vectoring case that overlaps WSASend/WSABUF in this matrix, but
  does not affect the file-write path vmsplice targets.

## 6. Feed-forward

- **WIN-P.8 (#3689) implement-if-ship:** close with **no
  implementation**. Reference this audit and WIN-S.4 in the closing
  comment. The stub at `crates/fast_io/src/vmsplice_writer.rs:207`
  remains as the correct non-Linux arm; no Windows wire-up is
  required.
- **WIN-P.6 (#3687) decision matrix:** record vmsplice as
  `verdict = permanent-gap`, `windows-reach = zero`,
  `linux-win = bounded < 3%`. Same row shape as the WIN-P.2 splice
  decision.
- **WIN-TIER2 caveat:** the "Windows is Tier 2" framing in
  `WIN-TIER2.5` should explicitly mention vmsplice as a
  documented-permanent-gap rather than a missing feature. This
  prevents future contributors from filing a new task to "wire
  vmsplice on Windows" without re-reading WIN-S.4 and this audit.
- **No follow-up tasks required.** The `Writer::Vmsplice` variant is
  cfg-gated correctly and the non-Linux stub is classified Class E in
  WIN-P.1; no porting-regression risk surfaces in the WIN-P.1
  classification axis.

## 7. Tracking

- Parent: **WIN-P** (#3681).
- This audit: **WIN-P.3** (#3684).
- Predecessor: **WIN-P.1** (#3682, `docs/audits/win-p-1-fast-io-stubs.md`).
- Sibling: **WIN-P.2** (#3683, splice Windows equivalent).
- Closes-with-no-impl recommendation for: **WIN-P.8** (#3689).
- Feeds into: **WIN-P.6** (#3687) decision matrix.
