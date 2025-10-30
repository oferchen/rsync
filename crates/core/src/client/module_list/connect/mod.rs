mod direct;
mod program;
mod proxy;

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

pub(crate) fn resolve_connect_timeout(
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

pub(super) enum DaemonStream {
    Tcp(TcpStream),
    Program(ConnectProgramStream),
}

impl DaemonStream {
    fn tcp(stream: TcpStream) -> Self {
        Self::Tcp(stream)
    }

    fn program(stream: ConnectProgramStream) -> Self {
        Self::Program(stream)
    }
}

impl Read for DaemonStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(stream) => stream.read(buf),
            Self::Program(stream) => stream.read(buf),
        }
    }
}

impl Write for DaemonStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(stream) => stream.write(buf),
            Self::Program(stream) => stream.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Tcp(stream) => stream.flush(),
            Self::Program(stream) => stream.flush(),
        }
    }
}
