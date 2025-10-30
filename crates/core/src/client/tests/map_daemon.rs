use super::prelude::*;


#[test]
fn map_daemon_handshake_error_converts_error_payload() {
    let addr = DaemonAddress::new("127.0.0.1".to_string(), 873).expect("address");
    let error = io::Error::new(
        io::ErrorKind::InvalidData,
        NegotiationError::MalformedLegacyGreeting {
            input: "@ERROR module unavailable".to_string(),
        },
    );

    let mapped = map_daemon_handshake_error(error, &addr);
    assert_eq!(mapped.exit_code(), PARTIAL_TRANSFER_EXIT_CODE);
    assert!(mapped.message().to_string().contains("module unavailable"));
}


#[test]
fn map_daemon_handshake_error_converts_plain_invalid_data_error() {
    let addr = DaemonAddress::new("127.0.0.1".to_string(), 873).expect("address");
    let error = io::Error::new(io::ErrorKind::InvalidData, "@ERROR daemon unavailable");

    let mapped = map_daemon_handshake_error(error, &addr);
    assert_eq!(mapped.exit_code(), PARTIAL_TRANSFER_EXIT_CODE);
    assert!(mapped.message().to_string().contains("daemon unavailable"));
}


#[test]
fn map_daemon_handshake_error_converts_other_malformed_greetings() {
    let addr = DaemonAddress::new("127.0.0.1".to_string(), 873).expect("address");
    let error = io::Error::new(
        io::ErrorKind::InvalidData,
        NegotiationError::MalformedLegacyGreeting {
            input: "@RSYNCD? unexpected".to_string(),
        },
    );

    let mapped = map_daemon_handshake_error(error, &addr);
    assert_eq!(mapped.exit_code(), PROTOCOL_INCOMPATIBLE_EXIT_CODE);
    assert!(mapped.message().to_string().contains("@RSYNCD? unexpected"));
}


#[test]
fn map_daemon_handshake_error_propagates_other_failures() {
    let addr = DaemonAddress::new("127.0.0.1".to_string(), 873).expect("address");
    let error = io::Error::new(io::ErrorKind::TimedOut, "timed out");

    let mapped = map_daemon_handshake_error(error, &addr);
    assert_eq!(mapped.exit_code(), SOCKET_IO_EXIT_CODE);
    let rendered = mapped.message().to_string();
    assert!(rendered.contains("timed out"));
    assert!(rendered.contains("negotiate with"));
}

