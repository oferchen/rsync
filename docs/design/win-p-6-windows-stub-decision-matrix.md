# WIN-P.6: Windows stub decision matrix

Status as of 2026-06-11. Synthesises the WIN-P.1..5 audits into a
single per-stub verdict matrix and a feed-forward scope cut for
WIN-P.7 through WIN-P.12.

This document is the resolution point for parent **WIN-P** (#3681).
Every Linux-only entry point with a non-Linux arm in `crates/fast_io/`
gets a verdict here: **SHIPPED**, **PERMANENT GAP**, **SHIP-EQUIVALENT**,
or **DEFER**. The four follow-up implementation tasks (WIN-P.7..10)
close as "no-implementation / permanent-gap-by-design" based on the
inputs below.

## 1. Source inputs

- `docs/audits/win-p-1-fast-io-stubs.md` (WIN-P.1, #3682) -
  single full inventory of Linux-only cfg-gated `fast_io` entry points
  with non-Linux arms; classifies each by caller-graceful-degradation
  class (A-E); feed-forward priority list for WIN-P.2..5.
- `docs/audits/win-p-2-splice-windows-equivalent.md` (WIN-P.2, #3683) -
  evaluates `WSARecv`/`WSASend`, `CreatePipe`, `CreateNamedPipeW`,
  `TransmitPackets`, `WriteFileEx`, `WriteFileGather` against splice's
  data-path / syscall-count / zero-copy / mode-switch dimensions.
  Verdict: **permanent gap**.
- `docs/audits/win-p-3-vmsplice-windows-equivalent.md` (WIN-P.3, #3684) -
  evaluates `WriteFileGather`, `ReadFileScatter`, Registered I/O `RIO_BUF`.
  Verdict: **permanent gap**.
- `docs/audits/win-p-4-landlock-windows-equivalent.md` (WIN-P.4, #3685) -
  evaluates restricted tokens (`CreateRestrictedToken`), AppContainer
  (`SECURITY_CAPABILITIES`), Job Objects with
  `JOB_OBJECT_LIMIT_ACTIVE_PROCESS`. Verdict: **permanent gap**;
  selected option (b) over option (a) implementation because the
  `dir_sandbox` seam itself is absent on Windows (SEC-1.l).
- `docs/audits/win-p-5-sqpoll-windows-equivalent.md` (WIN-P.5, #3686) -
  evaluates IOCP threadpool with `BindIoCompletionCallback`, Registered
  I/O `RIO_NOTIFY` queues. Verdict: **permanent gap**; IOCP +
  `GetQueuedCompletionStatusEx` already covers completion-side
  amortisation; the SQPOLL+mmap-basis race that motivates
  `sqpoll_basis::WiredBasisWindow` does not exist on Windows.
- `docs/design/win-s8-windows-stub-priority-matrix.md` (WIN-S.8) -
  pre-WIN-P priority ranking of `fast_io` stubs by correctness /
  throughput / frequency. The P1 latent defect noted there
  (`send_file_to_fd` writing to `io::sink()`) was closed via
  WIN-S.LAND.1.d before WIN-P.1's inventory captured it.
- `docs/audits/win-tier2-stub-inventory.md` (WIN-TIER2.1) -
  Cygwin-parity lens on stubs reachable from the Windows transfer
  path. WIN-P.1 superset-extends this for unreachable stubs.
- `docs/user/windows-support-matrix.md` (WPC-13) - companion
  user-facing matrix. The "I/O backends" section already records
  splice, vmsplice, Landlock, and SQPOLL as "Not implemented /
  Not applicable" with a `Tracked: WIN-P series` pointer.
- WIN-S.2 (TransmitFile), WIN-S.5 (FILE_FLAG_DELETE_ON_CLOSE),
  WIN-S.6 (Landlock options audit), WIN-S.10 (CopyFileExW / ReFS
  reflink), WPG-7/8/9 (io_uring opcode -> IOCP/RIO mapping series)
  are referenced from individual rows below.

## 2. Decision matrix

Each row covers one Linux primitive (or class of primitives) whose
Windows equivalent was investigated. The **Verdict** column uses the
four-value legend in section 3. Cross-reference column points at the
canonical audit; follow-up column closes the WIN-P.7..10 task tied to
the row.

| Linux primitive | Windows equivalent considered | Verdict | Rationale (one-line) | Follow-up |
|---|---|---|---|---|
| `splice(2)` (pipe-mediated socket->file zero-copy) | `WSARecv`/`WSASend`, `CreatePipe`, `CreateNamedPipeW`, `TransmitPackets` over a pipe pair | **PERMANENT GAP** | No Win32 primitive provides splice's pipe-mediated kernel-to-kernel zero-copy semantics on the receive direction. IOCP buffered receive is already faster than Cygwin's userspace `read`/`write` fallback. | **WIN-P.7 (#3688) close as no-implementation.** Pointer: WIN-P.2, WIN-S.3. |
| `vmsplice(2)` (gather user pages into a kernel pipe) | `WriteFileGather`, `ReadFileScatter`, RIO `RIO_BUF` for sockets | **PERMANENT GAP** | The Win32 vectored I/O primitives operate on file or socket handles directly, not on pipes. No equivalent for "hand user pages to the kernel without copy via a pipe" exists. `Writer::Vmsplice` enum variant is cfg-gated out on Windows entirely. | **WIN-P.8 (#3689) close as no-implementation.** Pointer: WIN-P.3, WIN-S.4, WPG-9. |
| Landlock LSM (per-thread filesystem-access restriction) | `CreateRestrictedToken`, AppContainer (`SECURITY_CAPABILITIES`), Job Objects with `JOB_OBJECT_LIMIT_*` | **PERMANENT GAP** | All three Windows candidates are process-level, not per-thread runtime, and none give symlink-resistance without a parent-dirfd-style sandbox primitive. The `dir_sandbox` seam Landlock layers on top of is `#![cfg(unix)]`; an equivalent is itself a permanent gap until SEC-1.l's NTFS handle-based gap is closed. | **WIN-P.9 (#3690) close as no-implementation.** Pointer: WIN-P.4, WIN-S.6, SEC-1.l audit. |
| `IORING_SETUP_SQPOLL` (kernel-side submission polling) | IOCP threadpool with `BindIoCompletionCallback`, `GetQueuedCompletionStatusEx` batched dequeue, RIO `RIO_NOTIFY` completion polling | **PERMANENT GAP** | Completion-side amortisation is already shipped via IOCP + GQCS-Ex (WPG-3, WPG-5). Submission-side amortisation has no IOCP analogue for files - every file dispatch goes through `NtWriteFile`/`NtReadFile` with one syscall per overlapped op; RIO covers sockets only. The SQPOLL+mmap-basis race the `WiredBasisWindow` Linux wrapper guards against does not exist on Windows. | **WIN-P.10 (#3691) close as no-implementation.** Pointer: WIN-P.5, WPG-3, WPG-5, WPG-9. |
| `sendfile(2)` (file->socket zero-copy) | `TransmitFile` (already shipped); buffered `copy_via_fd_write` fallback on non-unix | **SHIPPED** | `crates/fast_io/src/iocp/transmit_file.rs` is wired and tested. The pre-WIN-P P1 latent defect (`send_file_to_fd` non-unix arm writing to `io::sink()`) was closed via WIN-S.LAND.1.d (PR #5295). | None - shipped. WIN-S.LAND.1.c.1..4 PlatformSendFile seam still in flight for the 30%+ wall-time threshold. |
| `copy_file_range(2)` (file->file kernel-side copy) | `CopyFileExW` (default path), ReFS `FSCTL_DUPLICATE_EXTENTS_TO_FILE` (reflink on ReFS volumes), buffered read/write fallback | **SHIPPED** | `crates/fast_io/src/platform_copy/dispatch.rs` dispatches ReFS reflink, then `CopyFileExW` with `COPY_FILE_NO_BUFFERING` for >4 MB transfers, then std::fs::copy. The non-Linux non-Windows arm returns `Err(Unsupported)` and falls through to a 256 KB read/write loop. | None - shipped. Per WIN-S.10. |
| `FICLONE` ioctl (file CoW reflink) | `FSCTL_DUPLICATE_EXTENTS_TO_FILE` on ReFS; `clonefile` on macOS | **SHIPPED** | `platform_copy::dispatch::platform_preferred_method` selects the ReFS reflink path when the volume reports CoW support. The Linux-only stub `try_ficlone_impl` returning `Err(Unsupported)` on non-Linux is correct because Windows and macOS callers do not take that branch. | None - shipped. |
| io_uring full surface (`file_reader`, `file_writer`, `socket_reader`, `socket_writer`, `disk_batch`, `linkat`, `statx`, `renameat2`, `send_zc`, `registered_buffers`, `buffer_ring`, `session_pool`, `cancel`, `linked_chain`) | IOCP file path: `iocp/file_reader.rs`, `iocp/file_writer.rs`, `iocp/disk_batch.rs`, `iocp/completion_port.rs`, `iocp/overlapped.rs`; RIO `RIO_BUF` for sockets (WPG-9) | **SHIPPED** (structural replacement) | IOCP is the production Windows path for every io_uring-equivalent code path. The `io_uring_stub/` module survives only so cross-crate imports compile uniformly post IUS-8.c; no Windows caller dispatches through it. WPG-7.a/b/c mapped every io_uring opcode to its IOCP equivalent or documented gap. | None - shipped. The 73 KB stub maintenance cost was closed via IUS-8 trait abstraction. |
| `IORING_OP_SEND_ZC` (zero-copy SEND) | `TransmitFile` (file->socket), RIO `RIO_BUF` (socket-vectored writes with pinned buffers) | **SHIPPED** (structural replacement) | WPG-8 audit established `TransmitFile` + WSASend cover the SEND_ZC equivalent for file-source sockets. WPG-9 shipped RIO_BUF on the socket-vectored side. No per-opcode IOCP gap remains. | None - shipped. SZC series production-readiness on Linux is separate. |
| Registered buffers (`IORING_REGISTER_BUFFERS`) | RIO `RIO_BUF` via `WSAIoctl(SIO_GET_EXTENSION_FUNCTION_POINTER, WSAID_REGISTERED_BUFFER)` | **SHIPPED** (sockets only) | WPG-9 wired RIO_BUF for socket sends; matches the io_uring `IORING_REGISTER_BUFFERS` socket-side feature. Files do not have a Windows-side registered-buffer equivalent because the NT I/O Manager allocates IRPs per overlapped op. | None - sockets shipped; file side is part of the splice/vmsplice/SQPOLL permanent-gap cluster. |
| `O_TMPFILE` (anonymous file in directory) | `FILE_FLAG_DELETE_ON_CLOSE` via `CreateFileW`; `MoveFileEx(MOVEFILE_REPLACE_EXISTING)` for commit | **SHIPPED** | `crates/fast_io/src/win_tmpfile/` is wired into `temp_file_strategy.rs` (WIN-S.5, WIN-S.9). The Linux-only `o_tmpfile::*` stub returning `Err(Unsupported)` on non-Linux is not reached on Windows because the dispatcher takes the `win_tmpfile` arm. | None - shipped. |
| `mknodat(2)` (device-node creation in a sandboxed dir) | None | **PERMANENT GAP** | NTFS does not have device nodes. Upstream Cygwin rsync treats `mknod` as a no-op on Windows; oc-rsync matches. The `--specials` / `-D` CLI flags are documented no-ops on Windows per the support matrix. | None - permanent gap. Documented under WIN-TIER2.5. |
| `mkfifoat(2)` (named-FIFO creation in a sandboxed dir) | None | **PERMANENT GAP** | NTFS has no named-pipe filesystem object that matches POSIX FIFO semantics. Win32 named pipes are an IPC primitive, not a filesystem entry. | None - permanent gap. Documented under WIN-TIER2.5. |
| `dir_sandbox` (parent-dirfd carrier + openat2 RESOLVE_BENEATH chain) | NTFS handle-based APIs (`FILE_OPEN_BY_FILE_ID`, `NtOpenFile` with relative `RootDirectory`) sidestep path-TOCTOU; no Win32 primitive matches RESOLVE_BENEATH semantics for symlink rejection | **PERMANENT GAP** (with mitigation) | The whole `dir_sandbox` module is `#![cfg(unix)]`. SEC-1.l documented that NTFS handle-based APIs sidestep the path-TOCTOU class of issues that motivate the openat2 chain on Linux. A Windows equivalent of the parent-dirfd model would require redesigning every receiver path through NT relative-open handles; not in scope. | None - permanent gap. Mitigation: NTFS handle-based APIs per SEC-1.l. |
| `linux_capabilities` (CAP_NET_BIND_SERVICE drop, PR_SET_NO_NEW_PRIVS) | Windows has no per-process capability bitset; equivalent is integrity levels / restricted tokens at process spawn | **PERMANENT GAP** | Windows lacks the `CAP_*` granularity. The non-Linux arm returns `Ok(())` no-op for capability drops and `false` for queries, which is correct because there is no capability bit to drop. LSM-CAP series is Linux-scope by design. | None - permanent gap. |
| `container::detect_rootless_container` (cgroup + /proc inspection) | None | **PERMANENT GAP** | Rootless containers are a Linux+cgroup concept. Windows containers (Hyper-V isolation, Process isolation) have different security models and do not have the CAP_SYS_NICE requirement that motivates the helper. The Windows arm returns `false`, which is the correct semantic. | None - permanent gap. SQP-LAND series is Linux-scope by design. |
| `signal::install_signal_handler` (sigaction with SA_RESTART) | `SetConsoleCtrlHandler` via the `ctrlc` crate; `crates/platform/src/signal.rs` already wires this on Windows | **SHIPPED** | The Linux-only `fast_io::signal::unix` returning `Ok(())` on non-Linux is a stub for cross-platform compile; real Ctrl+C handling lives in `ctrlc` consumed by `crates/platform`. WIN-TIER2.5 records this. | None - shipped. |
| `lsm` (`/sys/kernel/security/lsm` parsing, active LSM detection) | NTFS DACL / SACL audit (`AuditAccess`); not a 1:1 equivalent | **PERMANENT GAP** | Diagnostic-only surface. Windows uses ACL/SACL auditing for the equivalent observability; `--lsm-status` returns "no LSM" on Windows, which is correct because no LSM kernel module concept exists. | None - permanent gap. LSM-DETECT series is Linux-scope by design. |
| `mmap_reader` (real `mmap2`-backed reader for large basis / hash inputs) | `WindowsChunkedReader` (chunked stream replacement); fallback today is `Vec<u8>`-per-file in `mmap_reader_stub.rs` | **DEFER** (in flight) | WIN-S.LAND.1.b series ships the chunked-stream replacement behind a feature flag; call-site migration tracked at WIN-S.LAND.1.b.4.1..6. Default path on Windows today is the Vec stub, which is correct but doubles peak RSS for multi-GB basis. This is the only Class-A-degraded stub in the WIN-P.1 inventory. | None - tracked under WIN-S.LAND.1.b series (out of WIN-P.6 scope). |
| `o_tmpfile::*` non-Windows stubs (BSD/illumos arm) | None | **PERMANENT GAP** | Out of project scope - oc-rsync does not target BSD/illumos. Returning `Err(Unsupported)` and falling through to named temp files matches upstream rsync. | None - permanent gap, not in WIN-P.6 follow-up. |
| `kqueue_stub` (kqueue API surface on non-macOS) | None | **PERMANENT GAP** | macOS-only primitive; Linux uses `epoll` / io_uring; Windows uses IOCP. No cross-platform equivalent needed. Tracked under DASYNC / ASY-4 for tokio-portable async work, not WIN-P. | None - permanent gap, not in WIN-P.6 follow-up. |

## 3. Verdict legend

- **SHIPPED** - Windows equivalent already wired in production. The
  cell cites the production source file. No WIN-P.6 follow-up.
- **PERMANENT GAP** - No Windows analog exists or is worth shipping.
  The cell documents the reason in WIN-TIER2 and the WIN-P audit
  trail. WIN-P follow-up closes as no-implementation.
- **SHIP-EQUIVALENT** - A Windows analog is worth implementing.
  This verdict is **not used** in the current matrix - every row is
  either SHIPPED, PERMANENT GAP, or DEFER. If a future audit finds a
  candidate, the row would feed forward into WIN-P.7..10 with an
  implementation task.
- **DEFER** - Re-evaluate when WCI (Windows CI coverage expansion)
  or WIN-S.LAND series benches surface throughput evidence.
  Currently used only for `mmap_reader` because the chunked-reader
  replacement is in flight under a separate series.

## 4. Net result for WIN-P.7..10

Every implementation task that was scoped under WIN-P.7..10 has its
input row land at **PERMANENT GAP** in section 2. The four tasks
close as follows:

| Task | Title | Resolution |
|---|---|---|
| **WIN-P.7 (#3688)** | Implement splice Windows equivalent | **Close as no-implementation needed (permanent gap).** No Win32 primitive matches splice's pipe-mediated kernel-to-kernel zero-copy semantics. Resolution annotation: pointer to WIN-P.2 §4 and WIN-S.3 design doc. |
| **WIN-P.8 (#3689)** | Implement vmsplice Windows equivalent | **Close as no-implementation needed (permanent gap).** `WriteFileGather` and RIO `RIO_BUF` cover narrow socket-vectored cases but not the pipe-gather semantics. Resolution annotation: pointer to WIN-P.3 §4, WIN-S.4, WPG-9. |
| **WIN-P.9 (#3690)** | Implement Landlock Windows equivalent | **Close as no-implementation needed (permanent gap).** Restricted tokens, AppContainer, and Job Objects are all process-level, not per-thread runtime, and the `dir_sandbox` seam is absent on Windows. Resolution annotation: pointer to WIN-P.4 §4 option (b), WIN-S.6, SEC-1.l. |
| **WIN-P.10 (#3691)** | Implement SQPOLL Windows equivalent | **Close as no-implementation needed (permanent gap).** IOCP + `GetQueuedCompletionStatusEx` already covers completion-side syscall amortisation; the Windows file-I/O model has no syscall-free submission path. Resolution annotation: pointer to WIN-P.5 §4, WPG-3, WPG-5, WPG-9. |

## 5. Scoped-down WIN-P.11 and WIN-P.12

With no new Windows implementations to ship, the two follow-up
tasks targeting regression tests and matrix updates reduce in scope.

### WIN-P.11 (#3692) - Windows regression tests

**Original scope:** "Add Windows regression tests for each shipped
stub replacement."

**Reduced scope:** No new stub replacements ship under WIN-P. The
existing Windows test coverage already exercises every SHIPPED row
in section 2:

- `TransmitFile` (sendfile equivalent): coverage at
  `crates/fast_io/tests/iocp_transmit_file.rs` (WIN-S.11).
- `CopyFileExW` + ReFS reflink: coverage at
  `crates/fast_io/tests/platform_copy_dispatch.rs` (WIN-S.10).
- `FILE_FLAG_DELETE_ON_CLOSE`: coverage at
  `crates/fast_io/tests/win_tmpfile.rs` (WIN-S.9).
- IOCP file path: coverage across the iocp test family
  (WTD-2, WTD-3, WPG-1..6).

**Action under WIN-P.11:** add one Tier-2 caveat to the Windows
support matrix per permanent-gap row, citing the WIN-P audit that
produced the verdict. No new test infrastructure.

### WIN-P.12 (#3693) - Update Windows support matrix

**Original scope:** "Update Windows support matrix with shipped vs
permanent-gap status."

**Reduced scope:** the matrix at `docs/user/windows-support-matrix.md`
already records splice, vmsplice, Landlock, and SQPOLL as "Not
implemented / Not applicable" with a `Tracked: WIN-P series` pointer.
WIN-P.12 reduces to:

- Bump the cross-reference cells from "Tracked: WIN-P series" to
  point at this decision matrix (`docs/design/win-p-6-windows-stub-decision-matrix.md`)
  and the specific WIN-P.2..5 audit doc.
- Add a one-line caveat under section 3 ("I/O backends") explaining
  the structural reason for each permanent gap, with a pointer to
  the relevant audit doc - not a full design write-up.

No new matrix infrastructure; no new "cells" added beyond the
explanatory caveats.

## 6. Action items

| ID | Action | Owner | Status |
|---|---|---|---|
| WIN-P.7 | Close as no-implementation (permanent gap); pointer to WIN-P.2 | WIN-P | Pending close on this PR landing |
| WIN-P.8 | Close as no-implementation (permanent gap); pointer to WIN-P.3 | WIN-P | Pending close on this PR landing |
| WIN-P.9 | Close as no-implementation (permanent gap); pointer to WIN-P.4 option (b) | WIN-P | Pending close on this PR landing |
| WIN-P.10 | Close as no-implementation (permanent gap); pointer to WIN-P.5 | WIN-P | Pending close on this PR landing |
| WIN-P.11 | Add permanent-gap caveats to Tier-2 doc + Windows support matrix; no new tests | WIN-P | Pending on this PR landing |
| WIN-P.12 | Update Windows support matrix cross-references to point at this doc + the specific WIN-P audits; no new cells | WIN-P | Pending on this PR landing |

## 7. Cross-reference index

- Parent: **WIN-P** (#3681).
- Input audits: WIN-P.1 (#3682), WIN-P.2 (#3683), WIN-P.3 (#3684),
  WIN-P.4 (#3685), WIN-P.5 (#3686).
- Pre-WIN-P companion docs: WIN-S.2, WIN-S.3, WIN-S.4, WIN-S.5,
  WIN-S.6, WIN-S.8, WIN-S.10, WIN-S.11; WPG-1..10;
  WIN-TIER2.1..5; SEC-1.l; SEC-MK.
- This matrix closes: WIN-P.6 (#3687); resolution feed-forward for
  WIN-P.7 (#3688), WIN-P.8 (#3689), WIN-P.9 (#3690),
  WIN-P.10 (#3691), WIN-P.11 (#3692), WIN-P.12 (#3693).
- Companion user-facing doc: `docs/user/windows-support-matrix.md`
  (WPC-13).
- Companion Tier-2 disclosure: `docs/user/windows-feature-matrix.md`,
  README "Windows tier 2" caveat (WIN-TIER2.5).
- Out-of-scope related work: WIN-S.LAND.1.b series
  (`mmap_reader_stub` -> `WindowsChunkedReader`); WCI series
  (Windows CI coverage expansion); WIN-G series (real-hardware
  validation).
