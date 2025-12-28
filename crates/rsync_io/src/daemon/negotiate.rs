use super::types::LegacyDaemonHandshake;
use crate::negotiation::{
    NegotiatedStream, sniff_negotiation_stream, sniff_negotiation_stream_with_sniffer,
};
use logging::debug_log;
use protocol::{
    LEGACY_DAEMON_PREFIX, LEGACY_DAEMON_PREFIX_LEN, LegacyDaemonGreetingOwned, NegotiationPrologue,
    NegotiationPrologueSniffer, ProtocolVersion,
};
use std::cmp;
use std::io::{self, Read, Write};

/// Performs the legacy ASCII rsync daemon negotiation.
///
/// The helper mirrors upstream rsync's client behaviour when connecting to an
/// `rsync://` daemon: it sniffs the negotiation prologue, parses the
/// `@RSYNCD:` greeting, clamps the negotiated protocol to the
/// caller-provided cap, and sends the client's greeting line before returning
/// the replaying stream.
///
/// # Errors
///
/// - [`io::ErrorKind::InvalidData`] if the negotiation prologue indicates a
///   binary handshake, which is handled by different transports.
/// - Any I/O error reported while sniffing the prologue, reading the greeting,
///   writing the client's banner, or flushing the stream.
#[doc(alias = "@RSYNCD")]
pub fn negotiate_legacy_daemon_session<R>(
    reader: R,
    desired_protocol: ProtocolVersion,
) -> io::Result<LegacyDaemonHandshake<R>>
where
    R: Read + Write,
{
    let stream = sniff_negotiation_stream(reader)?;
    negotiate_legacy_daemon_session_from_stream(stream, desired_protocol)
}

/// Performs the legacy ASCII negotiation with a caller-supplied sniffer.
///
/// Reusing a [`NegotiationPrologueSniffer`] allows higher layers to amortize
/// allocations when establishing many daemon sessions. The sniffer is reset
/// before any bytes are observed so state from previous negotiations is fully
/// cleared. Behaviour otherwise matches [`negotiate_legacy_daemon_session`].
#[doc(alias = "@RSYNCD")]
pub fn negotiate_legacy_daemon_session_with_sniffer<R>(
    reader: R,
    desired_protocol: ProtocolVersion,
    sniffer: &mut NegotiationPrologueSniffer,
) -> io::Result<LegacyDaemonHandshake<R>>
where
    R: Read + Write,
{
    let stream = sniff_negotiation_stream_with_sniffer(reader, sniffer)?;
    negotiate_legacy_daemon_session_from_stream(stream, desired_protocol)
}

/// Performs the legacy ASCII negotiation using a pre-sniffed [`NegotiatedStream`].
///
/// This helper accepts the [`NegotiatedStream`] produced by
/// [`sniff_negotiation_stream`] (or its sniffer-backed counterpart) and drives
/// the remainder of the daemon handshake without repeating the prologue
/// detection. The stream is verified to contain the `@RSYNCD:` prefix before the
/// greeting is parsed and echoed back to the server.
///
/// # Errors
///
/// - [`io::ErrorKind::InvalidData`] if the supplied stream does not represent a
///   legacy daemon negotiation or if formatting the client banner fails.
/// - Any I/O error reported while exchanging the greeting with the daemon.
#[doc(alias = "@RSYNCD")]
pub fn negotiate_legacy_daemon_session_from_stream<R>(
    mut stream: NegotiatedStream<R>,
    desired_protocol: ProtocolVersion,
) -> io::Result<LegacyDaemonHandshake<R>>
where
    R: Read + Write,
{
    debug_log!(Connect, 1, "legacy daemon negotiation started");

    stream.ensure_decision(
        NegotiationPrologue::LegacyAscii,
        "legacy daemon negotiation requires @RSYNCD: prefix",
    )?;

    let mut line = Vec::with_capacity(LEGACY_DAEMON_PREFIX_LEN + 32);
    let greeting = stream.read_and_parse_legacy_daemon_greeting_details(&mut line)?;
    let server_greeting = LegacyDaemonGreetingOwned::from(greeting);

    debug_log!(
        Proto,
        1,
        "daemon server protocol={}.{}",
        server_greeting.protocol().as_u8(),
        server_greeting.subprotocol()
    );

    let negotiated_protocol = cmp::min(desired_protocol, server_greeting.protocol());

    debug_log!(
        Proto,
        1,
        "negotiated protocol={} (desired={})",
        negotiated_protocol.as_u8(),
        desired_protocol.as_u8()
    );

    let banner = build_client_greeting(&server_greeting, negotiated_protocol);
    stream.write_all(&banner)?;
    stream.flush()?;

    Ok(LegacyDaemonHandshake::from_components(
        server_greeting,
        negotiated_protocol,
        stream,
    ))
}

fn build_client_greeting(
    server_greeting: &LegacyDaemonGreetingOwned,
    negotiated_protocol: ProtocolVersion,
) -> Vec<u8> {
    let mut greeting = String::with_capacity(
        LEGACY_DAEMON_PREFIX.len()
            + 16
            + server_greeting
                .digest_list()
                .map_or(0, |list| list.len() + 1),
    );

    greeting.push_str(LEGACY_DAEMON_PREFIX);
    greeting.push(' ');
    greeting.push_str(&negotiated_protocol.as_u8().to_string());
    greeting.push('.');

    let fractional = if negotiated_protocol == server_greeting.protocol() {
        server_greeting.subprotocol()
    } else {
        0
    };
    greeting.push_str(&fractional.to_string());

    if let Some(digests) = server_greeting.digest_list() {
        greeting.push(' ');
        greeting.push_str(digests);
    }

    greeting.push('\n');
    greeting.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::parse_legacy_daemon_greeting_owned;

    fn parse_greeting(line: &str) -> LegacyDaemonGreetingOwned {
        parse_legacy_daemon_greeting_owned(line).expect("valid greeting")
    }

    // ==== build_client_greeting basic tests ====

    #[test]
    fn build_greeting_includes_prefix() {
        let server = parse_greeting("@RSYNCD: 31.0");
        let protocol = ProtocolVersion::from_supported(31).unwrap();
        let greeting = build_client_greeting(&server, protocol);
        assert!(greeting.starts_with(b"@RSYNCD:"));
    }

    #[test]
    fn build_greeting_ends_with_newline() {
        let server = parse_greeting("@RSYNCD: 31.0");
        let protocol = ProtocolVersion::from_supported(31).unwrap();
        let greeting = build_client_greeting(&server, protocol);
        assert!(greeting.ends_with(b"\n"));
    }

    #[test]
    fn build_greeting_includes_protocol_version() {
        let server = parse_greeting("@RSYNCD: 31.0");
        let protocol = ProtocolVersion::from_supported(31).unwrap();
        let greeting = build_client_greeting(&server, protocol);
        let greeting_str = String::from_utf8_lossy(&greeting);
        assert!(greeting_str.contains("31."));
    }

    // ==== Protocol version matching ====

    #[test]
    fn build_greeting_preserves_subprotocol_when_matching() {
        let server = parse_greeting("@RSYNCD: 31.9");
        let protocol = ProtocolVersion::from_supported(31).unwrap();
        let greeting = build_client_greeting(&server, protocol);
        let greeting_str = String::from_utf8_lossy(&greeting);
        // When negotiated == server protocol, we preserve subprotocol
        assert!(greeting_str.contains("31.9"), "got: {greeting_str}");
    }

    #[test]
    fn build_greeting_uses_zero_subprotocol_when_downgraded() {
        let server = parse_greeting("@RSYNCD: 31.5");
        let protocol = ProtocolVersion::from_supported(30).unwrap();
        let greeting = build_client_greeting(&server, protocol);
        let greeting_str = String::from_utf8_lossy(&greeting);
        // When negotiated < server protocol, subprotocol is 0
        assert!(greeting_str.contains("30.0"), "got: {greeting_str}");
    }

    #[test]
    fn build_greeting_zero_subprotocol_when_server_has_none() {
        let server = parse_greeting("@RSYNCD: 29.0");
        let protocol = ProtocolVersion::from_supported(29).unwrap();
        let greeting = build_client_greeting(&server, protocol);
        let greeting_str = String::from_utf8_lossy(&greeting);
        assert!(greeting_str.contains("29.0"), "got: {greeting_str}");
    }

    // ==== Digest list handling ====

    #[test]
    fn build_greeting_includes_digest_list_when_present() {
        let server = parse_greeting("@RSYNCD: 31.0 md4 md5 xxh3");
        let protocol = ProtocolVersion::from_supported(31).unwrap();
        let greeting = build_client_greeting(&server, protocol);
        let greeting_str = String::from_utf8_lossy(&greeting);
        assert!(greeting_str.contains("md4 md5 xxh3"), "got: {greeting_str}");
    }

    #[test]
    fn build_greeting_omits_digest_list_when_absent() {
        let server = parse_greeting("@RSYNCD: 30.0");
        let protocol = ProtocolVersion::from_supported(30).unwrap();
        let greeting = build_client_greeting(&server, protocol);
        let greeting_str = String::from_utf8_lossy(&greeting);
        // Should only have "@RSYNCD: 30.0\n"
        assert_eq!(greeting_str.trim(), "@RSYNCD: 30.0");
    }

    #[test]
    fn build_greeting_with_single_digest() {
        let server = parse_greeting("@RSYNCD: 31.0 xxh3");
        let protocol = ProtocolVersion::from_supported(31).unwrap();
        let greeting = build_client_greeting(&server, protocol);
        let greeting_str = String::from_utf8_lossy(&greeting);
        assert!(greeting_str.contains(" xxh3"), "got: {greeting_str}");
    }

    // ==== Protocol version edge cases ====

    #[test]
    fn build_greeting_with_protocol_28() {
        let server = parse_greeting("@RSYNCD: 28.0");
        let protocol = ProtocolVersion::from_supported(28).unwrap();
        let greeting = build_client_greeting(&server, protocol);
        let greeting_str = String::from_utf8_lossy(&greeting);
        assert!(greeting_str.contains("28.0"), "got: {greeting_str}");
    }

    #[test]
    fn build_greeting_with_highest_subprotocol() {
        let server = parse_greeting("@RSYNCD: 31.99");
        let protocol = ProtocolVersion::from_supported(31).unwrap();
        let greeting = build_client_greeting(&server, protocol);
        let greeting_str = String::from_utf8_lossy(&greeting);
        assert!(greeting_str.contains("31.99"), "got: {greeting_str}");
    }

    // ==== Format validation ====

    #[test]
    fn build_greeting_format_is_valid_ascii() {
        let server = parse_greeting("@RSYNCD: 31.0 md5");
        let protocol = ProtocolVersion::from_supported(31).unwrap();
        let greeting = build_client_greeting(&server, protocol);
        assert!(greeting.iter().all(|&b| b.is_ascii()));
    }

    #[test]
    fn build_greeting_has_space_after_prefix() {
        let server = parse_greeting("@RSYNCD: 30.0");
        let protocol = ProtocolVersion::from_supported(30).unwrap();
        let greeting = build_client_greeting(&server, protocol);
        let greeting_str = String::from_utf8_lossy(&greeting);
        // Should have "@RSYNCD: 30.0" with space after colon
        assert!(greeting_str.starts_with("@RSYNCD: "));
    }

    #[test]
    fn build_greeting_has_dot_between_major_minor() {
        let server = parse_greeting("@RSYNCD: 31.5");
        let protocol = ProtocolVersion::from_supported(31).unwrap();
        let greeting = build_client_greeting(&server, protocol);
        let greeting_str = String::from_utf8_lossy(&greeting);
        // Should contain "31.5" (dot separating major.minor)
        assert!(greeting_str.contains('.'));
    }

    // ==== Complete greeting tests ====

    #[test]
    fn build_greeting_complete_without_digests() {
        let server = parse_greeting("@RSYNCD: 30.0");
        let protocol = ProtocolVersion::from_supported(30).unwrap();
        let greeting = build_client_greeting(&server, protocol);
        assert_eq!(greeting, b"@RSYNCD: 30.0\n");
    }

    #[test]
    fn build_greeting_complete_with_digests() {
        let server = parse_greeting("@RSYNCD: 31.0 md5 xxh3");
        let protocol = ProtocolVersion::from_supported(31).unwrap();
        let greeting = build_client_greeting(&server, protocol);
        assert_eq!(greeting, b"@RSYNCD: 31.0 md5 xxh3\n");
    }

    #[test]
    fn build_greeting_downgraded_with_digests() {
        let server = parse_greeting("@RSYNCD: 31.5 md4 md5");
        let protocol = ProtocolVersion::from_supported(29).unwrap();
        let greeting = build_client_greeting(&server, protocol);
        // Downgraded to 29.0, but digests still included
        assert_eq!(greeting, b"@RSYNCD: 29.0 md4 md5\n");
    }
}
