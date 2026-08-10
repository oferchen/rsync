## WPG-8 - Windows equivalent for io_uring `SEND_ZC`

Audit-only design doc for the Windows zero-copy socket-send path. This
document decides whether the `IORING_OP_SEND_ZC` semantics that
`crates/fast_io/src/io_uring/send_zc.rs` exposes have a viable peer on
the IOCP backend, and if so, which Win32 primitive(s) to standardise
on and how to slot them into the existing IOCP send path. No source
changes are made by this task.

Inputs:

- WPG-7.a opcode inventory: `docs/design/wpg-7-iouring-opcode-inventory.md`.
  `IORING_OP_SEND_ZC` is the only zero-copy primitive wired in;
  default-off, feature-gated behind `iouring-send-zc`, runtime-probed
  via `IORING_REGISTER_PROBE`, payload-floored at
  `MIN_SEND_ZC_PAYLOAD = 4 KiB`
  (WPG-7.a lines 42, 132-133; `send_zc.rs:236`).
- WPG-7.b IOCP mapping: `docs/design/wpg-7b-iouring-iocp-mapping.md`.
  `SEND_ZC` is classified as a P0 zero-copy peer with the closest
  Win32 matches being `TransmitFile`, `TransmitPackets`, and `RIOSend`
  against a registered buffer; notification semantics (value CQE +
  release CQE) have no direct peer
  (WPG-7.b lines 49, 86-90, 106-112).
- WPG-7.c gap list: `docs/design/wpg-7c-iocp-gap-list.md`. `SEND_ZC`
  is **P3** in the prioritised gap list with WPG-8 owning the
  feasibility decision; the gap-list explicitly excludes it from the
  four-gap total because viable peers exist with a different shape
  (WPG-7.c lines 27-28, 36-37, 58-62).
- Memory note: `project_iouring_send_zc_optin_only.md`. SEND_ZC is
  not in default features; `--zero-copy` advertises the primitive but
  default builds silently downgrade to plain `SEND`.
- Existing partial implementation: `crates/fast_io/src/iocp/transmit_file.rs`
  (synchronous TransmitFile primitive, gated behind the
  `transmitfile` feature; `Cargo.toml:80-87`). `IocpSocketWriter::
  try_transmit_file_path` already wires the fast path with a
  `WSASend` fallback on `ERROR_NOT_SUPPORTED`
  (`crates/fast_io/src/iocp/socket.rs:362-401`).
- Companion design docs already on master:
  `docs/design/windows-transmitfile.md` (#2130 API survey),
  `docs/design/windows-transmitfile-zerocopy.md` (#2130 integration
  plan), `docs/design/iouring-send-zc.md` (Linux design).

## 1. What `SEND_ZC` actually does on Linux

`IORING_OP_SEND_ZC` was added in Linux 6.0 and removes the kernel
copy from a userspace buffer into the socket send queue. The
mechanism:

1. Caller posts a `SEND_ZC` SQE that references a userspace buffer.
2. The kernel pins the buffer pages via `get_user_pages_fast`,
   keeps a reference to them, and queues the bytes for transmit. It
   does **not** copy the bytes into a socket-buffer skb; the skb
   carries the pinned page references directly.
3. The kernel posts a **transfer CQE** (`IORING_CQE_F_MORE` set in
   `flags()`) as soon as the data is queued. `result()` carries the
   byte count or `-errno` exactly like a plain `SEND` CQE.
4. The kernel posts a **notification CQE** (`IORING_CQE_F_NOTIF`
   set) once it has released its reference to the pinned pages: NIC
   DMA has completed, the loopback peer has consumed the bytes, or
   the send was cancelled. The two CQEs share `user_data`; demux is
   on the flag bits, not the data field.

Caller invariants:

- The buffer must remain valid and unmodified until the notification
  CQE arrives. `crates/fast_io/src/io_uring/send_zc.rs:152-189`
  enforces this by blocking on both CQEs before returning, making
  the wrapper synchronous to callers even though the kernel page
  release is asynchronous.
- The CQE volume doubles. `docs/design/iouring-send-zc.md:147-155`
  caps in-flight SEND_ZC SQEs at `sq_entries / 2` so the CQ ring
  does not overflow.
- Sub-page payloads lose to plain `SEND`. The dispatch floor is
  `SEND_ZC_DISPATCH_MIN_BYTES = 4 KiB`
  (`crates/fast_io/src/io_uring/send_zc.rs:236`).
- Fragmentation / partial send may force the kernel to copy
  internally anyway. The "zero" in zero-copy is best-effort, not a
  guarantee.

Net win on representative workloads: 25-40% sys-CPU reduction on
loopback per kernel-suite numbers cited in `iouring-send-zc.md:298-301`
and the IUS-4/5/6 design. Wall time on a saturated NIC may not move
because the bottleneck shifts off CPU; CPU headroom matters for
concurrent transfers.

## 2. Windows zero-copy socket-send equivalents

Four candidate APIs exist on Win32; only the first three have any
zero-copy posture.

### 2.1 `TransmitFile` (file -> socket; **kernel DMA from file cache**)

`mswsock.dll`'s `TransmitFile` hands a file `HANDLE` directly to the
TCP stack. The kernel DMAs file bytes from the system file cache
straight into the socket's send queue, with no userspace buffer in
the loop at all. This is the closest analogue to `sendfile(2)`, not
to `SEND_ZC` per se: `SEND_ZC`'s source is a userspace buffer that
the kernel pins; `TransmitFile`'s source is a kernel-side file cache
page.

Capabilities:

- Source: regular-file `HANDLE` (NTFS, ReFS local). Returns
  `ERROR_NOT_SUPPORTED` on SMB/DFS shares and on some encrypted /
  compressed volumes. `windows-transmitfile.md:65-69` documents the
  per-volume probe via
  `GetFileInformationByHandleEx(FileRemoteProtocolInfo)`.
- Sink: TCP `SOCKET` only. Datagram or UNIX-domain sockets are not
  supported.
- Header / trailer iovec via `LPTRANSMIT_FILE_BUFFERS`. Lets the
  multiplex 4-byte envelope ride on the same kernel call as the file
  payload (`windows-transmitfile-zerocopy.md:97-117`).
- 32-bit length cap (`DWORD nNumberOfBytesToWrite`); files > 4 GiB
  loop with `SetFilePointerEx` (`windows-transmitfile.md:78`).
- OVERLAPPED + IOCP completion. Single overlapped completion per
  submission, not the value + release pair io_uring posts. The
  current `transmit_file.rs:121-131` shim uses the synchronous form
  with `lpOverlapped = NULL`; section 3 of
  `windows-transmitfile-zerocopy.md` reserves the OVERLAPPED form
  for the IOCP integration step.
- Availability: Windows NT 4.0+. Effectively present everywhere
  oc-rsync runs.

### 2.2 `WSASend` against an `RIO_BUF` (registered userspace buffer)

Winsock Registered I/O (RIO) is the closest peer to `SEND_ZC`'s
"pin a userspace buffer, send it without copying" semantics. The
shape:

1. Caller registers a slab via `RIORegisterBuffer(slab_ptr,
   slab_len)`, getting back a `RIO_BUFFERID`. The slab is pinned for
   the lifetime of the registration.
2. Caller hands out `RIO_BUF { BufferId, Offset, Length }`
   descriptors that index into the registered slab.
3. `RIOSend(rq, &rio_buf, ...)` posts the send into the request
   queue without a syscall (the request queue is a lock-free
   userspace structure mapped into the kernel).
4. Completion arrives on a `RIO_CQ` (RIO completion queue), drained
   via `RIODequeueCompletion`. RIO completions do **not** flow
   through the IOCP completion port by default; they go to a
   separate event handle or to a dedicated IOCP via
   `RIOCreateCompletionQueue` with the IOCP method.

Capabilities:

- Source: pre-registered userspace buffer. Per-send copy from the
  user's actual data into the registered slab is required unless the
  caller composes data directly into RIO-owned memory (the upstream
  rsync token loop does not - the `MultiplexWriter` buffer is the
  one that gets reused).
- Sink: any socket type (TCP, UDP, AF_UNIX). Socket must be created
  with `WSA_FLAG_REGISTERED_IO`.
- Lower syscall overhead than `WSASend`: enqueue + dequeue are
  lock-free userspace operations. The trade-off is RIO setup cost
  amortised across many sends.
- No header/trailer iovec equivalent; multiple `RIO_BUF` entries
  per send via `RIOSendEx` cover the same ground.
- Availability: Windows 8 / Server 2012+. Recent enough that
  oc-rsync's minimum-Windows-version baseline must be checked
  before committing to the path.

`RIOSend` is genuinely zero-copy at the socket layer: the kernel
holds a reference to the registered slab pages, not a copy of the
bytes. The notification model differs from `SEND_ZC` in that there
is only one completion (the equivalent of the transfer CQE); the
slab pin lifetime is tied to the `RIO_BUFFERID` registration, not
to a per-send notification. Buffer reuse is therefore safe as soon
as the single completion arrives, which actually maps to "after the
notification CQE" in `SEND_ZC` terms - the value CQE has no
analogue.

### 2.3 `TransmitPackets`

`TransmitPackets` accepts a mixed array of `TRANSMIT_PACKETS_ELEMENT`
entries, each of which is either a `HANDLE`+offset+length (file
range, kernel DMA) or a buffer pointer+length (userspace memory).
The kernel composes the elements into a single send queue insertion.

Capabilities:

- Source: mix of file ranges and userspace buffers.
- Sink: connection-oriented socket (TCP).
- Documented as legacy: Microsoft's own guidance recommends
  `TransmitFile` for file-only sends and `WSASend` (or RIO) for
  buffer-only sends. `TransmitPackets` survives for the rare case
  that needs both in one syscall.
- Availability: Windows XP+.

No new capability vs. `TransmitFile` + header iovec, with a more
complex element-array setup. Section 6 of `windows-transmitfile.md`
already notes it as "not pursued"; this audit reaches the same
conclusion (see section 3).

### 2.4 Standard `WSASend` overlapped (the status quo, **not** zero-copy)

`WSASend` against a plain `WSABUF` array always copies the user
buffer into the socket send buffer. This is the path
`crates/fast_io/src/iocp/socket.rs:284-335`
(`IocpSocketWriter::send_async`) takes today. No kernel mechanism
exists to make it zero-copy without changing the socket type
(RIO) or the source (TransmitFile).

### 2.5 Capability summary

| Primitive | Source | Sink | Zero-copy at socket layer | Header iovec | Min OS | Completion model |
|---|---|---|---|---|---|---|
| `TransmitFile` | regular file HANDLE | TCP SOCKET | yes (file-cache page) | yes (`TRANSMIT_FILE_BUFFERS.Head/Tail`) | NT 4.0 | single OVERLAPPED CQE |
| `RIOSend` + `RIO_BUF` | registered userspace slab | any socket type | yes (pinned slab page) | no (multi-buf via `RIOSendEx`) | Win8 / 2012 | single RIO_CQ entry |
| `TransmitPackets` | mix (file or buffer) | TCP SOCKET | partial (file elements only) | implicit | XP | single OVERLAPPED CQE |
| `WSASend` | userspace buffer | any socket type | no | no | NT 3.5 | single OVERLAPPED CQE |

Critical asymmetry vs. Linux: `SEND_ZC`'s **two-CQE** notification
shape (transfer + release) has no Win32 peer. Every Windows
candidate posts **one** completion. This is not a gap - it is a
different contract. The Linux contract exists because the kernel
needs to tell the caller "you can reuse the buffer now"; Windows
defers that question to the buffer lifecycle of the chosen primitive:

- `TransmitFile`: the file HANDLE owns the data, the caller never
  saw it in userspace, so the notification is trivially "send
  finished" with no buffer-reuse signal needed.
- `RIO_BUF`: buffer lifecycle is bounded by the `RIO_BUFFERID`
  registration, not by individual sends. The single completion
  signals "this RIO_BUF slot is free to reuse".
- `WSASend`: the buffer is already copied into the socket send queue
  before the completion fires.

The two-CQE design exists only because `SEND_ZC` pins user pages
*per submission* without a registration. The closest analogue on
Windows is to not need it at all - either move the source to a file
(TransmitFile) or amortise the pin via registration (RIO).

## 3. Mapping decision

Per producer of "send bytes from process X to socket":

| Producer | Source today | Recommended Windows primitive | Rationale |
|---|---|---|---|
| Sender delta-pipeline `Literal` chunks (non-compressed) | file `HANDLE` already open, bytes never need to be in userspace | **`TransmitFile`** with `lpTransmitBuffers.Head` = 4-byte envelope | File handle is open, multiplex header is tiny and stack-allocatable, kernel DMA from file cache is the highest-leverage win. Matches the recommendation already locked in `windows-transmitfile-zerocopy.md:107-117`. |
| Sender delta-pipeline `Copy` tokens (offset+length reference to basis) | file `HANDLE` (basis) + `SetFilePointerEx` to offset | **`TransmitFile`** with `lpOverlapped.Offset` set | Same primitive as Literal but with an explicit offset; no codec involvement, no userspace buffer required. The 4-byte multiplex envelope rides via `Head` as above. |
| Multiplex frame headers and small control frames (`MSG_INFO`, `MSG_ERROR`, ...) | small userspace bytes, not from file | **standard `WSASend`** (status quo) | Per-message overhead of RIO setup outweighs the per-send saving for headers that fit in cache lines. Multiplex writer already serialises sends so there is no contention to amortise away. |
| Daemon greeting / negotiation bytes | one-time userspace strings | **standard `WSASend`** | One-shot at session start; not worth optimising. |
| Compressed token streams (`-z`) | userspace post-codec bytes, file payload no longer reaches the wire literally | **standard `WSASend`** | Codec output is in a userspace buffer; `TransmitFile` does not apply. RIO could apply but only if the compressed-output buffer is already the registered slab, which would require touching the codec API. Out of scope for WPG-8. |
| Future: large in-memory bulk sends from a long-lived registered slab (e.g., precomputed checksum blocks) | userspace buffer reused across many sends | **`RIOSend` against an `RIO_BUFFERID`** | This is the only producer pattern where RIO's amortised registration cost pays off. None of today's producers fit; the row exists to mark the slot in case a future producer (e.g., a registered-buffer ring on the Windows side - WPG-9) creates one. |

**Primary Windows API recommendation: `TransmitFile`** for every
producer where the source is a file. The hot path for an rsync
sender is overwhelmingly file-to-socket bytes; the Literal +
Copy-token rows alone cover > 95% of bytes on the wire for a
representative `--whole-file` or low-match-ratio delta transfer. The
synchronous shim already exists; the asynchronous (IOCP-routed)
form is the WPG-8 implementation surface.

`RIOSend` is **not** recommended as a first step. The cost-benefit
flips only when there is a producer whose source bytes already live
in a registered slab; today there is none, and adding RIO before a
caller exists is dead infrastructure. Re-evaluate when WPG-9
lands a Windows registered-buffer scheme.

`TransmitPackets` is **not** recommended at any step. It offers no
capability `TransmitFile` + `WSASend` together do not, at the cost
of a more awkward element-array surface. The WPG-7.b table includes
it for completeness only.

## 4. Implementation surface

### 4.1 Where the file-to-socket path lives today

The Windows sender's per-byte path:

- `crates/transfer/src/generator/delta.rs:245-263` reads file bytes
  into a `Vec<u8>` scratch buffer with the 4-byte envelope prefixed
  in place, then calls `writer.write_all(&buf[wire_off..])`.
- `crates/protocol/src/multiplex/writer.rs:179-195` (`flush_buffer`)
  wraps the bytes in `MSG_DATA` frames via `send_msg`.
- `crates/protocol/src/multiplex/io/send.rs:16-22` (`send_msg`) is
  the only frame-emitter on the wire side.
- `crates/fast_io/src/iocp/socket.rs:284-335`
  (`IocpSocketWriter::send_async`) issues `WSASend` with a single
  `WSABUF` pointing at the userspace buffer.

That path performs two avoidable copies on Windows: kernel page
cache -> user buffer (in `read_exact`), then user buffer -> kernel
socket buffer (in `WSASend`). `TransmitFile` collapses both into a
single kernel-mode DMA.

The wiring point already exists. `IocpSocketWriter::
try_transmit_file_path` (`crates/fast_io/src/iocp/socket.rs:
361-401`) takes a file handle + length and dispatches through the
synchronous shim, with a built-in `WSASend` fallback on
`ERROR_NOT_SUPPORTED`. The fallback path reads up to
`fallback_buf.len()` bytes from the file and sends them via
`send_async`, returning whatever the fallback transmits and leaving
the remainder for the caller's outer loop.

Today the trait shape called for in `windows-transmitfile-zerocopy.md`
section 8 step 2 (`PlatformSendFile`) is **not yet** wired into
`crates/transfer/src/generator/delta.rs`; the sender still goes
through `writer.write_all`. The WPG-8 implementation surface is the
wiring step, not a new primitive.

### 4.2 Capability detection

| Capability | Detection mechanism | Cache key |
|---|---|---|
| `TransmitFile` is callable | Compile-time presence in `windows-sys::Win32::Networking::WinSock`. NT 4.0 floor means every supported Windows target has it; no runtime probe needed. | n/a |
| Source volume is eligible (local, uncompressed, unencrypted) | Per-source-volume probe via `GetFileInformationByHandleEx(FileRemoteProtocolInfo)` plus `GetVolumeInformationByHandleW` on first use of the volume | `(VolumeSerialNumber)` |
| Socket is overlapped + IOCP-bound | Typed at construction via an `OverlappedSocket` newtype (`windows-transmitfile.md:113-115`); refuses non-overlapped handles at compile/construction time | n/a |
| `RIOSend` is callable (when WPG-9 wants it) | `WSAIoctl(SIO_GET_EXTENSION_FUNCTION_POINTER, WSAID_MULTIPLE_RIO)` returning a non-NULL function-table pointer | process-wide `OnceLock` |
| AV interception degrading `TransmitFile` to a buffered copy | First-use warmup benchmark: 1 MiB TransmitFile vs 1 MiB read+WSASend; disable the fast path for the run if cycles/byte ratio > 0.7x | process-wide `OnceLock` |

This mirrors WPG-7.b's general guidance (`wpg-7b-iouring-iocp-mapping.md:67`)
to probe per feature and cache in a `OnceLock`.

### 4.3 Feature-gate naming

The Linux side uses `iouring-send-zc` (see `project_iouring_send_zc_optin_only.md`).
The IOCP synchronous shim already lives behind `transmitfile`
(`crates/fast_io/Cargo.toml:87`). The recommendation is:

- Keep `transmitfile` as the umbrella feature for the IOCP fast
  path. Today it gates the synchronous shim; extend it to gate the
  asynchronous (IOCP-routed) form and the
  `PlatformSendFile`-trait wiring described in
  `windows-transmitfile-zerocopy.md` section 8.
- Default-on once the trigger conditions in
  `windows-transmitfile-zerocopy.md` section 7 are met (Windows
  sender CPU > 20% in memcpy on a representative workload, or a
  user report of > 25% Windows-vs-Linux throughput gap). Until
  then ship default-off; mirror SEND_ZC's posture exactly.
- Do **not** introduce an `iocp-transmitfile` feature in addition
  to `transmitfile`. The Cargo.toml dependency graph already chains
  `transmitfile = ["iocp"]`, so the IOCP-gated naming is implicit.

For the RIO path (deferred to WPG-9), introduce a separate
`iocp-rio` feature when a producer materialises. Keep RIO and
TransmitFile orthogonal so each can be promoted to default-on
independently of the other.

## 5. Cost analysis

### 5.1 Throughput improvement estimate

Published Microsoft guidance for `TransmitFile` vs. read+WSASend
is concentrated in two places: the Windows Internals series (Russinovich,
chapter 8 on the network stack) and the MSDN sample documentation
(`TransmitFile` overview). The numbers cited in
`docs/design/windows-transmitfile.md:36` and
`docs/design/windows-transmitfile-zerocopy.md:140-157` derive from
the in-tree #2130 profile on Windows Server 2022:

- `--whole-file`, 1 GiB file, 10 GbE, warm cache: **30-50% wall-time
  reduction**, sender CPU 22% -> ~9%.
- `--whole-file`, 1 GiB file, 1 GbE: **5-10% wall-time**, sender CPU
  18% -> 6% (NIC-bound).
- Delta mode, 1 GiB file, 90% match ratio: **5-15% wall-time** (literal
  runs are smaller; per-call setup overhead matters more).
- `< 64 KiB file`: **none or slight regression** (setup cost dominates;
  fast path disabled below threshold).

These align with the Linux `SEND_ZC` numbers
(`iouring-send-zc.md:298-301`) where the 25-40% sys-CPU reduction was
observed on loopback. The pattern is consistent: when the bottleneck
is user/kernel-copy CPU, zero-copy primitives free 25-50% of that
budget; when the bottleneck is the NIC, wall time barely moves but
CPU headroom rises.

Microsoft has not published vendor-neutral `TransmitFile`-vs-WSASend
microbenchmark numbers; the closest cite-able sources are kernel-suite
runs and the rsync community's own measurements. Treat the cost
analysis as a hypothesis to validate, not a guarantee
(`windows-transmitfile-zerocopy.md:159-162` documents the same gate).

### 5.2 CPU reduction estimate

The dominant cost on the current path is the two `memcpy` hops
(file-cache -> user, user -> socket buffer). At 64 KiB chunks and
typical L1/L2 cache pressure during a `--whole-file` push,
`memcpy` accounts for ~22% of sender CPU on Windows Server 2022.
`TransmitFile` eliminates both, leaving:

- ~5% in syscall + completion-port dispatch.
- ~3% in multiplex framing (header build + envelope encode).
- Residual ~5-10% in file-cache page fault and TCP send-queue admin.

Net: **~12-15% absolute CPU reduction** on the hot path, matching
the 22% -> 9% projection above and Linux SEND_ZC's 25-40% sys-CPU
delta when both userspace and kernel copies are eliminated. The
delta on the Linux side is bigger because Linux is removing one
copy (kernel-side) on top of the userspace one rsync also avoids
via `read_exact` into a reused buffer; Windows is removing both
because the current Windows path performs both.

### 5.3 Per-send overhead change

| Cost | Status quo (`WSASend`) | `TransmitFile` | Notes |
|---|---|---|---|
| Kernel handle args | 1 (SOCKET) | 2 (SOCKET + HANDLE) | Extra `HANDLE` is already open in the caller. |
| Userspace buffer alloc | per-call (or reused `Vec<u8>`) | none | Header lives on the stack. |
| Syscalls per chunk | `ReadFile` + `WSASend` = 2 | `TransmitFile` = 1 | Half the syscall rate per chunk. |
| User->kernel copies | 2 | 0 | The whole point. |
| Completion-port events per chunk | 1 | 1 | Same shape; no two-CQE-like doubling. |
| Lifetime invariants | none (kernel copies before completion) | OVERLAPPED + file HANDLE must outlive the in-flight call | Already enforced by the existing async OVERLAPPED bookkeeping in `iocp::pump`. |
| Cache-manager prefetch | per `ReadFile` call (request-granularity) | sequential when file is opened with `FILE_FLAG_SEQUENTIAL_SCAN` | Caller must open with the flag; `SequentialFile` newtype in step 1 of `windows-transmitfile-zerocopy.md:236-243` enforces this. |
| AV-interception risk | none specific | possible degradation; warmup probe detects and disables | Detection cost = single 1 MiB probe per process. |
| SMB/DFS/encrypted volume | works | `ERROR_NOT_SUPPORTED`; falls back to `WSASend` | Per-volume eligibility cache from section 4.2. |

Per-send overhead is **net lower** on every dimension except handle
count, and the extra handle is essentially free (already open).

## 6. Test plan

If WPG-8 implementation tasks are filed, this is the minimum
acceptance test surface. Mirrors the shape of the Linux SEND_ZC
bench plan in `iouring-send-zc.md` sections 5-6.

### 6.1 Wire-byte parity

Property test (`crates/fast_io/tests/`): for the same input file +
the same multiplex envelope, the byte stream a receiver observes on
the wire is **byte-for-byte identical** between the `WSASend`
status quo path and the `TransmitFile` fast path.

- Inputs: synthetic files of 1 KiB, 64 KiB, 1 MiB, 16 MiB - 1,
  5 GiB (loop-over-DWORD-cap regression), and 0 bytes.
- Capture wire output via a loopback `TcpListener` pinned to a
  recording buffer (`socket2::Socket::set_recv_buffer_size` set to
  the test file size).
- Diff captured streams; equal length and equal content.
- Repeat with multiplex framing enabled (header + payload + 0-byte
  trailer where applicable).
- Repeat for every combination of `--whole-file` and delta-mode
  Literal tokens. Compressed tokens are out of scope per section 3.

### 6.2 Throughput benchmark

Two reference workloads:

- **10 MiB file, loopback**. Median wall time across 10 hyperfine
  runs. Acceptance: `TransmitFile` path within 5% of `WSASend`
  path. Sub-second runs - the goal is to surface per-call setup
  overhead regressions.
- **1 GiB file, loopback**. Median wall time across 5 hyperfine
  runs. Acceptance: `TransmitFile` path **at least 20%** faster
  than `WSASend` path on warm cache; sender CPU (`perf stat`)
  drops by at least 50% (target: 22% -> < 12%).

Reference rig: rsync-profile container in Windows mode (Windows
Server 2022 base or windows-rs cross-compile). Loopback only for
CI; 10 GbE measurement recorded as release-notes content but not
a merge gate (mirrors the Linux Workload C deferral in
`iouring-send-zc.md:314-318`).

### 6.3 Concurrent-connection stress

`TransmitFile` historically had per-socket scheduling concerns: a
single in-flight call per socket is recommended
(`windows-transmitfile.md:74-76`). The multiplex writer already
serialises sends so this is naturally satisfied, but the test
exists as a regression guard:

- 16 concurrent loopback transfers, each pushing a 64 MiB file.
- Acceptance: no head-of-line blocking visible in per-connection
  wall-time variance; the slowest connection completes within 1.5x
  the median.
- Repeat with 64 concurrent connections to surface AFD scheduling
  fairness regressions on later Windows builds.

### 6.4 Per-volume eligibility regression

- Loopback test against an NTFS volume: eligible, fast path engaged.
- Loopback test against an SMB share (CI fixture via
  `\\127.0.0.1\admin$` on Windows runners): ineligible, falls back
  to `WSASend`, no error.
- Loopback test against a BitLocker-encrypted volume (skip on CI
  unless the runner provides one).
- Eligibility cache hit rate: second use of the same volume MUST
  NOT re-probe.

### 6.5 AV-interception detection

- Mock the warmup probe to return cycles/byte > 0.7x and verify
  the fast path is disabled for the run with a `--debug=io` notice.
- Smoke test on a CI runner with Microsoft Defender enabled:
  warmup probe returns the actual ratio; no acceptance threshold
  (this is a diagnostic, not a gate).

## 7. Open questions and follow-up subtasks

The mapping decision in section 3 commits to **TransmitFile** as
the primary primitive. The questions below either need an explicit
WPG-8.x subtask or a documented "decided not to pursue" outcome
before WPG-8 closes.

| # | Question | Disposition | Subtask |
|---|---|---|---|
| Q1 | When does the asynchronous (OVERLAPPED-routed) `TransmitFile` form replace the synchronous shim in `crates/fast_io/src/iocp/transmit_file.rs`? | Required for IOCP parity; the sync form blocks a worker thread per call. | **WPG-8.a** - async TransmitFile via the existing `iocp::pump` completion-port plumbing. |
| Q2 | Does the multiplex 4-byte envelope ride via `LPTRANSMIT_FILE_BUFFERS.Head` in-kernel, or does the caller drain the buffered writer first and emit envelope + payload as two distinct calls? | In-kernel via `Head` is the design; the buffered-writer drain is the fallback. | **WPG-8.b** - wire `Head` and verify byte-parity against the buffered-writer path (see section 6.1). |
| Q3 | Is RIO worth pursuing as a second fast path, distinct from `TransmitFile`? | **No** under today's producer set (section 3); reconsider only when WPG-9 lands a Windows registered-buffer scheme that creates a producer whose source already lives in a registered slab. | **WPG-8.c (deferred)** - RIO investigation. Park behind WPG-9 acceptance criteria; do not file as an active subtask. |
| Q4 | Does the `PlatformSendFile` trait described in `windows-transmitfile-zerocopy.md` section 8 step 2 belong in `crates/fast_io/src/platform_sendfile/`, or is the existing `IocpSocketWriter::try_transmit_file_path` shape enough? | Trait is needed for cross-platform symmetry (Linux `sendfile`, macOS `sendfile`, Windows `TransmitFile`, scalar fallback all dispatch through it). | **WPG-8.d** - introduce `PlatformSendFile` trait + scalar `ReadWriteSendFile` impl + wire to `crates/transfer/src/generator/delta.rs`. |
| Q5 | What is the kernel-version equivalent of the Linux 6.0 floor? | Windows NT 4.0 for `TransmitFile`. Win8 / Server 2012 for RIO. No CI gate needed for `TransmitFile`; gate RIO when/if WPG-8.c re-opens. | Captured in section 4.2; no subtask. |
| Q6 | Does the AV-interception warmup probe (section 4.2) belong in WPG-8 or in a separate "Windows runtime gating" task? | In WPG-8; the probe is meaningless without the primitive it probes. | **WPG-8.e** - warmup probe + per-process disable. |
| Q7 | Is `TransmitPackets` ever worth revisiting? | **No.** Section 2.3 conclusion; row 4 of WPG-7.c restates it. | No subtask. |
| Q8 | Does `--zero-copy` advertise the TransmitFile path in the same way it advertises Linux SEND_ZC? | Recommended: yes, with the same default-off / opt-in posture; user-visible flag name stays portable. | **WPG-8.f** - CLI flag wiring + `--debug=io` notice surface. |

## 8. Cross-references

- WPG-7.a (opcode inventory): `docs/design/wpg-7-iouring-opcode-inventory.md`
  - `IORING_OP_SEND_ZC` SQE row: line 42.
  - Dispatch classification (feature-gated + probe): line 85.
  - Zero-copy posture summary: lines 131-133.
- WPG-7.b (IOCP mapping): `docs/design/wpg-7b-iouring-iocp-mapping.md`
  - `IORING_OP_SEND_ZC` mapping row: line 49.
  - Cross-reference to WPG-8: lines 106-112.
  - Notification-shape asymmetry note: line 49 (value + release CQE
    have no direct peer).
- WPG-7.c (gap list): `docs/design/wpg-7c-iocp-gap-list.md`
  - Row 4 (`SEND_ZC` as P3, deferred to WPG-8): line 37.
  - Severity rationale paragraph: lines 58-62.
  - Sprint recommendation (WPG-8 sequenced after WPG-9): lines 91-95.
- Companion design docs (already on master):
  - `docs/design/windows-transmitfile.md` (#2130 API survey).
  - `docs/design/windows-transmitfile-zerocopy.md` (#2130 integration
    plan with the 5-step implementation roadmap WPG-8 ratifies).
  - `docs/design/iouring-send-zc.md` (Linux SEND_ZC design; section 2
    "IORING_OP_SEND_ZC semantics" mirrored in section 1 above).
- Source surface:
  - `crates/fast_io/src/iocp/transmit_file.rs` - synchronous shim, gated
    behind `transmitfile`.
  - `crates/fast_io/src/iocp/socket.rs:284-335` - `WSASend` status quo
    path.
  - `crates/fast_io/src/iocp/socket.rs:361-401` -
    `try_transmit_file_path` with `WSASend` fallback.
  - `crates/fast_io/src/io_uring/send_zc.rs` - Linux reference
    implementation; section 1 invariants distilled from `:108-189` and
    `:269-274`.
- Memory note: `project_iouring_send_zc_optin_only.md` - default-off
  posture and `--zero-copy` advertisement-vs-behaviour mismatch the
  Windows path should not replicate.
