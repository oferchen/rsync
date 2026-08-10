# WPG-7.b - io_uring opcode -> IOCP equivalent mapping

Audit-only mapping from each io_uring opcode catalogued in
`docs/design/wpg-7-iouring-opcode-inventory.md` (WPG-7.a) to its Windows I/O
Completion Port (IOCP) peer, or an explicit gap when no direct equivalent
exists. No source changes are made by this task.

Inputs:

- WPG-7.a inventory: 23 distinct opcodes in use (15 SQE-side, 8 register/setup).
- Windows reference surface: Win32 file I/O (`ReadFile`, `WriteFile`,
  `ReadFileScatter`, `WriteFileGather`), Winsock2 (`WSARecv`, `WSASend`,
  `WSAPoll`, `WSAEventSelect`), Winsock Registered I/O (RIO -
  `RIORegisterBuffer`, `RIOCreateCompletionQueue`, `RIOCreateRequestQueue`,
  `RIOReceive`, `RIOSend`), kernel-mode zero-copy (`TransmitFile`,
  `TransmitPackets`), metadata (`GetFileInformationByHandleEx`,
  `MoveFileExW`, `CreateHardLinkW`, `FlushFileBuffers`), and cancellation
  / timers (`CancelIoEx`, `CreateWaitableTimerExW`,
  `SetThreadpoolWait`).

Outputs feed:

- **WPG-8** - zero-copy socket send on Windows (`SEND_ZC` -> `TransmitFile`
  / RIO equivalent).
- **WPG-9** - registered-buffer scheme on Windows (`READ_FIXED` /
  `WRITE_FIXED` / `REGISTER_BUFFERS` -> Winsock RIO buffer registration).

## Severity legend

- **P0** - data-path, hot. A gap here taxes every byte transferred.
- **P1** - metadata or control-path, frequent. A gap here taxes every file
  or every batch but not every byte.
- **P2** - utility or rarely engaged. Closed-form workarounds are cheap.
- **N/A** - opcode is not applicable on Windows (kernel-side ring
  bring-up, capability probe, slot-table maintenance) and IOCP simply does
  not have a peer concept.

## Submission-queue opcodes (SQE side)

| io_uring opcode | IOCP equivalent | Severity | Notes |
|---|---|---|---|
| `IORING_OP_NOP` | GAP - no direct equivalent | P2 | Test-only stub. Closest Win32 analogue is `PostQueuedCompletionStatus` with a sentinel completion key to round-trip the port. No production caller. |
| `IORING_OP_READ` | `ReadFile` with `OVERLAPPED` queued to the IOCP | P0 | Direct peer. Windows associates the handle with the IOCP via `CreateIoCompletionPort`; completion arrives at `GetQueuedCompletionStatusEx`. Per-call pinning of the user buffer is implicit (the kernel locks pages for the duration of the request) - no `READ_FIXED` shortcut on the file path. |
| `IORING_OP_WRITE` | `WriteFile` with `OVERLAPPED` queued to the IOCP | P0 | Direct peer. Same overlapped/IOCP wiring as `ReadFile`. Buffered or unbuffered (`FILE_FLAG_NO_BUFFERING`) is selected at handle-open time, not per SQE. |
| `IORING_OP_READ_FIXED` | GAP on file handles; RIO covers sockets only | P0 | Win32 has no pre-registered-buffer scheme for file I/O. RIO (`RIORegisterBuffer` + `RIO_BUF`) registers user buffers, but RIO is socket-only - file `ReadFile` cannot reference a registered slot. Mitigation: keep an upper-bound buffer pool and rely on the kernel's per-request page locking. This is a WPG-9 input. |
| `IORING_OP_WRITE_FIXED` | GAP on file handles; RIO covers sockets only | P0 | Symmetric to `READ_FIXED`. Same WPG-9 input row. |
| `IORING_OP_FSYNC` | `FlushFileBuffers` (synchronous; queue via thread-pool work item to keep IOCP discipline) | P1 | No native overlapped fsync on Win32. Pattern: dispatch `FlushFileBuffers` from a `TrySubmitThreadpoolCallback` worker and post the result back to the IOCP with `PostQueuedCompletionStatus`. Matters per disk batch and per file close (see WPG-7.a `disk_batch.rs:238`, `file_writer.rs:407`). |
| `IORING_OP_SEND` | `WSASend` with `OVERLAPPED` (or `RIOSend` when registered) | P0 | Direct peer. RIO offers a lower-syscall path for steady-state streaming via `RIOSend` against a `RIO_BUF`. Default IOCP path uses `WSASend` with an `OVERLAPPED` plus optional `WSABUF` array for gather. |
| `IORING_OP_SEND_ZC` | `TransmitFile` (file -> socket zero-copy) or `TransmitPackets` (memory + file mix) | P0 | Closest zero-copy peer. `TransmitFile` requires a file handle source; pure in-memory zero-copy send is not a first-class Win32 primitive. RIO's `RIOSend` against a registered buffer is the practical near-peer for in-memory streams - it avoids per-call buffer locking by reusing the pre-registered `RIO_BUFFERID`, but does not bypass the data copy itself. Notification semantics differ: io_uring posts a value CQE plus a release CQE; `TransmitFile` posts a single overlapped completion when the send finishes. This row is the primary input to WPG-8. |
| `IORING_OP_RECV` | `WSARecv` with `OVERLAPPED` (or `RIOReceive` when registered) | P0 | Direct peer. Same shape as `WSASend`. RIO `RIOReceive` is the lower-overhead path for registered buffers. |
| `IORING_OP_POLL_ADD` | `WSAPoll` (sync) or association via `WSAEventSelect` + IOCP `WSAAsyncSelect`-style readiness | P1 | No exact peer. Windows IOCP is completion-based, not readiness-based, so the typical idiom is to skip the poll step and post the `WSASend` / `WSARecv` directly with `OVERLAPPED`; the kernel reports `WSAEWOULDBLOCK` via the completion. When readiness gating is required (the back-pressure pattern from WPG-7.a `shared_ring.rs:262`), use `WSAEventSelect(FD_WRITE)` and wait on the event from the IOCP thread-pool. Caveat: this gives up the linked-timeout trick (next row). |
| `IORING_OP_STATX` | `GetFileInformationByHandleEx` with `FileBasicInfo` + `FileStandardInfo` (or `FILE_STAT_INFORMATION` via `NtQueryInformationFile`) | P1 | Functional peer but synchronous. No overlapped variant. Run on a thread-pool worker for batch metadata lookups. For directory enumeration, prefer `GetFileInformationByHandleEx(FileIdBothDirectoryInfo)` to amortise the syscall over many entries. |
| `IORING_OP_RENAMEAT` | `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING` (or `SetFileInformationByHandle(FileRenameInfoEx)` for cross-volume atomic semantics) | P1 | Synchronous. For commit-after-write atomicity, `SetFileInformationByHandle` with `FileRenameInfoEx` and `FILE_RENAME_FLAG_POSIX_SEMANTICS` is the closest match to `renameat2(2)`'s atomic-replace semantics on NTFS. Queue from a thread-pool worker; route completion through IOCP via `PostQueuedCompletionStatus`. |
| `IORING_OP_LINKAT` | `CreateHardLinkW` | P1 | Synchronous. NTFS-only. ReFS does not support hard links. Wrap in a thread-pool worker. Required for `--link-dest` parity. |
| `IORING_OP_ASYNC_CANCEL` (classic) | `CancelIoEx(handle, lpOverlapped)` | P1 | Direct peer for cancel-by-overlapped (matches io_uring's cancel-by-`user_data` shape). The `OVERLAPPED*` plays the role of `user_data`. |
| `IORING_OP_ASYNC_CANCEL` (extended, by-fd / cancel-all) | `CancelIoEx(handle, NULL)` (cancel-all for a handle) and `CancelSynchronousIo` (for the thread variant) | P1 | Direct peer for cancel-all-on-handle. Cancel-all-on-thread maps to `CancelSynchronousIo`. No process-wide cancel primitive. |
| `IORING_OP_LINK_TIMEOUT` | GAP - synthesised via `CreateWaitableTimerExW` + `SetThreadpoolWait` + `CancelIoEx` | P1 | No linked-SQE primitive on IOCP. Pattern: post the `WSASend` / `ReadFile`, arm a waitable timer, and on timer fire call `CancelIoEx` against the in-flight overlapped. Each operation needs its own bookkeeping; the io_uring linkage that fires the cancel atomically inside the kernel does not exist. Bounds the back-pressured `WSASend` from WPG-7.a `batching.rs:194` against deadlock. |

## Registration / setup opcodes (ring-side)

| io_uring opcode | IOCP equivalent | Severity | Notes |
|---|---|---|---|
| `IORING_REGISTER_FILES` | `CreateIoCompletionPort` (associates a handle with an IOCP) | N/A | Conceptually closest: associating a handle with the completion port. There is no integer-slot indirection; the `HANDLE` itself is the identity. Per-submission file-table lookups simply do not exist on Win32. |
| `IORING_UNREGISTER_FILES` | `CloseHandle` (or detach by closing the IOCP-associated handle) | N/A | No explicit detach API. Closing the handle disassociates it. |
| `IORING_REGISTER_BUFFERS` | `RIORegisterBuffer` returning a `RIO_BUFFERID` | P0 | RIO peer for sockets only. Pins user memory and produces a registration ID that subsequent `RIO_BUF`s reference by `BufferId` + `Offset` + `Length`. This is the primary WPG-9 input. No file-side equivalent exists. |
| `IORING_UNREGISTER_BUFFERS` | `RIODeregisterBuffer(RIO_BUFFERID)` | P0 | RIO peer for sockets only. Same WPG-9 input row. |
| `IORING_REGISTER_PROBE` | GAP - no central capability registry | N/A | Windows publishes Winsock and RIO availability via per-API discovery: `WSAIoctl(SIO_GET_EXTENSION_FUNCTION_POINTER, WSAID_MULTIPLE_RIO)` for RIO, OS-version checks for `TransmitFile`, `MoveFileEx` flags, `SetFileInformationByHandle` classes. Probe per feature, cache in a `OnceLock`, mirror the io_uring probe contract. |
| `IORING_REGISTER_PBUF_RING` | `RIOCreateRequestQueue` + `RIOCreateCompletionQueue` (provided-buffer-equivalent via RIO buffer pool) | P0 | RIO request queues hand out `RIO_BUF` descriptors from a registered pool; the completion queue (RIOCQ) plays the role of the io_uring CQE flag-encoded buffer ID. Closest equivalent to the provided-buffer-ring semantics. WPG-9 input. |
| `IORING_UNREGISTER_PBUF_RING` | `RIOCloseCompletionQueue` (and queue teardown) | P0 | Paired teardown for the RIO request/completion queues. WPG-9 input. |
| `IORING_SETUP_SQPOLL` | GAP - thread-pool callbacks via `SetThreadpoolWait` are the closest pattern | N/A | No kernel-side polling thread that drains a ring on the application's behalf. IOCP already amortises syscalls by completing many overlapped requests through a single `GetQueuedCompletionStatusEx` wake-up; SQPOLL's goal (zero submit syscalls) is partly addressed by RIO's lock-free request queue (`RIOSend`, `RIOReceive` enqueue without a syscall). |

## Gap summary

Counting any row marked **GAP - ...** in the IOCP-equivalent column.
Pure-`N/A` rows (concepts that simply do not apply to IOCP) are not
counted as gaps; they are nonexistent peers by design.

| # | Opcode | Gap kind |
|---|---|---|
| 1 | `IORING_OP_NOP` | No direct peer (test-only). |
| 2 | `IORING_OP_READ_FIXED` | No file-side registered-buffer scheme (RIO is socket-only). |
| 3 | `IORING_OP_WRITE_FIXED` | No file-side registered-buffer scheme (RIO is socket-only). |
| 4 | `IORING_OP_LINK_TIMEOUT` | No linked-SQE primitive; must synthesise via waitable timer + `CancelIoEx`. |

Total: **4 gaps**. The `READ_FIXED` / `WRITE_FIXED` pair is a single
conceptual gap (file-side registered buffers) counted as two rows because
the io_uring side splits read and write opcodes. `IORING_OP_SEND_ZC` is
not in the gap list because `TransmitFile` and `RIOSend` cover the
zero-copy semantics, albeit with different shapes - the delta is a
WPG-8 design task, not a missing peer.

Notable non-gap caveats (peer exists but with friction worth flagging):

- `IORING_OP_FSYNC` -> `FlushFileBuffers` is synchronous-only; the
  thread-pool detour is unavoidable.
- `IORING_OP_STATX` / `IORING_OP_RENAMEAT` / `IORING_OP_LINKAT` are
  synchronous on Win32; batched async behaviour requires a thread-pool
  fan-out.
- `IORING_OP_POLL_ADD` is conceptually mismatched: IOCP is
  completion-based, so the natural Windows pattern skips the readiness
  check entirely and consumes `WSAEWOULDBLOCK` as the trigger to back
  off.

## Cross-references

- **WPG-8** (zero-copy socket send on Windows). Fed by:
  - `IORING_OP_SEND_ZC` row (SQE table) - primary delta is
    `TransmitFile` vs `WSASend`-zero-copy vs `RIOSend` against a
    registered buffer; notification semantics (value + release CQE)
    have no direct peer.
  - `IORING_OP_SEND` row (SQE table) - baseline path that WPG-8's
    zero-copy variant must coexist with on the same socket.
- **WPG-9** (registered-buffer scheme on Windows). Fed by:
  - `IORING_OP_READ_FIXED` and `IORING_OP_WRITE_FIXED` rows (SQE
    table) - file-side gap; no Win32 peer, design must decide between
    accepting the gap or front-ending the buffer pool with RIO when
    the underlying handle is a socket.
  - `IORING_REGISTER_BUFFERS` and `IORING_UNREGISTER_BUFFERS` rows
    (register / setup table) - RIO `RIORegisterBuffer` /
    `RIODeregisterBuffer` are the direct peers for the socket case.
  - `IORING_REGISTER_PBUF_RING` and `IORING_UNREGISTER_PBUF_RING`
    rows (register / setup table) - RIO request / completion queue
    pair (`RIOCreateRequestQueue`, `RIOCreateCompletionQueue`,
    `RIOCloseCompletionQueue`) is the provided-buffer-ring peer.
