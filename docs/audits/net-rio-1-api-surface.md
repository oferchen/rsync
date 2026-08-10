# NET-RIO.1 - Windows Registered I/O API surface audit

Status: scaffolding shipped under PR #5821 on branch `feat/windows-rio-socket-io`.
This document audits the primitive surface, identifies the daemon socket I/O
call sites that NET-RIO.3 would migrate, defines the gap matrix between the two,
specifies a minimum-viable migration target, and enumerates the risks.

The RIO primitives live at `crates/fast_io/src/iocp/rio.rs` (980 lines) with a
cross-platform stub at `crates/fast_io/src/iocp_stub/rio.rs` (225 lines).
Re-exported from `crates/fast_io/src/iocp/mod.rs:65-70` so downstream crates can
name the public types behind a single `iocp::` path.

## 1. PR #5821 API surface

| Item | Kind | Signature (abbrev) | Line | Semantics |
|------|------|--------------------|------|-----------|
| `DEFAULT_RIO_POOL_BYTES` | const usize | `= 1 MiB` | 70 | Default registered block size. Sized to cover multiplex working set under non-paged-pool budget. |
| `DEFAULT_RIO_SLOT_BYTES` | const usize | `= 32 KiB` | 75 | Per-slot carve size. One `RegisteredBuffer` per slot, one `RIO_BUF` per RIO submission. |
| `RIO_ENV_VAR` | const &str | `"OC_RSYNC_WINDOWS_RIO"` | 111 | Env-var name controlling runtime mode. |
| `RioMode` | enum | `Off / Auto / On` | 91 | Three-state opt-in knob. Default `Off`. `Auto` falls back to IOCP, `On` errors when RIO unavailable. |
| `rio_enabled_from_env()` | fn | `() -> RioMode` | 129 | Reads `OC_RSYNC_WINDOWS_RIO`, case-insensitive parse. |
| `parse_rio_env()` | fn (doc-hidden) | `(Option<&str>) -> RioMode` | 137 | Pure parser for unit-testing without mutating env. |
| `RioFunctions` | struct (Copy) | 9 non-null function pointers | 154 | Resolved RIO extension function table. `is_available()` is always true on resolved instance. |
| `try_init_rio()` | fn | `() -> io::Result<Option<RioFunctions>>` | 246 | Probes via `WSASocketW` + `WSAIoctl(SIO_GET_MULTIPLE_EXTENSION_FUNCTION_POINTER, WSAID_MULTIPLE_RIO)`. Returns `Ok(None)` on `WSAEINVAL` (10022) / `WSAEOPNOTSUPP` (10045); `Err` on other failures. Closes probe socket before return. |
| `RioBufferPool` | struct | block + free-slot Mutex | 377 | Pre-registered buffer block carved into fixed slots. Send+Sync via raw-ptr ownership pattern documented at line 397. |
| `RioBufferPool::new()` | fn | `(&RioFunctions) -> io::Result<Self>` | 420 | Defaults: 1 MiB pool / 32 KiB slot. |
| `RioBufferPool::with_capacity()` | fn | `(&RioFunctions, total, slot) -> io::Result<Self>` | 435 | Validates non-zero, slot <= total. Errors mapped from `RIORegisterBuffer` failure; `RIO_INVALID_BUFFERID == -1` (line 82) detected. |
| `RioBufferPool::slot_size / slot_count / available_slots` | accessors | `-> u32 / u32 / usize` | 508-526 | Capacity introspection. |
| `RioBufferPool::acquire()` | fn | `(&self) -> Option<RegisteredBuffer>` | 535 | Pops a slot off free list. Returns None on exhaustion. |
| `RegisteredBuffer` | struct | owns one slot via Arc<inner> | 584 | RAII handle. Drop returns slot to free list. |
| `RegisteredBuffer::as_rio_buf / as_rio_buf_with_len` | fn | `-> RIO_BUF` | 597, 613 | Descriptor for `RIOSend` / `RIOReceive`. |
| `RegisteredBuffer::as_slice / as_mut_slice` | fn | `&[u8] / &mut [u8]` | 628, 652 | Safe accessors into the registered slot. |
| `RioCompletionQueue` | struct | wraps `RIO_CQ` + close + dequeue fn ptrs | 680 | Send+Sync. Drop calls `RIOCloseCompletionQueue` when handle != `RIO_INVALID_CQ` (-1). |
| `RioCompletionQueue::new()` | fn | `(&RioFunctions, depth: u32) -> io::Result<Self>` | 706 | Poll-only completion (null `RIO_NOTIFICATION_COMPLETION`). Errors: `InvalidInput` on depth=0, `WSAENOBUFS` from kernel when depth > queue quota. |
| `RioCompletionQueue::dequeue()` | fn | `(&self, &mut [RIORESULT]) -> usize` | 739 | Drains up to slice length. Returns 0 when empty. |
| `RioCompletionQueue::raw()` | fn | `-> RIO_CQ` | 757 | Borrowed handle for `rio_create_request_queue`. |
| `rio_send()` | fn | `(&RioFunctions, RIO_RQ, &RIO_BUF, flags, ctx) -> io::Result<()>` | 786 | Submits a single-buffer `RIOSend`. `flags=0` for commit-and-notify, `RIO_MSG_DEFER` for batched submit. Errors: `last_os_error()` when `RIOSend` returns FALSE. |
| `rio_recv()` | fn | same signature | 822 | Submits `RIOReceive`. Kernel writes into the registered slot on completion. |
| `rio_create_request_queue()` | fn | `(&RioFunctions, RawSocket, max_recv, max_send, &recv_cq, &send_cq) -> io::Result<RIO_RQ>` | 860 | Per-socket queue. Common error: socket created without `WSA_FLAG_REGISTERED_IO`. |
| `rio_notify()` | fn (later in file) | `(&RioFunctions, &RioCompletionQueue) -> io::Result<()>` | re-exported at mod.rs:69 | Arms the sleep-wake notify path. |

Error codes the wrappers fan out:

- `WSAEINVAL` / `WSAEOPNOTSUPP` -> `try_init_rio` returns `Ok(None)`. Anything
  else -> `Err`.
- `RIO_INVALID_BUFFERID` (`-1`) -> `RioBufferPool::with_capacity` reclaims the
  raw allocation and returns `last_os_error()`.
- `RIO_INVALID_CQ` (`-1`) -> `RioCompletionQueue::new` returns
  `last_os_error()`; Drop skips close on sentinel.
- `RIOSend` / `RIOReceive` FALSE return -> `last_os_error()`.

Unsafe code is fully contained in this module per the workspace unsafe-code
policy; consumer crates depend only on safe public APIs.

## 2. Daemon socket I/O call sites

Inventory of daemon TCP read/write/accept surfaces under `crates/daemon/src/`.

| Site | File:Line | Role | Hot path? | Wrapped by |
|------|-----------|------|-----------|------------|
| Sync listener bind / backlog config | `daemon/sections/server_runtime/listener.rs:126-144` | One-shot startup | No | `std::net::TcpListener` |
| Sync accept loop / dispatcher | `daemon/sections/server_runtime/accept_loop.rs:151-339` | Control path, one accept per connection | No (cold path) | `std::net::TcpListener::accept` + MPSC fan-in |
| Sync per-connection worker (read/write) | `daemon/sections/server_runtime/connection.rs:125-260` | Every accepted client; full multiplex transfer | **Yes** | `DaemonStream::Plain(TcpStream)` |
| Sync `DaemonStream` Read/Write impls | `daemon_stream.rs:188-218` | Every byte read/written on a session | **Yes** | `impl Read / Write` over `TcpStream` or `rustls::StreamOwned` |
| Sync `DaemonStream` read/write timeout setters | `daemon_stream.rs:88-110` | One-shot per connection | No | `TcpStream::set_read_timeout / set_write_timeout` |
| Sync `DaemonStream::tcp_stream()` accessor | `daemon_stream.rs:140-167` | Used for socket-options + shutdown | No | Direct borrow of `&TcpStream` |
| Async tokio listener | `daemon/async_session/listener.rs:111-184` | Optional async daemon path | **Yes (when enabled)** | `tokio::net::TcpListener` |
| Async tokio per-session stream | `daemon/async_session/session.rs:29-263` | Async multiplex I/O via `BufReader<TcpStream>` / `BufWriter<TcpStream>` | **Yes (when enabled)** | `tokio::net::TcpStream` |
| Module-access transfer relay | `daemon/sections/module_access/transfer.rs` | Connection passes through `DaemonStream` | Inherits | Indirect via `DaemonStream` |
| Negotiation error handler | `daemon/sections/session_runtime.rs` | Per-session error write | No | `DaemonStream::write_all` |

Cold (control / one-shot) sites are unaffected by RIO and stay on the standard
Winsock path. The hot-path sites for NET-RIO are the three rows in bold:
sync per-connection worker (through `DaemonStream::Plain`), the `DaemonStream`
`Read`/`Write` impls (the only per-byte choke point on the sync path), and the
async tokio per-session stream (a separate code path requiring its own
adapter).

## 3. Gap matrix - what NET-RIO.3 must add per hot-path site

| Concern | Sync `DaemonStream::Plain` path | Async `tokio::net::TcpStream` path |
|---------|---------------------------------|-------------------------------------|
| Socket creation | `std::net::TcpListener::accept` returns a `TcpStream` whose underlying SOCKET was created without `WSA_FLAG_REGISTERED_IO`. NET-RIO.3 must intercept `accept` and rebuild the socket with the flag (via `WSASocketW`) before handing it to `DaemonStream::plain`. Alternative: opt out from `TcpListener` and accept directly via `WSAAccept`. | `tokio::net::TcpStream` wraps a `mio`-managed socket. Either replicate the same `WSA_FLAG_REGISTERED_IO` recreation or fall through unchanged on the async path for v1. |
| Registered buffer source | `DaemonStream` currently writes/reads through caller-provided `&[u8]` / `&mut [u8]`. RIO requires the data to live in a registered slot. Two design options: (a) the multiplex layer allocates bytes directly into a `RegisteredBuffer` (deep change); (b) `DaemonStream::Plain` copies caller bytes into / out of a pooled `RegisteredBuffer` on each I/O (one extra memcpy per direction, simpler). MVP picks (b). | Same options; tokio path likely defers to v2. |
| Buffer pool ownership | One `RioBufferPool` per process is the natural granularity (default 1 MiB / 32 KiB slot = 32 slots). At 32 KiB / slot the pool exhausts at ~32 concurrent inflight ops; needs `with_capacity(rio, N * 64 KiB, 32 KiB)` sizing tied to expected concurrency. Add to `OcRsyncDaemonConfig`. | Share the per-process pool. |
| Completion polling | Need a thread (or shared completion-pump worker) that calls `RioCompletionQueue::dequeue` in a loop and matches `RIORESULT::RequestContext` back to the in-flight slot. `request_context: usize` already round-trips a token. Use one shared CQ for many sockets per upstream guidance (line 853). | Async pump can be `tokio::task::spawn_blocking` around `dequeue` + `RIONotify`; native async awaits the IOCP-backed notify if NET-RIO.3 switches to `RIO_IOCP_COMPLETION` notification. |
| Request queue per socket | `rio_create_request_queue(rio, raw_socket, max_recv, max_send, &recv_cq, &send_cq)` once per connection. Lifetime tied to the socket; close socket -> RQ implicitly released. Store the `RIO_RQ` alongside the slot in a new `RioConnection` wrapper. | Same. |
| Error mapping | `RIO_INVALID_BUFFERID` -> `io::Error::last_os_error()` (already shipped). `WSAENOBUFS` from `acquire` exhaustion needs a typed daemon error so the accept loop can backpressure instead of dropping bytes. `RIOSend` FALSE -> propagate `last_os_error()` up the multiplex stack mirroring current `DaemonStream::Plain` semantics. | Same mapping. |
| Feature gate | New `daemon-rio` Cargo feature on the `daemon` crate. Gates the `RioConnection` wrapper + the accept-loop rebuild branch. `OC_RSYNC_WINDOWS_RIO=auto` chooses RIO at runtime; the feature flag chooses whether the code is compiled at all. NET-RIO.5 flips `auto` to default but leaves the feature on by default in stable builds. | Same `daemon-rio` feature. |
| Fallback | Cold-path: any RIO failure during `try_init_rio` or `acquire` -> log + reuse existing `DaemonStream::Plain` path on a per-connection basis. The dispatcher decides at accept time, not mid-transfer. | Same. |

## 4. Minimum-viable migration spec

NET-RIO.3 should target a single hot-path site to validate the bench delta
before broad rollout. Recommended order:

1. **Sync `DaemonStream::Plain` write side (`impl Write for DaemonStream`)** -
   the simplest closed loop. Add a `RioConnection` variant alongside
   `Plain`/`Tls`, intercept `write` to copy caller bytes into a `RegisteredBuffer`,
   submit `rio_send`, drain the completion queue with a short timeout, return
   bytes written. No changes to the multiplex layer. Read side stays on the
   existing `Plain` path during NET-RIO.3 so the bench can attribute the win.
2. **Sync `DaemonStream::Plain` read side** - mirror the write side after the
   .1 bench numbers land. Pre-post `rio_recv` calls per slot; the read returns
   bytes from the next completed slot. Adds ordering carefully so multiplex
   framing stays intact.

Defer the async tokio path entirely until NET-RIO.4 confirms RIO wins on the
sync path. The async path already gets some of the same benefit from
tokio's IOCP-backed pump; the marginal RIO delta may not justify the wrapper
duplication.

## 5. Risk list and mitigations

| Risk | Mitigation |
|------|------------|
| Windows version floor: RIO ships in Windows 8 / Server 2012 (per MSDN `RIORegisterBuffer`). Older NT 6.1 hosts hit `WSAEINVAL` from `try_init_rio` and fall back to IOCP. | Already handled by `Ok(None)` return on `WSAEINVAL` / `WSAEOPNOTSUPP`. Document the Windows 8 floor in the user-facing RIO docs alongside NET-RIO.5. |
| WSL2 Winsock layer does not expose RIO. Daemon running inside WSL2 will see `Ok(None)` on probe. | Same `Ok(None)` fallback path. Add a one-shot `log::info` line at startup so WSL2 operators know why RIO is disabled. |
| ARM64 Windows: RIO is documented as available, but the Winsock catalog on Insider images has historically been incomplete. Risk is `try_init_rio` returns `Ok(None)` for legitimate hosts. | Add a `--windows-rio=status` diagnostic CLI to mirror `--io-uring=status` so operators can verify the runtime decision without enabling. Track in a follow-up NET-RIO.6 task; not a blocker. |
| Non-paged pool exhaustion: `RIORegisterBuffer` pins memory in non-paged pool. A 1 MiB pool is safe on any modern workstation; an operator-tunable pool could exhaust the pool under aggressive sizing. | `with_capacity` already validates `total_bytes` / `slot_bytes`. Cap the upper bound at a documented value (suggest 64 MiB) in the daemon config layer when NET-RIO.3 wires the config knob. |
| Buffer pool exhaustion under load: 32 slots at default sizing is too few for hundreds of concurrent transfers. | Daemon config knob sized to expected `max_connections * outstanding_per_conn`. Surface `RioBufferPool::available_slots` as a metric; alert when it stays at 0 for > N seconds. |
| `WSA_FLAG_REGISTERED_IO` requirement is silent: omitting the flag returns `rio_create_request_queue` failure rather than an obvious symptom. | NET-RIO.3 must rebuild every accepted socket via `WSASocketW` with the flag and explicitly assert success. Adding an integration test that asserts a non-RIO socket produces the expected typed error is cheap insurance. |
| Completion ordering across the shared CQ: with one CQ feeding many sockets, dequeue order is FIFO across sockets, not per-socket. The multiplex layer assumes per-connection serial order. | Use per-connection request-context tagging (already supported via `request_context: usize`) to route completions back to the owning socket's submission queue. MVP can use one CQ per socket at the cost of more handles; production tuning can move to shared CQs once the routing is exercised. |
| Send/recv mixing on shared completion queues: `RIOSend` and `RIOReceive` completions land on the same CQ unless separated. | The wrapper already lets callers pass `recv_cq` and `send_cq` separately (`rio_create_request_queue`). MVP should use distinct CQs to keep the dispatcher trivial. |

## 6. References

- MSDN: `WSAIoctl` with `SIO_GET_MULTIPLE_EXTENSION_FUNCTION_POINTER` and
  `WSAID_MULTIPLE_RIO` (extension function table resolution).
- MSDN: `RIORegisterBuffer`, `RIODeregisterBuffer`, `RIOSend`, `RIOReceive`,
  `RIOCreateCompletionQueue`, `RIOCloseCompletionQueue`,
  `RIOCreateRequestQueue`, `RIODequeueCompletion`, `RIONotify` (extension
  function table contract used by `RioFunctions`).
- MSDN: `WSASocketW` flag `WSA_FLAG_REGISTERED_IO` (required on every RIO
  socket; failure surfaces only at `rio_create_request_queue` time).
- MSDN: `RIORESULT` and `RIO_NOTIFICATION_COMPLETION` (passing null selects
  poll-only mode, used by `RioCompletionQueue::new` at line 722).
- Upstream rsync does **not** use RIO. Cygwin's Winsock shim does not expose
  the extension. This is an oc-rsync-specific Windows optimisation; the wire
  bytes are identical to the IOCP `WSARecv` / `WSASend` path.
