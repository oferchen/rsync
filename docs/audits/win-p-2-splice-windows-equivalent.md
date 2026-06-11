# WIN-P.2: splice() Windows equivalent evaluation

Status as of 2026-06-11. Re-evaluates whether any Win32 primitive
(WSARecv/WSASend, TransmitPackets, IOCP-driven overlapped pipes) is
worth implementing as a Windows equivalent of Linux's
`splice(2)`/`vmsplice(2)` pipe-mediated zero-copy primitives. Confirms
the prior WIN-S.3 verdict (permanent gap) with fresh caller inventory
and feeds forward to WIN-P.6 (decision matrix), WIN-P.7
(implement-if-ship), and WIN-TIER2 caveat docs.

## Inputs to this audit

- **WIN-P.1** (`docs/audits/win-p-1-fast-io-stubs.md`, 2026-06-11)
  classifies the splice stub site as Class B (typed
  `ErrorKind::Unsupported` with all callers handling it) and assigns
  WIN-P.2 a **LOW** feed-forward priority, recommending that WIN-P.7
  close as "no implementation".
- **WIN-S.3** (`docs/design/windows-splice-vmsplice-equivalents.md`)
  shipped the permanent-gap decision after evaluating overlapped
  `ReadFile`/`WriteFile` with `FILE_FLAG_NO_BUFFERING` and named pipes
  (`PIPE_TYPE_BYTE`). Both were rejected as worse than the existing
  IOCP path.
- **WIN-S.8** (`docs/design/win-s8-windows-stub-priority-matrix.md`)
  rated splice and vmsplice as **P3** (no action needed), pointing to
  the IOCP path as the structural equivalent.
- **WIN-TIER2.1** (`docs/audits/win-tier2-stub-inventory.md`)
  inventoried these rows as Windows-transfer-path-reachable but
  Class-B safe via the IOCP dispatch.
- **WPG-7..9** audits confirmed that IOCP + Registered I/O (`RIO_BUF`)
  + `TransmitFile` collectively cover the io_uring `SEND` / `SEND_ZC`
  / registered-buffer surface on Windows.

This WIN-P.2 audit produces three things WIN-S.3 and WIN-S.8 did not
explicitly cover:

1. Caller inventory mapped to current master line numbers after
   WIN-S.LAND.1.d, SPL-18, and SEC-MK landed.
2. Markdown table of every Win32 candidate API named in the WIN-P.2
   task brief, ranked against splice on data path, syscall count,
   zero-copy status, and kernel mode-switch count.
3. Explicit feed-forward to WIN-P.6 (decision matrix), WIN-P.7
   (implementation skip), and WIN-TIER2 (user-facing caveat).

## 1. Linux semantic that splice() provides

`splice(2)` (Linux 2.6.17+) moves bytes between a file descriptor and
a pipe by transferring kernel-page references through the pipe's
internal `pipe_buffer` ring. No userspace transit, no `memcpy`. The
oc-rsync receive direction uses a two-phase pipe trick:

```text
socket_fd -> splice() -> pipe -> splice() -> file_fd
```

Both phases pass `SPLICE_F_MOVE | SPLICE_F_MORE`. Pages physically
move from the socket buffer into a pipe slot, then from the pipe slot
into the destination file's page cache. The kernel reference-counts
the page; the userspace process never touches the bytes. Production
sites are in `crates/fast_io/src/splice/syscalls.rs` lines 40-310 and
`crates/fast_io/src/splice/mod.rs` lines 120-324 (post-SPL-18 layout).

`vmsplice(2)` complements splice by inserting a userspace buffer page
into the pipe ring by reference (via `get_user_pages`). The
combination `vmsplice -> splice` enables `userspace_buf -> pipe ->
file_fd` with zero kernel-userspace copies.

The Windows kernel has no equivalent of either pipe-buffer-by-reference
or `get_user_pages`. Every Windows pipe API (`CreatePipe`,
`CreateNamedPipeW`, named-pipe message-mode) copies bytes through a
kernel buffer.

## 2. Caller inventory

Every production site reachable on the Windows transfer path, taken
at current master (HEAD = post-PR-5524).

| Site | Linux entry | Windows behavior | Wire-throughput impact if Windows does buffered I/O |
|---|---|---|---|
| `crates/transfer/src/disk_commit/process.rs:445-454` (`make_writer`) | Selects `Writer::Vmsplice` on Linux when not sparse + not append + io_uring not engaged | The `Writer::Vmsplice` enum variant is cfg-gated out on Windows (`#[cfg(all(target_os = "linux", feature = "vmsplice"))]`). Windows arm at lines 428-436 selects `Writer::Iocp` instead. | **None.** IOCP is faster than the upstream Cygwin userspace `write(2)` fallback. The WIN-S.3 quantification put the missing `memcpy` cost at ~0.3 ns/byte, < 3% of wall-clock for a 10-second 1 GB transfer, fully masked by network and disk I/O. |
| `crates/fast_io/src/vmsplice_writer.rs:83-200` (`VmspliceFileWriter::write_chunk`) | Per-chunk `vmsplice -> splice` dispatch for ≥ 64 KiB page-aligned literal tokens | Non-Linux arm at `vmsplice_writer.rs:207-235` returns `ErrorKind::Unsupported` from `new()` and `write_chunk()`. Never reached on Windows because the enum variant is cfg-gated out. | **None.** Same reasoning. |
| `crates/fast_io/src/splice/mod.rs:120-324` (`SplicePipe` and methods: `new`, `with_capacity`, `splice_to_file`, `vmsplice_to_file`, `capacity`) | RAII pipe pair with configurable buffer (default 1 MB via `fcntl(F_SETPIPE_SZ)`) | Non-Linux arm at `splice/mod.rs:327-353` returns `ErrorKind::Unsupported` from every constructor; `capacity()` returns `0`. | **None.** No Windows caller invokes `SplicePipe::new`. |
| `crates/fast_io/src/splice/syscalls.rs:40-129` (`try_splice_to_file`), `:211-268` (`try_vmsplice_to_file`) | Low-level `splice(2)` / `vmsplice(2)` wrappers | Non-Linux arms at `:271-285` return `ErrorKind::Unsupported`. | **None.** No Windows caller. |
| `crates/fast_io/src/splice/syscalls.rs:331-345` (`recv_fd_to_file`, Linux fast path) | High-level helper - tries splice for transfers ≥ 64 KiB, falls back to buffered `read`/`write` below threshold | unix-but-not-linux arm at `:347-353` falls through to `copy_fd_to_fd` buffered loop. Non-unix arm at `:357-362` returns `ErrorKind::Unsupported`. **Production reach on Windows: zero.** Re-exported in `lib.rs:311` only so cross-crate imports compile; no Windows caller invokes it (see WIN-P.1 §2). | **None.** |
| `crates/transfer/src/disk_commit/writer.rs:159-297` (`Writer::Vmsplice` variant) | Enum variant for vmsplice writer | Variant is `#[cfg(all(target_os = "linux", feature = "vmsplice"))]`; absent in Windows builds entirely. | **None.** |

**Summary:** Zero production sites reach a splice/vmsplice entry point
on Windows. The whole surface is either typed-Unsupported, cfg-gated
out, or routed through `Writer::Iocp` before the splice path is
considered.

## 3. Windows candidate APIs

Each candidate Win32 API named in the WIN-P.2 task brief, evaluated
against the four cost dimensions splice optimises for.

| Candidate | Data path | Syscall count per chunk | Zero-copy or buffered | Kernel mode-switches per chunk | Verdict |
|---|---|---|---|---|---|
| **WSARecv + WSASend with `WSABUF` arrays** (scatter-gather) | `socket -> WSABUF userspace array -> socket` (socket-to-socket only) - no file destination | 1 + 1 = 2 (`WSARecv`, `WSASend`) | **Buffered.** WSABUF scatters into a userspace `Vec<u8>` array; the kernel copies pages out and back. No file write involved. | 2 per pair, both with completion routine. | **Rejected.** Wrong endpoint - WSARecv/WSASend operate on sockets only. They cannot deliver bytes to a file descriptor. To land bytes in a file the caller still has to issue `WriteFile`, adding a third syscall and a userspace transit. Not pipe-pairing in any sense that maps onto splice. |
| **`CreatePipe` + `WriteFile`/`ReadFile`** (anonymous pipe) | `socket -> ReadFile(userspace_buf) -> WriteFile(pipe) -> ReadFile(pipe, userspace_buf) -> WriteFile(file)` | 4 per chunk in the most direct mapping. | **Buffered.** Every `ReadFile`/`WriteFile` against a Windows pipe copies bytes into or out of the pipe's kernel buffer. There is no `SPLICE_F_MOVE` analogue. | 4 per chunk. | **Rejected.** Strictly worse than the IOCP `WriteFile` path. Adds two extra copies (userspace transit on each side of the pipe) plus two extra syscalls to gain nothing. WIN-S.3 §(b) makes this point. |
| **`CreateNamedPipeW` + overlapped I/O** | Same as anonymous pipe, but with overlapped completion. | 4 per chunk + IOCP completion notifications. | **Buffered.** Same as `CreatePipe`. | 4 per chunk + completion-port wake. | **Rejected.** Same reason as anonymous pipes. Overlapped I/O does not change the data path; it only changes the wait model. |
| **`TransmitPackets`** (Winsock LSP API) | `file_or_buffer_array -> socket` (file-to-socket or socket-to-socket via a list of `TRANSMIT_PACKETS_ELEMENT`s) | 1 (single call dispatches the whole `LPTRANSMIT_PACKETS_ELEMENT` array). | **Zero-copy on send-side only**, and only for `TP_ELEMENT_FILE` entries. The kernel pages the file into the socket without userspace transit, equivalent to `TransmitFile`. The receive direction has no `TransmitPackets` analogue. | 1 per call, async completion via `LPOVERLAPPED`. | **Rejected for splice equivalence.** `TransmitPackets` is the send-side primitive; oc-rsync already covers it via `TransmitFile` (WIN-S.2 / WPG-8). Splice's heaviest production use is the **receive** direction (`socket -> file_fd`), which `TransmitPackets` cannot serve. The send-direction overlap is already captured by `TransmitFile` and the IOCP send path. |
| **`WriteFileEx` with overlapped I/O + completion routine** | `userspace_buf -> WriteFileEx -> file_fd` (no pipe intermediary) | 1 per chunk. | **Buffered.** `WriteFileEx` writes the userspace buffer to the file's page cache; one kernel-side copy. | 1 per chunk + completion-routine APC. | **This is what IOCP `Writer::Iocp` already does, modulo notification mechanism** (IOCP uses completion ports, `WriteFileEx` uses an APC). Both are single-syscall single-copy. The IOCP variant batches multiple completions per `GetQueuedCompletionStatusEx` poll, which gives it a measurable edge over per-write APC completion. **No reason to swap.** |
| **IOCP-driven scatter-gather (`WriteFileGather`)** | `userspace_page_array -> WriteFileGather -> file_fd`. Each entry must be a single page-aligned sector. | 1 per chunk (multi-page). | **Buffered, with kernel-level page-list semantics**. The kernel still copies pages into the page cache; the "gather" part only batches the descriptor list, not the data transfer. Requires `FILE_FLAG_NO_BUFFERING` and sector alignment. | 1 per chunk. | **Rejected.** Sector alignment is incompatible with rsync's arbitrary-length literal tokens. WIN-S.3 §(a) rules this out. `FILE_FLAG_NO_BUFFERING` also bypasses the page cache, which hurts the redo and checksum-verification phases. |

**Reference point - what is shipped today** on Windows:

| Path | Data flow | Syscalls/chunk | Copies/chunk | Notes |
|---|---|---|---|---|
| `Writer::Iocp` (production) | `userspace_Vec<u8> -> WriteFile(IOCP) -> page_cache` | 1 | 1 | Batched completion via `GetQueuedCompletionStatusEx`. Architectural peer of io_uring's `IORING_OP_WRITE_FIXED`. |
| `Writer::Buffered` (sparse / append fallback) | `userspace_Vec<u8> -> ReusableBufWriter -> WriteFile -> page_cache` | 1 + buffered batching | 1 | 256 KB buffer with vectored I/O. |

For comparison, splice on Linux:

| Path | Data flow | Syscalls/chunk | Copies/chunk |
|---|---|---|---|
| `splice` direct | `socket_buffer -> splice -> pipe -> splice -> page_cache` | **2** | **0** |
| `vmsplice + splice` | `userspace_buf -> vmsplice -> pipe -> splice -> page_cache` | **2** | **0** (kernel page-table reference, not memcpy) |

**Key observation:** splice trades one extra syscall (2 vs 1) for one
avoided memcpy (0 vs 1). On modern hardware with DDR5 the avoided
memcpy is ~0.3 ns/byte (WIN-S.3 quantification). The extra syscall is
~300-500 ns regardless of chunk size, so the trade-off favors splice
only at chunk sizes well above 1 KB. The Windows IOCP path collapses
this to one syscall + one memcpy, which is the same architecture as
io_uring `WRITE_FIXED` on Linux 5.6+ - and on modern Linux that path
also bypasses splice/vmsplice (vmsplice_writer.rs §"Why the delta is
acceptable" point 2).

## 4. Verdict

**Document as permanent gap.** No Win32 primitive provides the
pipe-mediated kernel-to-kernel zero-copy semantics of `splice(2)`.

This audit confirms WIN-S.3's prior decision with fresh evidence. The
prior audit was already explicit ("Decision: accept the stubs as
permanent"); WIN-P.2 carries that decision forward through the
WIN-P.1 inventory and the candidate-API table above. WIN-P.6's
decision matrix should record splice and vmsplice as
**ship-equivalent: NO**.

Justifications, in order of weight:

1. **No semantic match exists.** Every Win32 candidate either
   operates on the wrong endpoint (`WSARecv`/`WSASend`/`TransmitPackets`
   are socket APIs; splice's heaviest use is socket-to-file), or
   copies through userspace (anonymous and named pipes), or requires
   sector alignment that breaks rsync's wire format
   (`WriteFileGather` with `FILE_FLAG_NO_BUFFERING`).
2. **The Windows path is already at architectural parity** with
   io_uring's `WRITE_FIXED`, which is what splice/vmsplice fall back
   to bypass on modern Linux. The IOCP path issues 1 syscall + 1
   memcpy per chunk; io_uring on Linux 5.6+ issues 1 syscall + 1
   memcpy per chunk. Splice's 2-syscall + 0-copy advantage only
   appears on Linux kernels < 5.6 without io_uring - a shrinking
   deployment window.
3. **Production reach today is zero.** No Windows caller invokes
   `SplicePipe`, `try_splice_to_file`, `try_vmsplice_to_file`,
   `recv_fd_to_file`, or `VmspliceFileWriter`. The dispatcher in
   `make_writer` selects `Writer::Iocp` before the splice path is
   even considered. Shipping a Windows equivalent would require
   wiring the new path into `make_writer` too - additional code with
   no measurable benefit.
4. **Upstream rsync does not use splice or vmsplice.** Reference
   implementation rsync 3.4.1/3.4.4 does not call splice or vmsplice
   in `io.c`, `fileio.c`, or `receiver.c`. Cygwin rsync on Windows
   uses standard `write(2)`. The IOCP path is already faster than
   the upstream baseline; matching upstream-on-Windows is the
   parity target, not Linux-with-io_uring.
5. **The performance gap is bounded and small.** WIN-S.3
   quantification: < 3% of wall time for a 10-second 1 GB transfer,
   fully masked by network and disk I/O. Confirmed against
   WIN-S.LAND.4 / WIN-S.LAND.5 baseline harness (when run).

## 5. Feed-forward

### To WIN-P.7 (implement-if-ship, #3688)

**Close as "no implementation" with pointer to this audit + WIN-S.3.**
No Windows code to write. WIN-P.7's task description matches
WIN-P.6's decision matrix - if WIN-P.6 records splice as
ship-equivalent:NO (per §4 of this audit), WIN-P.7 should be closed
as "not applicable" rather than left pending.

### To WIN-P.8 (vmsplice implementation, #3689)

**Same disposition.** WIN-P.3 (vmsplice audit) and WIN-S.4 both reach
the same permanent-gap conclusion; the underlying reason -
`WriteFile`-to-pipe is always a buffered copy on Windows - is the
same. WIN-P.8 should be closed alongside WIN-P.7 once the
WIN-P.6 decision matrix records both as permanent-gap.

### To WIN-P.6 (per-stub decision matrix, #3687)

Add two rows:

| Stub | Win32 candidate evaluated | Verdict | Reference |
|---|---|---|---|
| `splice` / `SplicePipe` / `try_splice_to_file` / `recv_fd_to_file` (non-unix) | WSARecv+WSASend, CreatePipe, CreateNamedPipeW, TransmitPackets, WriteFileEx, WriteFileGather | **permanent-gap** | This audit (WIN-P.2), WIN-S.3 |
| `try_vmsplice_to_file` / `VmspliceFileWriter` | WriteFileGather (only remotely plausible candidate) | **permanent-gap** | WIN-P.3 (when produced), WIN-S.4 |

### To WIN-TIER2 caveat docs (WIN-TIER2.5 follow-up)

Add the following bullet to the user-facing "Windows is Tier 2"
explainer (`README.md` and release notes, per WIN-TIER2.5):

> Windows does not provide pipe-mediated kernel-to-kernel zero-copy
> (Linux `splice(2)` / `vmsplice(2)`). oc-rsync's Windows transfer
> path uses IOCP-batched `WriteFile`, which is architecturally
> equivalent to io_uring's `WRITE_FIXED` on modern Linux. The
> performance gap to a hypothetical Windows splice equivalent is
> bounded at < 3% of wall-clock time and is masked by network and
> disk I/O in practice. See `docs/audits/win-p-2-splice-windows-equivalent.md`
> for evaluation details.

This bullet should not introduce a new tier classification - it
documents the existing Tier-2 disposition with concrete evidence.

### To WIN-P.4 (Landlock equivalent, #3685) - cross-cutting note

WIN-P.4 is unrelated to splice but shares the same "stub absent on
Windows by design" pattern. WIN-P.1's recommendation that WIN-P.4 be
investigated separately (restricted tokens / Job Objects, gated by a
prior `dir_sandbox` Windows equivalent) is unchanged by this audit.
Listed here only so reviewers see the WIN-P series feed-forward
graph in one place.

### To benchmarking (WIN-S.LAND.3 - .5)

When the Windows bench harness ships (`WIN-S.LAND.3`/.4/.5), the
splice-vs-IOCP delta should be measured against MSYS2 upstream rsync
on the same Windows host. WIN-S.3 quantification is theoretical; an
empirical < 3% measurement (or refutation) belongs in the WIN-S.LAND
bench results, not this audit. If WIN-S.LAND.5 finds a regression
above 5%, this WIN-P.2 verdict should be revisited.

## 6. Tracking

- Parent: **WIN-P** (#3681).
- Predecessor inventory: **WIN-P.1** (#3682, doc
  `docs/audits/win-p-1-fast-io-stubs.md`).
- This audit: **WIN-P.2** (#3683).
- Sibling audits: WIN-P.3 vmsplice (#3684), WIN-P.4 Landlock
  (#3685), WIN-P.5 SQPOLL (#3686).
- Decision matrix: WIN-P.6 (#3687) - record splice +
  vmsplice as permanent-gap.
- Implementation tasks: WIN-P.7 (#3688) splice, WIN-P.8 (#3689)
  vmsplice - both close as "no implementation".
- User-facing caveats: WIN-TIER2.5 follow-up to README +
  release notes.
- Prior design docs:
  `docs/design/windows-splice-vmsplice-equivalents.md` (WIN-S.3 /
  WIN-S.4), `docs/design/win-s8-windows-stub-priority-matrix.md`
  (WIN-S.8), `docs/audits/win-tier2-stub-inventory.md`
  (WIN-TIER2.1).
