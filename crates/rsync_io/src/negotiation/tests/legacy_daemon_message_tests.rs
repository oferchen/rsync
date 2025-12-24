#[test]
fn read_and_parse_legacy_daemon_message_routes_capabilities() {
    let mut stream = sniff_bytes(b"@RSYNCD: CAP 0x1f 0x2\n").expect("sniff succeeds");
    let mut line = Vec::new();
    match stream
        .read_and_parse_legacy_daemon_message(&mut line)
        .expect("message parses")
    {
        LegacyDaemonMessage::Capabilities { flags } => {
            assert_eq!(flags, "0x1f 0x2");
        }
        other => panic!("unexpected message: {other:?}"),
    }
    assert_eq!(line, b"@RSYNCD: CAP 0x1f 0x2\n");
}

#[test]
fn read_and_parse_legacy_daemon_message_routes_versions() {
    let mut stream = sniff_bytes(b"@RSYNCD: 29.0\n").expect("sniff succeeds");
    let mut line = Vec::new();
    match stream
        .read_and_parse_legacy_daemon_message(&mut line)
        .expect("message parses")
    {
        LegacyDaemonMessage::Version(version) => {
            let expected = ProtocolVersion::from_supported(29).expect("supported version");
            assert_eq!(version, expected);
        }
        other => panic!("unexpected message: {other:?}"),
    }
    assert_eq!(line, b"@RSYNCD: 29.0\n");
}

#[test]
fn read_and_parse_legacy_daemon_message_propagates_parse_errors() {
    let mut stream = sniff_bytes(b"@RSYNCD:\n").expect("sniff succeeds");
    let mut line = Vec::new();
    let err = stream
        .read_and_parse_legacy_daemon_message(&mut line)
        .expect_err("message parsing should fail");
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn read_and_parse_legacy_daemon_error_message_returns_payload() {
    let mut stream = sniff_bytes(b"@ERROR: something went wrong\n").expect("sniff succeeds");
    let mut line = Vec::new();
    {
        let payload = stream
            .read_and_parse_legacy_daemon_error_message(&mut line)
            .expect("error payload parses")
            .expect("payload is present");
        assert_eq!(payload, "something went wrong");
    }
    assert_eq!(line, b"@ERROR: something went wrong\n");
}

#[test]
fn read_and_parse_legacy_daemon_error_message_allows_empty_payloads() {
    let mut stream = sniff_bytes(b"@ERROR:\n").expect("sniff succeeds");
    let mut line = Vec::new();
    {
        let payload = stream
            .read_and_parse_legacy_daemon_error_message(&mut line)
            .expect("empty payload parses");
        assert_eq!(payload, Some(""));
    }
    assert_eq!(line, b"@ERROR:\n");
}

#[test]
fn read_and_parse_legacy_daemon_warning_message_returns_payload() {
    let mut stream = sniff_bytes(b"@WARNING: check perms\n").expect("sniff succeeds");
    let mut line = Vec::new();
    {
        let payload = stream
            .read_and_parse_legacy_daemon_warning_message(&mut line)
            .expect("warning payload parses")
            .expect("payload is present");
        assert_eq!(payload, "check perms");
    }
    assert_eq!(line, b"@WARNING: check perms\n");
}

#[test]
fn read_legacy_daemon_line_errors_when_prefix_already_consumed() {
    let mut stream = sniff_bytes(b"@RSYNCD: 29.0\nrest").expect("sniff succeeds");
    let mut prefix_chunk = [0u8; 4];
    stream
        .read_exact(&mut prefix_chunk)
        .expect("prefix chunk is replayed before parsing");

    let mut line = Vec::new();
    let err = stream
        .read_legacy_daemon_line(&mut line)
        .expect_err("consuming prefix first should fail");
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn read_legacy_daemon_line_errors_for_incomplete_prefix_state() {
    let mut stream = NegotiatedStream::from_raw_parts(
        Cursor::new(b" 31.0\n".to_vec()),
        NegotiationPrologue::LegacyAscii,
        LEGACY_DAEMON_PREFIX_LEN - 1,
        0,
        b"@RSYNCD".to_vec(),
    );

    let mut line = Vec::new();
    let err = stream
        .read_legacy_daemon_line(&mut line)
        .expect_err("incomplete prefix must error");
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    assert!(line.is_empty());
}

#[test]
fn read_legacy_daemon_line_errors_for_binary_negotiation() {
    let mut stream = sniff_bytes(&[0x00, 0x12, 0x34]).expect("sniff succeeds");
    let mut line = Vec::new();
    let err = stream
        .read_legacy_daemon_line(&mut line)
        .expect_err("binary negotiations do not yield legacy lines");
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn read_legacy_daemon_line_errors_on_eof_before_newline() {
    let mut stream = sniff_bytes(b"@RSYNCD:").expect("sniff succeeds");
    let mut line = Vec::new();
    let err = stream
        .read_legacy_daemon_line(&mut line)
        .expect_err("EOF before newline must error");
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
}

#[test]
fn read_and_parse_legacy_daemon_message_errors_when_prefix_partially_consumed() {
    let mut stream = sniff_bytes(b"@RSYNCD: AUTHREQD module\n").expect("sniff succeeds");
    let mut prefix_fragment = [0u8; 3];
    stream
        .read_exact(&mut prefix_fragment)
        .expect("prefix fragment is replayed before parsing");

    let mut line = Vec::new();
    let err = stream
        .read_and_parse_legacy_daemon_message(&mut line)
        .expect_err("partial prefix consumption must error");
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    assert!(line.is_empty());
}

#[test]
fn read_and_parse_legacy_daemon_message_clears_line_on_error() {
    let mut stream = sniff_bytes(b"\x00rest").expect("sniff succeeds");
    let mut line = b"stale".to_vec();

    let err = stream
        .read_and_parse_legacy_daemon_message(&mut line)
        .expect_err("binary negotiation cannot parse legacy message");

    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    assert!(line.is_empty());
}

#[test]
fn read_and_parse_legacy_daemon_greeting_from_stream() {
    let mut stream = sniff_bytes(b"@RSYNCD: 31.0\n").expect("sniff succeeds");
    let mut line = Vec::new();
    let version = stream
        .read_and_parse_legacy_daemon_greeting(&mut line)
        .expect("greeting parses");
    assert_eq!(version, ProtocolVersion::from_supported(31).unwrap());
    assert_eq!(line, b"@RSYNCD: 31.0\n");
}

#[test]
fn read_and_parse_legacy_daemon_greeting_details_from_stream() {
    let mut stream = sniff_bytes(b"@RSYNCD: 31.0 md4 md5\n").expect("sniff succeeds");
    let mut line = Vec::new();
    let details = stream
        .read_and_parse_legacy_daemon_greeting_details(&mut line)
        .expect("detailed greeting parses");
    assert_eq!(
        details.protocol(),
        ProtocolVersion::from_supported(31).unwrap()
    );
    assert_eq!(details.digest_list(), Some("md4 md5"));
    assert!(details.has_subprotocol());
    assert_eq!(line, b"@RSYNCD: 31.0 md4 md5\n");
}

#[derive(Debug)]
struct NonVectoredCursor(Cursor<Vec<u8>>);

impl NonVectoredCursor {
    fn new(bytes: Vec<u8>) -> Self {
        Self(Cursor::new(bytes))
    }
}

impl Read for NonVectoredCursor {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}
