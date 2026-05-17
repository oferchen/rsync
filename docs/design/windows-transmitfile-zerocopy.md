# Windows TransmitFile zero-copy network send (#2130)

Tracking issue: oc-rsync task #2130. Design-only; no code lands in
this PR.

This document focuses on the runtime integration questions left
open by the original API survey in
`docs/design/windows-transmitfile.md`: where the user-space hop
lives today, how the rsync multiplex framing constrains a
file-to-socket primitive, the quantitative hypothesis, the
explicit recommendation with trigger conditions, and a five-step
implementation plan. The API surface, equivalence table, and
risk register are not re-litigated here; consult the companion
doc.

## 1. TransmitFile API at a glance

```c
BOOL TransmitFile(
    SOCKET                  hSocket,
    HANDLE                  hFile,
    DWORD                   nNumberOfBytesToWrite,
    DWORD                   nNumberOfBytesPerSend,
    LPOVERLAPPED            lpOverlapped,
    LPTRANSMIT_FILE_BUFFERS lpTransmitBuffers,
    DWORD                   dwFlags);
```

Three call-site choices we have to lock down up front:

- `nNumberOfBytesPerSend = 65536` (64 KiB), matching upstream
  `IO_BUFFER_SIZE`. Zero defers to the driver and silently caps
  at 1 MSS on older NICs without TSO.
- `lpOverlapped` must be non-null; the SOCKET must be created
  with `WSA_FLAG_OVERLAPPED` and associated with the same
  completion port that `crates/fast_io/src/iocp/pump.rs` services.
  Synchronous calls (`lpOverlapped = NULL`) block a worker
  thread and defeat IOCP.
- `lpTransmitBuffers` is the API hook for header + trailer
  iovecs. See section 4: the rsync multiplex header is exactly
  what `Head` was designed for, but only for whole-frame sends.
- File handle requires `FILE_FLAG_SEQUENTIAL_SCAN` for the
  Windows cache manager's read-ahead heuristic to fire; without
  it the file pages are fetched at request granularity and the
  win shrinks to noise on cold cache.

## 2. Comparison with Linux sendfile(2)

| Property | Linux `sendfile(2)` | Windows `TransmitFile` |
|---|---|---|
| Source | `fd` (regular file, since 2.6.33) | `HANDLE` (regular file) |
| Sink | `fd` (any, since 2.6.33; was socket only) | `SOCKET` (TCP only) |
| Header / trailer iovec | No (caller must `writev` first) | Yes (`lpTransmitBuffers.Head/Tail`) |
| Async model | Blocking + `EAGAIN` on non-blocking | OVERLAPPED + IOCP completion |
| Short writes | Returns transferred count; caller loops | All-or-nothing; partial = error |
| Max single call | `SSIZE_MAX` | 32-bit length (`DWORD`) |
| Remote / unusual FS | Works on most local FS | Returns `ERROR_NOT_SUPPORTED` on
  SMB/DFS shares, some encrypted volumes |
| Counterpart in tree | `crates/fast_io/src/sendfile.rs:205`
  (`send_file_to_fd_with_policy`) | absent (this design) |

Both primitives push the DMA from page cache directly to the NIC
ring; the user-space mapping is never touched. TransmitFile's
edge is the iovec support, which lets the rsync multiplex header
ride on the same kernel call as the payload.

## 3. The user-space hop being avoided

The Windows sender, when serialising literal-data tokens during
delta or whole-file transfers, reads file bytes into a heap
buffer and then writes that buffer through the multiplex writer
to the TCP socket:

- `crates/transfer/src/generator/delta.rs:245-263`. The sender
  resizes a reusable scratch buffer to `4 + read_size`, calls
  `source.read_exact(&mut buf[4..4 + to_read])`, encodes the
  4-byte length prefix in place, and finally
  `writer.write_all(&buf[wire_off..wire_off + 4 + chunk])`.
- `crates/protocol/src/wire/delta/token.rs:42-52`
  (`write_token_literal`) is the shared chunking helper for
  callers that pass an in-memory slice.
- `crates/protocol/src/multiplex/writer.rs:179-195`
  (`flush_buffer`) wraps the bytes in `MSG_DATA` frames via
  `send_msg`, which is `send.rs:16-22`.
- `crates/fast_io/src/iocp/socket.rs:284-335`
  (`IocpSocketWriter::send_async`) issues `WSASend` with a
  single `WSABUF` pointing at the user-space buffer.

That path performs two avoidable copies on Windows: kernel page
cache -> user buffer (in `read_exact`), and user buffer -> kernel
socket buffer (in `WSASend`). TransmitFile collapses both into a
single kernel-mode DMA from the file system cache straight to the
socket's send queue.

## 4. Multiplex framing constraint

Rsync multiplexes `MSG_DATA`, `MSG_INFO`, `MSG_ERROR`,
`MSG_WARNING`, and friends on the same TCP stream. Every payload
chunk carries a 4-byte envelope header (`MPLEX_BASE + tag` in the
high byte, 24-bit length in the low bytes), produced by
`crates/protocol/src/multiplex/io/send.rs:16-22`. The 24-bit
length cap is `MAX_PAYLOAD_LENGTH = 0x00FF_FFFF`
(`crates/protocol/src/envelope/constants.rs:5`).

That has two consequences for TransmitFile:

1. **Header attachment.** The 4-byte envelope must precede each
   payload chunk. `lpTransmitBuffers.Head` is exactly this:
   `TRANSMIT_FILE_BUFFERS { Head, HeadLength, Tail, TailLength }`
   lets the kernel prepend the header in the same syscall. We
   build the header in a 4-byte stack buffer, point `Head` at it,
   and let the kernel emit `[header | file-range]` atomically.
2. **Chunk granularity.** A single `TransmitFile` call covers at
   most `MAX_PAYLOAD_LENGTH = 16 MiB - 1` of payload before a new
   header is needed. We will cap each call at 16 MiB and loop;
   the cache manager keeps the file resident across calls.

The non-DATA tags (`MSG_INFO`, `MSG_ERROR`, etc.) are still
issued through the existing buffered writer because they are
small, infrequent, and carry no file payload. `MplexWriter::
write_message` (`multiplex/writer.rs:226-232`) already flushes
buffered DATA first to preserve ordering, so a TransmitFile-based
fast path for literal runs slots in cleanly: drain the buffered
writer, then hand the literal range to TransmitFile, then resume
buffered output for the next token.

In-band compression (`-z`) bypasses this fast path entirely.
Compressed token streams (`crates/transfer/src/transfer_ops/
token_loop.rs:133`) go through the codec, so the source bytes do
not reach the wire literally. TransmitFile applies only when the
codec is `None` and the token is a `Literal` run from the source
file.

## 5. Performance hypothesis

Working numbers (to be validated, not promised):

| Scenario | Expected gain |
|---|---|
| `--whole-file`, 1 GiB file, 10 GbE, warm cache | 30-50% wall time
  reduction; sender CPU ~22% -> ~9% |
| `--whole-file`, 1 GiB file, 1 GbE | 5-10% wall time
  (NIC-bound); sender CPU 18% -> 6% |
| Delta mode, 1 GiB file, 90% match | 5-15% wall time (literal
  runs are smaller; per-call setup overhead matters more) |
| < 64 KiB file | None or slight regression (setup dominates;
  fast path disabled below this threshold) |

Justification: profiles in #2130 show 22% sender CPU on Windows
Server 2022 spent in `memcpy` between the `ReadFile` destination
buffer and the `WSASend` source buffer for a 1 GiB whole-file
push over 10 GbE. Eliminating both copies frees roughly that
fraction of CPU, lifting the bottleneck to the NIC and shrinking
wall time proportionally on NIC-headroom links. On a saturated
1 GbE link the wall-time win is modest because the NIC is already
the bottleneck, but CPU headroom matters for concurrent transfers
and for laptops on battery.

Validation gate: a 30% wall-time win on the 10 GbE / 1 GiB /
warm-cache reference workload is the trigger to ship the feature
by default; below that we ship gated behind `--io-policy=
transmitfile=on` until further profiling.

## 6. Failure modes and fallback

| Failure | Detection | Action |
|---|---|---|
| Source on SMB / DFS / encrypted volume | `TransmitFile` returns
  FALSE with `ERROR_NOT_SUPPORTED`; probe up front via
  `GetFileInformationByHandleEx(FileRemoteProtocolInfo)` | Mark
  the volume as not-eligible (per-volume LRU cache), fall back to
  read+write |
| Socket not overlapped | Detect at socket-construction time;
  newtype `OverlappedSocket` forbids construction from a
  non-overlapped handle | Compile-time refusal |
| AV interception degrading the call | Warmup benchmark on first
  use: 1 MiB transmit vs 1 MiB read+write; if cycles/byte ratio
  exceeds 0.7x, disable for the process | Emit
  `--debug=io` notice and fall back |
| File > 4 GiB | `nNumberOfBytesToWrite` is `DWORD` | Loop with
  `SetFilePointerEx` and a new OVERLAPPED.Offset per call |
| Concurrent send on same socket | Multiplex writer already
  serialises sends; debug-assert in the wrapper | Refuse second
  in-flight call |
| Send buffer pressure / partial completion | Completion port
  reports transferred bytes; mismatch with requested length is
  treated as `io::ErrorKind::WriteZero` | Surface as transfer
  error; rsync session retries the file in phase 2 |

Whenever the fast path refuses, the existing read+write loop in
`generator/delta.rs:245-263` is the unconditional fallback. No
session-fatal errors are introduced.

## 7. Recommendation

**Defer.** Implementation is recommended once any of the
following triggers fires; until then the engineering cost
outweighs the gain.

Trigger conditions (any one suffices):

1. A reproducible profile shows sender CPU > 20% in `memcpy` on
   Windows for a representative workload (10 GbE LAN push,
   1 GiB+ file, warm cache).
2. A user report shows sustained throughput on Windows lagging
   Linux by more than 25% on the same NIC with the same
   workload.
3. The IOCP-disk-batch effort (#1897, #1898, #1929, #1930)
   lands and stabilises, removing the prerequisite that the
   completion port is the only hot Windows infrastructure under
   churn.

Rationale for deferring rather than rejecting:

- The win is real and quantified, but it is concentrated on the
  10 GbE / whole-file / warm-cache slice. Most Windows users in
  scope today run over 1 GbE or slower WAN links where the NIC
  caps throughput before user-space memcpy does.
- The IOCP completion-port plumbing is still hardening (per the
  open #1929/#1930 tasks). Adding a second IOCP-bound primitive
  while the first is unstable doubles the surface area.
- The fallback path is the existing code, so deferring loses
  nothing for users who do not hit the trigger workloads.

Reject case: if the IOCP path is eventually retired in favour of
a different async model on Windows, TransmitFile should be
re-evaluated; it is tightly coupled to overlapped sockets and
would not survive a pivot to, e.g., Registered I/O (RIO) without
redesign.

## 8. Five-step implementation plan

If the trigger fires, implement in this order. Each step is an
independent PR.

1. **Newtype + factory.** Add `OverlappedSocket(SOCKET)` in
   `crates/fast_io/src/iocp/socket.rs` with the sole constructor
   asserting `WSA_FLAG_OVERLAPPED` and completion-port
   association. Add `SequentialFile(HANDLE)` for the source
   side, opened with `FILE_FLAG_SEQUENTIAL_SCAN`. Tests:
   construction refuses non-overlapped sockets and non-sequential
   files; round-trip a small file via existing `send_async` to
   prove the newtypes do not regress the slow path.
2. **`PlatformSendFile` trait + scalar impl.** Introduce
   `crates/fast_io/src/platform_sendfile/{mod,types,
   read_write}.rs` with the trait shape sketched in
   `docs/design/windows-transmitfile.md` section 5. Provide only
   the `ReadWriteSendFile` portable impl in this step. Wire
   `crates/transfer/src/generator/delta.rs:245-263` to call the
   trait instead of `writer.write_all`. No behaviour change;
   this step exists to land the abstraction without coupling to
   a platform call.
3. **`WindowsTransmitFile` impl.** Add the
   `cfg(target_os = "windows")` impl using the `windows` crate
   binding `windows::Win32::Networking::WinSock::TransmitFile`.
   Wire `lpTransmitBuffers.Head` to the 4-byte multiplex header.
   Cap each call at `MAX_PAYLOAD_LENGTH`; loop with
   `OVERLAPPED.Offset` for files > 4 GiB. Tests under
   `crates/fast_io/tests/`: loopback TCP transmit of 1 KiB,
   64 KiB, 1 MiB, 16 MiB - 1, and 5 GiB synthetic files; verify
   byte-for-byte equality at the receiver.
4. **Policy + eligibility probe.** Add
   `TransmitFilePolicy { Auto, Enabled, Disabled }` to
   `crates/fast_io/src/policy.rs`. `Auto` probes
   `GetFileInformationByHandleEx(FileRemoteProtocolInfo)` and
   the AV warmup benchmark on first use per source volume,
   memoising results in a `OnceLock<DashMap<VolumeId,
   Eligibility>>`. Expose `--io-policy=transmitfile={auto|on|
   off}` in `crates/cli/src/`. Tests: SMB share probe returns
   ineligible; local NTFS returns eligible; AV simulation flips
   eligibility.
5. **Default-on rollout.** Flip `TransmitFilePolicy::Auto` to
   `Enabled` for whole-file pushes over local NTFS / ReFS when
   the reference workload meets the 30% wall-time gate from
   section 5. Add a benchmark line to
   `scripts/benchmark.sh` and a CI matrix entry analogous to
   `docs/design/iocp-ci-matrix-entry.md`. Documentation update:
   note the new policy flag in the README I/O section.

## 9. Out of scope

- Linux `sendfile(2)` already exists in
  `crates/fast_io/src/sendfile.rs:205`. Hoisting it under the
  new `PlatformSendFile` trait is part of step 2; tuning it is
  not.
- macOS `sendfile(2)` with iovec trailers is a natural sibling
  but is tracked separately. The trait shape leaves room for it.
- Compressed token streams (`-z`, `--zc`) bypass the fast path;
  optimising the compressed sender is a different problem.
- Receiver-side `splice(2)` for the disk write hop is tracked in
  `docs/design/splice-vmsplice-zero-copy.md`.
