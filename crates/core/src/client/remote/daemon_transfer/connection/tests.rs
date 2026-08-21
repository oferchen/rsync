use super::*;
use crate::auth as core_auth;
use crate::auth::{DaemonAuthDigest, parse_daemon_digest_list};

fn digests_of(greeting: &str) -> Vec<DaemonAuthDigest> {
    parse_daemon_digest_list(
        advertised_digests_in_greeting(greeting)
            .names()
            .unwrap_or_default(),
    )
}

#[test]
fn greeting_digest_list_parses_the_full_advertisement() {
    let digests = digests_of("@RSYNCD: 31.0 sha512 sha256 sha1 md5 md4\n");
    assert_eq!(
        digests,
        [
            DaemonAuthDigest::Sha512,
            DaemonAuthDigest::Sha256,
            DaemonAuthDigest::Sha1,
            DaemonAuthDigest::Md5,
            DaemonAuthDigest::Md4,
        ]
    );
}

#[test]
fn greeting_digest_list_parses_a_partial_advertisement() {
    let digests = digests_of("@RSYNCD: 30.0 sha256 md5\n");
    assert_eq!(digests, [DaemonAuthDigest::Sha256, DaemonAuthDigest::Md5]);
}

#[test]
fn greeting_digest_list_is_empty_when_none_was_advertised() {
    assert!(digests_of("@RSYNCD: 29.0\n").is_empty());
}

#[test]
fn greeting_digest_list_ignores_unknown_names() {
    let digests = digests_of("@RSYNCD: 31.0 sha512 unknown sha1 bogus md4\n");
    assert_eq!(
        digests,
        [
            DaemonAuthDigest::Sha512,
            DaemonAuthDigest::Sha1,
            DaemonAuthDigest::Md4,
        ]
    );
}

#[test]
fn parse_protocol_from_greeting_extracts_version() {
    let greeting = "@RSYNCD: 31.0 sha512 sha256\n";
    let protocol = parse_protocol_from_greeting(greeting).unwrap();
    assert_eq!(protocol.as_u8(), 31);
}

#[test]
fn advertised_digests_strip_the_trailing_newline() {
    // upstream: compat.c:843-844 - the level-2 NSTR echo must render the
    // digest list verbatim, without the greeting's trailing newline.
    assert_eq!(
        advertised_digests_in_greeting("@RSYNCD: 31.0 sha512 sha256 sha1 md5 md4\n").names(),
        Some("sha512 sha256 sha1 md5 md4"),
    );
    assert_eq!(
        advertised_digests_in_greeting("@RSYNCD: 30.0 md5 md4\r\n").names(),
        Some("md5 md4"),
    );
}

#[test]
fn advertised_digests_are_absent_for_a_version_only_greeting() {
    assert!(advertised_digests_in_greeting("@RSYNCD: 29.0\n").is_absent());
    assert!(advertised_digests_in_greeting("@RSYNCD: 30.0\r\n").is_absent());
}

// upstream: clientserver.c:199-203 - a server greeting that ends in the
// separating space advertises an EMPTY list, which the client must NOT read as
// "no list": upstream aborts on it (compat.c:383-406) instead of falling back to
// the protocol-keyed default.
#[test]
fn a_trailing_space_in_the_server_greeting_advertises_an_empty_list() {
    let advertised = advertised_digests_in_greeting("@RSYNCD: 31.0 \n");
    assert!(advertised.is_present());
    assert_eq!(advertised.names(), Some(""));
    assert_eq!(
        negotiate_client_daemon_digest(advertised, 31),
        Err(core_auth::NoMutualDaemonAuthDigest),
    );
}

#[test]
fn advertised_digests_preserve_unknown_tokens() {
    // upstream: compat.c:844 emits the raw banner string, including
    // unknown algorithm names. Parity matters for the diagnostic.
    assert_eq!(
        advertised_digests_in_greeting("@RSYNCD: 31.0 sha512 unknown sha1 bogus md4\n").names(),
        Some("sha512 unknown sha1 bogus md4"),
    );
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
            address: DaemonAddress::new("127.0.0.1".to_owned(), 873),
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
        let err =
            handle_daemon_at_error("@ERROR: access denied to chrootmod from 127.0.0.1 (127.0.0.1)");

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

    /// Regression: an unexpected daemon disconnect during the module-response
    /// phase must fail promptly, not busy-loop printing blank lines.
    ///
    /// A daemon that detects a fatal condition (e.g. a strict-modes secrets
    /// violation) and drops the socket without a proper `@RSYNCD:`/`@ERROR`
    /// line leaves the client's response reader at EOF. Before the fix, the
    /// `read_line` loop ignored the 0-byte return, `trim()` produced an empty
    /// string matching no branch, and the loop spun forever emitting empty
    /// MOTD lines. The guard must convert EOF into a clean protocol error.
    ///
    /// upstream: clientserver.c:359-361 - `read_line_old()` returning false
    /// yields "didn't get server startup line" and `return -1` (RERR_PROTOCOL).
    #[test]
    fn handshake_errors_on_daemon_eof_instead_of_looping() {
        use std::io::{BufReader, Cursor};

        let request = DaemonTransferRequest {
            address: DaemonAddress::new("127.0.0.1".to_owned(), 873),
            module: "mod".to_owned(),
            path: String::new(),
            username: None,
        };

        // Valid greeting (subprotocol + digest list, as a real protocol-32
        // daemon sends), then EOF: the daemon accepted the greeting but closed
        // the control socket before replying to the module request.
        let mut reader = BufReader::new(Cursor::new(
            b"@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4\n".to_vec(),
        ));
        let mut writer: Vec<u8> = Vec::new();

        let result = perform_daemon_handshake(
            &mut reader,
            &mut writer,
            &request,
            true,
            &[],
            None,
            None,
            None,
        );

        let err = result.expect_err("EOF mid-handshake must be an error, not a hang");
        assert_eq!(
            err.exit_code(),
            CLIENT_SERVER_PROTOCOL_EXIT_CODE,
            "daemon EOF must map to the client-server protocol exit code"
        );
    }

    fn handshake_over_greeting(greeting: &[u8]) -> Result<ProtocolVersion, ClientError> {
        use std::io::{BufReader, Cursor};

        let request = DaemonTransferRequest {
            address: DaemonAddress::new("127.0.0.1".to_owned(), 873),
            module: "mod".to_owned(),
            path: String::new(),
            username: None,
        };
        let mut reader = BufReader::new(Cursor::new(greeting.to_vec()));
        let mut writer: Vec<u8> = Vec::new();
        perform_daemon_handshake(
            &mut reader,
            &mut writer,
            &request,
            true,
            &[],
            None,
            None,
            None,
        )
    }

    // upstream: clientserver.c:189-194 (am_client == 1) - a server greeting at
    // protocol >= 30 that omits the ".subprotocol" suffix is fatal:
    // `rsync: the server omitted the subprotocol value: <buf>` + RERR_STARTCLIENT.
    // The gate fires at protocol 30, one below the shared parser's own leniency
    // boundary, so this is exactly the divergence the client must now reject.
    #[test]
    fn client_rejects_server_greeting_missing_subprotocol() {
        let err = handshake_over_greeting(b"@RSYNCD: 30\n")
            .expect_err("missing subprotocol at protocol >= 30 must abort");
        assert_eq!(err.exit_code(), CLIENT_SERVER_PROTOCOL_EXIT_CODE);
        assert!(
            err.message()
                .to_string()
                .contains("the server omitted the subprotocol value: @RSYNCD: 30"),
            "unexpected message: {}",
            err.message()
        );
    }

    // upstream: clientserver.c:205-210 (am_client == 1) - a protocol > 31 greeting
    // that omits the digest name list is fatal:
    // `rsync: the server omitted the digest name list: <buf>` + RERR_STARTCLIENT.
    #[test]
    fn client_rejects_server_greeting_missing_digest_list() {
        let err = handshake_over_greeting(b"@RSYNCD: 32.0\n")
            .expect_err("missing digest list at protocol > 31 must abort");
        assert_eq!(err.exit_code(), CLIENT_SERVER_PROTOCOL_EXIT_CODE);
        assert!(
            err.message()
                .to_string()
                .contains("the server omitted the digest name list: @RSYNCD: 32.0"),
            "unexpected message: {}",
            err.message()
        );
    }

    // upstream: clientserver.c:196 - protocol < 30 defaults remote_sub to 0 and
    // needs no digest list, so the greeting is accepted and the handshake
    // proceeds (here it reaches the module-response phase and hits EOF). The
    // failure must NOT be the greeting-omission refusal.
    #[test]
    fn client_accepts_legacy_server_greeting_without_tokens() {
        let err = handshake_over_greeting(b"@RSYNCD: 29\n")
            .expect_err("EOF after a valid legacy greeting still errors");
        assert!(
            !err.message().to_string().contains("the server omitted"),
            "legacy greeting must not be refused for omitted tokens: {}",
            err.message()
        );
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

#[cfg(feature = "quic")]
mod quic_url_tests {
    use super::*;
    use crate::client::module_list::Transport;

    #[test]
    fn parse_rsync_url_selects_tcp() {
        // WHY: `rsync://` transfers keep the TCP transport (default behaviour).
        let request =
            DaemonTransferRequest::parse_rsync_url("rsync://host/mod/path").expect("parse");
        assert_eq!(request.address.transport(), Transport::Tcp);
        assert_eq!(request.address.port(), 873);
        assert_eq!(request.module, "mod");
    }

    #[test]
    fn parse_quic_url_selects_quic_default_port() {
        // WHY (QUIC-8b): `quic://` is parsed beside `rsync://` and yields the
        // QUIC transport on the shared default port 873 (873/udp).
        let request = DaemonTransferRequest::parse_quic_url("quic://host/mod/path").expect("parse");
        assert_eq!(request.address.transport(), Transport::Quic);
        assert_eq!(request.address.port(), 873);
        assert_eq!(request.module, "mod");
        assert_eq!(request.path, "path");
    }

    #[test]
    fn parse_quic_url_honours_explicit_port() {
        // WHY: an explicit `:port` overrides the 873 default, as for `rsync://`.
        let request = DaemonTransferRequest::parse_quic_url("quic://host:4321/mod").expect("parse");
        assert_eq!(request.address.port(), 4321);
        assert_eq!(request.address.transport(), Transport::Quic);
    }

    #[test]
    fn parse_quic_url_requires_module() {
        // WHY: a module is mandatory, and the diagnostic names the quic scheme.
        let err = DaemonTransferRequest::parse_quic_url("quic://host/").expect_err("no module");
        assert!(
            err.message()
                .to_string()
                .contains("quic:// URL must specify a module")
        );
    }

    #[test]
    fn with_transport_upgrades_double_colon_to_quic() {
        // WHY (QUIC-8c): `--quic` upgrades an ordinary `host::` target to QUIC.
        let request = DaemonTransferRequest::parse_double_colon("host::mod/path")
            .expect("parse")
            .with_transport(Transport::Quic);
        assert_eq!(request.address.transport(), Transport::Quic);
        assert_eq!(request.address.port(), 873);
    }
}

/// The username sent in the `@RSYNCD` auth response, one case per row of the
/// table measured against the real rsync 3.5.0 client.
///
/// upstream: clientserver.c:289-292 (`USER` then `LOGNAME`) composed with
/// authenticate.c:451-452 (`if (!user || !*user) user = "nobody";`).
mod daemon_auth_username_tests {
    use super::super::resolve_auth_username;

    /// `USER` wins when set, and is the only variable consulted.
    #[test]
    fn user_env_is_preferred() {
        assert_eq!(resolve_auth_username(None, Some("alice"), None), "alice");
        assert_eq!(
            resolve_auth_username(None, Some("alice"), Some("bob")),
            "alice"
        );
    }

    /// `LOGNAME` is the second variable - NOT `USERNAME`, which upstream never
    /// reads. oc previously consulted `USERNAME` and so authenticated as
    /// `rsync` on any host that sets only `LOGNAME`.
    #[test]
    fn logname_is_the_second_variable() {
        assert_eq!(resolve_auth_username(None, None, Some("bob")), "bob");
    }

    /// An unset environment authenticates as `nobody`, not `rsync`.
    ///
    /// upstream: authenticate.c:452.
    #[test]
    fn absent_environment_falls_back_to_nobody() {
        assert_eq!(resolve_auth_username(None, None, None), "nobody");
    }

    /// A SET-but-empty `USER` stops the chain: upstream's `getenv` returns a
    /// non-NULL empty string, so `if (!user)` is false and `LOGNAME` is never
    /// consulted; only `auth_client`'s `!*user` rescues it, as `nobody`.
    ///
    /// This is the row most likely to be got wrong by a naive
    /// "first non-empty wins" reading, and the real 3.5.0 client was measured
    /// producing `nobody` here.
    #[test]
    fn empty_user_env_does_not_fall_through_to_logname() {
        assert_eq!(resolve_auth_username(None, Some(""), Some("bob")), "nobody");
    }

    /// An explicit `rsync://user@host/` name skips the environment entirely.
    #[test]
    fn an_explicit_username_skips_the_environment() {
        assert_eq!(
            resolve_auth_username(Some("carol"), Some("alice"), Some("bob")),
            "carol"
        );
    }

    /// ...and an explicitly empty one still degrades to `nobody`, because
    /// upstream applies the emptiness check inside `auth_client` regardless of
    /// where the name came from.
    #[test]
    fn an_explicit_empty_username_degrades_to_nobody() {
        assert_eq!(
            resolve_auth_username(Some(""), Some("alice"), None),
            "nobody"
        );
    }
}
