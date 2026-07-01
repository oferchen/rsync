//! Async transport wrapper seam over the socket-backed transport (ASY-4).
//!
//! This module provides an internal `tokio::io::AsyncRead + AsyncWrite` adapter
//! over the real transport socket. It is the prerequisite the coupled receiver
//! conversion (re-chartered ASY-7, see `docs/design/asy-7-receiver-tokio-prototype.md`
//! section 5) consumes: an async socket boundary that a future async-prefetch
//! task can `.await`, feeding the sync demux stack under `spawn_blocking`.
//!
//! # Scope (ASY-4)
//!
//! This is the transport wrapper **only**. It is additive scaffolding and is
//! **not** wired into the receiver read path, the multiplex demux, the SPSC
//! disk bridge, or `core::session`. Wiring it into a transfer is the deferred
//! coupled ASY-7-redo rung. Nothing here changes the default build: the module
//! is compiled out entirely unless `tokio-transfer` is on.
//!
//! # Which transports it applies to
//!
//! Only transports backed by a real socket file descriptor. The daemon's
//! `rsync://` connection carries a concrete `std::net::TcpStream`
//! (`daemon::DaemonStream::Plain`, exposed via
//! `DaemonStream::tcp_stream() -> Option<&TcpStream>`). SSH / stdio transports
//! are pipe-backed (`DaemonStream::Stdio`, `Box<dyn Read>` / `Box<dyn Write>`)
//! and have no async socket to adopt. Callers mirror the NSV-1 `Option<fd>`
//! shape: they take the socket only when one exists and skip the wrapper
//! otherwise. See [`from_std_tcp`](AsyncTransport::from_std_tcp) for the
//! socket case; the pipe case has no constructor here by design.
//!
//! # Runtime-context requirement
//!
//! [`tokio::net::TcpStream::from_std`] must be called from within a tokio
//! runtime context (a reactor must be registered for the socket). Within the
//! ASY-3 current-thread runtime this holds: `core`'s session shim builds or
//! adopts a runtime and `block_on`s the transfer future, so any code driven
//! under that `block_on` (or a `Handle::enter()` guard) satisfies the
//! requirement. Constructing an `AsyncTransport` outside any runtime context
//! returns an `io::Error` from tokio rather than panicking; the unit tests
//! exercise the in-context path.
//!
//! # No public tokio surface
//!
//! Per `docs/design/asy-2-tokio-runtime-feature.md` section 4, no tokio type
//! appears in any public signature of `transfer` or `core`. This type is
//! `pub(crate)` and never escapes into an exported signature; it is an
//! implementation detail of the future tokio receiver.

// ASY-4 is deliberately unwired scaffolding: the adapter is exercised by the
// module's own tests but has no non-test caller yet (the coupled ASY-7-redo
// receiver rung is deferred). Allow dead_code so the seam can land ahead of its
// consumer without tripping `-D warnings`; the allow is removed when the
// receiver rung wires it in.
#![allow(dead_code)]

use std::io;
use std::net::TcpStream;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream as TokioTcpStream;

/// An `AsyncRead + AsyncWrite` adapter over a socket-backed transport.
///
/// Wraps a [`tokio::net::TcpStream`] adopted from a blocking
/// [`std::net::TcpStream`]. All poll methods delegate to the inner tokio
/// socket, so the adapter adds no buffering of its own (avoiding the
/// double-buffering ASY-2 open-question 2 flags). It exists so the coupled
/// ASY-7-redo receiver rung has a single async socket boundary to build on.
pub(crate) struct AsyncTransport {
    inner: TokioTcpStream,
}

impl AsyncTransport {
    /// Adopts a blocking [`std::net::TcpStream`] as an async transport.
    ///
    /// The socket is switched to non-blocking mode inside this constructor
    /// (`TcpStream::set_nonblocking(true)`), which is the precondition
    /// [`tokio::net::TcpStream::from_std`] requires: tokio drives the socket
    /// via its reactor and a blocking socket would stall the runtime. Callers
    /// therefore need not pre-configure the socket.
    ///
    /// Only call this for a transport that owns a real socket fd (the daemon
    /// `rsync://` path via `DaemonStream::Plain`). Pipe-backed transports
    /// (SSH / stdio) have no socket and must not reach this constructor; there
    /// is no equivalent for them by design (they stay on the sync path).
    ///
    /// # Runtime context
    ///
    /// Must be invoked from within a tokio runtime context so the socket can
    /// register with the reactor. Under the ASY-3 model this is any code driven
    /// by `core`'s session `block_on` / `Handle`. Outside a runtime context
    /// tokio returns an `io::Error` (it does not panic).
    ///
    /// # Errors
    ///
    /// Returns any error from `set_nonblocking` or
    /// [`tokio::net::TcpStream::from_std`] (including the no-reactor case).
    pub(crate) fn from_std_tcp(stream: TcpStream) -> io::Result<Self> {
        stream.set_nonblocking(true)?;
        let inner = TokioTcpStream::from_std(stream)?;
        Ok(Self { inner })
    }
}

impl AsyncRead for AsyncTransport {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for AsyncTransport {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
}

#[cfg(test)]
mod tests {
    use super::AsyncTransport;
    use std::net::{TcpListener, TcpStream};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::runtime::Builder;

    /// Builds a connected blocking `TcpStream` pair on loopback. Returns the
    /// two ends of one connection so a byte written into one is read from the
    /// other. Both ends are blocking; the wrapper flips them to non-blocking.
    fn connected_pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let client = TcpStream::connect(addr).unwrap();
        let (server, _) = listener.accept().unwrap();
        (client, server)
    }

    /// `from_std_tcp` flips a blocking socket to non-blocking, which is the
    /// precondition `tokio::net::TcpStream::from_std` requires. This proves the
    /// constructor sets the mode the runtime relies on rather than leaving the
    /// caller to do it.
    #[test]
    fn from_std_tcp_sets_nonblocking() {
        let rt = Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let (client, _server) = connected_pair();
            // Adopt the blocking client socket; the constructor must have
            // set it non-blocking for the reactor to drive it.
            let adopted = AsyncTransport::from_std_tcp(client).expect("adopt socket");
            // Round-tripping the tokio socket back to std must observe the
            // non-blocking mode set by the constructor: a std read on a
            // non-blocking socket with no data returns WouldBlock rather than
            // parking the thread.
            let std_again = adopted.inner.into_std().expect("into_std");
            let mut probe = std_again;
            use std::io::Read;
            let mut buf = [0u8; 1];
            let err = probe.read(&mut buf).unwrap_err();
            assert_eq!(err.kind(), std::io::ErrorKind::WouldBlock);
        });
    }

    /// Bytes written into one end of a real loopback socket via the async
    /// wrapper are read byte-exact from the other end, all from within the
    /// ASY-3 current-thread runtime. This is the core round-trip contract the
    /// coupled receiver rung will rely on, and proves no runtime-context panic
    /// on construction or I/O.
    #[test]
    fn async_round_trip_over_loopback_socket() {
        let rt = Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let (client, server) = connected_pair();
            let mut writer = AsyncTransport::from_std_tcp(client).expect("adopt client");
            let mut reader = AsyncTransport::from_std_tcp(server).expect("adopt server");

            let payload: &[u8] = b"delta\x00token\x01frame\xffbytes";
            writer.write_all(payload).await.expect("async write_all");
            writer.flush().await.expect("async flush");

            let mut got = vec![0u8; payload.len()];
            reader.read_exact(&mut got).await.expect("async read_exact");
            assert_eq!(got, payload, "async round-trip must be byte-exact");
        });
    }

    /// The wrapper is usable within a current-thread runtime's `block_on`
    /// without a runtime-context panic - the exact context ASY-3's driver runs
    /// the server body in. Guards against a regression where construction or
    /// I/O would require a multi-thread runtime.
    #[test]
    fn usable_from_current_thread_block_on() {
        let rt = Builder::new_current_thread().enable_all().build().unwrap();
        let echoed: Vec<u8> = rt.block_on(async {
            let (client, server) = connected_pair();
            let mut a = AsyncTransport::from_std_tcp(client).expect("adopt a");
            let mut b = AsyncTransport::from_std_tcp(server).expect("adopt b");
            a.write_all(b"ping").await.unwrap();
            a.flush().await.unwrap();
            let mut buf = [0u8; 4];
            b.read_exact(&mut buf).await.unwrap();
            buf.to_vec()
        });
        assert_eq!(echoed, b"ping");
    }
}
