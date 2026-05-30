# WIN-S.2 - sendfile Windows equivalent via TransmitFile audit

Source-grounded audit of the Windows `TransmitFile` primitive as a
replacement for the Linux `sendfile(2)` zero-copy file-to-socket path.
No code changes.

Inputs:

- Cross-platform parity matrix:
  `docs/audits/cross-platform-parity-matrix.md` section 2.3 classifies
  sendfile as "L+F" (Linux-only with documented fallback). The Windows
  fallback is `io::sink()` - a no-op that discards bytes.
- Existing design docs:
  `docs/design/windows-transmitfile.md` (#2130 API survey),
  `docs/design/windows-transmitfile-zerocopy.md` (#2130 integration
  plan and 5-step implementation sequence).
- WPG-8 audit: `docs/design/wpg-8-send-zc-windows-equivalent.md`
  covers the SEND_ZC peer question and references the TransmitFile
  primitive already landed.
- Existing implementation:
  `crates/fast_io/src/iocp/transmit_file.rs` (synchronous primitive,
  feature-gated behind `transmitfile`), with integration in
  `crates/fast_io/src/iocp/socket.rs:362-401`
  (`IocpSocketWriter::try_transmit_file_path`).

## 1. Current state of the sendfile path on Windows

### 1.1 Linux/macOS: real zero-copy

On Linux, `send_file_to_fd` (`crates/fast_io/src/sendfile/mod.rs:161`)
dispatches to `try_sendfile` (`sendfile/linux.rs:46`) which calls
`libc::sendfile(out_fd, in_fd, NULL, count)`. The kernel DMAs file
pages from the page cache directly to the socket send queue. Files
below 64 KiB use a buffered `read`/`write` loop instead
(`SENDFILE_THRESHOLD`).

On macOS, `send_file_to_fd` (`sendfile/mod.rs:177`) dispatches to
`try_sendfile_macos` (`sendfile/macos.rs:57`) using Darwin's BSD-style
`sendfile(fd, s, offset, &len, hdtr, flags)`. Same zero-copy semantics,
different syscall signature.

### 1.2 Windows: no-op stub

The Windows stub (`sendfile/mod.rs:193-196`) compiles under
`#[cfg(not(unix))]` and routes to:

```rust
pub fn send_file_to_fd(source: &File, _dest_fd: i32, length: u64) -> io::Result<u64> {
    send_file_to_writer(source, &mut io::sink(), length)
}
```

This discards all bytes into `io::sink()`. The `dest_fd: i32` parameter
is meaningless on Windows (no POSIX file descriptors for sockets). The
policy-aware variant `send_file_to_fd_with_policy`
(`sendfile/mod.rs:236-243`) delegates to the same sink.

The stub is not a performance fallback - it is a compile-time
placeholder that produces zero useful work. Any caller that reaches
this path on Windows silently drops the file data.

### 1.3 Existing TransmitFile primitive

A synchronous `TransmitFile` wrapper already exists at
`crates/fast_io/src/iocp/transmit_file.rs`, gated behind
`#[cfg(all(target_os = "windows", feature = "transmitfile"))]`. It
exposes `try_transmit_file(socket: RawSocket, file: RawHandle, length:
u64) -> io::Result<usize>` and handles:

- Zero-length short-circuit (returns `Ok(0)`)
- Length exceeding `DWORD` cap (`u32::MAX`) rejection
- `ERROR_NOT_SUPPORTED` mapping to `io::ErrorKind::Unsupported`
- 64 KiB `BYTES_PER_SEND` to avoid the driver's 1-MSS cap

`IocpSocketWriter::try_transmit_file_path`
(`iocp/socket.rs:362-401`) wraps this with a `WSASend` fallback: if
`try_transmit_file` returns `Unsupported`, it reads from the file
handle into a caller-supplied buffer and sends via `send_async`.

Both are feature-gated and not wired into the `sendfile` module's
public API.

## 2. TransmitFile vs Linux sendfile comparison

| Property | Linux `sendfile(2)` | Windows `TransmitFile` |
|---|---|---|
| Source | `fd` (regular file) | `HANDLE` (regular file) |
| Sink | `fd` (any fd since 2.6.33; was socket-only) | `SOCKET` (TCP only) |
| Header/trailer iovec | No | Yes (`lpTransmitBuffers.Head/Tail`) |
| Async model | Blocking; `EAGAIN` on non-blocking fd | Synchronous (null OVERLAPPED) or async (OVERLAPPED + IOCP) |
| Short writes | Returns bytes transferred; caller loops | All-or-nothing: TRUE = full length, FALSE = error |
| Max per call | `SSIZE_MAX` (~8 EB on 64-bit) | `DWORD` (4 GiB - 1) |
| Remote/unusual FS | Works on most local and NFS | `ERROR_NOT_SUPPORTED` on SMB, DFS, encrypted volumes |
| Kernel copy path | Page-cache to socket via `splice` pipes internally | Page-cache to NIC via kernel DMA |
| Chunk tuning | N/A (kernel chooses internally) | `nNumberOfBytesPerSend` (0 = driver default, often 1 MSS) |
| Available since | Linux 2.2 (socket-only); 2.6.33 (any fd) | Windows NT 4.0 |
| Existing code | `crates/fast_io/src/sendfile/linux.rs` | `crates/fast_io/src/iocp/transmit_file.rs` |

### 2.1 Key differences affecting integration

**Handle types.** Linux uses `i32` file descriptors for both files and
sockets. Windows uses `HANDLE` for files and `SOCKET` for sockets -
different types at the ABI level. The `sendfile` module's public API
passes `dest_fd: i32`, which has no meaning on Windows. Wiring
TransmitFile into the existing `send_file_to_fd` signature would
require a type shim or a parallel entry point.

**All-or-nothing semantics.** Linux `sendfile` may return fewer bytes
than requested (short write on signal or socket buffer pressure). The
caller loops. Windows `TransmitFile` either completes fully (returns
TRUE) or fails entirely. Callers that expect short-write loops must
adapt: the loop is over 4 GiB chunks (DWORD cap), not over partial
sends.

**Header support.** TransmitFile can attach a header buffer
(`lpTransmitBuffers.Head`) that the kernel prepends to the file data
in the same syscall. This maps directly to rsync's 4-byte multiplex
`MSG_DATA` envelope header, enabling single-syscall framed sends. Linux
`sendfile` has no equivalent - the header must be a separate `write`
or `writev`.

**TCP-only sink.** TransmitFile requires a TCP socket destination.
Linux `sendfile` (since 2.6.33) works with any writable fd (pipes,
files). This is acceptable for oc-rsync because the sendfile path is
only used for daemon TCP transfers.

## 3. Gap analysis: what the stub costs

The `io::sink()` stub means the `send_file_to_fd` / `send_file_to_fd_with_policy`
functions produce no useful work on Windows. This is safe only if no
caller on Windows actually reaches these functions expecting real I/O.

Current callers:

- The public re-export `fast_io::send_file_to_fd_with_policy`
  (`lib.rs:273`) is the only `pub use` of this family.
- `crates/transfer/src/config/mod.rs:71-76` documents
  `ZeroCopyPolicy` as covering sendfile, but the transfer crate does
  not call `send_file_to_fd` directly. The daemon TCP send path uses
  `IocpSocketWriter::send_async` (plain `WSASend`) or, when the
  `transmitfile` feature is enabled,
  `IocpSocketWriter::try_transmit_file_path`.

**Verdict:** The sink stub is currently harmless because no Windows
code path calls `send_file_to_fd`. The daemon TCP path already has
its own TransmitFile integration through `IocpSocketWriter`. However,
the stub creates a maintenance trap: any future caller that
naively uses the cross-platform `send_file_to_fd` on Windows will
silently lose data.

## 4. Can TransmitFile be a drop-in replacement?

**No.** TransmitFile cannot be wired behind the existing
`send_file_to_fd(source: &File, dest_fd: i32, length: u64)` signature
because:

1. The `dest_fd: i32` parameter is a POSIX file descriptor. Windows
   sockets are `SOCKET` (`usize`/`u64`), not `i32`. A cast from
   `i32` to `SOCKET` is lossy on 64-bit Windows.
2. The synchronous `TransmitFile` blocks the calling thread. The Linux
   `sendfile` path also blocks, so this is parity, but the existing
   IOCP infrastructure expects overlapped I/O. A synchronous call
   on a thread that services the completion port would deadlock the
   pump.
3. The 4 GiB per-call cap requires a chunking loop with
   `SetFilePointerEx` between calls. Linux's loop is over short
   writes; Windows's loop is over the DWORD ceiling. The loop shapes
   differ.

### 4.1 Recommended integration path

The existing `IocpSocketWriter::try_transmit_file_path` is the correct
integration point. It already:

- Accepts `RawHandle` (file) and operates on `RawSocket` (self)
- Falls back to `WSASend` on `ERROR_NOT_SUPPORTED`
- Lives in the `fast_io` crate where unsafe code is permitted
- Is feature-gated behind `transmitfile` (which implies `iocp`)

The remaining work is not a new primitive but wiring:

1. **Enable by default.** The `transmitfile` feature is off by default.
   Enabling it (or folding it into `iocp`) makes the fast path
   available without opt-in.
2. **Wire into the daemon send loop.** The daemon TCP path that sends
   literal tokens should call `try_transmit_file_path` for large
   literal runs when compression is off. The multiplexer must drain
   its buffer first to preserve ordering.
3. **Add the 4 GiB chunking loop.** `try_transmit_file` rejects
   lengths above `u32::MAX`. The caller
   (`try_transmit_file_path`) should loop with
   `SetFilePointerEx` for files > 4 GiB. This matches the design
   doc's section 4 (`windows-transmitfile.md`).
4. **Wire `lpTransmitBuffers.Head`.** Prepend the 4-byte multiplex
   header via the TransmitFile header buffer instead of a separate
   `WSASend`. This saves one syscall per chunk and is a natural
   advantage TransmitFile has over Linux `sendfile`.
5. **Fix the sink stub.** Replace `io::sink()` with either a proper
   `TransmitFile` call (if the `transmitfile` feature is on) or a
   buffered `read`/`Write::write_all` loop against the destination
   (if the feature is off). The current sink silently drops data.

### 4.2 Stub replacement for non-transmitfile builds

Even without TransmitFile, the `#[cfg(not(unix))]` stub should not
use `io::sink()`. A correct Windows fallback for `send_file_to_fd`
would use the `send_file_to_writer` path against a `TcpStream`
wrapped from the raw socket. However, since `dest_fd: i32` is
meaningless on Windows, the cleanest fix is to mark
`send_file_to_fd` as `#[cfg(unix)]`-only and provide a separate
`send_file_to_socket(source: &File, socket: RawSocket, length: u64)`
for Windows that dispatches to TransmitFile or a `WSASend` fallback.

## 5. Performance characteristics

### 5.1 Expected gains

TransmitFile eliminates two userspace copies per literal chunk:

1. `ReadFile` (kernel page cache -> user buffer)
2. `WSASend` (user buffer -> kernel socket buffer)

Replaced by a single kernel-mode DMA from the file system cache to
the NIC's send queue.

| Scenario | Expected gain | Notes |
|---|---|---|
| `--whole-file`, 1 GiB, 10 GbE, warm cache | 30-50% wall time | Sender CPU ~22% -> ~9% (profile #2130) |
| `--whole-file`, 1 GiB, 1 GbE | 5-10% wall time | NIC-bound; CPU savings still significant |
| Delta mode, 90% match ratio | 5-15% wall time | Literal runs smaller; per-call overhead matters more |
| Files < 64 KiB | None or slight regression | Setup cost dominates; below `SENDFILE_THRESHOLD` |

### 5.2 Limitations

- **SMB/DFS/encrypted volumes:** `ERROR_NOT_SUPPORTED` forces
  fallback to `WSASend`. The volume eligibility probe
  (`GetFileInformationByHandleEx(FileRemoteProtocolInfo)`) can
  memoize per-volume results.
- **AV interception:** Some Windows antivirus products hook
  `TransmitFile` and degrade it to a buffered copy. Detectable via a
  warmup benchmark (1 MiB transmit vs 1 MiB read+write; if
  cycles/byte ratio exceeds 0.7x, disable for the process).
- **Single in-flight per socket:** No concurrent `TransmitFile` /
  `WSASend` on the same socket. The protocol multiplexer already
  serializes sends.
- **32-bit length cap:** `nNumberOfBytesToWrite` is `DWORD`. Files
  > 4 GiB require a chunking loop with `SetFilePointerEx`.

## 6. Assessment summary

| Question | Answer |
|---|---|
| Is the TransmitFile primitive already implemented? | Yes, at `iocp/transmit_file.rs` (synchronous, feature-gated) |
| Is it wired into the sendfile module? | No. The sendfile module's Windows stub uses `io::sink()` |
| Is it wired into the daemon send path? | Partially. `try_transmit_file_path` exists but is feature-gated off by default |
| Can TransmitFile replace `send_file_to_fd` directly? | No. The `i32` fd signature does not map to `SOCKET`/`HANDLE` on Windows |
| Is the existing stub harmful? | Not today (no callers reach it), but it is a data-loss trap for future callers |
| What is the right integration point? | `IocpSocketWriter::try_transmit_file_path`, already implemented |
| What remains? | Enable by default, wire into daemon send loop, add >4 GiB chunking, wire header buffers, fix the sink stub |
| Priority | Medium. The fast path exists but is off by default. The send loop on Windows uses `WSASend`, which is correct but not zero-copy. Gains are significant only on 10 GbE or faster links. |

## 7. Relationship to other WIN-S tasks

- **WIN-S.1** (stub inventory): This audit confirms that
  `sendfile/mod.rs:193-196` and `:236-243` are no-op stubs on
  Windows, and that `iocp/transmit_file.rs` is the real
  implementation behind a feature gate.
- **WIN-S.8** (priority): TransmitFile is a high-throughput-impact
  stub replacement for the daemon network send path. It ranks
  behind IOCP disk writes (already wired) but ahead of other stubs
  like `madvise` or `SEEK_DATA`/`SEEK_HOLE` which are
  metadata-only optimizations.
- **WPG-8** (SEND_ZC peer): TransmitFile is the Windows peer of both
  Linux `sendfile(2)` and `IORING_OP_SEND_ZC`. The WPG-8 audit
  (`docs/design/wpg-8-send-zc-windows-equivalent.md`) recommends
  TransmitFile as the viable Windows counterpart with the caveat
  that its notification model (all-or-nothing vs dual-CQE) differs.
