fn read_trimmed_line<R: BufRead>(reader: &mut R) -> io::Result<Option<String>> {
    let mut line = String::new();
    let bytes = reader.read_line(&mut line)?;

    if bytes == 0 {
        return Ok(None);
    }

    while line.ends_with('\n') || line.ends_with('\r') {
        line.pop();
    }

    Ok(Some(line))
}

fn legacy_daemon_error_payload(line: &str) -> Option<String> {
    if let Some(payload) = parse_legacy_error_message(line) {
        return Some(payload.to_string());
    }

    let trimmed = line.trim_matches(['\r', '\n']).trim_start();
    let remainder = strip_prefix_ignore_ascii_case(trimmed, "@ERROR")?;

    if remainder
        .chars()
        .next()
        .filter(|ch| *ch != ':' && !ch.is_ascii_whitespace())
        .is_some()
    {
        return None;
    }

    let payload = remainder
        .trim_start_matches(|ch: char| ch == ':' || ch.is_ascii_whitespace())
        .trim();

    Some(payload.to_string())
}

fn map_daemon_handshake_error(error: io::Error, addr: &DaemonAddress) -> ClientError {
    if let Some(mapped) = handshake_error_to_client_error(&error) {
        mapped
    } else {
        match daemon_error_from_invalid_data(&error) {
            Some(mapped) => mapped,
            None => socket_error("negotiate with", addr.socket_addr_display(), error),
        }
    }
}

fn handshake_error_to_client_error(error: &io::Error) -> Option<ClientError> {
    let negotiation_error = error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<NegotiationError>())?;

    if let Some(input) = negotiation_error.malformed_legacy_greeting() {
        if let Some(payload) = legacy_daemon_error_payload(input) {
            return Some(daemon_error(payload, PARTIAL_TRANSFER_EXIT_CODE));
        }

        return Some(daemon_protocol_error(input));
    }

    None
}

fn daemon_error_from_invalid_data(error: &io::Error) -> Option<ClientError> {
    if error.kind() != io::ErrorKind::InvalidData {
        return None;
    }

    let payload_candidates = error
        .get_ref()
        .map(|inner| inner.to_string())
        .into_iter()
        .chain(std::iter::once(error.to_string()));

    for candidate in payload_candidates {
        if let Some(payload) = legacy_daemon_error_payload(&candidate) {
            return Some(daemon_error(payload, PARTIAL_TRANSFER_EXIT_CODE));
        }
    }

    None
}

/// Target daemon address used for module listing requests.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DaemonAddress {
    host: String,
    port: u16,
}

impl DaemonAddress {
    /// Creates a new daemon address from the supplied host and port.
    pub fn new(host: String, port: u16) -> Result<Self, ClientError> {
        let trimmed = host.trim();
        if trimmed.is_empty() {
            return Err(daemon_error(
                "daemon host must be non-empty",
                FEATURE_UNAVAILABLE_EXIT_CODE,
            ));
        }
        Ok(Self {
            host: trimmed.to_string(),
            port,
        })
    }

    /// Returns the daemon host name or address.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Returns the daemon TCP port.
    #[must_use]
    pub const fn port(&self) -> u16 {
        self.port
    }

    fn socket_addr_display(&self) -> SocketAddrDisplay<'_> {
        SocketAddrDisplay {
            host: &self.host,
            port: self.port,
        }
    }
}

struct SocketAddrDisplay<'a> {
    host: &'a str,
    port: u16,
}

impl fmt::Display for SocketAddrDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.host.contains(':') && !self.host.starts_with('[') {
            write!(f, "[{}]:{}", self.host, self.port)
        } else {
            write!(f, "{}:{}", self.host, self.port)
        }
    }
}

/// Specification describing a daemon module listing request parsed from CLI operands.
///
/// The request retains the optional username embedded in the operand so future
/// authentication flows can reuse the caller-supplied identity even though the
/// current module listing implementation performs anonymous queries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleListRequest {
    address: DaemonAddress,
    username: Option<String>,
    protocol: ProtocolVersion,
}

impl ModuleListRequest {
    /// Default TCP port used by rsync daemons when a port is not specified.
    pub const DEFAULT_PORT: u16 = 873;

    /// Attempts to derive a module listing request from CLI-style operands.
    pub fn from_operands(operands: &[OsString]) -> Result<Option<Self>, ClientError> {
        Self::from_operands_with_port(operands, Self::DEFAULT_PORT)
    }

    /// Equivalent to [`Self::from_operands`] but allows overriding the default
    /// daemon port.
    pub fn from_operands_with_port(
        operands: &[OsString],
        default_port: u16,
    ) -> Result<Option<Self>, ClientError> {
        if operands.len() != 1 {
            return Ok(None);
        }

        Self::from_operand(&operands[0], default_port)
    }

    fn from_operand(operand: &OsString, default_port: u16) -> Result<Option<Self>, ClientError> {
        let text = operand.to_string_lossy();

        if let Some(rest) = strip_prefix_ignore_ascii_case(&text, "rsync://") {
            return parse_rsync_url(rest, default_port);
        }

        if let Some((host_part, module_part)) = split_daemon_host_module(&text)? {
            if module_part.is_empty() {
                let target = parse_host_port(host_part, default_port)?;
                return Ok(Some(Self::new(target.address, target.username)));
            }
            return Ok(None);
        }

        Ok(None)
    }

    fn new(address: DaemonAddress, username: Option<String>) -> Self {
        Self {
            address,
            username,
            protocol: ProtocolVersion::NEWEST,
        }
    }

    /// Returns the parsed daemon address.
    #[must_use]
    pub fn address(&self) -> &DaemonAddress {
        &self.address
    }

    /// Returns the optional username supplied in the daemon URL or legacy syntax.
    #[must_use]
    pub fn username(&self) -> Option<&str> {
        self.username.as_deref()
    }

    /// Returns the desired protocol version for daemon negotiation.
    #[must_use]
    pub const fn protocol(&self) -> ProtocolVersion {
        self.protocol
    }

    /// Returns a new request that clamps the negotiation to the provided protocol.
    #[must_use]
    pub const fn with_protocol(mut self, protocol: ProtocolVersion) -> Self {
        self.protocol = protocol;
        self
    }
}

/// Configuration toggles that influence daemon module listings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleListOptions {
    suppress_motd: bool,
    address_mode: AddressMode,
    connect_program: Option<OsString>,
    bind_address: Option<SocketAddr>,
}

impl ModuleListOptions {
    /// Creates a new options structure with all toggles disabled.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            suppress_motd: false,
            address_mode: AddressMode::Default,
            connect_program: None,
            bind_address: None,
        }
    }

    /// Returns a new configuration that suppresses daemon MOTD lines.
    #[must_use]
    pub const fn suppress_motd(mut self, suppress: bool) -> Self {
        self.suppress_motd = suppress;
        self
    }

    /// Returns whether MOTD lines should be suppressed.
    #[must_use]
    pub const fn suppresses_motd(&self) -> bool {
        self.suppress_motd
    }

    /// Configures the preferred address family for the daemon connection.
    #[must_use]
    #[doc(alias = "--ipv4")]
    #[doc(alias = "--ipv6")]
    pub const fn with_address_mode(mut self, mode: AddressMode) -> Self {
        self.address_mode = mode;
        self
    }

    /// Returns the preferred address family.
    #[must_use]
    pub const fn address_mode(&self) -> AddressMode {
        self.address_mode
    }

    /// Supplies an explicit connect program command.
    #[must_use]
    #[doc(alias = "--connect-program")]
    pub fn with_connect_program(mut self, program: Option<OsString>) -> Self {
        self.connect_program = program;
        self
    }

    /// Returns the configured connect program command, if any.
    #[must_use]
    pub fn connect_program(&self) -> Option<&OsStr> {
        self.connect_program.as_deref()
    }

    /// Configures the bind address used when contacting the daemon directly or via a proxy.
    #[must_use]
    pub const fn with_bind_address(mut self, address: Option<SocketAddr>) -> Self {
        self.bind_address = address;
        self
    }

    /// Returns the configured bind address, if any.
    #[must_use]
    pub const fn bind_address(&self) -> Option<SocketAddr> {
        self.bind_address
    }
}

impl Default for ModuleListOptions {
    fn default() -> Self {
        Self::new()
    }
}

fn parse_rsync_url(
    rest: &str,
    default_port: u16,
) -> Result<Option<ModuleListRequest>, ClientError> {
    let mut parts = rest.splitn(2, '/');
    let host_port = parts.next().unwrap_or("");
    let remainder = parts.next();

    if remainder.is_some_and(|path| !path.is_empty()) {
        return Ok(None);
    }

    let target = parse_host_port(host_port, default_port)?;
    Ok(Some(ModuleListRequest::new(
        target.address,
        target.username,
    )))
}

struct ParsedDaemonTarget {
    address: DaemonAddress,
    username: Option<String>,
}

fn parse_host_port(input: &str, default_port: u16) -> Result<ParsedDaemonTarget, ClientError> {
    const DEFAULT_HOST: &str = "localhost";

    let (username, input) = split_daemon_username(input)?;
    let username = username.map(decode_daemon_username).transpose()?;

    if input.is_empty() {
        let address = DaemonAddress::new(DEFAULT_HOST.to_string(), default_port)?;
        return Ok(ParsedDaemonTarget { address, username });
    }

    if let Some(host) = input.strip_prefix('[') {
        let (address, port) = parse_bracketed_host(host, default_port)?;
        let address = DaemonAddress::new(address, port)?;
        return Ok(ParsedDaemonTarget { address, username });
    }

    if let Some((host, port)) = split_host_port(input) {
        let port = port
            .parse::<u16>()
            .map_err(|_| daemon_error("invalid daemon port", FEATURE_UNAVAILABLE_EXIT_CODE))?;
        let host = decode_host_component(host)?;
        let address = DaemonAddress::new(host, port)?;
        return Ok(ParsedDaemonTarget { address, username });
    }

    let host = decode_host_component(input)?;
    let address = DaemonAddress::new(host, default_port)?;
    Ok(ParsedDaemonTarget { address, username })
}

fn split_daemon_host_module(input: &str) -> Result<Option<(&str, &str)>, ClientError> {
    if !input.contains('[') {
        let segments = input.split("::");
        if segments.clone().count() > 2 {
            return Err(daemon_error(
                "IPv6 daemon addresses must be enclosed in brackets",
                FEATURE_UNAVAILABLE_EXIT_CODE,
            ));
        }
    }

    let mut in_brackets = false;
    let mut previous_colon = None;

    for (idx, ch) in input.char_indices() {
        match ch {
            '[' => {
                in_brackets = true;
                previous_colon = None;
            }
            ']' => {
                in_brackets = false;
                previous_colon = None;
            }
            ':' if !in_brackets => {
                if let Some(prev) = previous_colon.filter(|prev| *prev + 1 == idx) {
                    let host = &input[..prev];
                    if !host.contains('[') {
                        let colon_count = host.chars().filter(|&ch| ch == ':').count();
                        if colon_count > 1 {
                            return Err(daemon_error(
                                "IPv6 daemon addresses must be enclosed in brackets",
                                FEATURE_UNAVAILABLE_EXIT_CODE,
                            ));
                        }
                    }
                    let module = &input[idx + 1..];
                    return Ok(Some((host, module)));
                }
                previous_colon = Some(idx);
            }
            _ => {
                previous_colon = None;
            }
        }
    }

    Ok(None)
}

fn strip_prefix_ignore_ascii_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    if text.len() < prefix.len() {
        return None;
    }

    let (candidate, remainder) = text.split_at(prefix.len());
    if candidate.eq_ignore_ascii_case(prefix) {
        Some(remainder)
    } else {
        None
    }
}

fn parse_bracketed_host(host: &str, default_port: u16) -> Result<(String, u16), ClientError> {
    let (addr, remainder) = host.split_once(']').ok_or_else(|| {
        daemon_error(
            "invalid bracketed daemon host",
            FEATURE_UNAVAILABLE_EXIT_CODE,
        )
    })?;

    let decoded = decode_host_component(addr)?;

    if remainder.is_empty() {
        return Ok((decoded, default_port));
    }

    let port = remainder
        .strip_prefix(':')
        .ok_or_else(|| {
            daemon_error(
                "invalid bracketed daemon host",
                FEATURE_UNAVAILABLE_EXIT_CODE,
            )
        })?
        .parse::<u16>()
        .map_err(|_| daemon_error("invalid daemon port", FEATURE_UNAVAILABLE_EXIT_CODE))?;

    Ok((decoded, port))
}

fn decode_host_component(input: &str) -> Result<String, ClientError> {
    decode_percent_component(
        input,
        invalid_percent_encoding_error,
        invalid_host_utf8_error,
    )
}

fn decode_daemon_username(input: &str) -> Result<String, ClientError> {
    decode_percent_component(
        input,
        invalid_username_percent_encoding_error,
        invalid_username_utf8_error,
    )
}

fn decode_percent_component(
    input: &str,
    truncated_error: fn() -> ClientError,
    invalid_utf8_error: fn() -> ClientError,
) -> Result<String, ClientError> {
    if !input.contains('%') {
        return Ok(input.to_string());
    }

    let mut decoded = Vec::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err(truncated_error());
            }

            let hi = hex_value(bytes[index + 1]);
            let lo = hex_value(bytes[index + 2]);

            if let (Some(hi), Some(lo)) = (hi, lo) {
                decoded.push((hi << 4) | lo);
                index += 3;
                continue;
            }
        }

        decoded.push(bytes[index]);
        index += 1;
    }

    String::from_utf8(decoded).map_err(|_| invalid_utf8_error())
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(10 + byte - b'a'),
        b'A'..=b'F' => Some(10 + byte - b'A'),
        _ => None,
    }
}

fn invalid_percent_encoding_error() -> ClientError {
    daemon_error(
        "invalid percent-encoding in daemon host",
        FEATURE_UNAVAILABLE_EXIT_CODE,
    )
}

fn invalid_host_utf8_error() -> ClientError {
    daemon_error(
        "daemon host contains invalid UTF-8 after percent-decoding",
        FEATURE_UNAVAILABLE_EXIT_CODE,
    )
}

fn invalid_username_percent_encoding_error() -> ClientError {
    daemon_error(
        "invalid percent-encoding in daemon username",
        FEATURE_UNAVAILABLE_EXIT_CODE,
    )
}

fn invalid_username_utf8_error() -> ClientError {
    daemon_error(
        "daemon username contains invalid UTF-8 after percent-decoding",
        FEATURE_UNAVAILABLE_EXIT_CODE,
    )
}

fn split_host_port(input: &str) -> Option<(&str, &str)> {
    let idx = input.rfind(':')?;
    let (host, port) = input.split_at(idx);
    if host.contains(':') {
        return None;
    }
    Some((host, &port[1..]))
}

fn split_daemon_username(input: &str) -> Result<(Option<&str>, &str), ClientError> {
    if let Some(idx) = input.rfind('@') {
        let (user, host) = input.split_at(idx);
        if user.is_empty() {
            return Err(daemon_error(
                "daemon username must be non-empty",
                FEATURE_UNAVAILABLE_EXIT_CODE,
            ));
        }

        return Ok((Some(user), &host[1..]));
    }

    Ok((None, input))
}

/// Describes the module entries advertised by a daemon together with ancillary metadata.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ModuleList {
    motd: Vec<String>,
    warnings: Vec<String>,
    capabilities: Vec<String>,
    entries: Vec<ModuleListEntry>,
}

impl ModuleList {
    /// Creates a new list from the supplied entries, warning lines, MOTD lines, and capability advertisements.
    fn new(
        motd: Vec<String>,
        warnings: Vec<String>,
        capabilities: Vec<String>,
        entries: Vec<ModuleListEntry>,
    ) -> Self {
        Self {
            motd,
            warnings,
            capabilities,
            entries,
        }
    }

    /// Returns the advertised module entries.
    #[must_use]
    pub fn entries(&self) -> &[ModuleListEntry] {
        &self.entries
    }

    /// Returns the optional message-of-the-day lines emitted by the daemon.
    #[must_use]
    pub fn motd_lines(&self) -> &[String] {
        &self.motd
    }

    /// Returns the warning messages emitted by the daemon while processing the request.
    #[must_use]
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Returns the capability strings advertised by the daemon via `@RSYNCD: CAP` lines.
    #[must_use]
    pub fn capabilities(&self) -> &[String] {
        &self.capabilities
    }
}

/// Entry describing a single daemon module.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleListEntry {
    name: String,
    comment: Option<String>,
}

impl ModuleListEntry {
    fn from_line(line: &str) -> Self {
        match line.split_once('\t') {
            Some((name, comment)) => Self {
                name: name.to_string(),
                comment: if comment.is_empty() {
                    None
                } else {
                    Some(comment.to_string())
                },
            },
            None => Self {
                name: line.to_string(),
                comment: None,
            },
        }
    }

    /// Returns the module name advertised by the daemon.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the optional comment associated with the module.
    #[must_use]
    pub fn comment(&self) -> Option<&str> {
        self.comment.as_deref()
    }
}

/// Performs a daemon module listing by connecting to the supplied address.
///
/// The helper honours the `RSYNC_PROXY` environment variable, establishing an
/// HTTP `CONNECT` tunnel through the specified proxy before negotiating with
/// the daemon when the variable is set. This mirrors the behaviour of
/// upstream rsync.
pub fn run_module_list(request: ModuleListRequest) -> Result<ModuleList, ClientError> {
    run_module_list_with_options(request, ModuleListOptions::default())
}

fn open_daemon_stream(
    addr: &DaemonAddress,
    connect_timeout: Option<Duration>,
    io_timeout: Option<Duration>,
    address_mode: AddressMode,
    connect_program: Option<&OsStr>,
    bind_address: Option<SocketAddr>,
) -> Result<DaemonStream, ClientError> {
    if let Some(program) = load_daemon_connect_program(connect_program)? {
        return connect_via_program(addr, &program);
    }

    let stream = match load_daemon_proxy()? {
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

fn connect_direct(
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

fn resolve_daemon_addresses(
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

fn connect_with_optional_bind(
    target: SocketAddr,
    bind_address: Option<SocketAddr>,
    timeout: Option<Duration>,
) -> io::Result<TcpStream> {
    if let Some(bind) = bind_address {
        if target.is_ipv4() != bind.is_ipv4() {
            return Err(io::Error::new(
                ErrorKind::AddrNotAvailable,
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

fn connect_via_proxy(
    addr: &DaemonAddress,
    proxy: &ProxyConfig,
    connect_timeout: Option<Duration>,
    io_timeout: Option<Duration>,
    bind_address: Option<SocketAddr>,
) -> Result<TcpStream, ClientError> {
    let target = (proxy.host.as_str(), proxy.port);
    let addrs = target
        .to_socket_addrs()
        .map_err(|error| socket_error("resolve proxy address for", proxy.display(), error))?;

    let mut last_error: Option<(SocketAddr, io::Error)> = None;
    let mut stream_result: Option<TcpStream> = None;

    for candidate in addrs {
        match connect_with_optional_bind(candidate, bind_address, connect_timeout) {
            Ok(stream) => {
                stream_result = Some(stream);
                break;
            }
            Err(error) => last_error = Some((candidate, error)),
        }
    }

    let mut stream = if let Some(stream) = stream_result {
        stream
    } else if let Some((candidate, error)) = last_error {
        return Err(socket_error("connect to", candidate, error));
    } else {
        return Err(socket_error(
            "resolve proxy address for",
            proxy.display(),
            io::Error::new(
                ErrorKind::AddrNotAvailable,
                "proxy resolution returned no addresses",
            ),
        ));
    };

    establish_proxy_tunnel(&mut stream, addr, proxy)?;

    if let Some(duration) = io_timeout {
        stream
            .set_read_timeout(Some(duration))
            .map_err(|error| socket_error("configure", proxy.display(), error))?;
        stream
            .set_write_timeout(Some(duration))
            .map_err(|error| socket_error("configure", proxy.display(), error))?;
    }

    Ok(stream)
}

fn establish_proxy_tunnel(
    stream: &mut TcpStream,
    addr: &DaemonAddress,
    proxy: &ProxyConfig,
) -> Result<(), ClientError> {
    let mut request = format!("CONNECT {}:{} HTTP/1.0\r\n", addr.host(), addr.port());

    if let Some(header) = proxy.authorization_header() {
        request.push_str("Proxy-Authorization: Basic ");
        request.push_str(&header);
        request.push_str("\r\n");
    }

    request.push_str("\r\n");

    stream
        .write_all(request.as_bytes())
        .map_err(|error| socket_error("write to", proxy.display(), error))?;
    stream
        .flush()
        .map_err(|error| socket_error("flush", proxy.display(), error))?;

    let mut line = Vec::with_capacity(128);
    read_proxy_line(stream, &mut line, proxy.display())?;
    let status = String::from_utf8(line.clone())
        .map_err(|_| proxy_response_error("proxy status line contained invalid UTF-8"))?;
    line.clear();

    let trimmed_status = status.trim_start_matches([' ', '\t']);
    if !trimmed_status
        .get(..5)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("HTTP/"))
    {
        return Err(proxy_response_error(format!(
            "proxy response did not start with HTTP/: {status}"
        )));
    }

    let mut parts = trimmed_status.split_whitespace();
    let _ = parts.next();
    let code = parts.next().ok_or_else(|| {
        proxy_response_error(format!("proxy response missing status code: {status}"))
    })?;

    if !code.starts_with('2') {
        return Err(proxy_response_error(format!(
            "proxy rejected CONNECT with status {status}"
        )));
    }

    loop {
        read_proxy_line(stream, &mut line, proxy.display())?;
        if line.is_empty() {
            break;
        }
    }

    Ok(())
}

fn connect_via_program(
    addr: &DaemonAddress,
    program: &ConnectProgramConfig,
) -> Result<DaemonStream, ClientError> {
    let command = program
        .format_command(addr.host(), addr.port())
        .map_err(|error| daemon_error(error, FEATURE_UNAVAILABLE_EXIT_CODE))?;

    let shell = program
        .shell()
        .cloned()
        .unwrap_or_else(|| OsString::from("sh"));

    let mut builder = Command::new(&shell);
    builder.arg("-c").arg(&command);
    builder.stdin(Stdio::piped());
    builder.stdout(Stdio::piped());
    builder.stderr(Stdio::inherit());
    builder.env("RSYNC_PORT", addr.port().to_string());

    let mut child = builder.spawn().map_err(|error| {
        daemon_error(
            format!(
                "failed to spawn RSYNC_CONNECT_PROG using shell '{}': {error}",
                Path::new(&shell).display()
            ),
            FEATURE_UNAVAILABLE_EXIT_CODE,
        )
    })?;

    let stdin = child.stdin.take().ok_or_else(|| {
        daemon_error(
            "RSYNC_CONNECT_PROG command did not expose a writable stdin",
            FEATURE_UNAVAILABLE_EXIT_CODE,
        )
    })?;

    let stdout = child.stdout.take().ok_or_else(|| {
        daemon_error(
            "RSYNC_CONNECT_PROG command did not expose a readable stdout",
            FEATURE_UNAVAILABLE_EXIT_CODE,
        )
    })?;

    Ok(DaemonStream::program(ConnectProgramStream::new(
        child, stdin, stdout,
    )))
}

fn load_daemon_connect_program(
    override_template: Option<&OsStr>,
) -> Result<Option<ConnectProgramConfig>, ClientError> {
    if let Some(template) = override_template {
        if template.is_empty() {
            return Err(connect_program_configuration_error(
                "the --connect-program option requires a non-empty command",
            ));
        }

        let shell = env::var_os("RSYNC_SHELL").filter(|value| !value.is_empty());
        return ConnectProgramConfig::new(OsString::from(template), shell)
            .map(Some)
            .map_err(connect_program_configuration_error);
    }

    let Some(template) = env::var_os("RSYNC_CONNECT_PROG") else {
        return Ok(None);
    };

    if template.is_empty() {
        return Err(connect_program_configuration_error(
            "RSYNC_CONNECT_PROG must not be empty",
        ));
    }

    let shell = env::var_os("RSYNC_SHELL").filter(|value| !value.is_empty());

    ConnectProgramConfig::new(template, shell)
        .map(Some)
        .map_err(connect_program_configuration_error)
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

struct ConnectProgramConfig {
    template: OsString,
    shell: Option<OsString>,
}

impl ConnectProgramConfig {
    fn new(template: OsString, shell: Option<OsString>) -> Result<Self, String> {
        if template.is_empty() {
            return Err("RSYNC_CONNECT_PROG must not be empty".to_string());
        }

        if shell.as_ref().is_some_and(|value| value.is_empty()) {
            return Err("RSYNC_SHELL must not be empty".to_string());
        }

        Ok(Self { template, shell })
    }

    fn shell(&self) -> Option<&OsString> {
        self.shell.as_ref()
    }

    /// Expands the configured template by substituting daemon metadata placeholders.
    ///
    /// `%H` is replaced with the daemon host, `%P` with the decimal TCP port, and `%%`
    /// yields a literal percent sign.
    fn format_command(&self, host: &str, port: u16) -> Result<OsString, String> {
        #[cfg(unix)]
        {
            let template = self.template.as_bytes();
            let mut rendered = Vec::with_capacity(template.len() + host.len());
            let mut iter = template.iter().copied();
            let host_bytes = host.as_bytes();
            let port_string = port.to_string();
            let port_bytes = port_string.as_bytes();

            while let Some(byte) = iter.next() {
                if byte == b'%' {
                    match iter.next() {
                        Some(b'%') => rendered.push(b'%'),
                        Some(b'H') => rendered.extend_from_slice(host_bytes),
                        Some(b'P') => rendered.extend_from_slice(port_bytes),
                        Some(other) => {
                            rendered.push(b'%');
                            rendered.push(other);
                        }
                        None => rendered.push(b'%'),
                    }
                } else {
                    rendered.push(byte);
                }
            }

            Ok(OsString::from_vec(rendered))
        }

        #[cfg(not(unix))]
        {
            let template = self.template.as_os_str().to_string_lossy();
            let mut rendered = String::with_capacity(template.len() + host.len());
            let mut chars = template.chars();
            let port_string = port.to_string();

            while let Some(ch) = chars.next() {
                if ch == '%' {
                    match chars.next() {
                        Some('%') => rendered.push('%'),
                        Some('H') => rendered.push_str(host),
                        Some('P') => rendered.push_str(&port_string),
                        Some(other) => {
                            rendered.push('%');
                            rendered.push(other);
                        }
                        None => rendered.push('%'),
                    }
                } else {
                    rendered.push(ch);
                }
            }

            Ok(OsString::from(rendered))
        }
    }
}

struct ConnectProgramStream {
    child: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
}

impl ConnectProgramStream {
    fn new(child: Child, stdin: ChildStdin, stdout: ChildStdout) -> Self {
        Self {
            child,
            stdin,
            stdout,
        }
    }
}

impl Read for ConnectProgramStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.stdout.read(buf)
    }
}

impl Write for ConnectProgramStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.stdin.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stdin.flush()
    }
}

impl Drop for ConnectProgramStream {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct ProxyConfig {
    host: String,
    port: u16,
    credentials: Option<ProxyCredentials>,
}

impl ProxyConfig {
    fn display(&self) -> SocketAddrDisplay<'_> {
        SocketAddrDisplay {
            host: &self.host,
            port: self.port,
        }
    }

    fn authorization_header(&self) -> Option<String> {
        self.credentials
            .as_ref()
            .map(ProxyCredentials::authorization_value)
    }
}

struct ProxyCredentials {
    username: String,
    password: String,
}

impl ProxyCredentials {
    fn new(username: String, password: String) -> Self {
        Self { username, password }
    }

    fn authorization_value(&self) -> String {
        let mut bytes = Vec::with_capacity(self.username.len() + self.password.len() + 1);
        bytes.extend_from_slice(self.username.as_bytes());
        bytes.push(b':');
        bytes.extend_from_slice(self.password.as_bytes());
        STANDARD.encode(bytes)
    }
}

fn load_daemon_proxy() -> Result<Option<ProxyConfig>, ClientError> {
    match env::var("RSYNC_PROXY") {
        Ok(value) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            parse_proxy_spec(trimmed).map(Some)
        }
        Err(VarError::NotPresent) => Ok(None),
        Err(VarError::NotUnicode(_)) => Err(proxy_configuration_error(
            "RSYNC_PROXY value must be valid UTF-8",
        )),
    }
}

fn parse_proxy_spec(spec: &str) -> Result<ProxyConfig, ClientError> {
    let trimmed = spec.trim();
    if trimmed.is_empty() {
        return Err(proxy_configuration_error(
            "RSYNC_PROXY must specify a proxy host",
        ));
    }

    let mut remainder = trimmed;
    if let Some(idx) = trimmed.find("://") {
        let (scheme, rest_with_separator) = trimmed.split_at(idx);
        let rest = &rest_with_separator[3..];
        if rest.is_empty() {
            return Err(proxy_configuration_error(
                "RSYNC_PROXY must specify a proxy host",
            ));
        }

        if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
            return Err(proxy_configuration_error(
                "RSYNC_PROXY scheme must be http:// or https://",
            ));
        }

        remainder = rest;
    }

    if remainder.contains('/') {
        return Err(proxy_configuration_error(
            "RSYNC_PROXY must not include a path component",
        ));
    }

    let (credentials, remainder) = if let Some(idx) = remainder.rfind('@') {
        let (userinfo, host_part) = remainder.split_at(idx);
        if userinfo.is_empty() {
            return Err(proxy_configuration_error(
                "RSYNC_PROXY user information must be non-empty when '@' is present",
            ));
        }

        let mut segments = userinfo.splitn(2, ':');
        let username = segments.next().unwrap();
        let password = segments.next().ok_or_else(|| {
            proxy_configuration_error("RSYNC_PROXY credentials must use USER:PASS@HOST:PORT format")
        })?;

        let username = decode_proxy_component(username, "username")?;
        let password = decode_proxy_component(password, "password")?;
        let credentials = ProxyCredentials::new(username, password);
        (Some(credentials), &host_part[1..])
    } else {
        (None, remainder)
    };

    let (host, port) = parse_proxy_host_port(remainder)?;

    Ok(ProxyConfig {
        host,
        port,
        credentials,
    })
}

fn parse_proxy_host_port(input: &str) -> Result<(String, u16), ClientError> {
    if input.is_empty() {
        return Err(proxy_configuration_error(
            "RSYNC_PROXY must specify a proxy host and port",
        ));
    }

    if let Some(rest) = input.strip_prefix('[') {
        let (host, port) = parse_bracketed_host(rest, 0).map_err(|_| {
            proxy_configuration_error("RSYNC_PROXY contains an invalid bracketed host")
        })?;
        if port == 0 {
            return Err(proxy_configuration_error(
                "RSYNC_PROXY bracketed host must include a port",
            ));
        }
        return Ok((host, port));
    }

    let idx = input
        .rfind(':')
        .ok_or_else(|| proxy_configuration_error("RSYNC_PROXY must be in HOST:PORT form"))?;
    let host = &input[..idx];
    let port_text = &input[idx + 1..];

    if port_text.is_empty() {
        return Err(proxy_configuration_error(
            "RSYNC_PROXY must include a proxy port",
        ));
    }

    let host = decode_host_component(host).map_err(|_| {
        proxy_configuration_error("RSYNC_PROXY proxy host contains invalid percent-encoding")
    })?;
    let port = port_text
        .parse::<u16>()
        .map_err(|_| proxy_configuration_error("RSYNC_PROXY specified an invalid port"))?;

    Ok((host, port))
}

fn decode_proxy_component(input: &str, field: &str) -> Result<String, ClientError> {
    if !input.contains('%') {
        return Ok(input.to_string());
    }

    let mut decoded = Vec::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err(proxy_configuration_error(format!(
                    "RSYNC_PROXY {field} contains truncated percent-encoding"
                )));
            }

            let hi = hex_value(bytes[index + 1]).ok_or_else(|| {
                proxy_configuration_error(format!(
                    "RSYNC_PROXY {field} contains invalid percent-encoding"
                ))
            })?;
            let lo = hex_value(bytes[index + 2]).ok_or_else(|| {
                proxy_configuration_error(format!(
                    "RSYNC_PROXY {field} contains invalid percent-encoding"
                ))
            })?;

            decoded.push((hi << 4) | lo);
            index += 3;
            continue;
        }

        decoded.push(bytes[index]);
        index += 1;
    }

    String::from_utf8(decoded).map_err(|_| {
        proxy_configuration_error(format!(
            "RSYNC_PROXY {field} contains invalid UTF-8 after percent-decoding"
        ))
    })
}

fn proxy_configuration_error(text: impl Into<String>) -> ClientError {
    let message = rsync_error!(SOCKET_IO_EXIT_CODE, "{}", text.into()).with_role(Role::Client);
    ClientError::new(SOCKET_IO_EXIT_CODE, message)
}

fn proxy_response_error(text: impl Into<String>) -> ClientError {
    let message =
        rsync_error!(SOCKET_IO_EXIT_CODE, "proxy error: {}", text.into()).with_role(Role::Client);
    ClientError::new(SOCKET_IO_EXIT_CODE, message)
}

fn connect_program_configuration_error(text: impl Into<String>) -> ClientError {
    let message =
        rsync_error!(FEATURE_UNAVAILABLE_EXIT_CODE, "{}", text.into()).with_role(Role::Client);
    ClientError::new(FEATURE_UNAVAILABLE_EXIT_CODE, message)
}

fn read_proxy_line(
    stream: &mut TcpStream,
    buffer: &mut Vec<u8>,
    proxy: SocketAddrDisplay<'_>,
) -> Result<(), ClientError> {
    buffer.clear();

    loop {
        let mut byte = [0u8; 1];
        match stream.read(&mut byte) {
            Ok(0) => {
                return Err(proxy_response_error(
                    "proxy closed the connection during CONNECT negotiation",
                ));
            }
            Ok(_) => {
                buffer.push(byte[0]);
                if byte[0] == b'\n' {
                    while matches!(buffer.last(), Some(b'\n' | b'\r')) {
                        buffer.pop();
                    }
                    break;
                }
                if buffer.len() > 4096 {
                    return Err(proxy_response_error(
                        "proxy response line exceeded 4096 bytes",
                    ));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(socket_error("read from", proxy, error)),
        }
    }

    Ok(())
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
                        password_bytes = load_daemon_password().map(SensitiveBytes::new);
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
                        send_daemon_auth_credentials(&mut reader, &context, challenge, addr)?;
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

                    send_daemon_auth_credentials(&mut reader, context, challenge, addr)?;
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

                    if is_motd_payload(payload) {
                        if !suppress_motd {
                            motd.push(normalize_motd_payload(payload));
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

/// Derives the socket connect timeout from the explicit connection setting and
/// the general transfer timeout.
fn resolve_connect_timeout(
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

struct DaemonAuthContext {
    username: String,
    secret: SensitiveBytes,
}

impl DaemonAuthContext {
    fn new(username: String, secret: Vec<u8>) -> Self {
        Self {
            username,
            secret: SensitiveBytes::new(secret),
        }
    }

    fn secret(&self) -> &[u8] {
        self.secret.as_slice()
    }
}

#[cfg(test)]
impl DaemonAuthContext {
    fn into_zeroized_secret(self) -> Vec<u8> {
        self.secret.into_zeroized_vec()
    }
}

struct SensitiveBytes(Vec<u8>);

impl SensitiveBytes {
    fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    fn to_vec(&self) -> Vec<u8> {
        self.0.clone()
    }

    fn as_slice(&self) -> &[u8] {
        &self.0
    }
}

#[cfg(test)]
impl SensitiveBytes {
    fn into_zeroized_vec(mut self) -> Vec<u8> {
        for byte in &mut self.0 {
            *byte = 0;
        }
        std::mem::take(&mut self.0)
    }
}

impl Drop for SensitiveBytes {
    fn drop(&mut self) {
        for byte in &mut self.0 {
            *byte = 0;
        }
    }
}

fn send_daemon_auth_credentials<S>(
    reader: &mut BufReader<S>,
    context: &DaemonAuthContext,
    challenge: &str,
    addr: &DaemonAddress,
) -> Result<(), ClientError>
where
    S: Write,
{
    let digest = compute_daemon_auth_response(context.secret(), challenge);
    let mut command = String::with_capacity(context.username.len() + digest.len() + 2);
    command.push_str(&context.username);
    command.push(' ');
    command.push_str(&digest);
    command.push('\n');

    reader
        .get_mut()
        .write_all(command.as_bytes())
        .map_err(|error| socket_error("write to", addr.socket_addr_display(), error))?;
    reader
        .get_mut()
        .flush()
        .map_err(|error| socket_error("flush", addr.socket_addr_display(), error))?;

    Ok(())
}

#[cfg(test)]
thread_local! {
    static TEST_PASSWORD_OVERRIDE: RefCell<Option<Vec<u8>>> = const { RefCell::new(None) };
}

#[cfg(test)]
fn set_test_daemon_password(password: Option<Vec<u8>>) {
    TEST_PASSWORD_OVERRIDE.with(|slot| *slot.borrow_mut() = password);
}

fn load_daemon_password() -> Option<Vec<u8>> {
    #[cfg(test)]
    if let Some(password) = TEST_PASSWORD_OVERRIDE.with(|slot| slot.borrow().clone()) {
        return Some(password);
    }

    env::var_os("RSYNC_PASSWORD").map(|value| {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;

            value.into_vec()
        }

        #[cfg(not(unix))]
        {
            value.to_string_lossy().into_owned().into_bytes()
        }
    })
}

fn compute_daemon_auth_response(secret: &[u8], challenge: &str) -> String {
    let mut hasher = Md5::new();
    hasher.update(secret);
    hasher.update(challenge.as_bytes());
    let digest = hasher.finalize();
    STANDARD_NO_PAD.encode(digest)
}

fn normalize_motd_payload(payload: &str) -> String {
    if !is_motd_payload(payload) {
        return payload.to_string();
    }

    let remainder = &payload[4..];
    let remainder = remainder.trim_start_matches([' ', '\t', ':']);
    remainder.trim_start().to_string()
}

fn is_motd_payload(payload: &str) -> bool {
    let bytes = payload.as_bytes();
    if bytes.len() < 4 {
        return false;
    }

    if !bytes[..4].eq_ignore_ascii_case(b"motd") {
        return false;
    }

    if bytes.len() == 4 {
        return true;
    }

    matches!(bytes[4], b' ' | b'\t' | b'\r' | b'\n' | b':')
}
use std::env::{self, VarError};
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::{self, BufRead, BufReader, ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use rsync_checksums::strong::Md5;
use rsync_protocol::{
    LEGACY_DAEMON_PREFIX, LegacyDaemonMessage, NegotiationError, ProtocolVersion,
    parse_legacy_daemon_message, parse_legacy_error_message, parse_legacy_warning_message,
};
use rsync_transport::negotiate_legacy_daemon_session;
use socket2::{Domain, Protocol, SockAddr, Socket, Type};

use super::{
    AddressMode, ClientError, DAEMON_SOCKET_TIMEOUT, FEATURE_UNAVAILABLE_EXIT_CODE,
    PARTIAL_TRANSFER_EXIT_CODE, SOCKET_IO_EXIT_CODE, TransferTimeout, daemon_access_denied_error,
    daemon_authentication_failed_error, daemon_authentication_required_error, daemon_error,
    daemon_protocol_error, socket_error,
};
use crate::{message::Role, rsync_error};

#[cfg(unix)]
use std::os::unix::ffi::{OsStrExt, OsStringExt};
