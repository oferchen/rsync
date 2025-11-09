use super::types::LegacyDaemonHandshake;
use crate::negotiation::{
    NegotiatedStream, sniff_negotiation_stream, sniff_negotiation_stream_with_sniffer,
};
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
    stream.ensure_decision(
        NegotiationPrologue::LegacyAscii,
        "legacy daemon negotiation requires @RSYNCD: prefix",
    )?;

    let mut line = Vec::with_capacity(LEGACY_DAEMON_PREFIX_LEN + 32);
    let greeting = stream.read_and_parse_legacy_daemon_greeting_details(&mut line)?;
    let server_greeting = LegacyDaemonGreetingOwned::from(greeting);

    let negotiated_protocol = cmp::min(desired_protocol, server_greeting.protocol());

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
                .map(|list| list.len() + 1)
                .unwrap_or(0),
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
