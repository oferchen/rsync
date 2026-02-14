use std::io;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use socket2::{Domain, Protocol, SockAddr, Socket, Type};

use super::super::DaemonAddress;
use crate::client::{AddressMode, ClientError, SOCKET_IO_EXIT_CODE, daemon_error, socket_error};

/// Establishes a direct TCP connection to an rsync daemon.
///
/// Resolves the daemon address, iterates through candidates filtered by
/// `address_mode`, and returns the first successful connection. I/O timeouts
/// are applied to the resulting stream.
pub(crate) fn connect_direct(
    addr: &DaemonAddress,
    connect_timeout: Option<Duration>,
    io_timeout: Option<Duration>,
    address_mode: AddressMode,
    bind_address: Option<SocketAddr>,
) -> Result<TcpStream, ClientError> {
    let addresses = resolve_daemon_addresses(addr, address_mode)?;
    let mut last_error: Option<(SocketAddr, io::Error)> = None;

    for candidate in addresses {
        match connect_with_optional_bind(candidate, bind_address, connect_timeout) {
            Ok(stream) => {
                if let Some(duration) = io_timeout {
                    stream.set_read_timeout(Some(duration)).map_err(|error| {
                        socket_error("set read timeout on", addr.socket_addr_display(), error)
                    })?;
                    stream.set_write_timeout(Some(duration)).map_err(|error| {
                        socket_error("set write timeout on", addr.socket_addr_display(), error)
                    })?;
                }

                return Ok(stream);
            }
            Err(error) => last_error = Some((candidate, error)),
        }
    }

    let (candidate, error) = last_error.expect("no addresses available for daemon connection");
    Err(socket_error("connect to", candidate, error))
}

/// Resolves a [`DaemonAddress`] to a list of [`SocketAddr`]s, filtered by address family.
///
/// Returns an error when resolution fails or the requested family yields no
/// results.
pub(crate) fn resolve_daemon_addresses(
    addr: &DaemonAddress,
    mode: AddressMode,
) -> Result<Vec<SocketAddr>, ClientError> {
    let iterator = (addr.host.as_str(), addr.port)
        .to_socket_addrs()
        .map_err(|error| {
            socket_error(
                "resolve daemon address for",
                addr.socket_addr_display(),
                error,
            )
        })?;

    let addresses: Vec<SocketAddr> = iterator.collect();

    if addresses.is_empty() {
        return Err(daemon_error(
            format!(
                "daemon host '{}' did not resolve to any addresses",
                addr.host()
            ),
            SOCKET_IO_EXIT_CODE,
        ));
    }

    let filtered = match mode {
        AddressMode::Default => addresses,
        AddressMode::Ipv4 => {
            let retain: Vec<SocketAddr> = addresses
                .into_iter()
                .filter(|candidate| candidate.is_ipv4())
                .collect();
            if retain.is_empty() {
                return Err(daemon_error(
                    format!("daemon host '{}' does not have IPv4 addresses", addr.host()),
                    SOCKET_IO_EXIT_CODE,
                ));
            }
            retain
        }
        AddressMode::Ipv6 => {
            let retain: Vec<SocketAddr> = addresses
                .into_iter()
                .filter(|candidate| candidate.is_ipv6())
                .collect();
            if retain.is_empty() {
                return Err(daemon_error(
                    format!("daemon host '{}' does not have IPv6 addresses", addr.host()),
                    SOCKET_IO_EXIT_CODE,
                ));
            }
            retain
        }
    };

    Ok(filtered)
}

/// Opens a TCP connection to `target`, optionally binding to a local address first.
///
/// When `bind_address` is provided its port is forced to `0` so the OS picks
/// an ephemeral port. A `connect_timeout`, when given, is forwarded to the
/// underlying socket.
pub(crate) fn connect_with_optional_bind(
    target: SocketAddr,
    bind_address: Option<SocketAddr>,
    timeout: Option<Duration>,
) -> io::Result<TcpStream> {
    if let Some(bind) = bind_address {
        if target.is_ipv4() != bind.is_ipv4() {
            return Err(io::Error::new(
                io::ErrorKind::AddrNotAvailable,
                "bind address family does not match target",
            ));
        }

        let domain = if target.is_ipv4() {
            Domain::IPV4
        } else {
            Domain::IPV6
        };

        let socket = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))?;
        let mut bind_addr = bind;
        match &mut bind_addr {
            SocketAddr::V4(addr) => addr.set_port(0),
            SocketAddr::V6(addr) => addr.set_port(0),
        }
        socket.bind(&SockAddr::from(bind_addr))?;

        let target_addr = SockAddr::from(target);
        if let Some(duration) = timeout {
            socket.connect_timeout(&target_addr, duration)?;
        } else {
            socket.connect(&target_addr)?;
        }

        Ok(socket.into())
    } else if let Some(duration) = timeout {
        TcpStream::connect_timeout(&target, duration)
    } else {
        TcpStream::connect(target)
    }
}
