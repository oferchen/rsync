mod direct;
mod program;
mod proxy;
#[cfg(feature = "client-tls")]
pub(crate) mod tls;

use std::ffi::OsStr;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use super::super::{AddressMode, ClientError, TransferTimeout};
use super::DaemonAddress;
pub(crate) use direct::{connect_direct, resolve_daemon_addresses};
pub(crate) use program::ConnectProgramConfig;
use program::ConnectProgramStream;
pub(crate) use proxy::{
    ProxyConfig, ProxyCredentials, connect_via_proxy, establish_proxy_tunnel, load_daemon_proxy,
    parse_proxy_spec,
};

/// Read half of a [`DaemonStream`] after splitting.
pub(crate) enum DaemonStreamReader {
    /// Cloned TCP socket used for reading.
    Tcp(TcpStream),
    /// Child process stdout.
    Program(std::process::ChildStdout),
    /// TLS read half (full stream - TLS does not support independent halves).
    #[cfg(feature = "client-tls")]
    Tls(Box<tls::TlsStream>),
}

impl Read for DaemonStreamReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(stream) => stream.read(buf),
            Self::Program(stdout) => stdout.read(buf),
            #[cfg(feature = "client-tls")]
            Self::Tls(stream) => stream.read(buf),
        }
    }
}

/// Write half of a [`DaemonStream`] after splitting.
pub(crate) enum DaemonStreamWriter {
    /// Original TCP socket used for writing.
    Tcp(TcpStream),
    /// Child process stdin.
    Program(std::process::ChildStdin),
    /// TLS has no split - the writer variant is unused (see `DaemonStreamReader::Tls`).
    #[cfg(feature = "client-tls")]
    Tls(std::convert::Infallible),
}

impl Write for DaemonStreamWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(stream) => stream.write(buf),
            Self::Program(stdin) => stdin.write(buf),
            #[cfg(feature = "client-tls")]
            Self::Tls(infallible) => match *infallible {},
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Tcp(stream) => stream.flush(),
            Self::Program(stdin) => stdin.flush(),
            #[cfg(feature = "client-tls")]
            Self::Tls(infallible) => match *infallible {},
        }
    }
}

/// Opens a plain TCP connection to a daemon.
///
/// Respects `RSYNC_CONNECT_PROG` and `RSYNC_PROXY` environment
/// variables. For TLS-wrapped connections, use
/// [`open_daemon_stream_tls`] instead.
pub(super) fn open_daemon_stream(
    addr: &DaemonAddress,
    connect_timeout: Option<Duration>,
    io_timeout: Option<Duration>,
    address_mode: AddressMode,
    connect_program: Option<&OsStr>,
    bind_address: Option<SocketAddr>,
) -> Result<DaemonStream, ClientError> {
    if let Some(program) = program::load_daemon_connect_program(connect_program)? {
        return program::connect_via_program(addr, &program);
    }

    let stream = match load_daemon_proxy()? {
        Some(proxy) => {
            proxy::connect_via_proxy(addr, &proxy, connect_timeout, io_timeout, bind_address)?
        }
        None => connect_direct(
            addr,
            connect_timeout,
            io_timeout,
            address_mode,
            bind_address,
        )?,
    };

    Ok(DaemonStream::tcp(stream))
}

/// Opens a connection to a daemon and wraps it in TLS.
///
/// Establishes the TCP connection identically to [`open_daemon_stream`],
/// then performs a TLS handshake using the provided connector. The
/// hostname from `addr` is passed as the SNI server name.
#[cfg(feature = "client-tls")]
pub(super) fn open_daemon_stream_tls(
    addr: &DaemonAddress,
    connect_timeout: Option<Duration>,
    io_timeout: Option<Duration>,
    address_mode: AddressMode,
    bind_address: Option<SocketAddr>,
    connector: &tls::TlsConnector,
) -> Result<DaemonStream, ClientError> {
    use crate::client::socket_error;

    let stream = match load_daemon_proxy()? {
        Some(proxy) => {
            proxy::connect_via_proxy(addr, &proxy, connect_timeout, io_timeout, bind_address)?
        }
        None => connect_direct(
            addr,
            connect_timeout,
            io_timeout,
            address_mode,
            bind_address,
        )?,
    };

    let tls_stream = connector
        .wrap(stream, addr.host())
        .map_err(|e| socket_error("TLS handshake with", addr.socket_addr_display(), e))?;

    Ok(DaemonStream::Tls(Box::new(tls_stream)))
}

pub(crate) const fn resolve_connect_timeout(
    connect_timeout: TransferTimeout,
    fallback: TransferTimeout,
    default: Duration,
) -> Option<Duration> {
    match connect_timeout {
        TransferTimeout::Default => match fallback {
            TransferTimeout::Default => Some(default),
            TransferTimeout::Disabled => None,
            TransferTimeout::Seconds(value) => Some(Duration::from_secs(value.get())),
        },
        TransferTimeout::Disabled => None,
        TransferTimeout::Seconds(value) => Some(Duration::from_secs(value.get())),
    }
}

/// Bidirectional stream to an rsync daemon.
///
/// Abstracts over the underlying transport: plain TCP, a connect program
/// (`RSYNC_CONNECT_PROG`), or a TLS-wrapped TCP connection (when the
/// `client-tls` feature is enabled).
pub(crate) enum DaemonStream {
    /// Plain TCP connection.
    Tcp(TcpStream),
    /// Connection via an external connect program.
    Program(ConnectProgramStream),
    /// TLS-wrapped TCP connection (requires `client-tls` feature).
    ///
    /// Boxed to avoid inflating the enum size for the common non-TLS path.
    #[cfg(feature = "client-tls")]
    Tls(Box<tls::TlsStream>),
}

impl DaemonStream {
    const fn tcp(stream: TcpStream) -> Self {
        Self::Tcp(stream)
    }

    fn program(stream: ConnectProgramStream) -> Self {
        Self::Program(stream)
    }

    /// Returns a reference to the underlying `TcpStream` if this is a TCP
    /// connection. Used for applying socket-level options that only apply
    /// to real sockets (not connect programs or TLS).
    pub(crate) fn as_tcp_stream(&self) -> Option<&TcpStream> {
        match self {
            Self::Tcp(stream) => Some(stream),
            Self::Program(_) => None,
            #[cfg(feature = "client-tls")]
            Self::Tls(_) => None,
        }
    }

    /// Splits the daemon stream into independent read and write halves.
    ///
    /// For TCP, the socket is cloned (separate fd) so reader and writer
    /// can be used concurrently. For connect programs, the child's stdout
    /// and stdin pipes are returned directly.
    ///
    /// Returns `(reader, writer, guard)`. The guard must be held alive for
    /// the duration of the transfer - for connect programs it owns the
    /// `Child` process and kills it on drop.
    pub(crate) fn split(
        self,
    ) -> io::Result<(DaemonStreamReader, DaemonStreamWriter, DaemonStreamGuard)> {
        match self {
            Self::Tcp(stream) => {
                let reader = stream.try_clone()?;
                Ok((
                    DaemonStreamReader::Tcp(reader),
                    DaemonStreamWriter::Tcp(stream),
                    DaemonStreamGuard::None,
                ))
            }
            Self::Program(prog) => {
                let (child, stdin, stdout) = prog.into_parts();
                Ok((
                    DaemonStreamReader::Program(stdout),
                    DaemonStreamWriter::Program(stdin),
                    DaemonStreamGuard::Child(child),
                ))
            }
            #[cfg(feature = "client-tls")]
            Self::Tls(_) => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "TLS stream split not yet supported",
            )),
        }
    }

    /// Configures TCP-specific socket options for the transfer phase.
    ///
    /// Sets TCP_NODELAY and applies read/write timeouts. No-op for
    /// non-TCP transports (connect programs, TLS).
    pub(crate) fn configure_transfer_options(
        &self,
        nodelay: bool,
        timeout: Option<Duration>,
    ) -> io::Result<()> {
        if let Self::Tcp(stream) = self {
            if nodelay {
                stream.set_nodelay(true)?;
            }
            stream.set_read_timeout(timeout)?;
            stream.set_write_timeout(timeout)?;
        }
        Ok(())
    }
}

/// Ownership guard for resources backing a split [`DaemonStream`].
///
/// For connect programs, this holds the `Child` process handle and
/// kills/reaps it on drop. For TCP streams, no guard is needed.
pub(crate) enum DaemonStreamGuard {
    /// No resource to guard (TCP).
    None,
    /// Owns a connect program child process.
    Child(std::process::Child),
}

impl Drop for DaemonStreamGuard {
    fn drop(&mut self) {
        if let Self::Child(child) = self {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Read for DaemonStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(stream) => stream.read(buf),
            Self::Program(stream) => stream.read(buf),
            #[cfg(feature = "client-tls")]
            Self::Tls(stream) => stream.read(buf),
        }
    }
}

impl Write for DaemonStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(stream) => stream.write(buf),
            Self::Program(stream) => stream.write(buf),
            #[cfg(feature = "client-tls")]
            Self::Tls(stream) => stream.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Tcp(stream) => stream.flush(),
            Self::Program(stream) => stream.flush(),
            #[cfg(feature = "client-tls")]
            Self::Tls(stream) => stream.flush(),
        }
    }
}
