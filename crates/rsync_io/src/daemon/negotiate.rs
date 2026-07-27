use super::types::LegacyDaemonHandshake;
use crate::negotiation::{
    NegotiatedStream, sniff_negotiation_stream, sniff_negotiation_stream_with_sniffer,
};
use logging::debug_log;
use protocol::{
    LEGACY_DAEMON_PREFIX, LEGACY_DAEMON_PREFIX_LEN, LegacyDaemonGreetingOwned, NegotiationPrologue,
    NegotiationPrologueSniffer, ProtocolVersion, write_daemon_auth_digest_list,
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
/// server's greeting is parsed and this client's own greeting is sent.
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

/// Renders the client's `@RSYNCD:` greeting line.
///
/// The advertised digest list states *this build's* daemon-auth capabilities and
/// is never derived from the server's greeting. Upstream cannot derive it even in
/// principle: `exchange_protocols()` calls `output_daemon_greeting()`
/// (clientserver.c:157) before `read_line_old()` has read the server's line
/// (clientserver.c:174), so the client always renders its own
/// `get_default_nno_list()` (compat.c:462). Echoing the server's list instead
/// would let the server pick the digest out of a set it fully controls, since a
/// name we confirm as ours is a name we have agreed to accept.
///
/// The list is also never filtered by protocol version. Verified against rsync
/// 3.4.4: a client forced to `--protocol=28` still greets with
/// `@RSYNCD: 28.0 sha512 sha256 sha1 md5 md4`.
fn build_client_greeting(
    server_greeting: &LegacyDaemonGreetingOwned,
    negotiated_protocol: ProtocolVersion,
) -> Vec<u8> {
    let mut greeting = String::with_capacity(LEGACY_DAEMON_PREFIX.len() + 48);

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

    greeting.push(' ');
    write_daemon_auth_digest_list(&mut greeting).expect("writing to a String cannot fail");

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

    #[test]
    fn build_greeting_preserves_subprotocol_when_matching() {
        let server = parse_greeting("@RSYNCD: 31.9");
        let protocol = ProtocolVersion::from_supported(31).unwrap();
        let greeting = build_client_greeting(&server, protocol);
        let greeting_str = String::from_utf8_lossy(&greeting);
        assert!(greeting_str.contains("31.9"), "got: {greeting_str}");
    }

    #[test]
    fn build_greeting_uses_zero_subprotocol_when_downgraded() {
        let server = parse_greeting("@RSYNCD: 31.5");
        let protocol = ProtocolVersion::from_supported(30).unwrap();
        let greeting = build_client_greeting(&server, protocol);
        let greeting_str = String::from_utf8_lossy(&greeting);
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

    // The greeting must state OUR capabilities, never the server's. Measured
    // against rsync 3.4.4 driven at a stub advertising exactly `md4 md5 xxh3`:
    // the client still answered `@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4`.
    // A server that advertises only weak names must not be able to make us
    // confirm that narrowed set as our own, because a name we advertise is a
    // name we have agreed to accept.
    #[test]
    fn build_greeting_advertises_our_digests_not_the_servers() {
        let server = parse_greeting("@RSYNCD: 31.0 md4 md5 xxh3");
        let protocol = ProtocolVersion::from_supported(31).unwrap();
        let greeting = build_client_greeting(&server, protocol);
        assert_eq!(greeting, b"@RSYNCD: 31.0 sha512 sha256 sha1 md5 md4\n");
    }

    // upstream: clientserver.c:157 sends the greeting before clientserver.c:174
    // reads the server's, so a server that names no list cannot suppress ours.
    #[test]
    fn build_greeting_advertises_digests_when_the_server_named_none() {
        let server = parse_greeting("@RSYNCD: 30.0");
        let protocol = ProtocolVersion::from_supported(30).unwrap();
        let greeting = build_client_greeting(&server, protocol);
        assert_eq!(greeting, b"@RSYNCD: 30.0 sha512 sha256 sha1 md5 md4\n");
    }

    // The narrowest possible restriction: a single weak name. Verified against
    // rsync 3.4.4 under the same stub - the list it sends is unchanged.
    #[test]
    fn build_greeting_ignores_a_single_digest_restriction() {
        let server = parse_greeting("@RSYNCD: 31.0 md5");
        let protocol = ProtocolVersion::from_supported(31).unwrap();
        let greeting = build_client_greeting(&server, protocol);
        assert_eq!(greeting, b"@RSYNCD: 31.0 sha512 sha256 sha1 md5 md4\n");
    }

    // upstream: compat.c:838-842 `output_daemon_greeting()` emits
    // `get_default_nno_list()` verbatim with no version filtering. Verified
    // against rsync 3.4.4: `--protocol=28` through `--protocol=32` all send the
    // identical five names, differing only in the version field.
    #[test]
    fn build_greeting_digest_list_is_protocol_independent() {
        for version in [28u8, 29, 30, 31, 32] {
            let server = parse_greeting(&format!("@RSYNCD: {version}.0"));
            let protocol = ProtocolVersion::from_supported(version).unwrap();
            let greeting = build_client_greeting(&server, protocol);
            assert_eq!(
                greeting,
                format!("@RSYNCD: {version}.0 sha512 sha256 sha1 md5 md4\n").into_bytes(),
                "protocol {version} must advertise the full list",
            );
        }
    }

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
        assert!(greeting_str.starts_with("@RSYNCD: "));
    }

    #[test]
    fn build_greeting_has_dot_between_major_minor() {
        let server = parse_greeting("@RSYNCD: 31.5");
        let protocol = ProtocolVersion::from_supported(31).unwrap();
        let greeting = build_client_greeting(&server, protocol);
        let greeting_str = String::from_utf8_lossy(&greeting);
        assert!(greeting_str.contains('.'));
    }

    #[test]
    fn build_greeting_downgraded_still_advertises_our_digests() {
        let server = parse_greeting("@RSYNCD: 31.5 md4 md5");
        let protocol = ProtocolVersion::from_supported(29).unwrap();
        let greeting = build_client_greeting(&server, protocol);
        assert_eq!(greeting, b"@RSYNCD: 29.0 sha512 sha256 sha1 md5 md4\n");
    }

    // The greeting is a pure function of what WE support, so no combination of
    // server-controlled digest names may change the bytes after the version.
    #[test]
    fn build_greeting_digest_list_is_independent_of_the_server_greeting() {
        let protocol = ProtocolVersion::from_supported(31).unwrap();
        let expected = b"@RSYNCD: 31.0 sha512 sha256 sha1 md5 md4\n".to_vec();

        for advertised in [
            "@RSYNCD: 31.0",
            "@RSYNCD: 31.0 md5",
            "@RSYNCD: 31.0 sha512 md5",
            "@RSYNCD: 31.0 md4",
            "@RSYNCD: 31.0 sha512 sha256 sha1 md5 md4",
            "@RSYNCD: 31.0 blake3 sponge",
        ] {
            let server = parse_greeting(advertised);
            assert_eq!(
                build_client_greeting(&server, protocol),
                expected,
                "server greeting {advertised:?} must not change our advertised list",
            );
        }
    }
}
