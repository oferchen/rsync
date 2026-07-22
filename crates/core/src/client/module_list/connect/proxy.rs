use std::env::{self, VarError};
use std::ffi::OsStr;
use std::io::{self, ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

use super::direct::connect_with_optional_bind;
use crate::client::module_list::parsing::{decode_host_component, hex_value, parse_bracketed_host};
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
    let addrs = target
        .to_socket_addrs()
        .map_err(|error| socket_error("resolve proxy address for", proxy.display(), error))?;

    let mut last_error: Option<(SocketAddr, io::Error)> = None;
    let mut stream_result: Option<TcpStream> = None;

    for candidate in addrs {
        match connect_with_optional_bind(candidate, bind_address, connect_timeout, tfo, sockopts) {
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

pub(crate) fn establish_proxy_tunnel(
    stream: &mut TcpStream,
    addr: &DaemonAddress,
    proxy: &ProxyConfig,
) -> Result<(), ClientError> {
    let mut request = format!("CONNECT {}:{} HTTP/1.0\r\n", addr.host(), addr.port());

    if let Some(header) = proxy.authorization_header() {
        request.push_str("Proxy-Authorization: Basic ");
        request.push_str(header);
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

pub(crate) fn load_daemon_proxy() -> Result<Option<ProxyConfig>, ClientError> {
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

pub(crate) fn parse_proxy_spec(spec: &str) -> Result<ProxyConfig, ClientError> {
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
        return Ok(input.to_owned());
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
}

/// HTTP proxy credentials with a cached `Proxy-Authorization` header value.
pub(crate) struct ProxyCredentials {
    authorization: String,
}

impl ProxyCredentials {
    fn new(username: String, password: String) -> Self {
        let mut bytes = Vec::with_capacity(username.len() + password.len() + 1);
        bytes.extend_from_slice(username.as_bytes());
        bytes.push(b':');
        bytes.extend_from_slice(password.as_bytes());
        let authorization = STANDARD.encode(bytes);
        Self { authorization }
    }

    /// Returns the cached `Proxy-Authorization` header payload.
    fn authorization_value(&self) -> &str {
        &self.authorization
    }
}

/// Maximum size of a single CONNECT response line, matching upstream rsync's
/// `PROXY_BUF_SIZE - 1` loop bound in `establish_proxy_connection()`
/// (socket.c:86). Upstream's stack buffer is 1024 bytes, but its read loop
/// (`cp < &buffer[PROXY_BUF_SIZE - 1]`) writes at most positions 0..=1022,
/// then rejects when the post-loop cursor lands at `&buffer[1023]`. The
/// effective cap is therefore 1023 non-newline bytes.
const MAX_PROXY_LINE_BYTES: usize = 1023;

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
                if buffer.len() >= MAX_PROXY_LINE_BYTES {
                    return Err(proxy_response_error(format!(
                        "proxy response line too long (exceeded {MAX_PROXY_LINE_BYTES} bytes)"
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
    use super::*;

    #[test]
    fn parse_proxy_spec_simple_host_port() {
        let config = parse_proxy_spec("proxy.example.com:8080").unwrap();
        assert_eq!(config.host, "proxy.example.com");
        assert_eq!(config.port, 8080);
        assert!(config.credentials.is_none());
    }

    #[test]
    fn parse_proxy_spec_with_http_scheme() {
        let config = parse_proxy_spec("http://proxy.example.com:3128").unwrap();
        assert_eq!(config.host, "proxy.example.com");
        assert_eq!(config.port, 3128);
    }

    #[test]
    fn parse_proxy_spec_with_https_scheme() {
        let config = parse_proxy_spec("https://proxy.example.com:443").unwrap();
        assert_eq!(config.host, "proxy.example.com");
        assert_eq!(config.port, 443);
    }

    #[test]
    fn parse_proxy_spec_scheme_case_insensitive() {
        let config = parse_proxy_spec("HTTP://proxy.example.com:8080").unwrap();
        assert_eq!(config.host, "proxy.example.com");

        let config = parse_proxy_spec("HTTPS://proxy.example.com:8080").unwrap();
        assert_eq!(config.host, "proxy.example.com");
    }

    #[test]
    fn parse_proxy_spec_with_credentials() {
        let config = parse_proxy_spec("user:pass@proxy.example.com:8080").unwrap();
        assert_eq!(config.host, "proxy.example.com");
        assert_eq!(config.port, 8080);
        assert!(config.credentials.is_some());
    }

    #[test]
    fn parse_proxy_spec_with_scheme_and_credentials() {
        let config = parse_proxy_spec("http://user:pass@proxy.example.com:8080").unwrap();
        assert_eq!(config.host, "proxy.example.com");
        assert_eq!(config.port, 8080);
        assert!(config.credentials.is_some());
    }

    #[test]
    fn parse_proxy_spec_with_whitespace_trimmed() {
        let config = parse_proxy_spec("  proxy.example.com:8080  ").unwrap();
        assert_eq!(config.host, "proxy.example.com");
        assert_eq!(config.port, 8080);
    }

    #[test]
    fn parse_proxy_spec_empty_returns_error() {
        let result = parse_proxy_spec("");
        assert!(result.is_err());
    }

    #[test]
    fn parse_proxy_spec_whitespace_only_returns_error() {
        let result = parse_proxy_spec("   ");
        assert!(result.is_err());
    }

    #[test]
    fn parse_proxy_spec_invalid_scheme_returns_error() {
        let result = parse_proxy_spec("socks://proxy.example.com:1080");
        assert!(result.is_err());
    }

    #[test]
    fn parse_proxy_spec_scheme_only_returns_error() {
        let result = parse_proxy_spec("http://");
        assert!(result.is_err());
    }

    #[test]
    fn parse_proxy_spec_with_path_returns_error() {
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

    #[test]
    fn parse_proxy_spec_percent_encoded_credentials() {
        let config = parse_proxy_spec("user%40domain:p%40ss@proxy.example.com:8080").unwrap();
        assert_eq!(config.host, "proxy.example.com");
        assert!(config.credentials.is_some());
    }

    #[test]
    fn parse_proxy_spec_ipv4_address() {
        let config = parse_proxy_spec("192.168.1.1:8080").unwrap();
        assert_eq!(config.host, "192.168.1.1");
        assert_eq!(config.port, 8080);
    }

    #[test]
    fn parse_proxy_spec_ipv6_bracketed() {
        let config = parse_proxy_spec("[::1]:8080").unwrap();
        assert_eq!(config.host, "::1");
        assert_eq!(config.port, 8080);
    }

    #[test]
    fn parse_proxy_spec_ipv6_bracketed_full() {
        let config = parse_proxy_spec("[2001:db8::1]:3128").unwrap();
        assert_eq!(config.host, "2001:db8::1");
        assert_eq!(config.port, 3128);
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

    #[test]
    fn parse_proxy_host_port_ipv6_bracketed() {
        let (host, port) = parse_proxy_host_port("[::1]:8080").unwrap();
        assert_eq!(host, "::1");
        assert_eq!(port, 8080);
    }

    #[test]
    fn parse_proxy_host_port_ipv6_no_port_returns_error() {
        let result = parse_proxy_host_port("[::1]");
        assert!(result.is_err());
    }

    #[test]
    fn parse_proxy_host_port_percent_encoded_host() {
        let (host, _port) = parse_proxy_host_port("my%20host:8080").unwrap();
        assert_eq!(host, "my host");
    }

    #[test]
    fn decode_proxy_component_plain_text() {
        let result = decode_proxy_component("username", "test").unwrap();
        assert_eq!(result, "username");
    }

    #[test]
    fn decode_proxy_component_percent_encoded() {
        let result = decode_proxy_component("user%40domain", "test").unwrap();
        assert_eq!(result, "user@domain");
    }

    #[test]
    fn decode_proxy_component_multiple_encoded() {
        let result = decode_proxy_component("a%20b%20c", "test").unwrap();
        assert_eq!(result, "a b c");
    }

    #[test]
    fn decode_proxy_component_hex_case_insensitive() {
        let result1 = decode_proxy_component("a%2Fb", "test").unwrap();
        let result2 = decode_proxy_component("a%2fb", "test").unwrap();
        assert_eq!(result1, "a/b");
        assert_eq!(result2, "a/b");
    }

    #[test]
    fn decode_proxy_component_truncated_encoding_returns_error() {
        let result = decode_proxy_component("user%4", "field");
        assert!(result.is_err());
    }

    #[test]
    fn decode_proxy_component_invalid_hex_returns_error() {
        let result = decode_proxy_component("user%ZZ", "field");
        assert!(result.is_err());
    }

    #[test]
    fn decode_proxy_component_trailing_percent_returns_error() {
        let result = decode_proxy_component("test%", "field");
        assert!(result.is_err());
    }

    #[test]
    fn decode_proxy_component_invalid_utf8_returns_error() {
        let result = decode_proxy_component("%FF%FE", "field");
        assert!(result.is_err());
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
        let error = read_proxy_line(&mut stream, &mut buffer, display)
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
        let error = read_proxy_line(&mut stream, &mut buffer, display)
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
        let result = read_proxy_line(&mut stream, &mut buffer, display);
        handle.join().expect("server thread");
        (result, buffer)
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
