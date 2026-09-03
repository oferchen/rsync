use std::env::{self, VarError};
use std::ffi::OsStr;
use std::io::{self, ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

use super::direct::{connect_first_reachable, map_connect_failure};
use crate::client::module_list::{DaemonAddress, types::SocketAddrDisplay};
use crate::client::{ClientError, SOCKET_IO_EXIT_CODE, TcpFastOpenMode, socket_error};
use crate::message::Role;
use crate::rsync_error;

/// Connects to `addr`'s daemon through an HTTP(S) CONNECT proxy.
///
/// `sockopts`, when given, is applied to the socket used to reach the proxy
/// before `connect(2)` - upstream `open_socket_out()` resolves and connects to
/// the proxy host in place of the daemon host (socket.c:200-242), so
/// `set_socket_options(s, sockopts)` at socket.c:279 runs against that same
/// proxy-bound socket before its `connect(2)`.
pub(crate) fn connect_via_proxy(
    addr: &DaemonAddress,
    proxy: &ProxyConfig,
    connect_timeout: Option<Duration>,
    io_timeout: Option<Duration>,
    bind_address: Option<SocketAddr>,
    tfo: TcpFastOpenMode,
    sockopts: Option<&OsStr>,
) -> Result<TcpStream, ClientError> {
    let target = (proxy.host.as_str(), proxy.port);
    let addrs: Vec<SocketAddr> = target
        .to_socket_addrs()
        .map_err(|error| socket_error("resolve proxy address for", proxy.display(), error))?
        .collect();

    if addrs.is_empty() {
        return Err(socket_error(
            "resolve proxy address for",
            proxy.display(),
            io::Error::new(
                ErrorKind::AddrNotAvailable,
                "proxy resolution returned no addresses",
            ),
        ));
    }

    // upstream: socket.c:262-310 - open_socket_out() resolves and connects to
    // the proxy host in place of the daemon host, so the same per-address
    // --contimeout semantics (try each address, but abort immediately - not
    // move to the next address - once the alarm fires) apply here too.
    let mut stream =
        match connect_first_reachable(&addrs, bind_address, connect_timeout, tfo, sockopts) {
            Ok(stream) => stream,
            Err((candidate, error)) => {
                return Err(map_connect_failure(connect_timeout, candidate, error));
            }
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

pub(crate) fn establish_proxy_tunnel(
    stream: &mut TcpStream,
    addr: &DaemonAddress,
    proxy: &ProxyConfig,
) -> Result<(), ClientError> {
    // upstream: socket.c:63-70 (3.5.0) rejects the host before it can be
    // interpolated into the CONNECT request line. A control byte in the host
    // would otherwise close the request line early and let the remainder be
    // read as attacker-chosen HTTP headers (CRLF request/header injection).
    // The check is on raw bytes, not chars, so a multi-byte sequence cannot
    // smuggle one past a char-wise scan.
    if let Some(offset) = addr
        .host()
        .as_bytes()
        .iter()
        .position(|byte| *byte < 0x20 || *byte == 0x7f)
    {
        return Err(proxy_configuration_error(format!(
            "invalid control character in proxy CONNECT host at byte {offset}"
        )));
    }

    // upstream: socket.c:77-81 refuses before base64-encoding, on the length
    // the encoding *would* reach. Upstream writes that as `(len*8 + 5) / 6`,
    // which is `(len*8).div_ceil(6)` - the same value, and the spelling clippy
    // requires. The comparison is `>= PROXY_BUF_SIZE - 3`,
    // where `len` is the plaintext `user:pass`. oc keeps that plaintext length
    // so the predicate is upstream's exactly - comparing oc's stored value
    // instead would compare a PADDED encoding against an unpadded threshold
    // and refuse up to two bytes early.
    if proxy
        .credentials_plaintext_len()
        .is_some_and(|plain_len| (plain_len * 8).div_ceil(6) >= PROXY_BUF_SIZE - 3)
    {
        return Err(proxy_configuration_error(
            "authentication information is too long",
        ));
    }

    let mut request = format!("CONNECT {}:{} HTTP/1.0\r\n", addr.host(), addr.port());

    if let Some(header) = proxy.authorization_header() {
        request.push_str("Proxy-Authorization: Basic ");
        request.push_str(header);
        request.push_str("\r\n");
    }

    request.push_str("\r\n");

    // upstream: socket.c:90-94 assembles the request with `snprintf(buffer,
    // PROXY_BUF_SIZE, ...)` and refuses on the WOULD-BE length before the
    // `write()` at socket.c:95, so an oversized request never reaches the
    // proxy. Without this the peer closes mid-negotiation and oc reports the
    // close - the peer's symptom - instead of its own refusal.
    //
    // oc assembles the same bytes in a different order (request line + CRLF,
    // then the auth header + CRLF, then the blank line) as upstream's
    // `"CONNECT %s:%d HTTP/1.0%s%s\r\n\r\n"` with `%s%s` = `"\r\nProxy-
    // Authorization: Basic "` + payload, so the assembled length is identical
    // and the same bound applies.
    if request.len() >= PROXY_BUF_SIZE {
        return Err(proxy_configuration_error("proxy CONNECT request too long"));
    }

    stream
        .write_all(request.as_bytes())
        .map_err(|error| socket_error("write to", proxy.display(), error))?;
    stream
        .flush()
        .map_err(|error| socket_error("flush", proxy.display(), error))?;

    let mut line = Vec::with_capacity(128);
    read_proxy_line(stream, &mut line, proxy.display(), ProxyLineKind::Status)?;
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
        read_proxy_line(stream, &mut line, proxy.display(), ProxyLineKind::Header)?;
        if line.is_empty() {
            break;
        }
    }

    Ok(())
}

pub(crate) fn load_daemon_proxy() -> Result<Option<ProxyConfig>, ClientError> {
    match env::var("RSYNC_PROXY") {
        // upstream: socket.c:350-351 - `proxied = h != NULL && *h != '\0';`
        // only a zero-length value is treated as "unset"; a whitespace-only
        // value is proxied (and then fails to parse), not silently ignored.
        Ok(value) => {
            if value.is_empty() {
                return Ok(None);
            }
            parse_proxy_spec(&value).map(Some)
        }
        Err(VarError::NotPresent) => Ok(None),
        Err(VarError::NotUnicode(_)) => Err(proxy_configuration_error(
            "RSYNC_PROXY value must be valid UTF-8",
        )),
    }
}

/// Parses an `RSYNC_PROXY` value into a [`ProxyConfig`].
///
/// Mirrors upstream's raw buffer-splitting parser exactly (upstream:
/// socket.c:205-234 `open_socket_out()`): `[USER:PASS@]HOST:PORT`. There is
/// no URL scheme, no percent-decoding, and no bracketed-IPv6 syntax in
/// upstream's C parser - those all belong to the separate `rsync://` URL
/// grammar, not `RSYNC_PROXY`. The `USER:PASS@` prefix is found by the LAST
/// `@` (`strrchr`); both the `USER:PASS` split and the `HOST:PORT` split use
/// the FIRST `:` in their segment (`strchr`), so a plain unbracketed IPv6
/// literal is not representable here, matching upstream's own limitation.
pub(crate) fn parse_proxy_spec(spec: &str) -> Result<ProxyConfig, ClientError> {
    if spec.is_empty() {
        return Err(proxy_configuration_error(
            "RSYNC_PROXY must specify a proxy host",
        ));
    }

    let (credentials, remainder) = if let Some(idx) = spec.rfind('@') {
        let (userinfo, host_part) = spec.split_at(idx);

        let mut segments = userinfo.splitn(2, ':');
        let username = segments
            .next()
            .expect("splitn always yields at least one segment");
        let password = segments.next().ok_or_else(|| {
            proxy_configuration_error("invalid proxy specification: should be USER:PASS@HOST:PORT")
        })?;

        let credentials = ProxyCredentials::new(username.to_owned(), password.to_owned());
        (Some(credentials), &host_part[1..])
    } else {
        (None, spec)
    };

    let (host, port) = parse_proxy_host_port(remainder)?;

    Ok(ProxyConfig {
        host,
        port,
        credentials,
    })
}

/// Splits a `HOST:PORT` string at the first `:`, matching upstream's
/// `strchr(h, ':')` (upstream: socket.c:375-381).
fn parse_proxy_host_port(input: &str) -> Result<(String, u16), ClientError> {
    let idx = input.find(':').ok_or_else(|| {
        proxy_configuration_error("invalid proxy specification: should be HOST:PORT")
    })?;
    let host = &input[..idx];
    let port_text = &input[idx + 1..];

    // upstream copies the raw port text into a 10-byte buffer with no
    // numeric validation at parse time, deferring any failure to
    // getaddrinfo(); oc needs a concrete u16 up front to build the
    // candidate SocketAddrs, so the port must parse as a number here.
    let port = port_text
        .parse::<u16>()
        .map_err(|_| proxy_configuration_error("RSYNC_PROXY specified an invalid port"))?;

    Ok((host.to_owned(), port))
}

pub(crate) struct ProxyConfig {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) credentials: Option<ProxyCredentials>,
}

impl ProxyConfig {
    fn display(&self) -> SocketAddrDisplay<'_> {
        SocketAddrDisplay {
            host: &self.host,
            port: self.port,
        }
    }

    pub(crate) fn authorization_header(&self) -> Option<&str> {
        self.credentials
            .as_ref()
            .map(ProxyCredentials::authorization_value)
    }

    /// Length of the plaintext `user:pass` the credentials were built from, if
    /// any - the input to upstream's `authentication information is too long`
    /// predicate (socket.c:77-81).
    fn credentials_plaintext_len(&self) -> Option<usize> {
        self.credentials
            .as_ref()
            .map(ProxyCredentials::plaintext_len)
    }
}

/// HTTP proxy credentials with a cached `Proxy-Authorization` header value.
pub(crate) struct ProxyCredentials {
    authorization: String,
    /// Byte length of the `user:pass` plaintext, retained because upstream's
    /// length refusal is stated on the plaintext, not on the encoding.
    plaintext_len: usize,
}

impl ProxyCredentials {
    fn new(username: String, password: String) -> Self {
        let mut bytes = Vec::with_capacity(username.len() + password.len() + 1);
        bytes.extend_from_slice(username.as_bytes());
        bytes.push(b':');
        bytes.extend_from_slice(password.as_bytes());
        let plaintext_len = bytes.len();
        let authorization = STANDARD.encode(bytes);
        Self {
            authorization,
            plaintext_len,
        }
    }

    /// Byte length of the `user:pass` plaintext behind this header.
    fn plaintext_len(&self) -> usize {
        self.plaintext_len
    }

    /// Returns the cached `Proxy-Authorization` header payload.
    fn authorization_value(&self) -> &str {
        &self.authorization
    }
}

/// Upstream's single stack buffer for the whole CONNECT exchange: the request
/// it writes and every response line it reads are all bounded by this size.
///
/// upstream: socket.c:52 `#define PROXY_BUF_SIZE 1024`.
const PROXY_BUF_SIZE: usize = 1024;

/// Maximum size of a single CONNECT response line, matching upstream rsync's
/// `PROXY_BUF_SIZE - 1` loop bound in `establish_proxy_connection()`
/// (socket.c:86). Upstream's stack buffer is 1024 bytes, but its read loop
/// (`cp < &buffer[PROXY_BUF_SIZE - 1]`) writes at most positions 0..=1022,
/// then rejects when the post-loop cursor lands at `&buffer[1023]`. The
/// effective cap is therefore 1023 non-newline bytes.
const MAX_PROXY_LINE_BYTES: usize = PROXY_BUF_SIZE - 1;

/// Which line of the CONNECT response is being read.
///
/// Upstream reads the status line and the header lines in two separate loops
/// that share the `PROXY_BUF_SIZE - 1` bound but report *different*
/// diagnostics on overflow (socket.c:110 vs socket.c:141). Only the message
/// differs, so the read itself stays a single owner and the caller names the
/// line it asked for.
#[derive(Clone, Copy)]
enum ProxyLineKind {
    /// The `HTTP/1.x NNN` status line - upstream socket.c:100-112.
    Status,
    /// One line of the response header block - upstream socket.c:130-146.
    Header,
}

impl ProxyLineKind {
    /// Upstream's verbatim over-length diagnostic for this line kind.
    fn too_long_diagnostic(self) -> &'static str {
        match self {
            Self::Status => "proxy response line too long",
            Self::Header => "proxy response header line too long",
        }
    }
}

fn read_proxy_line(
    stream: &mut TcpStream,
    buffer: &mut Vec<u8>,
    proxy: SocketAddrDisplay<'_>,
    kind: ProxyLineKind,
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
                if buffer.len() >= MAX_PROXY_LINE_BYTES {
                    return Err(proxy_response_error(format!(
                        "{} (exceeded {MAX_PROXY_LINE_BYTES} bytes)",
                        kind.too_long_diagnostic()
                    )));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(socket_error("read from", proxy, error)),
        }
    }

    Ok(())
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

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::thread;

    use super::*;

    #[test]
    fn parse_proxy_spec_simple_host_port() {
        let config = parse_proxy_spec("proxy.example.com:8080").unwrap();
        assert_eq!(config.host, "proxy.example.com");
        assert_eq!(config.port, 8080);
        assert!(config.credentials.is_none());
    }

    // upstream: socket.c:205-234 - RSYNC_PROXY has no URL-scheme concept at
    // all; getenv()'s raw value is split directly on '@' and ':'. A
    // "scheme://" prefix is just more HOST text to upstream's parser, and
    // (since strchr(':') finds the FIRST ':', which lands right after the
    // scheme name) it is misparsed as bogus credentials rather than
    // rejected or specially recognised. oc must not invent scheme support.
    #[test]
    fn parse_proxy_spec_has_no_scheme_concept() {
        let error = match parse_proxy_spec("http://proxy.example.com:3128") {
            Ok(_) => panic!("a scheme prefix must not parse as a valid proxy spec"),
            Err(error) => error,
        };
        assert!(error.message().to_string().contains("invalid port"));
    }

    #[test]
    fn parse_proxy_spec_with_credentials() {
        let config = parse_proxy_spec("user:pass@proxy.example.com:8080").unwrap();
        assert_eq!(config.host, "proxy.example.com");
        assert_eq!(config.port, 8080);
        assert!(config.credentials.is_some());
    }

    // upstream never trims the getenv() value: `proxied = h != NULL && *h !=
    // '\0';` treats any non-empty string (including one that is all
    // whitespace) as set, then feeds it straight into the buffer splitter.
    // Leading/trailing whitespace becomes literal host/port text, which
    // fails to parse as a port rather than being silently stripped.
    #[test]
    fn parse_proxy_spec_leading_trailing_whitespace_is_literal_not_trimmed() {
        let error = match parse_proxy_spec("  proxy.example.com:8080  ") {
            Ok(_) => panic!("literal whitespace must not parse as a valid proxy spec"),
            Err(error) => error,
        };
        assert!(error.message().to_string().contains("invalid port"));
    }

    #[test]
    fn parse_proxy_spec_empty_returns_error() {
        let result = parse_proxy_spec("");
        assert!(result.is_err());
    }

    // upstream: socket.c:202-203 - a whitespace-only value is non-empty, so
    // it is "proxied" and fed to the parser (where it then fails to find a
    // ':' and errors), unlike an actually-empty value which upstream treats
    // as "no proxy configured" before ever reaching this parser.
    #[test]
    fn parse_proxy_spec_whitespace_only_returns_error() {
        let result = parse_proxy_spec("   ");
        assert!(result.is_err());
    }

    // A "/path" suffix is not a recognised construct in upstream's parser;
    // it is just more raw text that ends up in the port field and fails
    // u16 parsing (upstream would similarly fail later at getaddrinfo()
    // when handed a bogus "port/path" service string).
    #[test]
    fn parse_proxy_spec_with_path_suffix_fails_port_parse() {
        let result = parse_proxy_spec("proxy.example.com:8080/path");
        assert!(result.is_err());
    }

    #[test]
    fn parse_proxy_spec_empty_userinfo_returns_error() {
        let result = parse_proxy_spec("@proxy.example.com:8080");
        assert!(result.is_err());
    }

    #[test]
    fn parse_proxy_spec_missing_password_returns_error() {
        let result = parse_proxy_spec("user@proxy.example.com:8080");
        assert!(result.is_err());
    }

    // upstream never percent-decodes RSYNC_PROXY components (that grammar
    // belongs only to rsync:// URLs); a literal "%40" stays literal.
    #[test]
    fn parse_proxy_spec_does_not_percent_decode_credentials() {
        let config = parse_proxy_spec("user%40domain:pass@proxy.example.com:8080").unwrap();
        let creds = config.credentials.expect("credentials present");
        let decoded = STANDARD.decode(creds.authorization_value()).unwrap();
        assert_eq!(decoded, b"user%40domain:pass");
    }

    #[test]
    fn parse_proxy_spec_ipv4_address() {
        let config = parse_proxy_spec("192.168.1.1:8080").unwrap();
        assert_eq!(config.host, "192.168.1.1");
        assert_eq!(config.port, 8080);
    }

    // upstream's HOST:PORT split uses strchr() (the FIRST ':'), so a
    // bracketed IPv6 literal is not recognised: the first ':' lands inside
    // the brackets, producing a non-numeric "port" and an error. oc must
    // not accept a syntax upstream cannot parse.
    #[test]
    fn parse_proxy_spec_does_not_support_bracketed_ipv6() {
        let result = parse_proxy_spec("[::1]:8080");
        assert!(result.is_err());
    }

    #[test]
    fn parse_proxy_host_port_simple() {
        let (host, port) = parse_proxy_host_port("example.com:8080").unwrap();
        assert_eq!(host, "example.com");
        assert_eq!(port, 8080);
    }

    #[test]
    fn parse_proxy_host_port_empty_returns_error() {
        let result = parse_proxy_host_port("");
        assert!(result.is_err());
    }

    #[test]
    fn parse_proxy_host_port_no_port_returns_error() {
        let result = parse_proxy_host_port("example.com");
        assert!(result.is_err());
    }

    #[test]
    fn parse_proxy_host_port_empty_port_returns_error() {
        let result = parse_proxy_host_port("example.com:");
        assert!(result.is_err());
    }

    #[test]
    fn parse_proxy_host_port_invalid_port_returns_error() {
        let result = parse_proxy_host_port("example.com:notanumber");
        assert!(result.is_err());
    }

    #[test]
    fn parse_proxy_host_port_port_out_of_range_returns_error() {
        let result = parse_proxy_host_port("example.com:99999");
        assert!(result.is_err());
    }

    // upstream's strchr(h, ':') finds the FIRST colon; a host string that
    // itself contains a colon (as an unbracketed IPv6 literal would) splits
    // in the wrong place and fails the numeric port check.
    #[test]
    fn parse_proxy_host_port_first_colon_wins_over_embedded_colon() {
        let result = parse_proxy_host_port("::1:8080");
        assert!(result.is_err());
    }

    // oc does not percent-decode the host component; a literal "%20"
    // becomes literal host text (which then fails DNS/getaddrinfo like any
    // other invalid hostname), matching upstream exactly.
    #[test]
    fn parse_proxy_host_port_does_not_percent_decode_host() {
        let (host, _port) = parse_proxy_host_port("my%20host:8080").unwrap();
        assert_eq!(host, "my%20host");
    }

    #[test]
    fn proxy_credentials_authorization_value_basic_auth() {
        let creds = ProxyCredentials::new("user".to_owned(), "pass".to_owned());
        assert_eq!(creds.authorization_value(), "dXNlcjpwYXNz");
    }

    #[test]
    fn proxy_credentials_authorization_value_empty_password() {
        let creds = ProxyCredentials::new("user".to_owned(), "".to_owned());
        assert_eq!(creds.authorization_value(), "dXNlcjo=");
    }

    #[test]
    fn proxy_credentials_authorization_value_special_chars() {
        let creds = ProxyCredentials::new("user@domain".to_owned(), "p@ss:word".to_owned());
        let decoded = STANDARD.decode(creds.authorization_value()).unwrap();
        assert_eq!(decoded, b"user@domain:p@ss:word");
    }

    #[test]
    fn proxy_config_display_returns_socket_addr() {
        let config = parse_proxy_spec("proxy.example.com:8080").unwrap();
        let display = config.display();
        assert_eq!(display.host, "proxy.example.com");
        assert_eq!(display.port, 8080);
    }

    #[test]
    fn proxy_config_authorization_header_none_when_no_credentials() {
        let config = parse_proxy_spec("proxy.example.com:8080").unwrap();
        assert!(config.authorization_header().is_none());
    }

    #[test]
    fn proxy_config_authorization_header_present_with_credentials() {
        let config = parse_proxy_spec("user:pass@proxy.example.com:8080").unwrap();
        assert!(config.authorization_header().is_some());
        assert_eq!(config.authorization_header().unwrap(), "dXNlcjpwYXNz");
    }

    #[test]
    fn parse_proxy_spec_localhost() {
        let config = parse_proxy_spec("localhost:8080").unwrap();
        assert_eq!(config.host, "localhost");
        assert_eq!(config.port, 8080);
    }

    #[test]
    fn parse_proxy_spec_minimum_port() {
        let config = parse_proxy_spec("proxy.example.com:1").unwrap();
        assert_eq!(config.port, 1);
    }

    #[test]
    fn parse_proxy_spec_maximum_port() {
        let config = parse_proxy_spec("proxy.example.com:65535").unwrap();
        assert_eq!(config.port, 65535);
    }

    #[test]
    fn parse_proxy_spec_complex_password_with_special_chars() {
        let config = parse_proxy_spec("user:p%40ss%3Aword%2F123@proxy.example.com:8080").unwrap();
        assert_eq!(config.host, "proxy.example.com");
        assert!(config.credentials.is_some());
    }

    #[test]
    fn parse_proxy_spec_colon_in_password() {
        // Only first colon splits user:pass; remaining colons are part of the password.
        let config = parse_proxy_spec("user:pass:with:colons@proxy.example.com:8080").unwrap();
        assert!(config.credentials.is_some());
        let decoded = STANDARD
            .decode(config.credentials.unwrap().authorization_value())
            .unwrap();
        assert_eq!(decoded, b"user:pass:with:colons");
    }

    #[test]
    fn read_proxy_line_rejects_lines_above_upstream_cap() {
        use std::net::TcpListener;
        use std::thread;

        // Upstream `establish_proxy_connection()` exits its read loop and
        // rejects "too long" once the cursor reaches `&buffer[PROXY_BUF_SIZE -
        // 1]` (socket.c:86-98), i.e. after 1023 non-newline bytes. A
        // 1024-byte newline-free response must therefore be refused.
        assert_eq!(MAX_PROXY_LINE_BYTES, 1023);
        let payload = vec![b'A'; MAX_PROXY_LINE_BYTES + 1];

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback listener");
        let addr = listener.local_addr().expect("listener address");

        let server_payload = payload.clone();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            stream
                .write_all(&server_payload)
                .expect("write oversized response");
            stream.flush().expect("flush oversized response");
        });

        let mut stream = TcpStream::connect(addr).expect("connect to listener");
        let mut buffer = Vec::with_capacity(MAX_PROXY_LINE_BYTES + 2);
        let display = SocketAddrDisplay {
            host: "proxy.test",
            port: addr.port(),
        };
        let error = read_proxy_line(&mut stream, &mut buffer, display, ProxyLineKind::Status)
            .expect_err("oversized proxy line must be rejected");

        assert_eq!(error.exit_code(), SOCKET_IO_EXIT_CODE);
        let rendered = error.message().to_string();
        assert!(
            rendered.contains("proxy response line too long"),
            "unexpected error message: {rendered}"
        );
        assert!(
            rendered.contains("1023"),
            "error message should cite the 1023-byte cap: {rendered}"
        );

        handle.join().expect("server thread");
    }

    #[test]
    fn read_proxy_line_rejects_exactly_cap_bytes_then_close() {
        use std::net::TcpListener;
        use std::thread;

        // Upstream test `proxy-response-line-too-long`: a malicious proxy
        // sends exactly 1023 bytes (PROXY_BUF_SIZE - 1) without a newline,
        // then closes. Upstream's loop bound `cp < &buffer[PROXY_BUF_SIZE -
        // 1]` exits after writing positions 0..=1022, and the post-loop
        // check rejects with "proxy response line too long" before the EOF
        // is observed. oc-rsync must mirror that semantics rather than
        // surfacing the subsequent EOF as "proxy closed the connection".
        let payload = vec![b'X'; MAX_PROXY_LINE_BYTES];

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback listener");
        let addr = listener.local_addr().expect("listener address");

        let server_payload = payload.clone();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            stream
                .write_all(&server_payload)
                .expect("write cap-sized response");
            stream.flush().expect("flush cap-sized response");
            // Drop closes the stream, producing EOF on the client side.
        });

        let mut stream = TcpStream::connect(addr).expect("connect to listener");
        let mut buffer = Vec::with_capacity(MAX_PROXY_LINE_BYTES + 2);
        let display = SocketAddrDisplay {
            host: "proxy.test",
            port: addr.port(),
        };
        let error = read_proxy_line(&mut stream, &mut buffer, display, ProxyLineKind::Status)
            .expect_err("cap-sized newline-free proxy line must be rejected");

        assert_eq!(error.exit_code(), SOCKET_IO_EXIT_CODE);
        let rendered = error.message().to_string();
        assert!(
            rendered.contains("proxy response line too long"),
            "must report too-long, not EOF: {rendered}"
        );

        handle.join().expect("server thread");
    }

    /// Runs one `read_proxy_line` decode over a loopback connection whose server
    /// side writes `payload` and then closes. Returns the parser's result and
    /// the (post-parse) line buffer so invariants can be asserted on both.
    fn run_proxy_line(payload: &[u8]) -> (Result<(), ClientError>, Vec<u8>) {
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback listener");
        let addr = listener.local_addr().expect("listener address");

        let server_payload = payload.to_vec();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            // A malformed decoder could hang forever; the client caps its read,
            // so ignore write errors from an early client-side close.
            let _ = stream.write_all(&server_payload);
            let _ = stream.flush();
        });

        let mut stream = TcpStream::connect(addr).expect("connect to listener");
        let mut buffer = Vec::with_capacity(MAX_PROXY_LINE_BYTES + 2);
        let display = SocketAddrDisplay {
            host: "proxy.test",
            port: addr.port(),
        };
        let result = read_proxy_line(&mut stream, &mut buffer, display, ProxyLineKind::Status);
        handle.join().expect("server thread");
        (result, buffer)
    }

    /// A control byte in the CONNECT host must be refused before any write.
    ///
    /// WHY: `DaemonAddress::new` stores the host verbatim, so a CRLF anywhere
    /// in it survives into `format!("CONNECT {host}:{port} HTTP/1.0\r\n")`.
    /// This guard is the only thing that stops it - do not reintroduce
    /// trimming as a substitute, which would silently accept some hosts
    /// upstream refuses while still passing an interior CRLF through. Without
    /// the guard the request line terminates at the injected newline and the
    /// remainder of the host is read by the proxy as attacker-chosen headers.
    /// Asserting that the socket saw ZERO bytes is what makes this a real
    /// oracle: a test that only checked the error could still pass while a
    /// poisoned request line went out on the wire.
    #[test]
    fn connect_host_with_embedded_crlf_is_refused_before_any_write() {
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback listener");
        let listen_addr = listener.local_addr().expect("listener address");
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut seen = Vec::new();
            // Returns as soon as the client closes; no sleep, no deadline race.
            let _ = stream.read_to_end(&mut seen);
            seen
        });

        let mut stream = TcpStream::connect(listen_addr).expect("connect to listener");
        let addr = DaemonAddress::new("evil\r\nX-Injected: 1".to_owned(), 873);
        let proxy = ProxyConfig {
            host: listen_addr.ip().to_string(),
            port: listen_addr.port(),
            credentials: None,
        };

        let err = establish_proxy_tunnel(&mut stream, &addr, &proxy)
            .expect_err("a control byte in the CONNECT host must be refused");
        assert!(
            err.to_string().contains("control character"),
            "expected a control-character refusal, got: {err}"
        );

        drop(stream);
        let seen = handle.join().expect("server thread");
        assert!(
            seen.is_empty(),
            "nothing may be written to the proxy once the host is rejected; saw {:?}",
            String::from_utf8_lossy(&seen)
        );
    }

    /// The guard must not reject hosts rsync legitimately accepts.
    ///
    /// WHY: an IPv6 zone id carries `%`, and upstream deliberately keeps that
    /// legal. A cap that also refused these would be a silent regression for
    /// link-local targets, so the negative control is as load-bearing as the
    /// positive one.
    #[test]
    fn connect_host_allows_ipv6_zone_id_and_ordinary_names() {
        for host in ["fe80::1%eth0", "proxy.example.com", "192.0.2.10"] {
            let addr = DaemonAddress::new(host.to_owned(), 873);
            assert!(
                !addr
                    .host()
                    .as_bytes()
                    .iter()
                    .any(|byte| *byte < 0x20 || *byte == 0x7f),
                "{host} must not trip the control-byte guard"
            );
        }
    }

    /// A loopback proxy that answers every CONNECT successfully and reports
    /// each byte the client sent it.
    ///
    /// WHY it answers unconditionally: the tests below assert that a refusal
    /// happens *before any write*, so on the correct path the client writes
    /// nothing and reads nothing - the response is never consumed. It exists
    /// for the mutated path. With the bound removed the client writes its
    /// request and then waits for a response; a server that only read would
    /// leave both sides blocked and the test would HANG rather than fail.
    /// A hang is a worse oracle than no test at all: it reports nothing and
    /// stalls the suite. Answering makes a removed bound surface immediately,
    /// as `expect_err` on a tunnel that succeeded.
    struct RecordingProxy {
        addr: SocketAddr,
        handle: thread::JoinHandle<Vec<u8>>,
    }

    impl RecordingProxy {
        fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback listener");
            let addr = listener.local_addr().expect("listener address");
            let handle = thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("accept");
                let _ = stream.write_all(b"HTTP/1.0 200 Connection established\r\n\r\n");
                let _ = stream.flush();
                let mut seen = Vec::new();
                let _ = stream.read_to_end(&mut seen);
                seen
            });
            Self { addr, handle }
        }

        fn config(&self, credentials: Option<ProxyCredentials>) -> ProxyConfig {
            ProxyConfig {
                host: self.addr.ip().to_string(),
                port: self.addr.port(),
                credentials,
            }
        }

        /// Every byte the client sent, once it has closed its end.
        fn bytes_received(self) -> Vec<u8> {
            self.handle.join().expect("server thread")
        }
    }

    /// An oversized CONNECT request must be refused before any write.
    ///
    /// WHY: upstream assembles the request with `snprintf(buffer,
    /// PROXY_BUF_SIZE, ...)` and rejects on the WOULD-BE length at
    /// socket.c:90-94, *before* the `write()` at socket.c:95. Without that
    /// bound oc sends the oversized request, the proxy closes on it, and oc
    /// reports "proxy closed the connection during CONNECT negotiation" - the
    /// peer's symptom rather than its own refusal. Asserting the socket saw
    /// ZERO bytes is what makes this a before-write oracle; a message-only
    /// check would still pass while the request went out on the wire.
    #[test]
    fn oversized_connect_request_is_refused_before_any_write() {
        let server = RecordingProxy::start();
        let mut stream = TcpStream::connect(server.addr).expect("connect to listener");
        // The upstream cell uses 1500 'a's plus ".invalid"; any host that pushes
        // the assembled request to PROXY_BUF_SIZE reaches the same branch.
        let addr = DaemonAddress::new(format!("{}.invalid", "a".repeat(1500)), 873);

        let err = establish_proxy_tunnel(&mut stream, &addr, &server.config(None))
            .expect_err("an oversized CONNECT request must be refused");
        assert!(
            err.to_string().contains("proxy CONNECT request too long"),
            "expected upstream's request-too-long refusal, got: {err}"
        );

        drop(stream);
        let seen = server.bytes_received();
        assert!(
            seen.is_empty(),
            "nothing may be written once the request is rejected; saw {:?}",
            String::from_utf8_lossy(&seen)
        );
    }

    /// Oversized credentials must be refused before the request is built.
    ///
    /// WHY: upstream checks the length the base64 encoding *would* reach and
    /// bails at socket.c:77-81, before `base64_encode`. The predicate is stated
    /// on the `user:pass` plaintext, which is why oc retains that length rather
    /// than measuring its own stored (padded) encoding - a padded measurement
    /// would refuse up to two bytes earlier than upstream.
    #[test]
    fn oversized_proxy_credentials_are_refused_before_any_write() {
        let server = RecordingProxy::start();
        let mut stream = TcpStream::connect(server.addr).expect("connect to listener");
        let addr = DaemonAddress::new("proxy.test".to_owned(), 873);
        let credentials = ProxyCredentials::new("u".repeat(800), "p".repeat(800));

        let err = establish_proxy_tunnel(&mut stream, &addr, &server.config(Some(credentials)))
            .expect_err("oversized proxy credentials must be refused");
        assert!(
            err.to_string()
                .contains("authentication information is too long"),
            "expected upstream's auth-too-long refusal, got: {err}"
        );

        drop(stream);
        let seen = server.bytes_received();
        assert!(
            seen.is_empty(),
            "nothing may be written once the credentials are rejected; saw {:?}",
            String::from_utf8_lossy(&seen)
        );
    }

    /// Writes `response` to one accepted connection, then runs a full tunnel
    /// negotiation against it. The server never reads, so the client's write
    /// completes into the socket buffer and cannot deadlock against us.
    fn negotiate_against_response(response: Vec<u8>) -> Result<(), ClientError> {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback listener");
        let listen_addr = listener.local_addr().expect("listener address");
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let _ = stream.write_all(&response);
            let _ = stream.flush();
        });

        let mut stream = TcpStream::connect(listen_addr).expect("connect to listener");
        let addr = DaemonAddress::new("proxy.test".to_owned(), 873);
        let proxy = ProxyConfig {
            host: listen_addr.ip().to_string(),
            port: listen_addr.port(),
            credentials: None,
        };
        establish_proxy_tunnel(&mut stream, &addr, &proxy)
    }

    /// An over-long *header* line must report upstream's header diagnostic.
    ///
    /// WHY: upstream reads the status line and the header lines in two loops
    /// sharing one bound but reporting different messages - "proxy response
    /// line too long" (socket.c:110) versus "proxy response header line too
    /// long" (socket.c:141). oc had a single bound serving both, so an
    /// over-long header reported the *status-line* text. This is the test that
    /// separates them; `read_proxy_line` still owns the read and the bound.
    #[test]
    fn oversized_response_header_line_reports_the_header_diagnostic() {
        let mut response = b"HTTP/1.0 200 Connection established\r\n".to_vec();
        response.extend(std::iter::repeat_n(b'H', MAX_PROXY_LINE_BYTES + 1));

        let err = negotiate_against_response(response)
            .expect_err("an over-long response header must be refused");
        let rendered = err.to_string();
        assert!(
            rendered.contains("proxy response header line too long"),
            "an over-long HEADER must not report the status-line text: {rendered}"
        );
    }

    /// Negative control for the header bound: ordinary headers still succeed.
    ///
    /// WHY: without this, the test above would also pass if the header loop
    /// rejected *every* header line, or never terminated on the blank line.
    #[test]
    fn ordinary_response_headers_complete_the_tunnel() {
        let response = b"HTTP/1.0 200 Connection established\r\nX-Proxy: test\r\n\r\n".to_vec();

        negotiate_against_response(response)
            .expect("a well-formed CONNECT response must establish the tunnel");
    }

    /// Deterministic xorshift64 stream so failures are reproducible from `seed`.
    fn xorshift64(state: &mut u64) -> u64 {
        let mut x = *state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *state = x;
        x
    }

    #[test]
    fn read_proxy_line_never_panics_on_arbitrary_bytes() {
        // CVE-2026-45232 class: a hostile HTTP proxy returns a CONNECT status
        // line with no newline and arbitrary bytes. The decoder must honour the
        // MAX_PROXY_LINE_BYTES cap and return an error - never panic, overflow,
        // or grow the buffer without bound. Cover the exact upstream boundaries
        // (1023 / 1024 / 4096 bytes) plus a deterministic corpus of random
        // lines with embedded control, NUL, CR, and high bytes.
        for len in [MAX_PROXY_LINE_BYTES, MAX_PROXY_LINE_BYTES + 1, 4096] {
            let payload = vec![b'Z'; len];
            let (result, buffer) = run_proxy_line(&payload);
            assert!(
                result.is_err(),
                "newline-free {len}-byte line must be rejected"
            );
            assert!(
                buffer.len() <= MAX_PROXY_LINE_BYTES,
                "buffer grew to {} beyond the {MAX_PROXY_LINE_BYTES}-byte cap",
                buffer.len()
            );
        }

        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        for _ in 0..256 {
            // Random length spanning below, at, and above the cap.
            let len = (xorshift64(&mut state) % 4200) as usize;
            let payload: Vec<u8> = (0..len)
                .map(|_| (xorshift64(&mut state) & 0xFF) as u8)
                .collect();
            let (result, buffer) = run_proxy_line(&payload);
            // Ok or Err are both graceful; the invariants are cap-bounded buffer
            // and (on success) a stripped, newline-free line.
            assert!(
                buffer.len() <= MAX_PROXY_LINE_BYTES,
                "buffer grew to {} beyond the cap on a {len}-byte payload",
                buffer.len()
            );
            if result.is_ok() {
                // The read loop breaks on the first newline and strips the
                // trailing CR/LF (upstream socket.c), so an accepted line never
                // contains a newline. Interior CR is retained, matching upstream.
                assert!(
                    !buffer.contains(&b'\n'),
                    "accepted line must not contain a newline"
                );
            }
        }
    }

    #[test]
    fn read_proxy_line_accepts_capped_line_with_newline() {
        // A full cap-length line terminated by a newline is the largest legal
        // response; it must decode without error and be returned CR/LF-stripped.
        let mut payload = vec![b'H'; MAX_PROXY_LINE_BYTES - 1];
        payload.push(b'\n');
        let (result, buffer) = run_proxy_line(&payload);
        result.expect("cap-length line with newline must decode");
        assert_eq!(buffer.len(), MAX_PROXY_LINE_BYTES - 1);
        assert!(!buffer.contains(&b'\n'));
    }
}
