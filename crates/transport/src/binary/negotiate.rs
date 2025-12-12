use crate::negotiation::{
    NegotiatedStream, sniff_negotiation_stream, sniff_negotiation_stream_with_sniffer,
};
use protocol::{
    CompatibilityFlags, NegotiationPrologue, NegotiationPrologueSniffer, ProtocolVersion,
};
use std::cmp;
use std::io::{self, Read, Write};

use super::BinaryHandshake;

pub(super) fn local_compatibility_flags(protocol: ProtocolVersion) -> CompatibilityFlags {
    if protocol.uses_binary_negotiation() {
        CompatibilityFlags::INC_RECURSE
            | CompatibilityFlags::CHECKSUM_SEED_FIX
            | CompatibilityFlags::VARINT_FLIST_FLAGS
    } else {
        CompatibilityFlags::EMPTY
    }
}

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

    let remote_protocol = match ProtocolVersion::from_peer_advertisement(remote_advertised) {
        Ok(protocol) => protocol,
        Err(err) => {
            return Err(io::Error::from(err));
        }
    };
    let negotiated_protocol = cmp::min(desired_protocol, remote_protocol);
    let remote_compatibility_flags = if negotiated_protocol.uses_binary_negotiation() {
        let local_flags = local_compatibility_flags(negotiated_protocol);
        {
            let inner = stream.inner_mut();
            local_flags.write_to(inner)?;
            inner.flush()?;
        }
        let mut first = [0u8; 1];
        match stream.read(&mut first)? {
            0 => CompatibilityFlags::EMPTY,
            read @ 1 => {
                let mut chained = (&first[..read]).chain(&mut stream);
                CompatibilityFlags::read_from(&mut chained)?
            }
            _ => unreachable!("single-byte buffer limits read results to 0 or 1"),
        }
    } else {
        CompatibilityFlags::EMPTY
    };
    Ok(BinaryHandshake::from_components(
        remote_advertised,
        remote_protocol,
        desired_protocol,
        negotiated_protocol,
        remote_compatibility_flags,
        stream,
    ))
}
