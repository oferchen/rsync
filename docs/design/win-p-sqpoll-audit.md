# WIN-P.5: Windows equivalent for Linux io_uring SQPOLL audit

Status: AUDIT (feeds WIN-P.6 decision matrix and WIN-P.10 impl).

Linux `IORING_SETUP_SQPOLL` dedicates a kernel thread to polling the io_uring
SQ so userspace never calls `io_uring_enter` on the submission hot path.
The saved syscall is the entire point. SQP-LAND (PR #5833) shipped
rootless-container detection + graceful fallback, so SQPOLL is a wired,
opt-in, observable code path on Linux today. This audit asks: what is the
Windows analog?

## 1. Candidate mechanisms

### 1.1 IOCP threadpool (`CreateThreadpoolIo` + `StartThreadpoolIo`)

MSDN: `learn.microsoft.com/en-us/windows/win32/api/threadpoolapiset`.

- `CreateThreadpoolIo` associates a handle with a Win32 thread-pool I/O
  object; `StartThreadpoolIo` arms the pool to dispatch the completion
  callback when the kernel posts a result; `CloseThreadpoolIo` tears it
  down.
- Scope: any handle that supports overlapped I/O.
- Savings model: the pool is a userspace dispatcher over the same IOCP
  the OS uses anyway. Every submission still calls
  `ReadFile`/`WriteFile`/`WSARecv`/`WSASend` and every completion still
  goes through the kernel I/O subsystem. No per-submission syscall is
  removed - the pool just replaces a worker-thread +
  `GetQueuedCompletionStatusEx` loop with a system-managed equivalent.

Verdict: not analogous to SQPOLL. Solves worker management, not submission
syscalls.

### 1.2 RIO completion polling (`RIONotify` + `RIO_CQ` polled drain)

MSDN: `learn.microsoft.com/en-us/windows/win32/api/mswsockdef`.

- `RIOCreateCompletionQueue` with `NotificationCompletion` set to
  `RIO_EVENT_COMPLETION` + `NotifyReset = TRUE` + `INVALID_HANDLE_VALUE`
  arms the CQ for polled mode: no notification fires; the caller drains
  via `RIODequeueCompletion`, which is a lock-free user-mode dequeue with
  no syscall on the hot path.
- `RIOSend` / `RIOReceive` post by referencing slices of pre-registered
  buffers - submissions are ring-slot writes, not syscalls.
- Scope: socket I/O only (Winsock extension).
- Savings model: in polled mode, steady state on the socket hot path is
  pure userspace. The caller's polling thread spins on the ring; this is
  the structural mirror of SQPOLL's kernel-side poll thread, with the
  same idle-CPU trade-off and the same break-even constraint - polling
  only wins when event rate stays high enough to keep the thread busy.

Verdict: the SQPOLL analog for socket I/O. No file-I/O parity available.

### 1.3 Direct overlapped I/O with manual polling (`GetOverlappedResult`)

- `ReadFile`/`WriteFile`/`WSARecv`/`WSASend` + `OVERLAPPED` -> one syscall
  per submission. `GetOverlappedResult` -> one syscall per check.

Verdict: not analogous. Matches the non-SQPOLL Linux baseline, not SQPOLL.

## 2. oc-rsync SQPOLL gate-site map

| Site | Purpose |
| --- | --- |
| `crates/fast_io/src/io_uring/config.rs:122 should_skip_sqpoll_due_to_rootless` | Rootless container -> skip SQPOLL (one-shot) |
| `crates/fast_io/src/io_uring/config.rs:69 SQPOLL_DISABLED_BY_POLICY` | `--no-io-uring-sqpoll` CLI override |
| `crates/fast_io/src/io_uring/config.rs:56 SQPOLL_FALLBACK` | Diagnostic flag: did SQPOLL setup fail? |
| `crates/fast_io/src/io_uring/{shared_ring,per_thread_ring}.rs` | `setup_sqpoll(...)` builder calls |
| SQP-LAND series (#5833) | Detection, EPERM error mapping, IKV-F.7 observability |

Every site is `#[cfg(target_os = "linux")]`. The IOCP layer
(`crates/fast_io/src/iocp/`) has no SQPOLL counterpart today; the RIO
scaffolding in `iocp/rio.rs` is opt-in (`OC_RSYNC_WINDOWS_RIO=auto|on`)
and not yet bound into `iocp/socket.rs`.

## 3. Comparison

| Property | SQPOLL (Linux) | RIO POLLED | IOCP threadpool | Overlapped + manual poll |
| --- | --- | --- | --- | --- |
| Submission syscall removed | yes | yes (`RIOSend` is a ring-slot write) | no | no |
| Completion syscall removed | yes | yes (`RIODequeueCompletion`) | no | no |
| Scope | files + sockets + pipes | sockets only | any overlapped handle | any overlapped handle |
| Idle-CPU cost | configurable `idle` param | caller-implemented backoff | none | none |
| oc-rsync wiring today | shipped + observable | scaffolding in `iocp/rio.rs` only | not used | not used |

RIO POLLED is the only candidate that matches SQPOLL's savings model.

## 4. Scope limitation

The honest verdict: RIO POLLED is a *partial* analog. SQPOLL on Linux
also covers file-I/O hot paths (sender reads, receiver writes via
`fast_io::io_uring::file_reader` / `file_writer`). RIO has no
file-handle counterpart - Windows file I/O routes through
`iocp/file_reader.rs` / `iocp/file_writer.rs` and keeps its standard
overlapped syscalls regardless of any RIO wiring. This is structural;
Microsoft never extended Registered I/O outside Winsock. Daemon-style
workloads are socket-dominated on both OSes, so this is still worth
doing, but the parity matrix is asymmetric and must be documented as
such.

## 5. Cross-reference: NET-RIO audit

`docs/design/net-rio-windows-audit.md` (in flight) already covers
`RIORegisterBuffer` / `RIODeregisterBuffer` lifecycle, completion-queue
notification modes including `RIO_IOCP_COMPLETION` hybrid wiring,
`RIONotify` / `RIODequeueCompletion` semantics, pool sizing under daemon
concurrency, and per-socket function-table caveats. WIN-P.5
deliberately does not re-derive that scaffolding. The SQPOLL-equivalent
question is: assuming NET-RIO.2 lands the hybrid wiring, can we switch
the CQ from `RIO_IOCP_COMPLETION` (notification-driven) to a polled
drain? That is the WIN-P.10 implementation question, gated on NET-RIO
infrastructure landing first.

## 6. Recommendation for WIN-P.6

Defer WIN-P.10 implementation until NET-RIO.2/.3/.4 ship the RIO hybrid
wiring. Concretely:

1. WIN-P.6 records: RIO POLLED is the chosen SQPOLL analog; IOCP
   threadpool is rejected (no syscall savings); manual overlapped polling
   is rejected (baseline, not analog).
2. WIN-P.6 marks the file-I/O parity gap as permanent under the current
   Windows kernel surface.
3. WIN-P.10 sequences after NET-RIO.4 (bench cell) confirms hybrid RIO
   wiring delivers measurable wins on the socket hot path. Only then is
   polled-completion mode worth wiring.
4. The Windows support-matrix doc (WPC-13 / WIN-TIER2.5 referent) gets
   a row: "SQPOLL equivalent: socket I/O via RIO POLLED when NET-RIO
   ships; file I/O parity unavailable."

## 7. Risks

- **Polling-thread CPU cost.** Polled RIO burns a core. Below the
  break-even rate it is worse than `RIO_IOCP_COMPLETION`. WIN-P.10 needs
  a backoff parameter or runtime switch between polled and notify modes.
- **Scope-creep into file I/O.** Reviewers may read "SQPOLL equivalent"
  as full parity. Docs must call out that file-I/O submissions on
  Windows continue to pay a syscall each.
- **NET-RIO ship-order dependency.** WIN-P.10 has no work to do without
  NET-RIO infrastructure. Track WIN-P.10 as blocked-on-NET-RIO in the
  WIN-P.6 decision matrix.
- **Hybrid vs polled tradeoff.** NET-RIO.4 measures
  `RIO_IOCP_COMPLETION` vs overlapped baseline. WIN-P.10 needs a
  separate cell measuring polled vs notification mode under the same
  workload to justify the polling thread.
