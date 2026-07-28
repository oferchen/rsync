use super::types::LegacyDaemonHandshake;
use crate::negotiation::{
    NegotiatedStream, sniff_negotiation_stream, sniff_negotiation_stream_with_sniffer,
};
use logging::debug_log;
use protocol::{
    LEGACY_DAEMON_PREFIX, LEGACY_DAEMON_PREFIX_LEN, LegacyDaemonGreetingOwned, NegotiationPrologue,
    NegotiationPrologueSniffer, ProtocolVersion, check_sub_protocol, get_subprotocol_version,
    write_daemon_auth_digest_list,
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

    // upstream: clientserver.c:155 `our_sub = get_subprotocol_version()` is read
    // from this side's configured protocol version, so a release build always
    // advertises subprotocol 0.
    let our_sub = get_subprotocol_version(desired_protocol.as_u8());
    let negotiated_protocol = negotiate_protocol(desired_protocol, &server_greeting, our_sub);

    debug_log!(
        Proto,
        1,
        "negotiated protocol={} (desired={})",
        negotiated_protocol.as_u8(),
        desired_protocol.as_u8()
    );

    // upstream: clientserver.c:157 writes this side's own greeting BEFORE
    // clientserver.c:174 reads the server's, so the banner advertises our own
    // configured version and subprotocol, never the server's.
    let banner = build_client_greeting(desired_protocol, our_sub);
    stream.write_all(&banner)?;
    stream.flush()?;

    Ok(LegacyDaemonHandshake::from_components(
        server_greeting,
        negotiated_protocol,
        stream,
    ))
}

/// Reconciles this side's configured protocol against the server's greeting.
///
/// upstream: clientserver.c:213-226. After reading the server's `@RSYNCD:` line,
/// the client clamps its own `protocol_version` down to the server's and then,
/// mirroring `check_sub_protocol()`, drops one further step whenever the peer's
/// subprotocol is incompatible with ours. `min(desired, server)` followed by
/// `check_sub_protocol()` reproduces that block exactly: after the clamp the
/// peer's version is never below ours, so only the equal/greater branches fire,
/// and a nonzero server subprotocol against our release `our_sub == 0` forces the
/// documented one-step downgrade. A stock release server (subprotocol 0) leaves
/// the clamped version unchanged.
fn negotiate_protocol(
    desired_protocol: ProtocolVersion,
    server_greeting: &LegacyDaemonGreetingOwned,
    our_sub: u8,
) -> ProtocolVersion {
    let clamped = cmp::min(desired_protocol, server_greeting.protocol());
    // upstream: clientserver.c:180 stores the server's raw `remote_protocol` from
    // `sscanf`; the `protocol_version < remote_protocol` branch (clientserver.c:222)
    // needs that un-clamped value so a server advertising a newer protocol is not
    // mistaken for an equal one whose nonzero subprotocol would force a downgrade.
    let their_protocol = u8::try_from(server_greeting.advertised_protocol()).unwrap_or(u8::MAX);
    let their_sub = u8::try_from(server_greeting.subprotocol()).unwrap_or(u8::MAX);
    let reconciled = check_sub_protocol(clamped.as_u8(), our_sub, their_protocol, their_sub);
    if reconciled == clamped.as_u8() {
        return clamped;
    }
    // check_sub_protocol never raises the version. Clamp a sub-OLDEST downgrade to
    // the floor so the direction is preserved, matching upstream which advertises
    // the lowered value and lets the later version guard reject anything too old.
    let floor = ProtocolVersion::OLDEST.as_u8();
    ProtocolVersion::try_from(reconciled.max(floor)).unwrap_or(clamped)
}

/// Renders the client's `@RSYNCD:` greeting line.
///
/// upstream: clientserver.c:157 calls `output_daemon_greeting()` (compat.c:833)
/// BEFORE clientserver.c:174 reads the server's line, so every field is this
/// side's own advertisement, never derived from the server. The banner therefore
/// carries our configured `protocol_version` and `get_subprotocol_version()`
/// (compat.c:842 `io_printf(f_out, "@RSYNCD: %d.%d %s\n", protocol_version,
/// our_sub, tmpbuf)`) regardless of what the server advertised. Verified against
/// rsync 3.4.4: a client answering a server that greeted `@RSYNCD: 30.5` still
/// sends `@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4`.
///
/// The digest list is `get_default_nno_list()` (compat.c:840) and is likewise
/// never filtered by protocol version: `--protocol=28` still greets with
/// `@RSYNCD: 28.0 sha512 sha256 sha1 md5 md4`.
fn build_client_greeting(advertised_protocol: ProtocolVersion, subprotocol: u8) -> Vec<u8> {
    let mut greeting = String::with_capacity(LEGACY_DAEMON_PREFIX.len() + 48);

    greeting.push_str(LEGACY_DAEMON_PREFIX);
    greeting.push(' ');
    greeting.push_str(&advertised_protocol.as_u8().to_string());
    greeting.push('.');
    greeting.push_str(&subprotocol.to_string());

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

    fn proto(value: u8) -> ProtocolVersion {
        ProtocolVersion::from_supported(value).expect("supported protocol")
    }

    #[test]
    fn build_greeting_includes_prefix() {
        let greeting = build_client_greeting(proto(31), 0);
        assert!(greeting.starts_with(b"@RSYNCD:"));
    }

    #[test]
    fn build_greeting_ends_with_newline() {
        let greeting = build_client_greeting(proto(31), 0);
        assert!(greeting.ends_with(b"\n"));
    }

    #[test]
    fn build_greeting_has_space_after_prefix() {
        let greeting = build_client_greeting(proto(30), 0);
        assert!(greeting.starts_with(b"@RSYNCD: "));
    }

    #[test]
    fn build_greeting_has_dot_between_major_minor() {
        let greeting = build_client_greeting(proto(31), 0);
        assert!(greeting.contains(&b'.'));
    }

    #[test]
    fn build_greeting_format_is_valid_ascii() {
        let greeting = build_client_greeting(proto(31), 0);
        assert!(greeting.iter().all(u8::is_ascii));
    }

    // A release oc build always advertises subprotocol 0, so the greeting is
    // byte-exact. Pinned per version to guard the `<major>.<minor>` field.
    #[test]
    fn build_greeting_renders_version_and_subprotocol() {
        assert_eq!(
            build_client_greeting(proto(32), 0),
            b"@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4\n",
        );
        assert_eq!(
            build_client_greeting(proto(28), 0),
            b"@RSYNCD: 28.0 sha512 sha256 sha1 md5 md4\n",
        );
    }

    // The subprotocol field is not hard-coded: a hypothetical pre-release build
    // (SUBPROTOCOL_VERSION != 0) would render its own nonzero value. Guards the
    // field against being silently pinned to 0.
    #[test]
    fn build_greeting_renders_a_nonzero_subprotocol_verbatim() {
        assert_eq!(
            build_client_greeting(proto(32), 7),
            b"@RSYNCD: 32.7 sha512 sha256 sha1 md5 md4\n",
        );
    }

    // upstream: compat.c:840-842 `output_daemon_greeting()` emits
    // `get_default_nno_list()` verbatim with no version filtering. Verified
    // against rsync 3.4.4: `--protocol=28` through `--protocol=32` all send the
    // identical five names, differing only in the version field.
    #[test]
    fn build_greeting_digest_list_is_protocol_independent() {
        for version in [28u8, 29, 30, 31, 32] {
            assert_eq!(
                build_client_greeting(proto(version), 0),
                format!("@RSYNCD: {version}.0 sha512 sha256 sha1 md5 md4\n").into_bytes(),
                "protocol {version} must advertise the full list",
            );
        }
    }

    // upstream: clientserver.c:213-226. A stock release server (subprotocol 0)
    // clamps but never triggers the extra decrement.
    #[test]
    fn negotiate_protocol_release_server_only_clamps() {
        // Server older, no subprotocol: clamp to the server's version.
        assert_eq!(
            negotiate_protocol(proto(32), &parse_greeting("@RSYNCD: 30.0"), 0),
            proto(30),
        );
        // Server equal, no subprotocol: no change.
        assert_eq!(
            negotiate_protocol(proto(32), &parse_greeting("@RSYNCD: 32.0"), 0),
            proto(32),
        );
        // Server newer: clamp to our own newest, no decrement for a release side.
        assert_eq!(
            negotiate_protocol(proto(32), &parse_greeting("@RSYNCD: 33.9"), 0),
            proto(32),
        );
    }

    // upstream: clientserver.c:215-219 - a nonzero server subprotocol forces the
    // one-step decrement, both when the server is older and when it is equal.
    // Verified against rsync 3.4.4: greeting bytes stay `32.0` while the session
    // protocol drops. This case is invisible to an echo, which would report the
    // subprotocols as "matching" and suppress the decrement.
    #[test]
    fn negotiate_protocol_decrements_on_nonzero_server_subprotocol() {
        // Server older AND pre-release: min to 30, then decrement to 29.
        assert_eq!(
            negotiate_protocol(proto(32), &parse_greeting("@RSYNCD: 30.5"), 0),
            proto(29),
        );
        // Server equal, pre-release, our release sub 0 differs: decrement to 31.
        assert_eq!(
            negotiate_protocol(proto(32), &parse_greeting("@RSYNCD: 32.7"), 0),
            proto(31),
        );
        // A capped desired still clamps first, then decrements.
        assert_eq!(
            negotiate_protocol(proto(31), &parse_greeting("@RSYNCD: 30.5"), 0),
            proto(29),
        );
    }

    // The core of task #149: the greeting advertises OUR version and subprotocol,
    // never the server's. A discriminating stub (different version AND nonzero
    // subprotocol) makes echo and advertise byte-distinct: an echo would send
    // `30.5`, upstream 3.4.4 sends `32.0`. Simultaneously the negotiated session
    // protocol decrements to 29, proving the two are independent.
    #[test]
    fn greeting_advertises_our_version_while_session_protocol_decrements() {
        let server = parse_greeting("@RSYNCD: 30.5 md4 md5 xxh3");
        let desired = proto(32);
        let our_sub = get_subprotocol_version(desired.as_u8());

        let greeting = build_client_greeting(desired, our_sub);
        assert_eq!(
            greeting, b"@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4\n",
            "greeting must advertise our own 32.0, not the server's 30.5",
        );

        assert_eq!(
            negotiate_protocol(desired, &server, our_sub),
            proto(29),
            "the session protocol decrements even though the greeting stays 32.0",
        );
    }

    // The greeting is a pure function of what WE support, so no server-controlled
    // digest names may change the bytes after the version.
    #[test]
    fn build_greeting_digest_list_is_independent_of_the_server() {
        let expected = b"@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4\n".to_vec();
        for advertised in [
            "@RSYNCD: 31.0",
            "@RSYNCD: 31.0 md5",
            "@RSYNCD: 31.0 md4",
            "@RSYNCD: 31.0 sha512 sha256 sha1 md5 md4",
            "@RSYNCD: 31.0 blake3 sponge",
        ] {
            // The server greeting is parsed but must not influence our bytes.
            let _ = parse_greeting(advertised);
            assert_eq!(build_client_greeting(proto(32), 0), expected);
        }
    }
}
