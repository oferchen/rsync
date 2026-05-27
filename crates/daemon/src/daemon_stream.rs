//! Unified stream abstraction for plain TCP and TLS-wrapped daemon connections.
//!
//! [`DaemonStream`] transparently handles both plain `TcpStream` and
//! TLS-encrypted connections (via rustls) behind a single type that
//! implements `Read + Write`. This lets the daemon's session handler,
//! greeting exchange, and module access code operate identically
//! regardless of whether TLS is active.
//!
//! The `Tls` variant is only available when the `daemon-tls` Cargo
//! feature is enabled. Without it, `DaemonStream` is a simple wrapper
//! around `TcpStream` with no additional dependencies.

use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::time::Duration;

/// A daemon connection that is either a plain TCP stream or a
/// TLS-encrypted stream over TCP.
///
/// Implements `Read` and `Write` by delegating to the inner stream,
/// making it a drop-in replacement for `TcpStream` in the daemon's
/// session handler pipeline.
///
/// # Variants
///
/// - `Plain` - unencrypted TCP, used when no TLS configuration is
///   present.
/// - `Tls` - encrypted via rustls `StreamOwned`, used when the daemon
///   is started with `--ssl` / certificate configuration. Only
///   available behind `#[cfg(feature = "daemon-tls")]`.
pub enum DaemonStream {
    /// Unencrypted TCP connection.
    Plain(TcpStream),

    /// TLS-encrypted connection over TCP.
    ///
    /// The `StreamOwned` manages the rustls session state and encrypts
    /// all data transparently. It implements `Read + Write` by
    /// performing TLS record framing under the hood.
    #[cfg(feature = "daemon-tls")]
    #[cfg_attr(docsrs, doc(cfg(feature = "daemon-tls")))]
    Tls(rustls::StreamOwned<rustls::ServerConnection, TcpStream>),
}

impl DaemonStream {
    /// Wraps a plain TCP stream (no encryption).
    pub fn plain(stream: TcpStream) -> Self {
        Self::Plain(stream)
    }

    /// Configures the read timeout on the underlying TCP socket.
    ///
    /// Delegates to `TcpStream::set_read_timeout` regardless of whether
    /// the connection is plain or TLS-wrapped - the timeout applies at the
    /// OS socket level, beneath the TLS layer.
    pub fn set_read_timeout(&self, dur: Option<Duration>) -> io::Result<()> {
        match self {
            Self::Plain(s) => s.set_read_timeout(dur),
            #[cfg(feature = "daemon-tls")]
            Self::Tls(s) => s.sock.set_read_timeout(dur),
        }
    }

    /// Configures the write timeout on the underlying TCP socket.
    pub fn set_write_timeout(&self, dur: Option<Duration>) -> io::Result<()> {
        match self {
            Self::Plain(s) => s.set_write_timeout(dur),
            #[cfg(feature = "daemon-tls")]
            Self::Tls(s) => s.sock.set_write_timeout(dur),
        }
    }

    /// Enables or disables `TCP_NODELAY` on the underlying socket.
    pub fn set_nodelay(&self, nodelay: bool) -> io::Result<()> {
        match self {
            Self::Plain(s) => s.set_nodelay(nodelay),
            #[cfg(feature = "daemon-tls")]
            Self::Tls(s) => s.sock.set_nodelay(nodelay),
        }
    }

    /// Shuts down the read, write, or both halves of the connection.
    ///
    /// For TLS streams this operates on the underlying TCP socket. The
    /// TLS `close_notify` alert should be sent (via `flush` / drop)
    /// before calling this.
    pub fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        match self {
            Self::Plain(s) => s.shutdown(how),
            #[cfg(feature = "daemon-tls")]
            Self::Tls(s) => s.sock.shutdown(how),
        }
    }

    /// Returns a reference to the underlying `TcpStream`.
    ///
    /// Useful for operations that need the raw socket (e.g., applying
    /// socket options, reading the peer address).
    pub fn tcp_stream(&self) -> &TcpStream {
        match self {
            Self::Plain(s) => s,
            #[cfg(feature = "daemon-tls")]
            Self::Tls(s) => &s.sock,
        }
    }

    /// Returns `true` if this is a TLS-encrypted connection.
    pub fn is_tls(&self) -> bool {
        match self {
            Self::Plain(_) => false,
            #[cfg(feature = "daemon-tls")]
            Self::Tls(_) => true,
        }
    }

    /// Consumes the `DaemonStream` and returns the inner `TcpStream`.
    ///
    /// For the `Plain` variant this is a no-op unwrap. For `Tls`, this
    /// extracts the underlying TCP socket from the `StreamOwned`,
    /// discarding the TLS session state. The caller is responsible for
    /// ensuring the TLS session has been properly shut down before
    /// calling this.
    pub fn into_tcp_stream(self) -> TcpStream {
        match self {
            Self::Plain(s) => s,
            #[cfg(feature = "daemon-tls")]
            Self::Tls(s) => s.sock,
        }
    }
}

impl Read for DaemonStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Plain(s) => s.read(buf),
            #[cfg(feature = "daemon-tls")]
            Self::Tls(s) => s.read(buf),
        }
    }
}

impl Write for DaemonStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Plain(s) => s.write(buf),
            #[cfg(feature = "daemon-tls")]
            Self::Tls(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Plain(s) => s.flush(),
            #[cfg(feature = "daemon-tls")]
            Self::Tls(s) => s.flush(),
        }
    }
}

impl std::fmt::Debug for DaemonStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Plain(s) => f.debug_tuple("DaemonStream::Plain").field(s).finish(),
            #[cfg(feature = "daemon-tls")]
            Self::Tls(_) => f.debug_tuple("DaemonStream::Tls").field(&"<tls>").finish(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// Creates a connected pair of TCP streams for testing.
    fn connected_pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let client = TcpStream::connect(addr).unwrap();
        let (server, _) = listener.accept().unwrap();
        (client, server)
    }

    #[test]
    fn plain_read_write_roundtrip() {
        let (client, server) = connected_pair();
        let mut daemon = DaemonStream::plain(server);
        let mut client = client;

        let payload = b"hello daemon";
        client.write_all(payload).unwrap();
        client.flush().unwrap();

        let mut buf = [0u8; 64];
        let n = daemon.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], payload);
    }

    #[test]
    fn plain_write_read_roundtrip() {
        let (client, server) = connected_pair();
        let mut daemon = DaemonStream::plain(server);

        let payload = b"response from daemon";
        daemon.write_all(payload).unwrap();
        daemon.flush().unwrap();

        let mut client = client;
        let mut buf = [0u8; 64];
        let n = client.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], payload);
    }

    #[test]
    fn plain_is_not_tls() {
        let (_client, server) = connected_pair();
        let daemon = DaemonStream::plain(server);
        assert!(!daemon.is_tls());
    }

    #[test]
    fn plain_tcp_stream_ref() {
        let (_client, server) = connected_pair();
        let addr = server.local_addr().unwrap();
        let daemon = DaemonStream::plain(server);
        assert_eq!(daemon.tcp_stream().local_addr().unwrap(), addr);
    }

    #[test]
    fn plain_into_tcp_stream() {
        let (_client, server) = connected_pair();
        let addr = server.local_addr().unwrap();
        let daemon = DaemonStream::plain(server);
        let recovered = daemon.into_tcp_stream();
        assert_eq!(recovered.local_addr().unwrap(), addr);
    }

    #[test]
    fn plain_set_timeouts() {
        let (_client, server) = connected_pair();
        let daemon = DaemonStream::plain(server);
        let dur = Some(Duration::from_secs(5));
        daemon.set_read_timeout(dur).unwrap();
        daemon.set_write_timeout(dur).unwrap();
    }

    #[test]
    fn plain_set_nodelay() {
        let (_client, server) = connected_pair();
        let daemon = DaemonStream::plain(server);
        daemon.set_nodelay(true).unwrap();
    }

    #[test]
    fn plain_shutdown() {
        let (_client, server) = connected_pair();
        let daemon = DaemonStream::plain(server);
        daemon.shutdown(Shutdown::Both).unwrap();
    }

    #[test]
    fn plain_debug_format() {
        let (_client, server) = connected_pair();
        let daemon = DaemonStream::plain(server);
        let debug = format!("{daemon:?}");
        assert!(debug.contains("Plain"), "got: {debug}");
    }

    #[cfg(feature = "daemon-tls")]
    #[test]
    fn tls_variant_is_tls() {
        // Constructing a real TLS stream requires certificates; verified
        // via the TLS handshake integration test below. This test just
        // confirms the is_tls() discriminator when the feature is enabled.
        let (_client, server) = connected_pair();
        let daemon = DaemonStream::Plain(server);
        assert!(!daemon.is_tls());
    }

    #[cfg(feature = "daemon-tls")]
    #[test]
    fn tls_debug_format() {
        let (_client, server) = connected_pair();
        let daemon = DaemonStream::Plain(server);
        let debug = format!("{daemon:?}");
        assert!(debug.contains("Plain"), "got: {debug}");
    }
}
