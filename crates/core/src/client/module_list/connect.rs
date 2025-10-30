use std::ffi::OsStr;
use std::io::{self, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use rsync_protocol::{
    LEGACY_DAEMON_PREFIX, LegacyDaemonMessage, parse_legacy_daemon_message,
    parse_legacy_warning_message,
};
use rsync_transport::negotiate_legacy_daemon_session;
use socket2::{Domain, Protocol, SockAddr, Socket, Type};

use super::{
    auth::{self, DaemonAuthContext, SensitiveBytes},
    errors::{legacy_daemon_error_payload, map_daemon_handshake_error},
    program::{self, ConnectProgramStream},
    proxy::{self, ProxyConfig},
    response::{self, ModuleList, ModuleListEntry},
    util::read_trimmed_line,
};
use crate::client::{
    AddressMode, ClientError, DAEMON_SOCKET_TIMEOUT, DaemonAddress, ModuleListOptions,
    ModuleListRequest, PARTIAL_TRANSFER_EXIT_CODE, SOCKET_IO_EXIT_CODE, TransferTimeout,
    daemon_access_denied_error, daemon_authentication_failed_error,
    daemon_authentication_required_error, daemon_error, daemon_protocol_error, socket_error,
};

/// Performs a daemon module listing by connecting to the supplied address.
///
/// The helper honours the `RSYNC_PROXY` environment variable, establishing an
/// HTTP `CONNECT` tunnel through the specified proxy before negotiating with
/// the daemon when the variable is set. This mirrors the behaviour of
/// upstream rsync.
pub fn run_module_list(request: ModuleListRequest) -> Result<ModuleList, ClientError> {
    run_module_list_with_options(request, ModuleListOptions::default())
}

/// Performs a daemon module listing using caller-provided options.
///
/// This variant mirrors [`run_module_list`] while allowing callers to configure
/// behaviours such as suppressing daemon MOTD lines when `--no-motd` is supplied.
pub fn run_module_list_with_options(
    request: ModuleListRequest,
    options: ModuleListOptions,
) -> Result<ModuleList, ClientError> {
    run_module_list_with_password_and_options(
        request,
        options,
        None,
        TransferTimeout::Default,
        TransferTimeout::Default,
    )
}

/// Performs a daemon module listing using an optional password override.
///
/// When `password_override` is `Some`, the bytes are used for authentication
/// instead of loading `RSYNC_PASSWORD`. This mirrors `--password-file` in the
/// CLI and simplifies testing by avoiding environment manipulation.
pub fn run_module_list_with_password(
    request: ModuleListRequest,
    password_override: Option<Vec<u8>>,
    timeout: TransferTimeout,
) -> Result<ModuleList, ClientError> {
    run_module_list_with_password_and_options(
        request,
        ModuleListOptions::default(),
        password_override,
        timeout,
        TransferTimeout::Default,
    )
}

/// Performs a daemon module listing with the supplied options and password override.
///
/// The helper is primarily used by the CLI to honour flags such as `--no-motd`
/// while still exercising the optional password override path used for
/// `--password-file`. The [`ModuleListOptions`] parameter defaults to the same
/// behaviour as [`run_module_list`].
pub fn run_module_list_with_password_and_options(
    request: ModuleListRequest,
    options: ModuleListOptions,
    password_override: Option<Vec<u8>>,
    timeout: TransferTimeout,
    connect_timeout: TransferTimeout,
) -> Result<ModuleList, ClientError> {
    let addr = request.address();
    let username = request.username().map(str::to_owned);
    let mut password_bytes = password_override.map(SensitiveBytes::new);
    let mut auth_attempted = false;
    let mut auth_context: Option<DaemonAuthContext> = None;
    let suppress_motd = options.suppresses_motd();
    let address_mode = options.address_mode();

    let effective_timeout = timeout.effective(DAEMON_SOCKET_TIMEOUT);
    let connect_duration = resolve_connect_timeout(connect_timeout, timeout, DAEMON_SOCKET_TIMEOUT);

    let stream = open_daemon_stream(
        addr,
        connect_duration,
        effective_timeout,
        address_mode,
        options.connect_program(),
        options.bind_address(),
    )?;

    let handshake = negotiate_legacy_daemon_session(stream, request.protocol())
        .map_err(|error| map_daemon_handshake_error(error, addr))?;
    let stream = handshake.into_stream();
    let mut reader = BufReader::new(stream);

    reader
        .get_mut()
        .write_all(b"#list\n")
        .map_err(|error| socket_error("write to", addr.socket_addr_display(), error))?;
    reader
        .get_mut()
        .flush()
        .map_err(|error| socket_error("flush", addr.socket_addr_display(), error))?;

    let mut entries = Vec::new();
    let mut motd = Vec::new();
    let mut warnings = Vec::new();
    let mut capabilities = Vec::new();
    let mut acknowledged = false;

    while let Some(line) = read_trimmed_line(&mut reader)
        .map_err(|error| socket_error("read from", addr.socket_addr_display(), error))?
    {
        if let Some(payload) = legacy_daemon_error_payload(&line) {
            return Err(daemon_error(payload, PARTIAL_TRANSFER_EXIT_CODE));
        }

        if let Some(payload) = parse_legacy_warning_message(&line) {
            warnings.push(payload.to_string());
            continue;
        }

        if line.starts_with(LEGACY_DAEMON_PREFIX) {
            match parse_legacy_daemon_message(&line) {
                Ok(LegacyDaemonMessage::Ok) => {
                    acknowledged = true;
                    continue;
                }
                Ok(LegacyDaemonMessage::Exit) => break,
                Ok(LegacyDaemonMessage::Capabilities { flags }) => {
                    capabilities.push(flags.to_string());
                    continue;
                }
                Ok(LegacyDaemonMessage::AuthRequired { module }) => {
                    if auth_attempted {
                        return Err(daemon_protocol_error(
                            "daemon repeated authentication challenge",
                        ));
                    }

                    let username = username.as_deref().ok_or_else(|| {
                        daemon_authentication_required_error(
                            "supply a username in the daemon URL (e.g. rsync://user@host/)",
                        )
                    })?;

                    let secret = if let Some(secret) = password_bytes.as_ref() {
                        secret.to_vec()
                    } else {
                        password_bytes = auth::load_daemon_password().map(SensitiveBytes::new);
                        password_bytes
                            .as_ref()
                            .map(SensitiveBytes::to_vec)
                            .ok_or_else(|| {
                                daemon_authentication_required_error(
                                    "set RSYNC_PASSWORD before contacting authenticated daemons",
                                )
                            })?
                    };

                    let context = DaemonAuthContext::new(username.to_owned(), secret);
                    if let Some(challenge) = module {
                        auth::send_daemon_auth_credentials(&mut reader, &context, challenge, addr)?;
                    }

                    auth_context = Some(context);
                    auth_attempted = true;
                    continue;
                }
                Ok(LegacyDaemonMessage::AuthChallenge { challenge }) => {
                    let context = auth_context.as_ref().ok_or_else(|| {
                        daemon_protocol_error(
                            "daemon issued authentication challenge before requesting credentials",
                        )
                    })?;

                    auth::send_daemon_auth_credentials(&mut reader, context, challenge, addr)?;
                    continue;
                }
                Ok(LegacyDaemonMessage::Other(payload)) => {
                    if let Some(reason) = payload.strip_prefix("DENIED") {
                        return Err(daemon_access_denied_error(reason.trim()));
                    }

                    if let Some(reason) = payload.strip_prefix("AUTHFAILED") {
                        let reason = reason.trim();
                        return Err(daemon_authentication_failed_error(if reason.is_empty() {
                            None
                        } else {
                            Some(reason)
                        }));
                    }

                    if response::is_motd_payload(payload) {
                        if !suppress_motd {
                            motd.push(response::normalize_motd_payload(payload));
                        }
                        continue;
                    }

                    return Err(daemon_protocol_error(&line));
                }
                Ok(LegacyDaemonMessage::Version(_)) => {
                    return Err(daemon_protocol_error(&line));
                }
                Err(_) => {
                    return Err(daemon_protocol_error(&line));
                }
            }
        }

        if !acknowledged {
            return Err(daemon_protocol_error(&line));
        }

        entries.push(ModuleListEntry::from_line(&line));
    }

    if !acknowledged {
        return Err(daemon_protocol_error(
            "daemon did not acknowledge module listing",
        ));
    }

    Ok(ModuleList::new(motd, warnings, capabilities, entries))
}

fn open_daemon_stream(
    addr: &DaemonAddress,
    connect_timeout: Option<Duration>,
    io_timeout: Option<Duration>,
    address_mode: AddressMode,
    connect_program: Option<&OsStr>,
    bind_address: Option<SocketAddr>,
) -> Result<DaemonStream, ClientError> {
    if let Some(program) = program::load_daemon_connect_program(connect_program)? {
        return Ok(DaemonStream::program(program::spawn_connect_program(
            addr, &program,
        )?));
    }

    let stream = match proxy::load_daemon_proxy()? {
        Some(proxy) => connect_via_proxy(addr, &proxy, connect_timeout, io_timeout, bind_address)?,
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

pub(super) fn connect_direct(
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

pub(super) fn resolve_daemon_addresses(
    addr: &DaemonAddress,
    mode: AddressMode,
) -> Result<Vec<SocketAddr>, ClientError> {
    let iterator = (addr.host(), addr.port())
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

pub(super) fn connect_via_proxy(
    addr: &DaemonAddress,
    proxy: &ProxyConfig,
    connect_timeout: Option<Duration>,
    io_timeout: Option<Duration>,
    bind_address: Option<SocketAddr>,
) -> Result<TcpStream, ClientError> {
    proxy::connect_via_proxy(addr, proxy, connect_timeout, io_timeout, bind_address)
}

#[cfg(test)]
pub(super) fn establish_proxy_tunnel(
    stream: &mut TcpStream,
    addr: &DaemonAddress,
    proxy: &ProxyConfig,
) -> Result<(), ClientError> {
    proxy::establish_proxy_tunnel(stream, addr, proxy)
}

pub(super) fn resolve_connect_timeout(
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

pub(super) fn connect_with_optional_bind(
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

enum DaemonStream {
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
