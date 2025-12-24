use crate::binary::negotiate_binary_session_from_stream;
use crate::daemon::negotiate_legacy_daemon_session_from_stream;
use crate::negotiation::{
    NegotiatedStream, sniff_negotiation_stream, sniff_negotiation_stream_with_sniffer,
};
use logging::debug_log;
use protocol::{NegotiationPrologue, NegotiationPrologueSniffer, ProtocolVersion};
use std::io::{self, Read, Write};

use super::super::parts::SessionHandshakeParts;
use super::session::SessionHandshake;

/// Negotiates an rsync session, automatically detecting the handshake style.
///
/// The helper mirrors upstream rsync's behaviour when dialing a transport whose
/// negotiation style is unknown. It sniffs the prologue, dispatches to either
/// the binary or legacy negotiation helper, and returns a [`SessionHandshake`]
/// carrying the negotiated metadata together with the replaying stream.
///
/// # Errors
///
/// Propagates any I/O error reported by the underlying sniffing or variant
/// specific negotiation helper.
pub fn negotiate_session<R>(
    reader: R,
    desired_protocol: ProtocolVersion,
) -> io::Result<SessionHandshake<R>>
where
    R: Read + Write,
{
    let stream = sniff_negotiation_stream(reader)?;
    negotiate_session_from_stream(stream, desired_protocol)
}

/// Negotiates an rsync session and returns the decomposed handshake parts.
///
/// This is a convenience wrapper around [`negotiate_session`] that immediately
/// converts the negotiated handshake into [`SessionHandshakeParts`]. Callers
/// that intend to wrap the underlying transport typically need this split
/// representation to stash the replayed negotiation bytes while instrumenting
/// the stream before resuming the protocol exchange.
///
/// # Errors
///
/// Propagates any I/O error reported by [`negotiate_session`].
pub fn negotiate_session_parts<R>(
    reader: R,
    desired_protocol: ProtocolVersion,
) -> io::Result<SessionHandshakeParts<R>>
where
    R: Read + Write,
{
    let stream = sniff_negotiation_stream(reader)?;
    negotiate_session_parts_from_stream(stream, desired_protocol)
}

/// Negotiates an rsync session while reusing a caller supplied sniffer.
///
/// This mirrors [`negotiate_session`] but allows higher layers to reuse a
/// [`NegotiationPrologueSniffer`] across multiple negotiations, matching the
/// existing binary and legacy helper variants.
pub fn negotiate_session_with_sniffer<R>(
    reader: R,
    desired_protocol: ProtocolVersion,
    sniffer: &mut NegotiationPrologueSniffer,
) -> io::Result<SessionHandshake<R>>
where
    R: Read + Write,
{
    let stream = sniff_negotiation_stream_with_sniffer(reader, sniffer)?;
    negotiate_session_from_stream(stream, desired_protocol)
}

/// Negotiates an rsync session using a caller supplied sniffer and returns the
/// decomposed handshake parts.
///
/// This mirrors [`negotiate_session_parts`] but reuses the provided
/// [`NegotiationPrologueSniffer`], matching the semantics of
/// [`negotiate_session_with_sniffer`].
///
/// # Errors
///
/// Propagates any I/O error reported by
/// [`negotiate_session_with_sniffer`].
pub fn negotiate_session_parts_with_sniffer<R>(
    reader: R,
    desired_protocol: ProtocolVersion,
    sniffer: &mut NegotiationPrologueSniffer,
) -> io::Result<SessionHandshakeParts<R>>
where
    R: Read + Write,
{
    let stream = sniff_negotiation_stream_with_sniffer(reader, sniffer)?;
    negotiate_session_parts_from_stream(stream, desired_protocol)
}

/// Negotiates an rsync session using a pre-sniffed [`NegotiatedStream`].
///
/// Callers that already possess the [`NegotiatedStream`] returned by
/// [`sniff_negotiation_stream`](crate::sniff_negotiation_stream) (or its
/// sniffer-backed variant) can use this helper to complete the handshake without
/// repeating the prologue detection. The function dispatches to the binary or
/// legacy negotiation path based on the recorded decision and returns the
/// corresponding [`SessionHandshake`].
///
/// # Errors
///
/// Propagates any I/O error reported while driving the variant-specific
/// negotiation helper. If the negotiation prologue was not fully determined
/// the function returns [`io::ErrorKind::UnexpectedEof`] with the canonical
/// transport error message used by [`NegotiatedStream::ensure_decision`].
pub fn negotiate_session_from_stream<R>(
    stream: NegotiatedStream<R>,
    desired_protocol: ProtocolVersion,
) -> io::Result<SessionHandshake<R>>
where
    R: Read + Write,
{
    match stream.decision() {
        NegotiationPrologue::Binary => {
            debug_log!(Connect, 1, "detected binary handshake");
            negotiate_binary_session_from_stream(stream, desired_protocol)
                .map(SessionHandshake::Binary)
        }
        NegotiationPrologue::LegacyAscii => {
            debug_log!(Connect, 1, "detected legacy @RSYNCD: handshake");
            negotiate_legacy_daemon_session_from_stream(stream, desired_protocol)
                .map(SessionHandshake::Legacy)
        }
        NegotiationPrologue::NeedMoreData => Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            crate::negotiation::NEGOTIATION_PROLOGUE_UNDETERMINED_MSG,
        )),
    }
}

/// Negotiates an rsync session from a pre-sniffed stream and returns the decomposed parts.
///
/// This convenience wrapper mirrors [`negotiate_session_from_stream`] but immediately converts the
/// resulting [`SessionHandshake`] into [`SessionHandshakeParts`]. Callers that already possess a
/// [`NegotiatedStream`]—for instance after invoking
/// [`sniff_negotiation_stream`](crate::sniff_negotiation_stream)—can therefore obtain the replaying
/// stream parts and negotiated metadata without rebuilding the handshake manually.
///
/// # Errors
///
/// Propagates any I/O error produced while driving [`negotiate_session_from_stream`].
///
/// # Examples
///
/// ```
/// use protocol::ProtocolVersion;
/// use rsync_io::{
///     negotiate_session_parts_from_stream, sniff_negotiation_stream, SessionHandshakeParts,
/// };
/// use std::io::{self, Cursor, Read, Write};
///
/// #[derive(Debug)]
/// struct Loopback {
///     reader: Cursor<Vec<u8>>,
///     written: Vec<u8>,
/// }
///
/// impl Loopback {
///     fn new(advertised: ProtocolVersion) -> Self {
///         let bytes = u32::from(advertised.as_u8()).to_be_bytes();
///         Self { reader: Cursor::new(bytes.to_vec()), written: Vec::new() }
///     }
/// }
///
/// impl Read for Loopback {
///     fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
///         self.reader.read(buf)
///     }
/// }
///
/// impl Write for Loopback {
///     fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
///         self.written.extend_from_slice(buf);
///         Ok(buf.len())
///     }
///
///     fn flush(&mut self) -> io::Result<()> {
///         Ok(())
///     }
/// }
///
/// let remote = ProtocolVersion::from_supported(31).unwrap();
/// let stream = sniff_negotiation_stream(Loopback::new(remote)).unwrap();
/// let parts: SessionHandshakeParts<_> =
///     negotiate_session_parts_from_stream(stream, ProtocolVersion::NEWEST).unwrap();
///
/// assert!(parts.is_binary());
/// assert_eq!(parts.negotiated_protocol(), remote);
/// ```
pub fn negotiate_session_parts_from_stream<R>(
    stream: NegotiatedStream<R>,
    desired_protocol: ProtocolVersion,
) -> io::Result<SessionHandshakeParts<R>>
where
    R: Read + Write,
{
    negotiate_session_from_stream(stream, desired_protocol).map(SessionHandshake::into_stream_parts)
}
