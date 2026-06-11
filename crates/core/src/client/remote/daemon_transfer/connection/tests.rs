use super::*;
use crate::auth::DaemonAuthDigest;

#[test]
fn parse_digest_list_from_greeting_with_full_list() {
    let greeting = "@RSYNCD: 31.0 sha512 sha256 sha1 md5 md4\n";
    let digests = parse_digest_list_from_greeting(greeting);
    assert_eq!(digests.len(), 5);
    assert_eq!(digests[0], DaemonAuthDigest::Sha512);
    assert_eq!(digests[1], DaemonAuthDigest::Sha256);
    assert_eq!(digests[2], DaemonAuthDigest::Sha1);
    assert_eq!(digests[3], DaemonAuthDigest::Md5);
    assert_eq!(digests[4], DaemonAuthDigest::Md4);
}

#[test]
fn parse_digest_list_from_greeting_with_partial_list() {
    let greeting = "@RSYNCD: 30.0 sha256 md5\n";
    let digests = parse_digest_list_from_greeting(greeting);
    assert_eq!(digests.len(), 2);
    assert_eq!(digests[0], DaemonAuthDigest::Sha256);
    assert_eq!(digests[1], DaemonAuthDigest::Md5);
}

#[test]
fn parse_digest_list_from_greeting_without_digests() {
    let greeting = "@RSYNCD: 29.0\n";
    let digests = parse_digest_list_from_greeting(greeting);
    assert!(digests.is_empty());
}

#[test]
fn parse_digest_list_from_greeting_ignores_unknown() {
    let greeting = "@RSYNCD: 31.0 sha512 unknown sha1 bogus md4\n";
    let digests = parse_digest_list_from_greeting(greeting);
    assert_eq!(digests.len(), 3);
    assert_eq!(digests[0], DaemonAuthDigest::Sha512);
    assert_eq!(digests[1], DaemonAuthDigest::Sha1);
    assert_eq!(digests[2], DaemonAuthDigest::Md4);
}

#[test]
fn parse_protocol_from_greeting_extracts_version() {
    let greeting = "@RSYNCD: 31.0 sha512 sha256\n";
    let protocol = parse_protocol_from_greeting(greeting).unwrap();
    assert_eq!(protocol.as_u8(), 31);
}

#[test]
fn extract_digest_list_strips_trailing_newline() {
    // upstream: compat.c:843-844 - the level-2 NSTR echo must render the
    // digest list verbatim, without the greeting's trailing newline.
    let greeting = "@RSYNCD: 31.0 sha512 sha256 sha1 md5 md4\n";
    let list = extract_digest_list_from_greeting(greeting);
    assert_eq!(list, Some("sha512 sha256 sha1 md5 md4"));
}

#[test]
fn extract_digest_list_handles_crlf() {
    let greeting = "@RSYNCD: 30.0 md5 md4\r\n";
    let list = extract_digest_list_from_greeting(greeting);
    assert_eq!(list, Some("md5 md4"));
}

#[test]
fn extract_digest_list_returns_none_for_version_only() {
    let greeting = "@RSYNCD: 29.0\n";
    let list = extract_digest_list_from_greeting(greeting);
    assert!(list.is_none());
}

#[test]
fn extract_digest_list_returns_none_for_blank_after_version() {
    let greeting = "@RSYNCD: 30.0\r\n";
    let list = extract_digest_list_from_greeting(greeting);
    assert!(list.is_none());
}

#[test]
fn extract_digest_list_preserves_unknown_tokens() {
    // upstream: compat.c:844 emits the raw banner string, including
    // unknown algorithm names. Parity matters for the diagnostic.
    let greeting = "@RSYNCD: 31.0 sha512 unknown sha1 bogus md4\n";
    let list = extract_digest_list_from_greeting(greeting);
    assert_eq!(list, Some("sha512 unknown sha1 bogus md4"));
}

#[test]
fn parse_protocol_from_greeting_handles_version_only() {
    let greeting = "@RSYNCD: 28.0\n";
    let protocol = parse_protocol_from_greeting(greeting).unwrap();
    assert_eq!(protocol.as_u8(), 28);
}

mod early_input_tests {
    use super::*;

    #[test]
    fn read_normal_file() {
        let dir = test_support::create_tempdir();
        let file_path = dir.path().join("early.txt");
        std::fs::write(&file_path, b"hello early input").unwrap();

        let data = read_early_input_file(&file_path).unwrap();
        assert_eq!(data, b"hello early input");
    }

    #[test]
    fn read_empty_file() {
        let dir = test_support::create_tempdir();
        let file_path = dir.path().join("empty.txt");
        std::fs::write(&file_path, b"").unwrap();

        let data = read_early_input_file(&file_path).unwrap();
        assert!(data.is_empty());
    }

    #[test]
    fn read_file_exactly_at_limit() {
        let dir = test_support::create_tempdir();
        let file_path = dir.path().join("exact.bin");
        let content = vec![0xABu8; EARLY_INPUT_MAX_SIZE];
        std::fs::write(&file_path, &content).unwrap();

        let data = read_early_input_file(&file_path).unwrap();
        assert_eq!(data.len(), EARLY_INPUT_MAX_SIZE);
        assert_eq!(data, content);
    }

    #[test]
    fn read_file_exceeding_limit_is_truncated() {
        let dir = test_support::create_tempdir();
        let file_path = dir.path().join("large.bin");
        let content = vec![0xCDu8; EARLY_INPUT_MAX_SIZE + 1024];
        std::fs::write(&file_path, &content).unwrap();

        let data = read_early_input_file(&file_path).unwrap();
        assert_eq!(data.len(), EARLY_INPUT_MAX_SIZE);
        assert_eq!(data, &content[..EARLY_INPUT_MAX_SIZE]);
    }

    #[test]
    fn read_missing_file_returns_error() {
        let dir = test_support::create_tempdir();
        let file_path = dir.path().join("nonexistent.txt");

        let err = read_early_input_file(&file_path).unwrap_err();
        assert_eq!(err.exit_code(), CLIENT_SERVER_PROTOCOL_EXIT_CODE);
        assert!(err.to_string().contains("failed to open"));
    }

    #[test]
    fn max_size_constant_is_5k() {
        assert_eq!(EARLY_INPUT_MAX_SIZE, 5120);
    }

    #[test]
    fn read_file_with_binary_content() {
        let dir = test_support::create_tempdir();
        let file_path = dir.path().join("binary.bin");
        let content: Vec<u8> = (0..=255u8).cycle().take(1024).collect();
        std::fs::write(&file_path, &content).unwrap();

        let data = read_early_input_file(&file_path).unwrap();
        assert_eq!(data, content);
    }

    #[test]
    fn read_file_well_over_limit_truncated_to_max() {
        let dir = test_support::create_tempdir();
        let file_path = dir.path().join("huge.bin");
        let content = vec![0xFFu8; EARLY_INPUT_MAX_SIZE * 10];
        std::fs::write(&file_path, &content).unwrap();

        let data = read_early_input_file(&file_path).unwrap();
        assert_eq!(data.len(), EARLY_INPUT_MAX_SIZE);
    }

    #[test]
    fn wire_format_header_matches_upstream_protocol() {
        let data = b"test-payload";
        let header = format!("{EARLY_INPUT_CMD}{}\n", data.len());
        assert_eq!(header, "#early_input=12\n");
    }

    #[test]
    fn wire_format_uses_decimal_length() {
        let data = vec![0u8; 256];
        let header = format!("{EARLY_INPUT_CMD}{}\n", data.len());
        assert_eq!(header, "#early_input=256\n");
    }

    #[test]
    fn wire_format_at_max_size() {
        let header = format!("{EARLY_INPUT_CMD}{EARLY_INPUT_MAX_SIZE}\n");
        assert_eq!(header, "#early_input=5120\n");
    }

    #[test]
    fn wire_format_complete_message_structure() {
        let payload = b"auth-token";
        let header = format!("{EARLY_INPUT_CMD}{}\n", payload.len());
        let mut wire = header.into_bytes();
        wire.extend_from_slice(payload);

        let newline_pos = wire.iter().position(|&b| b == b'\n').unwrap();
        let header_part = std::str::from_utf8(&wire[..newline_pos]).unwrap();
        assert_eq!(header_part, "#early_input=10");
        assert_eq!(&wire[newline_pos + 1..], b"auth-token");
    }

    #[test]
    fn early_input_cmd_constant_matches_upstream() {
        assert_eq!(EARLY_INPUT_CMD, "#early_input=");
    }
}

/// Integration tests verifying the complete early-input round-trip:
/// client reads a file, sends it over a TCP socket, and the daemon-side
/// wire format is validated against protocol expectations.
mod early_input_roundtrip_tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read};
    use std::net::{TcpListener, TcpStream};

    fn test_request() -> DaemonTransferRequest {
        DaemonTransferRequest {
            address: DaemonAddress::new("127.0.0.1".to_owned(), 873).unwrap(),
            module: "test".to_owned(),
            path: String::new(),
            username: None,
        }
    }

    /// Reads the early-input wire message from a stream, parsing the
    /// `#early_input=<len>\n` header and the raw payload bytes.
    ///
    /// Returns `None` if no data was sent (e.g. empty file case).
    fn receive_early_input(reader: &mut BufReader<impl Read>) -> Option<Vec<u8>> {
        let mut line = String::new();
        let n = reader.read_line(&mut line).unwrap();
        if n == 0 {
            return None;
        }

        let trimmed = line.trim_end_matches('\n');
        let len_str = trimmed.strip_prefix(EARLY_INPUT_CMD)?;
        let data_len: usize = len_str.parse().unwrap();

        let mut buf = vec![0u8; data_len];
        reader.read_exact(&mut buf).unwrap();
        Some(buf)
    }

    #[test]
    fn roundtrip_normal_content() {
        let dir = test_support::create_tempdir();
        let file_path = dir.path().join("early.txt");
        let content = b"hello early-input roundtrip";
        std::fs::write(&file_path, content).unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let mut client = TcpStream::connect(addr).unwrap();
        let (server, _) = listener.accept().unwrap();

        let request = test_request();
        send_early_input(&mut client, &file_path, &request).unwrap();
        drop(client);

        let mut reader = BufReader::new(server);
        let received = receive_early_input(&mut reader).unwrap();
        assert_eq!(received, content);
    }

    #[test]
    fn roundtrip_empty_file_sends_nothing() {
        let dir = test_support::create_tempdir();
        let file_path = dir.path().join("empty.txt");
        std::fs::write(&file_path, b"").unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let mut client = TcpStream::connect(addr).unwrap();
        let (server, _) = listener.accept().unwrap();

        let request = test_request();
        send_early_input(&mut client, &file_path, &request).unwrap();
        drop(client);

        let mut reader = BufReader::new(server);
        let received = receive_early_input(&mut reader);
        assert!(
            received.is_none(),
            "empty file should not produce any wire data"
        );
    }

    #[test]
    fn roundtrip_file_exactly_at_5k_limit() {
        let dir = test_support::create_tempdir();
        let file_path = dir.path().join("exact.bin");
        let content = vec![0xABu8; EARLY_INPUT_MAX_SIZE];
        std::fs::write(&file_path, &content).unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let mut client = TcpStream::connect(addr).unwrap();
        let (server, _) = listener.accept().unwrap();

        let request = test_request();
        send_early_input(&mut client, &file_path, &request).unwrap();
        drop(client);

        let mut reader = BufReader::new(server);
        let received = receive_early_input(&mut reader).unwrap();
        assert_eq!(received.len(), EARLY_INPUT_MAX_SIZE);
        assert_eq!(received, content);
    }

    #[test]
    fn roundtrip_file_over_limit_is_truncated() {
        let dir = test_support::create_tempdir();
        let file_path = dir.path().join("large.bin");
        let content = vec![0xCDu8; EARLY_INPUT_MAX_SIZE + 2048];
        std::fs::write(&file_path, &content).unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let mut client = TcpStream::connect(addr).unwrap();
        let (server, _) = listener.accept().unwrap();

        let request = test_request();
        send_early_input(&mut client, &file_path, &request).unwrap();
        drop(client);

        let mut reader = BufReader::new(server);
        let received = receive_early_input(&mut reader).unwrap();
        assert_eq!(received.len(), EARLY_INPUT_MAX_SIZE);
        assert_eq!(received, &content[..EARLY_INPUT_MAX_SIZE]);
    }

    #[test]
    fn roundtrip_binary_content_preserves_all_byte_values() {
        let dir = test_support::create_tempdir();
        let file_path = dir.path().join("binary.bin");
        let content: Vec<u8> = (0..=255u8).cycle().take(1024).collect();
        std::fs::write(&file_path, &content).unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let mut client = TcpStream::connect(addr).unwrap();
        let (server, _) = listener.accept().unwrap();

        let request = test_request();
        send_early_input(&mut client, &file_path, &request).unwrap();
        drop(client);

        let mut reader = BufReader::new(server);
        let received = receive_early_input(&mut reader).unwrap();
        assert_eq!(received, content);
    }

    #[test]
    fn roundtrip_wire_header_matches_daemon_protocol() {
        let dir = test_support::create_tempdir();
        let file_path = dir.path().join("proto.txt");
        let content = b"auth-token-data";
        std::fs::write(&file_path, content).unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let mut client = TcpStream::connect(addr).unwrap();
        let (server, _) = listener.accept().unwrap();

        let request = test_request();
        send_early_input(&mut client, &file_path, &request).unwrap();
        drop(client);

        let mut raw = Vec::new();
        let mut server = server;
        std::io::Read::read_to_end(&mut server, &mut raw).unwrap();

        let expected_header = format!("#early_input={}\n", content.len());
        let header_len = expected_header.len();

        assert_eq!(
            std::str::from_utf8(&raw[..header_len]).unwrap(),
            expected_header
        );
        assert_eq!(&raw[header_len..], content);
    }

    #[test]
    fn roundtrip_single_byte_file() {
        let dir = test_support::create_tempdir();
        let file_path = dir.path().join("one.bin");
        std::fs::write(&file_path, [0x42]).unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let mut client = TcpStream::connect(addr).unwrap();
        let (server, _) = listener.accept().unwrap();

        let request = test_request();
        send_early_input(&mut client, &file_path, &request).unwrap();
        drop(client);

        let mut reader = BufReader::new(server);
        let received = receive_early_input(&mut reader).unwrap();
        assert_eq!(received, vec![0x42]);
    }

    #[test]
    fn roundtrip_content_with_newlines_and_nulls() {
        let dir = test_support::create_tempdir();
        let file_path = dir.path().join("special.bin");
        let content = b"line1\nline2\n\0\0\nline3\n";
        std::fs::write(&file_path, content).unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let mut client = TcpStream::connect(addr).unwrap();
        let (server, _) = listener.accept().unwrap();

        let request = test_request();
        send_early_input(&mut client, &file_path, &request).unwrap();
        drop(client);

        let mut reader = BufReader::new(server);
        let received = receive_early_input(&mut reader).unwrap();
        assert_eq!(received, content);
    }
}

/// Pins the daemon `@ERROR` -> client mapping.
///
/// Regression coverage for the UTS-22.b port of upstream's
/// `testsuite/daemon-chroot-acl_test.py`. The python test greps the
/// client's combined output for `@ERROR` to confirm the GHSA-rjfm-3w2m-jf4f
/// hostname-deny path fired; if the client renders only its envelope
/// (`oc-rsync error: access denied ... (code 5)`) without echoing the
/// raw `@ERROR:` line, the regression check silently fails-OPEN even
/// though the daemon correctly denied the connection.
///
/// The daemon-side GHSA hardening lives in
/// `crates/daemon/src/daemon/module_state/definition.rs::permits` and is
/// covered by `tests/chunks/module_hostname_deny_fails_closed_when_dns_unresolved.rs`;
/// this test pins the matching client-side rendering so the two halves
/// can never drift out of sync.
#[cfg(test)]
mod handle_at_error_tests {
    use super::*;
    use crate::client::error::CLIENT_SERVER_PROTOCOL_EXIT_CODE;

    #[test]
    fn payload_strips_at_error_prefix() {
        let err = handle_daemon_at_error(
            "@ERROR: access denied to chrootmod from 127.0.0.1 (127.0.0.1)",
        );

        let rendered = err.to_string();
        assert!(
            rendered.contains("access denied to chrootmod"),
            "expected payload in rendered error, got: {rendered}"
        );
        assert!(
            !rendered.contains("@ERROR: "),
            "structured envelope should not duplicate the @ERROR prefix, got: {rendered}"
        );
    }

    #[test]
    fn payload_falls_through_when_prefix_format_differs() {
        // upstream sometimes emits "@ERROR foo" (no colon) when the C
        // path uses io_printf with concatenated tokens; the strip must
        // not return None, the whole line is kept verbatim.
        let err = handle_daemon_at_error("@ERROR no colon variant");
        let rendered = err.to_string();
        assert!(
            rendered.contains("@ERROR no colon variant"),
            "fall-through path must preserve the whole line, got: {rendered}"
        );
    }

    #[test]
    fn maps_to_client_server_protocol_exit_code() {
        // upstream: main.c:1879 - @ERROR client-server handshake failures
        // exit with RERR_PROTOCOL (code 5).
        let err = handle_daemon_at_error("@ERROR: auth failed on module foo");
        assert_eq!(err.exit_code(), CLIENT_SERVER_PROTOCOL_EXIT_CODE);
    }

    #[test]
    fn ghsa_rjfm_3w2m_jf4f_payload_renders_intact() {
        // The exact wire string the daemon emits for the GHSA-rjfm-3w2m-jf4f
        // hostname-deny ACL path. upstream: clientserver.c:733 -
        // `@ERROR: access denied to %s from %s (%s)\n`.
        let wire = "@ERROR: access denied to chrootmod from 127.0.0.1 (127.0.0.1)";
        let err = handle_daemon_at_error(wire);

        let rendered = err.to_string();
        assert!(
            rendered.contains("access denied to chrootmod from 127.0.0.1 (127.0.0.1)"),
            "GHSA hostname-deny payload must round-trip into client error, got: {rendered}"
        );
        assert_eq!(err.exit_code(), CLIENT_SERVER_PROTOCOL_EXIT_CODE);
    }
}
