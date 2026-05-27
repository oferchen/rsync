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

    Ok(DaemonStream::Tls(tls_stream))
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
pub(super) enum DaemonStream {
    /// Plain TCP connection.
    Tcp(TcpStream),
    /// Connection via an external connect program.
    Program(ConnectProgramStream),
    /// TLS-wrapped TCP connection (requires `client-tls` feature).
    #[cfg(feature = "client-tls")]
    Tls(tls::TlsStream),
}

impl DaemonStream {
    const fn tcp(stream: TcpStream) -> Self {
        Self::Tcp(stream)
    }

    const fn program(stream: ConnectProgramStream) -> Self {
        Self::Program(stream)
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
