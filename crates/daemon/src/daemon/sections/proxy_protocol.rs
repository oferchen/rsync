//! PROXY protocol v1/v2 header parsing for daemon connections.
//!
//! When `proxy protocol = true` is set in the daemon configuration, the
//! daemon reads a PROXY protocol header from the TCP stream BEFORE the
//! `@RSYNCD:` greeting. This allows load balancers and proxies (e.g.,
//! HAProxy) to convey the original client address.
//!
//! upstream: clientserver.c:1298 - `if (lp_proxy_protocol() && !read_proxy_protocol_header(f_in))`
//!
//! References:
//! - PROXY protocol v1: <https://www.haproxy.org/download/1.8/doc/proxy-protocol.txt>
//! - PROXY protocol v2: same document, section 2.2

/// Binary signature that opens every PROXY protocol v2 header.
///
/// Defined by the PROXY protocol specification as 12 bytes that are
/// unlikely to collide with any other protocol's handshake.
const PROXY_V2_SIGNATURE: [u8; 12] = *b"\r\n\r\n\0\r\nQUIT\n";

/// Maximum length of a PROXY protocol v1 text line (including CRLF).
const PROXY_V1_MAX_LINE: usize = 108;

/// v2 command: connection established by the proxy itself (health check).
const CMD_LOCAL: u8 = 0x00;

/// v2 command: proxied connection carrying real client addresses.
const CMD_PROXY: u8 = 0x01;

/// v2 address family + transport: IPv4 over TCP (AF_INET, STREAM).
const FAM_TCP4: u8 = 0x11;

/// v2 address family + transport: IPv6 over TCP (AF_INET6, STREAM).
const FAM_TCP6: u8 = 0x21;

/// Parses a PROXY protocol v1 or v2 header from the given reader.
///
/// Returns `Ok(Some(addr))` when the header carries a proxied client
/// address, `Ok(None)` for LOCAL/UNKNOWN commands (health checks), or
/// an I/O error when the header is malformed or the stream is truncated.
fn parse_proxy_header_from<R: Read>(reader: &mut R) -> io::Result<Option<SocketAddr>> {
    let mut prefix = [0u8; 12];
    reader.read_exact(&mut prefix)?;

    if prefix == PROXY_V2_SIGNATURE {
        parse_v2_header(reader)
    } else if prefix.starts_with(b"PROXY") {
        parse_v1_header(&prefix, reader)
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unrecognized PROXY protocol header",
        ))
    }
}

/// Parses the remainder of a PROXY protocol v2 binary header.
///
/// The 12-byte signature has already been consumed. This reads the 4
/// remaining fixed bytes (ver_cmd, fam, len) plus the address payload.
fn parse_v2_header<R: Read>(reader: &mut R) -> io::Result<Option<SocketAddr>> {
    let mut tail = [0u8; 4];
    reader.read_exact(&mut tail)?;

    let ver_cmd = tail[0];
    let fam = tail[1];
    let payload_len = u16::from_be_bytes([tail[2], tail[3]]) as usize;

    // Version must be 0x2x.
    if ver_cmd & 0xF0 != 0x20 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported PROXY v2 version: {:#04x}", ver_cmd & 0xF0),
        ));
    }

    let cmd = ver_cmd & 0x0F;

    // Read and consume the entire payload regardless of command, so
    // the stream is correctly positioned for subsequent protocol data.
    let mut payload = vec![0u8; payload_len];
    if payload_len > 0 {
        reader.read_exact(&mut payload)?;
    }

    match cmd {
        CMD_LOCAL => Ok(None),
        CMD_PROXY => parse_v2_address(fam, &payload),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported PROXY v2 command: {cmd:#04x}"),
        )),
    }
}

/// Extracts a socket address from a v2 address payload.
///
/// For TCP4: 12 bytes (4 src + 4 dst + 2 src_port + 2 dst_port).
/// For TCP6: 36 bytes (16 src + 16 dst + 2 src_port + 2 dst_port).
fn parse_v2_address(fam: u8, payload: &[u8]) -> io::Result<Option<SocketAddr>> {
    match fam {
        FAM_TCP4 => {
            if payload.len() < 12 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "PROXY v2 TCP4 payload too short",
                ));
            }
            let src_ip = Ipv4Addr::new(payload[0], payload[1], payload[2], payload[3]);
            let src_port = u16::from_be_bytes([payload[8], payload[9]]);
            Ok(Some(SocketAddr::new(IpAddr::V4(src_ip), src_port)))
        }
        FAM_TCP6 => {
            if payload.len() < 36 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "PROXY v2 TCP6 payload too short",
                ));
            }
            let mut src_bytes = [0u8; 16];
            src_bytes.copy_from_slice(&payload[..16]);
            let src_ip = Ipv6Addr::from(src_bytes);
            let src_port = u16::from_be_bytes([payload[32], payload[33]]);
            Ok(Some(SocketAddr::new(IpAddr::V6(src_ip), src_port)))
        }
        _ => {
            // Unknown family (e.g., AF_UNIX) - treat as transparent pass-through.
            Ok(None)
        }
    }
}

/// Parses a PROXY protocol v1 text header.
///
/// The first 12 bytes have already been read into `prefix`. This reads
/// the remainder of the line (up to `PROXY_V1_MAX_LINE` total) and
/// extracts the source address and port.
fn parse_v1_header<R: Read>(prefix: &[u8; 12], reader: &mut R) -> io::Result<Option<SocketAddr>> {
    // Read remaining bytes of the v1 line (max 108 total, 12 consumed).
    let remaining_max = PROXY_V1_MAX_LINE - prefix.len();
    let mut rest = vec![0u8; remaining_max];
    let mut rest_len = 0;

    // Read byte by byte until newline or limit.
    for slot in rest.iter_mut().take(remaining_max) {
        let mut byte = [0u8; 1];
        reader.read_exact(&mut byte)?;
        *slot = byte[0];
        rest_len += 1;
        if byte[0] == b'\n' {
            break;
        }
    }

    // Assemble the full line.
    let mut line = Vec::with_capacity(prefix.len() + rest_len);
    line.extend_from_slice(prefix);
    line.extend_from_slice(&rest[..rest_len]);

    let line_str = std::str::from_utf8(&line).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "PROXY v1 header contains invalid UTF-8",
        )
    })?;

    let line_str = line_str.trim_end_matches(['\r', '\n']);

    // Format: "PROXY <proto> <src_addr> <dst_addr> <src_port> <dst_port>"
    let parts: Vec<&str> = line_str.split(' ').collect();
    if parts.len() < 2 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "PROXY v1 header too short",
        ));
    }

    match parts[1] {
        "UNKNOWN" => Ok(None),
        "TCP4" | "TCP6" => {
            if parts.len() < 6 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("PROXY v1 {} header missing fields", parts[1]),
                ));
            }
            let src_addr: IpAddr = parts[2].parse().map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("invalid PROXY v1 source address: {}", parts[2]),
                )
            })?;
            let src_port: u16 = parts[4].parse().map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("invalid PROXY v1 source port: {}", parts[4]),
                )
            })?;
            Ok(Some(SocketAddr::new(src_addr, src_port)))
        }
        proto => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported PROXY v1 protocol: {proto}"),
        )),
    }
}

/// Reads and parses a PROXY protocol header from a TCP stream.
///
/// Returns the original client address when a proxied header is present,
/// or `None` for health-check / LOCAL connections.
pub(crate) fn parse_proxy_header(stream: &TcpStream) -> io::Result<Option<SocketAddr>> {
    // Borrow as Read via reference - TcpStream implements Read for &TcpStream.
    let mut reader = BufReader::new(stream);
    parse_proxy_header_from(&mut reader)
}

#[cfg(test)]
mod proxy_protocol_tests {
    use super::*;

    #[test]
    fn v1_tcp4_header() {
        let header = b"PROXY TCP4 192.168.1.100 10.0.0.1 56324 873\r\n";
        let mut cursor = io::Cursor::new(header.to_vec());
        let result = parse_proxy_header_from(&mut cursor).unwrap();
        let addr = result.expect("should return Some for TCP4");
        assert_eq!(addr.ip(), IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100)));
        assert_eq!(addr.port(), 56324);
    }

    #[test]
    fn v1_tcp6_header() {
        let header = b"PROXY TCP6 2001:db8::1 2001:db8::2 56324 873\r\n";
        let mut cursor = io::Cursor::new(header.to_vec());
        let result = parse_proxy_header_from(&mut cursor).unwrap();
        let addr = result.expect("should return Some for TCP6");
        assert_eq!(
            addr.ip(),
            IpAddr::V6("2001:db8::1".parse::<Ipv6Addr>().unwrap())
        );
        assert_eq!(addr.port(), 56324);
    }

    #[test]
    fn v1_unknown_returns_none() {
        let header = b"PROXY UNKNOWN\r\n";
        let mut cursor = io::Cursor::new(header.to_vec());
        let result = parse_proxy_header_from(&mut cursor).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn v2_tcp4_cmd_proxy() {
        // Build a v2 header: signature + ver_cmd(0x21=v2+PROXY) + fam(0x11=TCP4) + len(12)
        // Payload: src_ip(4) + dst_ip(4) + src_port(2) + dst_port(2) = 12 bytes
        let mut header = Vec::new();
        header.extend_from_slice(&PROXY_V2_SIGNATURE);
        header.push(0x21); // version 2, command PROXY
        header.push(FAM_TCP4);
        header.extend_from_slice(&12u16.to_be_bytes()); // payload length
        // Source IP: 10.20.30.40
        header.extend_from_slice(&[10, 20, 30, 40]);
        // Destination IP: 192.168.1.1
        header.extend_from_slice(&[192, 168, 1, 1]);
        // Source port: 12345
        header.extend_from_slice(&12345u16.to_be_bytes());
        // Destination port: 873
        header.extend_from_slice(&873u16.to_be_bytes());

        let mut cursor = io::Cursor::new(header);
        let result = parse_proxy_header_from(&mut cursor).unwrap();
        let addr = result.expect("should return Some for v2 TCP4 PROXY");
        assert_eq!(addr.ip(), IpAddr::V4(Ipv4Addr::new(10, 20, 30, 40)));
        assert_eq!(addr.port(), 12345);
    }

    #[test]
    fn v2_tcp6_cmd_proxy() {
        let mut header = Vec::new();
        header.extend_from_slice(&PROXY_V2_SIGNATURE);
        header.push(0x21); // version 2, command PROXY
        header.push(FAM_TCP6);
        header.extend_from_slice(&36u16.to_be_bytes()); // payload length
        // Source IP: 2001:db8::1
        let src_ip: Ipv6Addr = "2001:db8::1".parse().unwrap();
        header.extend_from_slice(&src_ip.octets());
        // Destination IP: 2001:db8::2
        let dst_ip: Ipv6Addr = "2001:db8::2".parse().unwrap();
        header.extend_from_slice(&dst_ip.octets());
        // Source port: 54321
        header.extend_from_slice(&54321u16.to_be_bytes());
        // Destination port: 873
        header.extend_from_slice(&873u16.to_be_bytes());

        let mut cursor = io::Cursor::new(header);
        let result = parse_proxy_header_from(&mut cursor).unwrap();
        let addr = result.expect("should return Some for v2 TCP6 PROXY");
        assert_eq!(addr.ip(), IpAddr::V6(src_ip));
        assert_eq!(addr.port(), 54321);
    }

    #[test]
    fn v2_cmd_local_returns_none() {
        let mut header = Vec::new();
        header.extend_from_slice(&PROXY_V2_SIGNATURE);
        header.push(0x20); // version 2, command LOCAL
        header.push(0x00); // unspec family
        header.extend_from_slice(&0u16.to_be_bytes()); // no payload

        let mut cursor = io::Cursor::new(header);
        let result = parse_proxy_header_from(&mut cursor).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn invalid_header_returns_error() {
        let garbage = b"GET / HTTP/1.1\r\n";
        let mut cursor = io::Cursor::new(garbage.to_vec());
        let result = parse_proxy_header_from(&mut cursor);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("unrecognized PROXY protocol"));
    }

    #[test]
    fn truncated_stream_returns_error() {
        // Only 6 bytes - not enough for even the 12-byte prefix read.
        let short = b"PROXY ";
        let mut cursor = io::Cursor::new(short.to_vec());
        let result = parse_proxy_header_from(&mut cursor);
        assert!(result.is_err());
    }

    #[test]
    fn v2_truncated_payload_returns_error() {
        let mut header = Vec::new();
        header.extend_from_slice(&PROXY_V2_SIGNATURE);
        header.push(0x21); // version 2, command PROXY
        header.push(FAM_TCP4);
        header.extend_from_slice(&12u16.to_be_bytes()); // claims 12 bytes
        // Only provide 4 bytes of payload
        header.extend_from_slice(&[10, 20, 30, 40]);

        let mut cursor = io::Cursor::new(header);
        let result = parse_proxy_header_from(&mut cursor);
        assert!(result.is_err());
    }

    #[test]
    fn v2_unsupported_version_returns_error() {
        let mut header = Vec::new();
        header.extend_from_slice(&PROXY_V2_SIGNATURE);
        header.push(0x31); // version 3 (unsupported)
        header.push(FAM_TCP4);
        header.extend_from_slice(&0u16.to_be_bytes());

        let mut cursor = io::Cursor::new(header);
        let result = parse_proxy_header_from(&mut cursor);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unsupported PROXY v2 version"));
    }

    #[test]
    fn v1_missing_fields_returns_error() {
        let header = b"PROXY TCP4 192.168.1.1\r\n";
        let mut cursor = io::Cursor::new(header.to_vec());
        let result = parse_proxy_header_from(&mut cursor);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("missing fields"));
    }

    #[test]
    fn v2_unknown_family_returns_none() {
        let mut header = Vec::new();
        header.extend_from_slice(&PROXY_V2_SIGNATURE);
        header.push(0x21); // version 2, command PROXY
        header.push(0x31); // AF_UNIX + STREAM (unsupported family)
        header.extend_from_slice(&4u16.to_be_bytes());
        header.extend_from_slice(&[0, 0, 0, 0]); // dummy payload

        let mut cursor = io::Cursor::new(header);
        let result = parse_proxy_header_from(&mut cursor).unwrap();
        assert!(result.is_none());
    }
}
