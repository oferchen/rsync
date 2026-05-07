# Windows TransmitFile zero-copy network send (#2130)

Tracking issue: oc-rsync task #2130. Design-only; no code lands in
this PR.

Related: `crates/fast_io/src/sendfile.rs:176`
(`send_file_to_fd_with_policy`, the Linux template),
`crates/fast_io/src/splice.rs` (receiver-side `splice(2)`),
`crates/fast_io/src/platform_copy/types.rs:122` (`PlatformCopy`
trait template), `crates/fast_io/src/lib.rs:437,560` (`IocpPolicy`,
`ZeroCopyPolicy`), `crates/fast_io/src/iocp/{completion_port,
socket}.rs` (overlapped socket + completion port plumbing).

## 1. Equivalents across platforms

| OS | Primitive | Header / DLL |
|---|---|---|
| Linux | `sendfile(2)` (file -> socket), `splice(2)` (any -> pipe) | `<sys/sendfile.h>` |
| macOS | `sendfile(2)` (file -> socket; trailer iovec) | `<sys/socket.h>` |
| Windows | `TransmitFile` (HANDLE -> SOCKET) | Winsock2, `mswsock.dll` |

`TransmitFile` has been the canonical Windows zero-copy file-send
since NT 4.0 and is the only Win32 API that hands a file handle
directly to the TCP stack; `WSASend` over a buffer always copies.

## 2. Use case in oc-rsync

The sender, when transmitting literal-data tokens (non-matched
runs in delta mode, entire files in `--whole-file`), reads file
regions and writes to the remote socket. Today on Windows that is
`File::read_exact` into a `Vec<u8>` followed by
`TcpStream::write_all`: two userspace copies plus chunk-buffer
allocation churn. `TransmitFile` collapses this to one kernel-mode
DMA from the filesystem cache to the NIC. Win is largest on
`--whole-file` over 10 GbE, where the userspace copy dominates
(~22% sender CPU on Windows Server 2022, profile in #2130).

## 3. API

```c
BOOL TransmitFile(SOCKET hSocket, HANDLE hFile,
                  DWORD nNumberOfBytesToWrite,
                  DWORD nNumberOfBytesPerSend,
                  LPOVERLAPPED lpOverlapped,
                  LPTRANSMIT_FILE_BUFFERS lpTransmitBuffers,
                  DWORD dwFlags);
```

`nNumberOfBytesToWrite = 0` sends the entire file from the current
pointer. `nNumberOfBytesPerSend` tunes per-send chunk (0 = driver
default, often 1 MSS on legacy NICs; we pass 64 KiB to match
upstream `IO_BUFFER_SIZE`). `lpTransmitBuffers = NULL` because the
protocol header is written separately. `dwFlags` accepts
`TF_USE_KERNEL_APC | TF_WRITE_BEHIND` for batched throughput.

Bindings via the `windows` crate
(<https://github.com/microsoft/windows-rs>):
`windows::Win32::Networking::WinSock::{TransmitFile,
TRANSMIT_FILE_BUFFERS, WSA_FLAG_OVERLAPPED}`. No raw FFI; the
`windows` crate is already a dependency for ACLs and console
control, and the unsafe-policy keeps native calls inside `fast_io`.

## 4. Constraints

- **Local FS only.** `TransmitFile` returns `ERROR_NOT_SUPPORTED`
  on remote shares (SMB/DFS) and on cluster-mismatched compressed
  or encrypted volumes. Probe via `GetFileInformationByHandleEx
  (FileRemoteProtocolInfo)` plus `GetVolumeInformationByHandleW`.
- **Overlapped sockets.** SOCKET must carry `WSA_FLAG_OVERLAPPED`
  and be bound to a completion port; otherwise `TransmitFile`
  blocks (defeating IOCP) or returns `WSAENOTSOCK`. The existing
  `iocp::socket` constructor already produces overlapped sockets
  for the daemon listener; the sender path inherits it.
- **Single in-flight per socket.** No concurrent `TransmitFile` /
  `WSASend` on the same socket. The protocol multiplexer already
  serialises sends, so this matches naturally.
- **32-bit length.** `nNumberOfBytesToWrite` is `DWORD`; files >
  4 GiB loop with `SetFilePointerEx` between calls.

## 5. Integration

Add a new `PlatformSendFile` trait in
`crates/fast_io/src/platform_sendfile/types.rs` (do NOT overload
`PlatformCopy`; that trait covers file-to-file, mixing
file-to-socket muddies the abstraction):

```rust
pub trait PlatformSendFile: fmt::Debug + Send + Sync {
    fn send_file_to_socket(
        &self, source: &File, socket: RawSocket,
        offset: u64, length: u64, policy: ZeroCopyPolicy,
    ) -> io::Result<u64>;
    fn method(&self) -> SendFileMethod;
}
```

Impls: `LinuxSendfile` (wraps `sendfile.rs`), `MacOsSendfile` (BSD
`sendfile(2)` with iovec trailer), `WindowsTransmitFile` (new), and
`ReadWriteSendFile` portable fallback. `DefaultPlatformSendFile`
auto-selects per `cfg(target_os)`. A `TransmitFilePolicy` enum
mirrors `IocpPolicy` (`Auto | Enabled | Disabled`); `Auto` resolves
to `Enabled` when the source volume is local + uncompressed +
unencrypted, the SOCKET is overlapped + completion-port-bound, and
the file size exceeds 64 KiB. Otherwise sender falls through to
the existing read+write loop. `TransmitFile` posts an `OVERLAPPED`
to the same completion port `IocpDiskBatch` polls; the existing
`crates/fast_io/src/iocp/completion_port.rs` routes multi-source
completions, so no new lifecycle.

## 6. Risks

- **Overlapped-socket invariant.** A blocking SOCKET handed to
  `TransmitFile` fails at runtime. Mitigation: trait accepts a
  typed `OverlappedSocket` newtype, not raw `SOCKET`; the only
  constructor passes `WSA_FLAG_OVERLAPPED`.
- **Partial-completion semantics.** Linux `sendfile` returns
  bytes-transferred and a short return is normal on signal
  delivery. `TransmitFile` either completes fully (TRUE), fails
  (FALSE + `WSAGetLastError`), or queues (`WSA_IO_PENDING`). The
  IOCP path stores the requested length and treats deviation as
  error; the fallback emulates short-write by caller chunking.
- **AV interception.** Some Windows AV hooks `TransmitFile` and
  degrades it to a buffered copy. Detect at runtime via
  `QueryPerformanceCounter` over a 1 MiB warmup; if cycles/byte
  exceed 0.7x the read+write fallback, disable `TransmitFile` for
  the run and emit a `--debug=io` notice.
- **Driver default chunk.** `nNumberOfBytesPerSend = 0` lets the
  driver pick. Older NICs without TSO use 1 MSS (1460 bytes),
  defeating the win. Always pass 64 KiB explicitly.
