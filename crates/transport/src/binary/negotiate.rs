use crate::negotiation::{
    NegotiatedStream, sniff_negotiation_stream, sniff_negotiation_stream_with_sniffer,
};
use rsync_protocol::{NegotiationPrologue, NegotiationPrologueSniffer, ProtocolVersion};
use std::cmp;
use std::io::{self, Read, Write};

use super::BinaryHandshake;

/// Performs the binary rsync protocol negotiation against a fresh transport.
///
/// The helper mirrors upstream rsync's behaviour when establishing a
/// remote-shell session: it sniffs the connection to ensure a binary prologue,
/// writes the caller's desired protocol version, and returns the resulting
/// [`BinaryHandshake`].
///
/// # Errors
///
/// - [`io::ErrorKind::InvalidData`] if the transport advertises the legacy
///   `@RSYNCD:` negotiation or if the peer reports a protocol outside the
///   supported range.
/// - Any I/O error reported while sniffing the prologue or exchanging protocol
///   advertisements.
pub fn negotiate_binary_session<R>(
    reader: R,
    desired_protocol: ProtocolVersion,
) -> io::Result<BinaryHandshake<R>>
where
    R: Read + Write,
{
    let stream = sniff_negotiation_stream(reader)?;
    negotiate_binary_session_from_stream(stream, desired_protocol)
}

/// Performs the binary negotiation while reusing a caller-supplied sniffer.
///
/// This variant mirrors [`negotiate_binary_session`] but feeds the transport
/// through an existing [`NegotiationPrologueSniffer`]. Reusing the sniffer
/// avoids repeated allocations when higher layers maintain a pool of sniffers
/// for successive connections (for example when servicing multiple daemon
/// sessions). The sniffer is reset before it observes any bytes from the
/// transport, guaranteeing that stale state from a previous negotiation cannot
/// leak into the new session.
pub fn negotiate_binary_session_with_sniffer<R>(
    reader: R,
    desired_protocol: ProtocolVersion,
    sniffer: &mut NegotiationPrologueSniffer,
) -> io::Result<BinaryHandshake<R>>
where
    R: Read + Write,
{
    let stream = sniff_negotiation_stream_with_sniffer(reader, sniffer)?;
    negotiate_binary_session_from_stream(stream, desired_protocol)
}

/// Performs the binary negotiation using a pre-sniffed [`NegotiatedStream`].
///
/// Callers that already invoked [`sniff_negotiation_stream`] or supplied their
/// own [`NegotiationPrologueSniffer`] can reuse the captured
/// [`NegotiatedStream`] instead of repeating the prologue detection. The helper
/// validates that the buffered prefix corresponds to the binary handshake,
/// writes the caller's desired protocol advertisement, reads the peer's
/// response, and returns the resulting [`BinaryHandshake`].
///
/// # Errors
///
/// - [`io::ErrorKind::InvalidData`] if the supplied stream represents a
///   legacy ASCII negotiation or if the peer advertises a protocol below the
///   supported range. Future versions are clamped to the newest supported
///   value like upstream rsync.
/// - Any I/O error reported while exchanging the protocol advertisements.
pub fn negotiate_binary_session_from_stream<R>(
    mut stream: NegotiatedStream<R>,
    desired_protocol: ProtocolVersion,
) -> io::Result<BinaryHandshake<R>>
where
    R: Read + Write,
{
    stream.ensure_decision(
        NegotiationPrologue::Binary,
        "binary negotiation requires binary prologue",
    )?;

    let mut advertisement = [0u8; 4];
    let desired = desired_protocol.as_u8();
    advertisement.copy_from_slice(&u32::from(desired).to_be_bytes());
    {
        let inner = stream.inner_mut();
        inner.write_all(&advertisement)?;
        inner.flush()?;
    }

    let mut remote_buf = [0u8; 4];
    stream.read_exact(&mut remote_buf)?;
    let remote_advertised = u32::from_be_bytes(remote_buf);

    let remote_byte = remote_advertised.min(u32::from(u8::MAX)) as u8;

    let remote_protocol = match ProtocolVersion::from_peer_advertisement(remote_byte) {
        Ok(protocol) => protocol,
        Err(_) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "binary negotiation protocol identifier outside supported range",
            ));
        }
    };
    let negotiated_protocol = cmp::min(desired_protocol, remote_protocol);
    Ok(BinaryHandshake::from_components(
        remote_advertised,
        remote_protocol,
        desired_protocol,
        negotiated_protocol,
        stream,
    ))
}
