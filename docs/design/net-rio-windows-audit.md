# NET-RIO.1: Windows Registered I/O (RIO) API surface audit for daemon socket I/O

Status: AUDIT (feeds NET-RIO.2 impl / NET-RIO.3 wire / NET-RIO.4 bench).

Registered I/O (RIO) is a Winsock extension that registers user-mode buffer
pools with the kernel up front, eliminating the per-call page-pinning the
overlapped `WSARecv` / `WSASend` path pays on every operation. This document
inventories the RIO API surface required to wire RIO into oc-rsync's daemon
socket I/O, maps it against the existing IOCP socket call sites, and
recommends the migration shape for NET-RIO.2.

## 1. RIO API surface inventory

Required entry points (all under `windows-sys::Win32::Networking::WinSock`).
Citations refer to public MSDN documentation under `learn.microsoft.com/en-us/
windows/win32/api/mswsock` / `winsock2`.

| API | Purpose | MSDN ref |
| --- | --- | --- |
| `WSAIoctl(SIO_GET_MULTIPLE_EXTENSION_FUNCTION_POINTERS, &WSAID_MULTIPLE_RIO, ...)` | Resolves the RIO function table for a socket handle. Required because RIO functions are not exported by `ws2_32.dll` symbol; they are dispatched per-socket. | `winsock/sio-get-multiple-extension-function-pointers` |
| `RIORegisterBuffer(buffer, length)` | Pins a user-mode contiguous region into the kernel and returns a `RIO_BUFFERID`. One pinning per pool, amortised across every send/recv. | `winsock/nf-mswsockdef-rioregisterbuffer` |
| `RIODeregisterBuffer(bufferid)` | Releases the pinning. Caller must keep the user region alive until this returns. | `winsock/nf-mswsockdef-rioderegisterbuffer` |
| `RIOCreateCompletionQueue(QueueSize, NotificationCompletion)` | Allocates a kernel completion ring. `NotificationCompletion` can be `RIO_IOCP_COMPLETION` (deliver to an IOCP) or `RIO_EVENT_COMPLETION` (signal an event). | `winsock/nf-mswsockdef-riocreatecompletionqueue` |
| `RIOCloseCompletionQueue(cq)` | Tears down the ring. | `winsock/nf-mswsockdef-riocloseompletionqueue` |
| `RIOCreateRequestQueue(socket, maxOutstandingRecv, maxRecvDataBuffers, maxOutstandingSend, maxSendDataBuffers, recvCQ, sendCQ, key)` | Binds a socket to its per-socket request queue + the receive / send completion queues. Required before `RIOSend` / `RIOReceive`. | `winsock/nf-mswsockdef-riocreaterequestqueue` |
| `RIOSend(rq, RIO_BUF*, dataBufCount, flags, requestContext)` | Posts a send referencing a slice carved out of a registered buffer. No pinning per call. | `winsock/nf-mswsockdef-riosend` |
| `RIOReceive(rq, RIO_BUF*, dataBufCount, flags, requestContext)` | Symmetric for recv. | `winsock/nf-mswsockdef-rioreceive` |
| `RIONotify(cq)` | Arms a one-shot notification on a CQ created with `RIO_IOCP_COMPLETION` / `RIO_EVENT_COMPLETION`. Must be re-armed after every drain. | `winsock/nf-mswsockdef-rionotify` |
| `RIODequeueCompletion(cq, RIORESULT*, count)` | Drains up to `count` completions without blocking. The caller polls or arms `RIONotify` for wakeup. | `winsock/nf-mswsockdef-riodequeuecompletion` |

`RIO_BUF` layout (per MSDN `ns-mswsockdef-rio_buf`):

```
typedef struct _RIO_BUF {
    RIO_BUFFERID BufferId;
    ULONG        Offset;
    ULONG        Length;
} RIO_BUF, *PRIO_BUF;
```

Lifetime constraints:

- The `BufferId` must remain registered for the full lifetime of every
  outstanding `RIOSend` / `RIOReceive` referencing slices into it.
- The backing user-mode allocation behind the `BufferId` must remain mapped
  and aligned (the kernel page-pins it); deallocating it before
  `RIODeregisterBuffer` is UB.
- `Offset + Length` must stay within the registered region's bounds. Slices
  may overlap between concurrent operations; the kernel does not enforce
  exclusivity.

## 2. Existing IOCP socket I/O insertion-point map

Socket I/O in oc-rsync today is split between standard sync `std::net::TcpStream`
(daemon listener side) and the optional IOCP overlapped path in `fast_io`.

| Site | File:line | Direction | Pattern |
| --- | --- | --- | --- |
| `IocpSocketReader::recv_async` | `crates/fast_io/src/iocp/socket.rs:182` | recv | `WSARecv` + `OVERLAPPED` per call, completion via shared `CompletionPump` |
| `IocpSocketWriter::send_async` | `crates/fast_io/src/iocp/socket.rs:313` | send | `WSASend` + `OVERLAPPED` per call, same pump |
| `IocpSocketWriter::try_transmit_file_path` | `crates/fast_io/src/iocp/socket.rs:362` | send | `TransmitFile` fast path with `WSASend` fallback |
| `try_transmit_file` | `crates/fast_io/src/iocp/transmit_file.rs:122` | send | Zero-copy file-to-socket primitive (does not pin per call) |
| `DaemonStream::Plain(TcpStream)` | `crates/daemon/src/daemon_stream.rs:56` | send + recv | Synchronous `read` / `write` on standard `TcpStream` (no IOCP wiring) |
| RIO scaffolding | `crates/fast_io/src/iocp/rio.rs:318-326` | both | Function-table resolution + buffer pool already drafted, behind opt-in env gate `OC_RSYNC_WINDOWS_RIO` |

Migration scope for RIO is the two hot paths in `iocp/socket.rs` plus the
non-`TransmitFile` portion of `try_transmit_file_path`'s fallback. The
listener-side `DaemonStream::Plain` path stays out of scope here - it never
crosses the IOCP pump today, and RIO does not change accept semantics.

RIO does not replace `TransmitFile`. Zero-copy file-to-socket remains the
preferred large-file primitive; RIO is the optimisation for everything else
(multiplex envelopes, small-file payload, control frames).

## 3. RIO vs IOCP `WSASend` / `WSARecv` at a conceptual level

**Buffer registration cost**

- IOCP overlapped: every `WSARecv` / `WSASend` pages-in the caller's slice
  for the duration of the call. The kernel walks the page table, pins each
  page, and unpins on completion. At daemon concurrency (hundreds of
  multiplex envelopes per connection per second) this is the dominant
  per-call overhead.
- RIO: a single `RIORegisterBuffer` covers the whole pool. Per-call dispatch
  is a `RIO_BUF { BufferId, Offset, Length }` triple plus a ring-slot write.
  The kernel walks no page table on the hot path.

**Completion polling model**

- IOCP: completions land on a per-port queue; the pump dequeues via
  `GetQueuedCompletionStatusEx`. One thread per pump scales.
- RIO: completions land on a per-socket-set `RIO_CQ`. The application drains
  via `RIODequeueCompletion` (lock-free poll) and arms `RIONotify` for the
  next wakeup. The notification can be wired into an existing IOCP via
  `RIO_IOCP_COMPLETION`, so the existing `CompletionPump` can keep its
  single drain thread.
- Net effect: hybrid wiring (RIO completions notify IOCP, the pump drains
  RIO and overlapped slots from the same loop) is cheaper than running a
  parallel RIO-only event loop.

**Memory pinning implications**

- RIO requires the registered region to live for the full pool lifetime.
  Daemon worker exit must explicitly `RIODeregisterBuffer` before the pool
  allocation drops. This is non-negotiable for safe use.
- The pool sits in working set as committed pages; on Windows there is no
  copy-on-write reclaim. A 1 MiB default pool (`DEFAULT_RIO_POOL_BYTES`) is
  cheap at scale; pools above tens of MiB compete with the IOCP no-buffering
  data path for RAM and need an admission cap. The opt-in env knob already
  documents this.
- Buffer allocation must satisfy alignment requirements per MSDN
  `RIORegisterBuffer` remarks - cache line minimum, page boundary for very
  large pools.

## 4. Migration approach: hybrid

We recommend **hybrid** for NET-RIO.2 with one-shot replacement at the
`iocp/socket.rs` boundary:

1. Keep the IOCP listener (`AcceptEx` is not in the hot path; daemon accept
   throughput is dominated by handshake cost, not page-pinning).
2. Replace the `WSARecv` / `WSASend` calls with `RIOReceive` / `RIOSend`
   when `try_init_rio()` returns `Some`, falling back to the existing
   overlapped path otherwise.
3. Wire the RIO completion queue into the existing `CompletionPump` via
   `RIO_IOCP_COMPLETION` so we keep a single drain thread.
4. Carve `RIO_BUF` slots out of the `RioBufferPool` already drafted in
   `iocp/rio.rs`. The 32 KiB default slot already matches the multiplex
   envelope cap; per-connection lease and return on completion.
5. Leave `TransmitFile` exactly where it is. Large-file sends already pin
   nothing; RIO has no win on that path.

Full migration (replacing the entire `iocp/socket.rs` overlapped path)
is rejected for NET-RIO.2 because:

- The fallback overlapped path is needed for hosts without the RIO function
  table (Windows 7, container images that strip extension functions, and
  some virtualised NICs that refuse the extension query).
- Keeping both paths behind the existing `OC_RSYNC_WINDOWS_RIO=auto|on|off`
  knob means we can ship NET-RIO.2 dark, gate the bench in NET-RIO.4, and
  decide default-on in NET-RIO.5 from production evidence.

## 5. Risks

- **Windows 8+ floor.** RIO ships in Windows 8 / Server 2012 and later.
  Windows 7 SP1 cannot resolve the function table. oc-rsync's official
  Windows support tier already targets Windows 10+, so the floor is below
  our existing support contract; the runtime probe in `try_init_rio` keeps
  us safe on older or restricted hosts by returning `Ok(None)` and
  falling back to the overlapped path. Document the floor in the release
  notes when NET-RIO.5 flips default-on.
- **Pool pre-allocation cost.** Default 1 MiB is in noise; per-connection
  pools at high concurrency need to scale with the daemon `--max-connections`
  cap. Implementation should share a single global pool by default and only
  carve per-connection sub-pools when `OC_RSYNC_WINDOWS_RIO_POOL_BYTES` is
  raised.
- **Pinning under memory pressure.** Registered buffers stay in the working
  set. A daemon serving thousands of connections with a multi-MiB pool per
  connection will hit `STATUS_INSUFFICIENT_RESOURCES` from
  `RIORegisterBuffer` before any send. Mitigation: the pool factory must
  fall back to overlapped `WSASend` / `WSARecv` (already the non-RIO path)
  when registration fails, and emit a `log::warn` so deploys can resize.
- **Extension-table dispatch is per-socket.** `WSAIoctl` returns a function
  pointer table valid for the socket handle. Caching the table across
  sockets is undefined per MSDN; implementations re-resolve per accept.
  Our prototype already resolves once per process; verify in NET-RIO.2 that
  this matches real-world Winsock provider behaviour or move to per-socket
  resolution.

## 6. Recommendation for NET-RIO.2

Implementation order:

1. Lift the existing `RioBufferPool` + `RioCompletionQueue` scaffolding in
   `iocp/rio.rs` into the public path - they are already drafted but not
   wired into `iocp/socket.rs`.
2. Add `IocpSocketReader::recv_async_rio` and
   `IocpSocketWriter::send_async_rio` adjacent to the existing entry
   points; dispatch at construction time based on `RioMode`.
3. Wire `RIO_IOCP_COMPLETION` so completions reach the existing
   `CompletionPump`. Reuse `oneshot_handler` for one completion per call.
4. Add per-connection lease + return semantics on the `RegisteredBuffer`
   API so a slow consumer cannot starve the pool.
5. Gate everything behind `OC_RSYNC_WINDOWS_RIO=auto|on` with default
   `off`; NET-RIO.4 bench flips it on for the comparison; NET-RIO.5
   decides default.

The RIO scaffolding in `crates/fast_io/src/iocp/rio.rs` is the launch pad.
NET-RIO.2 wires it into the two hot paths and exposes the dispatch toggle.
