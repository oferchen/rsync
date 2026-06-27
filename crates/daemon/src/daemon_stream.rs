//! Unified stream abstraction for plain TCP and stdio daemon connections.
//!
//! [`DaemonStream`] transparently handles plain `TcpStream` and stdio-based
//! connections behind a single type that implements `Read + Write`. This lets
//! the daemon's session handler, greeting exchange, and module access code
//! operate identically regardless of the transport.
//!
//! The `Stdio` variant supports the `--server --daemon` remote-shell daemon
//! mode where stdin/stdout are used instead of a TCP socket.
//! upstream: main.c:1843-1844 - `if (am_server && am_daemon)
//! return start_daemon(STDIN_FILENO, STDOUT_FILENO);`

use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::time::Duration;

/// Joined stdin/stdout pair for daemon stdio mode.
///
/// Reads come from stdin, writes go to stdout. This supports the
/// `--server --daemon` remote-shell daemon mode where the daemon protocol
/// runs over an existing connection's stdin/stdout rather than a TCP socket.
///
/// upstream: clientserver.c - `start_daemon(STDIN_FILENO, STDOUT_FILENO)`
/// serves the daemon protocol over the inherited file descriptors.
pub struct StdioPair {
    reader: Box<dyn Read + Send>,
    writer: Box<dyn Write + Send>,
}

impl StdioPair {
    /// Creates a new stdio pair from the given reader and writer.
    pub fn new(reader: Box<dyn Read + Send>, writer: Box<dyn Write + Send>) -> Self {
        Self { reader, writer }
    }
}

/// A daemon connection that is either a plain TCP stream or a stdio pair for
/// remote-shell daemon mode.
///
/// Implements `Read` and `Write` by delegating to the inner stream,
/// making it a drop-in replacement for `TcpStream` in the daemon's
/// session handler pipeline.
///
/// # Variants
///
/// - `Plain` - unencrypted TCP.
/// - `Stdio` - stdin/stdout pair for `--server --daemon` remote-shell mode.
pub enum DaemonStream {
    /// Unencrypted TCP connection.
    Plain(TcpStream),

    /// Stdio-based connection for remote-shell daemon mode.
    ///
    /// Used when the daemon is invoked via `--server --daemon` over an
    /// existing connection (e.g., SSH). Reads from stdin, writes to stdout.
    /// upstream: main.c - `start_daemon(STDIN_FILENO, STDOUT_FILENO)`.
    Stdio(StdioPair),
}

impl DaemonStream {
    /// Wraps a plain TCP stream (no encryption).
    pub fn plain(stream: TcpStream) -> Self {
        Self::Plain(stream)
    }

    /// Wraps a stdio pair for remote-shell daemon mode.
    pub fn stdio(pair: StdioPair) -> Self {
        Self::Stdio(pair)
    }

    /// Configures the read timeout on the underlying TCP socket.
    ///
    /// Delegates to `TcpStream::set_read_timeout`. No-op for stdio streams
    /// (pipes do not support socket timeouts).
    pub fn set_read_timeout(&self, dur: Option<Duration>) -> io::Result<()> {
        match self {
            Self::Plain(s) => s.set_read_timeout(dur),
            Self::Stdio(_) => Ok(()),
        }
    }

    /// Configures the write timeout on the underlying TCP socket.
    ///
    /// No-op for stdio streams.
    pub fn set_write_timeout(&self, dur: Option<Duration>) -> io::Result<()> {
        match self {
            Self::Plain(s) => s.set_write_timeout(dur),
            Self::Stdio(_) => Ok(()),
        }
    }

    /// Enables or disables `TCP_NODELAY` on the underlying socket.
    ///
    /// No-op for stdio streams.
    pub fn set_nodelay(&self, nodelay: bool) -> io::Result<()> {
        match self {
            Self::Plain(s) => s.set_nodelay(nodelay),
            Self::Stdio(_) => Ok(()),
        }
    }

    /// Shuts down the read, write, or both halves of the connection.
    ///
    /// No-op for stdio streams (stdin/stdout are closed when the process
    /// exits).
    pub fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        match self {
            Self::Plain(s) => s.shutdown(how),
            Self::Stdio(_) => Ok(()),
        }
    }

    /// Returns a reference to the underlying `TcpStream`, if available.
    ///
    /// Returns `None` for stdio streams which have no underlying TCP socket.
    pub fn tcp_stream(&self) -> Option<&TcpStream> {
        match self {
            Self::Plain(s) => Some(s),
            Self::Stdio(_) => None,
        }
    }

    /// Returns `true` if this is a stdio-based connection.
    pub fn is_stdio(&self) -> bool {
        matches!(self, Self::Stdio(_))
    }

    /// Consumes the `DaemonStream` and returns the inner `TcpStream`.
    ///
    /// For the `Plain` variant this is a no-op unwrap.
    ///
    /// # Panics
    ///
    /// Panics if called on a `Stdio` variant, which has no `TcpStream`.
    pub fn into_tcp_stream(self) -> TcpStream {
        match self {
            Self::Plain(s) => s,
            Self::Stdio(_) => panic!("cannot extract TcpStream from Stdio variant"),
        }
    }
}

impl Read for DaemonStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Plain(s) => s.read(buf),
            Self::Stdio(pair) => pair.reader.read(buf),
        }
    }
}

impl Write for DaemonStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Plain(s) => s.write(buf),
            Self::Stdio(pair) => pair.writer.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Plain(s) => s.flush(),
            Self::Stdio(pair) => pair.writer.flush(),
        }
    }
}

impl std::fmt::Debug for DaemonStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Plain(s) => f.debug_tuple("DaemonStream::Plain").field(s).finish(),
            Self::Stdio(_) => f
                .debug_tuple("DaemonStream::Stdio")
                .field(&"<stdio>")
                .finish(),
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
    fn plain_tcp_stream_ref() {
        let (_client, server) = connected_pair();
        let addr = server.local_addr().unwrap();
        let daemon = DaemonStream::plain(server);
        assert_eq!(daemon.tcp_stream().unwrap().local_addr().unwrap(), addr);
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

    #[test]
    fn stdio_read_write_roundtrip() {
        let input = b"hello from client";
        let reader = io::Cursor::new(input.to_vec());
        let writer = Vec::new();
        let pair = StdioPair::new(Box::new(reader), Box::new(writer));
        let mut daemon = DaemonStream::stdio(pair);

        let mut buf = [0u8; 64];
        let n = daemon.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], input);

        let response = b"hello from daemon";
        daemon.write_all(response).unwrap();
        daemon.flush().unwrap();
    }

    #[test]
    fn stdio_is_stdio() {
        let pair = StdioPair::new(Box::new(io::Cursor::new(Vec::new())), Box::new(Vec::new()));
        let daemon = DaemonStream::stdio(pair);
        assert!(daemon.is_stdio());
    }

    #[test]
    fn plain_is_not_stdio() {
        let (_client, server) = connected_pair();
        let daemon = DaemonStream::plain(server);
        assert!(!daemon.is_stdio());
    }

    #[test]
    fn stdio_tcp_stream_is_none() {
        let pair = StdioPair::new(Box::new(io::Cursor::new(Vec::new())), Box::new(Vec::new()));
        let daemon = DaemonStream::stdio(pair);
        assert!(daemon.tcp_stream().is_none());
    }

    #[test]
    fn stdio_set_timeouts_are_noop() {
        let pair = StdioPair::new(Box::new(io::Cursor::new(Vec::new())), Box::new(Vec::new()));
        let daemon = DaemonStream::stdio(pair);
        let dur = Some(Duration::from_secs(5));
        daemon.set_read_timeout(dur).unwrap();
        daemon.set_write_timeout(dur).unwrap();
    }

    #[test]
    fn stdio_set_nodelay_is_noop() {
        let pair = StdioPair::new(Box::new(io::Cursor::new(Vec::new())), Box::new(Vec::new()));
        let daemon = DaemonStream::stdio(pair);
        daemon.set_nodelay(true).unwrap();
    }

    #[test]
    fn stdio_shutdown_is_noop() {
        let pair = StdioPair::new(Box::new(io::Cursor::new(Vec::new())), Box::new(Vec::new()));
        let daemon = DaemonStream::stdio(pair);
        daemon.shutdown(Shutdown::Both).unwrap();
    }

    #[test]
    fn stdio_debug_format() {
        let pair = StdioPair::new(Box::new(io::Cursor::new(Vec::new())), Box::new(Vec::new()));
        let daemon = DaemonStream::stdio(pair);
        let debug = format!("{daemon:?}");
        assert!(debug.contains("Stdio"), "got: {debug}");
    }
}
