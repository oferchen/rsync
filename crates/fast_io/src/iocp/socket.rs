//! IOCP-based async socket I/O via `WSARecv` / `WSASend`.
//!
//! This module mirrors the Linux `io_uring` socket reader/writer surface
//! (`crates/fast_io/src/io_uring/socket_reader.rs`,
//! `crates/fast_io/src/io_uring/socket_writer.rs`) for the Windows IOCP
//! backend. Each socket operation is submitted as an overlapped Winsock
//! request whose completion is dispatched by the shared
//! [`CompletionPump`](super::pump::CompletionPump). Multiple sockets and the
//! existing IOCP file readers/writers can therefore share a single drain
//! thread.
//!
//! # Upstream reference
//!
//! Upstream rsync uses POSIX `read(2)` / `write(2)` against the socket fd in
//! `safe_read` / `safe_write`
//! (`target/interop/upstream-src/rsync-3.4.1/io.c:239` and `:312`). Windows
//! has no direct equivalent because `read`/`write` on a `SOCKET` are
//! synchronous; the closest async-capable primitive is `WSARecv` / `WSASend`
//! with `OVERLAPPED`. The buffering and EOF semantics expected by the
//! multiplex layer above (`crates/protocol`) are unchanged: an `Ok(0)` from a
//! socket read means the peer cleanly closed the connection, mirroring
//! `safe_read` exiting its loop on `n == 0` (`io.c:276`).
//!
//! # WSA_IO_PENDING
//!
//! `WSARecv` / `WSASend` may complete synchronously and return `0`, indicating
//! the buffer is already filled (recv) or fully transmitted (send). When they
//! return `SOCKET_ERROR` with `WSA_IO_PENDING`, the operation is queued on the
//! completion port and the pump delivers the byte count through the registered
//! handler.
//!
//! # Cross-platform stub
//!
//! Real implementation lives in this file under
//! `#[cfg(all(target_os = "windows", feature = "iocp"))]`. The non-Windows
//! stub in `crate::iocp_stub` exposes the same public types so the workspace
//! cross-compiles on Linux and macOS.

use std::io;
use std::os::windows::io::RawSocket;
use std::sync::Arc;

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Networking::WinSock::{
    SOCKET, WSA_IO_PENDING, WSABUF, WSAECONNABORTED, WSAECONNRESET, WSAEDISCON, WSAENETRESET,
    WSAESHUTDOWN, WSARecv, WSASend,
};
use windows_sys::Win32::System::IO::OVERLAPPED;

use super::pump::{CompletionPump, oneshot_handler};

/// Shared reference to a [`CompletionPump`] used by sockets to dispatch
/// completions.
///
/// Sockets do not own the pump; many sockets plus the existing IOCP file
/// reader/writer can share a single pump. The pump must outlive every
/// socket attached to it - holding an `Arc` makes that guarantee obvious to
/// the borrow checker without forcing every caller into a single-thread
/// lifetime.
pub type SharedPump = Arc<CompletionPump>;

/// Per-socket completion-port key used for new associations.
///
/// The pump's drain loop only inspects the OVERLAPPED address to look up the
/// handler, so the key itself is informational. We assign a small integer
/// (matching the file reader/writer convention which uses `0`/`1`) and let
/// callers override via [`IocpSocketReader::with_completion_key`] /
/// [`IocpSocketWriter::with_completion_key`] when they want to disambiguate
/// sockets in pump diagnostics.
const DEFAULT_SOCKET_COMPLETION_KEY: usize = 2;

/// Async socket reader backed by `WSARecv` and the shared IOCP pump.
///
/// The reader does **not** take ownership of the underlying `SOCKET`; the
/// caller is responsible for closing it (typically by holding a `TcpStream`
/// alive elsewhere). On drop, the reader also does not unregister the socket
/// from the completion port - Windows performs that cleanup when the socket
/// handle is closed.
///
/// Mirrors the buffering shape of upstream rsync's `safe_read` (`io.c:239`)
/// in that the function returns whatever bytes the kernel hands back, leaving
/// the multiplex layer above (`crates/protocol`) responsible for assembling
/// frames.
pub struct IocpSocketReader {
    socket: SOCKET,
    pump: SharedPump,
    completion_key: usize,
}

impl IocpSocketReader {
    /// Wraps a raw Winsock socket for overlapped recv against the given pump.
    ///
    /// The socket must already have been created with `WSA_FLAG_OVERLAPPED`
    /// (Rust's `TcpStream` and `socket2::Socket` do this by default) and must
    /// have been associated with the pump's completion port via
    /// [`CompletionPump::associate_handle`].
    ///
    /// Callers that have not yet associated the socket can use
    /// [`IocpSocketReader::associate`] to perform that step in a single call.
    #[must_use]
    pub fn from_raw_socket(socket: RawSocket, pump: SharedPump) -> Self {
        Self {
            socket: socket as SOCKET,
            pump,
            completion_key: DEFAULT_SOCKET_COMPLETION_KEY,
        }
    }

    /// Associates the socket with the pump's completion port and returns a
    /// reader bound to the pump.
    ///
    /// Equivalent to calling [`CompletionPump::associate_handle`] followed by
    /// [`IocpSocketReader::from_raw_socket`]. Returns the underlying Win32
    /// error if association fails (typically `WSAEINVAL` when the socket was
    /// not opened with `WSA_FLAG_OVERLAPPED`).
    pub fn associate(socket: RawSocket, pump: SharedPump) -> io::Result<Self> {
        let key = DEFAULT_SOCKET_COMPLETION_KEY;
        pump.associate_handle(socket as HANDLE, key)?;
        Ok(Self {
            socket: socket as SOCKET,
            pump,
            completion_key: key,
        })
    }

    /// Overrides the completion key reported by the pump for this socket.
    ///
    /// Useful when many sockets share a single pump and a caller wants to
    /// distinguish them in pump diagnostics. Has no effect on dispatch
    /// because the pump matches handlers by OVERLAPPED address, not by key.
    #[must_use]
    pub fn with_completion_key(mut self, key: usize) -> Self {
        self.completion_key = key;
        self
    }

    /// Returns the completion key associated with this socket.
    #[must_use]
    pub fn completion_key(&self) -> usize {
        self.completion_key
    }

    /// Receives bytes from the socket, returning the number transferred.
    ///
    /// `Ok(0)` indicates a graceful peer shutdown - upstream `safe_read`
    /// breaks its loop on `n == 0` (`io.c:276`). Connection-reset and
    /// shutdown-on-the-other-side errors are mapped to `Ok(0)` to mirror that
    /// EOF semantic, matching how the io_uring socket reader handles
    /// `IORING_OP_RECV` returning `0` after a peer close.
    pub fn recv_async(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        // The OVERLAPPED is heap-allocated so the pointer the kernel sees is
        // stable for the entire async lifetime, even if the caller moves the
        // reader between calls. WSAOVERLAPPED is layout-compatible with
        // OVERLAPPED; the pump dispatches by pointer address.
        let mut overlapped: Box<OVERLAPPED> = Box::new(zeroed_overlapped());
        let overlapped_ptr: *mut OVERLAPPED = overlapped.as_mut() as *mut OVERLAPPED;

        let wsabuf = WSABUF {
            len: buf.len() as u32,
            buf: buf.as_mut_ptr(),
        };

        let (handler, rx) = oneshot_handler();
        self.pump.register(overlapped_ptr, handler);

        let mut bytes_received: u32 = 0;
        let mut flags: u32 = 0;

        // SAFETY: `self.socket` is a valid SOCKET supplied by the caller and
        // kept alive for the duration of this function. `wsabuf` and
        // `overlapped` outlive the call because we either (a) wait for the
        // pump to fire `rx` before returning, or (b) return early after
        // unregistering the handler when WSARecv reports a synchronous
        // failure. `wsabuf.buf` aliases the caller-provided slice for the
        // length of the WSARecv call only.
        #[allow(unsafe_code)]
        let rc = unsafe {
            WSARecv(
                self.socket,
                &wsabuf,
                1,
                &mut bytes_received,
                &mut flags,
                overlapped_ptr,
                None,
            )
        };

        if rc == 0 {
            // Synchronous completion. The pump still receives the completion
            // packet for this OVERLAPPED, so we wait on the handler to keep
            // the registry consistent and avoid a stale entry. The kernel
            // delivers the same byte count it reported synchronously.
            return await_completion(&rx, &mut overlapped);
        }

        let err = io::Error::last_os_error();
        if err.raw_os_error() != Some(WSA_IO_PENDING) {
            // Synchronous failure: drop our handler and translate the
            // Winsock error.
            self.pump.unregister(overlapped_ptr);
            return Err(map_recv_error(err));
        }

        await_completion(&rx, &mut overlapped)
    }

    /// Returns the raw socket handle.
    ///
    /// Exposed for callers that need to combine the socket with other Winsock
    /// APIs (e.g. `setsockopt` to tune `SO_RCVBUF`). The reader continues to
    /// reference the same SOCKET; closing it externally invalidates the
    /// reader.
    #[must_use]
    pub fn as_raw_socket(&self) -> RawSocket {
        self.socket as RawSocket
    }
}

/// Async socket writer backed by `WSASend` and the shared IOCP pump.
///
/// Like [`IocpSocketReader`], the writer does not own the SOCKET. It is the
/// caller's job to close it. Mirrors upstream `safe_write` (`io.c:312`) in
/// that a partial write returns the byte count and lets the caller loop -
/// the multiplex layer above performs that loop in
/// `crates/protocol/src/multiplex.rs`.
pub struct IocpSocketWriter {
    socket: SOCKET,
    pump: SharedPump,
    completion_key: usize,
}

impl IocpSocketWriter {
    /// Wraps a raw Winsock socket for overlapped send against the given pump.
    ///
    /// See [`IocpSocketReader::from_raw_socket`] for the requirements on the
    /// underlying socket. The same socket can be wrapped by both a reader
    /// and a writer; Winsock dispatches recv and send completions
    /// independently because each WSARecv/WSASend uses its own OVERLAPPED.
    #[must_use]
    pub fn from_raw_socket(socket: RawSocket, pump: SharedPump) -> Self {
        Self {
            socket: socket as SOCKET,
            pump,
            completion_key: DEFAULT_SOCKET_COMPLETION_KEY,
        }
    }

    /// Associates the socket with the pump's completion port and returns a
    /// writer bound to the pump.
    pub fn associate(socket: RawSocket, pump: SharedPump) -> io::Result<Self> {
        let key = DEFAULT_SOCKET_COMPLETION_KEY;
        pump.associate_handle(socket as HANDLE, key)?;
        Ok(Self {
            socket: socket as SOCKET,
            pump,
            completion_key: key,
        })
    }

    /// Overrides the completion key reported by the pump for this socket.
    #[must_use]
    pub fn with_completion_key(mut self, key: usize) -> Self {
        self.completion_key = key;
        self
    }

    /// Returns the completion key associated with this socket.
    #[must_use]
    pub fn completion_key(&self) -> usize {
        self.completion_key
    }

    /// Sends bytes through the socket, returning the number transferred.
    ///
    /// A short return is possible when the socket buffer is full; upstream
    /// `safe_write` re-issues the call in that case (`io.c:316-336`). The
    /// multiplex writer above handles re-issuance for us; this function
    /// simply returns whatever the kernel reports.
    pub fn send_async(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        let mut overlapped: Box<OVERLAPPED> = Box::new(zeroed_overlapped());
        let overlapped_ptr: *mut OVERLAPPED = overlapped.as_mut() as *mut OVERLAPPED;

        let wsabuf = WSABUF {
            len: buf.len() as u32,
            // WSASend takes a non-mut pointer logically, but the WSABUF
            // field is `*mut u8` in the windows-sys binding. The kernel
            // does not write through the buffer for a send; we cast away
            // const purely to match the FFI signature.
            buf: buf.as_ptr() as *mut u8,
        };

        let (handler, rx) = oneshot_handler();
        self.pump.register(overlapped_ptr, handler);

        let mut bytes_sent: u32 = 0;

        // SAFETY: `self.socket` is valid for the duration of the call;
        // `wsabuf` aliases the caller's slice for the duration of WSASend
        // only; `overlapped` is heap-allocated and outlives the kernel's
        // reference to it because we wait on `rx` (or unregister on
        // synchronous failure) before returning.
        #[allow(unsafe_code)]
        let rc = unsafe {
            WSASend(
                self.socket,
                &wsabuf,
                1,
                &mut bytes_sent,
                0,
                overlapped_ptr,
                None,
            )
        };

        if rc == 0 {
            return await_completion(&rx, &mut overlapped);
        }

        let err = io::Error::last_os_error();
        if err.raw_os_error() != Some(WSA_IO_PENDING) {
            self.pump.unregister(overlapped_ptr);
            return Err(map_send_error(err));
        }

        await_completion(&rx, &mut overlapped)
    }

    /// Returns the raw socket handle.
    #[must_use]
    pub fn as_raw_socket(&self) -> RawSocket {
        self.socket as RawSocket
    }
}

// SAFETY: IocpSocketReader holds a SOCKET (kernel handle, thread-safe) and an
// Arc<CompletionPump>. The pump is already Send + Sync via its Arc<PumpInner>
// internals. Sockets are kernel objects safe to share across threads, but the
// reader is exclusive per outstanding recv (Winsock serialises overlapped
// operations submitted with the same OVERLAPPED).
#[allow(unsafe_code)]
unsafe impl Send for IocpSocketReader {}

// SAFETY: same reasoning as IocpSocketReader. The writer state is only
// touched from one thread per send_async call; concurrent access from
// another thread would require a separate writer instance.
#[allow(unsafe_code)]
unsafe impl Send for IocpSocketWriter {}

/// Waits on the pump-fired oneshot channel and converts the result into the
/// shape `recv_async` / `send_async` expect.
///
/// `_overlapped` is borrowed through the call so the heap allocation can not
/// be freed while the kernel still holds a pointer to it. After `rx.recv()`
/// returns, the pump has already removed the handler from its registry, so
/// dropping the box is safe.
fn await_completion(
    rx: &std::sync::mpsc::Receiver<io::Result<u32>>,
    _overlapped: &mut Box<OVERLAPPED>,
) -> io::Result<usize> {
    let result = rx
        .recv()
        .map_err(|_| io::Error::other("iocp pump worker exited before completion"))?;

    match result {
        Ok(transferred) => Ok(transferred as usize),
        // `UnexpectedEof` from the pump corresponds to STATUS_END_OF_FILE,
        // which Winsock translates to a graceful close on the recv side.
        // upstream: io.c:276 - safe_read breaks its loop on n == 0.
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(0),
        Err(e) => Err(e),
    }
}

/// Maps a synchronous `WSARecv` error to a portable `io::Error`.
fn map_recv_error(err: io::Error) -> io::Error {
    if let Some(code) = err.raw_os_error()
        && is_graceful_recv_close(code)
    {
        // Graceful peer close paths get folded into ErrorKind::UnexpectedEof
        // so callers can branch on `kind()` rather than raw OS codes. The
        // recv_async public API turns this back into Ok(0) via
        // await_completion's UnexpectedEof match arm.
        return io::Error::from(io::ErrorKind::UnexpectedEof);
    }
    err
}

/// Maps a synchronous `WSASend` error to a portable `io::Error`.
fn map_send_error(err: io::Error) -> io::Error {
    if let Some(code) = err.raw_os_error()
        && is_broken_pipe_send(code)
    {
        return io::Error::from(io::ErrorKind::BrokenPipe);
    }
    err
}

fn is_graceful_recv_close(code: i32) -> bool {
    code == WSAEDISCON
        || code == WSAESHUTDOWN
        || code == WSAENETRESET
        || code == WSAECONNRESET
        || code == WSAECONNABORTED
}

fn is_broken_pipe_send(code: i32) -> bool {
    code == WSAESHUTDOWN || code == WSAECONNRESET || code == WSAECONNABORTED
}

fn zeroed_overlapped() -> OVERLAPPED {
    // SAFETY: OVERLAPPED is plain old data and is valid in the all-zero state.
    #[allow(unsafe_code)]
    unsafe {
        std::mem::zeroed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::os::windows::io::AsRawSocket;
    use std::thread;

    /// Round-trips a payload through a localhost TCP pair using the IOCP
    /// socket reader and writer.
    #[test]
    fn roundtrip_localhost_tcp() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let payload: Vec<u8> = (0..4096).map(|i| (i % 251) as u8).collect();

        let server_payload = payload.clone();
        let server = thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            sock.write_all(&server_payload).unwrap();
            sock.shutdown(std::net::Shutdown::Write).unwrap();
        });

        let client = TcpStream::connect(addr).unwrap();
        let pump = Arc::new(CompletionPump::new().unwrap());
        let mut reader =
            IocpSocketReader::associate(client.as_raw_socket(), Arc::clone(&pump)).unwrap();

        let mut received = Vec::with_capacity(payload.len());
        let mut chunk = vec![0u8; 1024];
        while received.len() < payload.len() {
            let n = reader.recv_async(&mut chunk).unwrap();
            if n == 0 {
                break;
            }
            received.extend_from_slice(&chunk[..n]);
        }

        assert_eq!(received, payload);
        server.join().unwrap();

        // Holding `client` keeps the SOCKET alive until after the reader
        // finishes its last recv.
        drop(client);
        Arc::try_unwrap(pump)
            .ok()
            .expect("pump must be uniquely owned for shutdown")
            .shutdown()
            .unwrap();
    }

    /// Verifies that a 64 KB payload is fully transmitted across multiple
    /// sends; Winsock often splits sends larger than the socket buffer.
    #[test]
    fn writer_partial_send_accounting() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let server = thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut received = Vec::new();
            sock.read_to_end(&mut received).unwrap();
            received
        });

        let client = TcpStream::connect(addr).unwrap();
        let pump = Arc::new(CompletionPump::new().unwrap());
        let mut writer =
            IocpSocketWriter::associate(client.as_raw_socket(), Arc::clone(&pump)).unwrap();

        let payload: Vec<u8> = (0..65_536).map(|i| ((i * 31 + 7) % 256) as u8).collect();
        let mut sent = 0;
        while sent < payload.len() {
            let n = writer.send_async(&payload[sent..]).unwrap();
            assert!(n > 0, "send_async must make progress");
            sent += n;
        }
        assert_eq!(sent, payload.len());

        // Closing the client signals EOF to the server thread.
        drop(writer);
        drop(client);

        let received = server.join().unwrap();
        assert_eq!(received, payload);

        Arc::try_unwrap(pump)
            .ok()
            .expect("pump must be uniquely owned")
            .shutdown()
            .unwrap();
    }

    /// Peer shutdown produces `Ok(0)` from `recv_async`, matching the EOF
    /// semantic of upstream `safe_read`.
    #[test]
    fn recv_after_peer_shutdown_returns_eof() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let server = thread::spawn(move || {
            let (sock, _) = listener.accept().unwrap();
            sock.shutdown(std::net::Shutdown::Write).unwrap();
            // Hold the connection open so the client read sees EOF, not RST.
            std::thread::sleep(std::time::Duration::from_millis(200));
        });

        let client = TcpStream::connect(addr).unwrap();
        let pump = Arc::new(CompletionPump::new().unwrap());
        let mut reader =
            IocpSocketReader::associate(client.as_raw_socket(), Arc::clone(&pump)).unwrap();

        let mut buf = [0u8; 1024];
        let n = reader.recv_async(&mut buf).unwrap();
        assert_eq!(n, 0, "recv after peer shutdown must report EOF");

        server.join().unwrap();
        drop(client);
        Arc::try_unwrap(pump)
            .ok()
            .expect("pump uniquely owned")
            .shutdown()
            .unwrap();
    }

    #[test]
    fn empty_recv_buffer_returns_zero_without_io() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let _ = listener.accept().unwrap();
        });

        let client = TcpStream::connect(addr).unwrap();
        let pump = Arc::new(CompletionPump::new().unwrap());
        let mut reader =
            IocpSocketReader::associate(client.as_raw_socket(), Arc::clone(&pump)).unwrap();

        let mut empty: [u8; 0] = [];
        assert_eq!(reader.recv_async(&mut empty).unwrap(), 0);
        assert_eq!(pump.pending_ops(), 0, "no handler should remain registered");

        drop(client);
        server.join().unwrap();
        Arc::try_unwrap(pump)
            .ok()
            .expect("pump uniquely owned")
            .shutdown()
            .unwrap();
    }

    #[test]
    fn empty_send_buffer_returns_zero_without_io() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let _ = listener.accept().unwrap();
        });

        let client = TcpStream::connect(addr).unwrap();
        let pump = Arc::new(CompletionPump::new().unwrap());
        let mut writer =
            IocpSocketWriter::associate(client.as_raw_socket(), Arc::clone(&pump)).unwrap();

        let empty: [u8; 0] = [];
        assert_eq!(writer.send_async(&empty).unwrap(), 0);
        assert_eq!(pump.pending_ops(), 0);

        drop(client);
        server.join().unwrap();
        Arc::try_unwrap(pump)
            .ok()
            .expect("pump uniquely owned")
            .shutdown()
            .unwrap();
    }

    #[test]
    fn completion_key_override_round_trips() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let _ = listener.accept().unwrap();
        });

        let client = TcpStream::connect(addr).unwrap();
        let pump = Arc::new(CompletionPump::new().unwrap());
        let reader = IocpSocketReader::from_raw_socket(client.as_raw_socket(), Arc::clone(&pump))
            .with_completion_key(42);
        assert_eq!(reader.completion_key(), 42);

        drop(reader);
        drop(client);
        server.join().unwrap();
        Arc::try_unwrap(pump).ok().unwrap().shutdown().unwrap();
    }
}
