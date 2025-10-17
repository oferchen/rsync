use crate::binary::{BinaryHandshake, negotiate_binary_session_from_stream};
use crate::daemon::{LegacyDaemonHandshake, negotiate_legacy_daemon_session_from_stream};
use crate::negotiation::{
    NegotiatedStream, NegotiatedStreamParts, sniff_negotiation_stream,
    sniff_negotiation_stream_with_sniffer,
};
use rsync_protocol::{
    LegacyDaemonGreetingOwned, NegotiationPrologue, NegotiationPrologueSniffer, ProtocolVersion,
};
use std::convert::TryFrom;
use std::io::{self, Read, Write};

/// Result of negotiating an rsync session over an arbitrary transport.
///
/// The enum wraps either the binary remote-shell handshake or the legacy ASCII
/// daemon negotiation while exposing convenience accessors that mirror the
/// per-variant helpers. Higher layers can match on the [`decision`] to branch on
/// the negotiated style without re-sniffing the transport.
#[derive(Debug)]
pub enum SessionHandshake<R> {
    /// Binary remote-shell style negotiation (protocols ≥ 30).
    Binary(BinaryHandshake<R>),
    /// Legacy `@RSYNCD:` daemon negotiation.
    Legacy(LegacyDaemonHandshake<R>),
}

impl<R> SessionHandshake<R> {
    /// Returns the detected negotiation style.
    #[must_use]
    pub const fn decision(&self) -> NegotiationPrologue {
        match self {
            Self::Binary(_) => NegotiationPrologue::Binary,
            Self::Legacy(_) => NegotiationPrologue::LegacyAscii,
        }
    }

    /// Returns the negotiated protocol version after applying the caller cap.
    #[must_use]
    pub fn negotiated_protocol(&self) -> ProtocolVersion {
        match self {
            Self::Binary(handshake) => handshake.negotiated_protocol(),
            Self::Legacy(handshake) => handshake.negotiated_protocol(),
        }
    }

    /// Returns the protocol version advertised by the peer before client caps are applied.
    #[must_use]
    pub fn remote_protocol(&self) -> ProtocolVersion {
        match self {
            Self::Binary(handshake) => handshake.remote_protocol(),
            Self::Legacy(handshake) => handshake.server_protocol(),
        }
    }

    /// Returns the raw protocol number advertised by the remote peer before clamping.
    #[must_use]
    pub fn remote_advertised_protocol(&self) -> u32 {
        match self {
            Self::Binary(handshake) => handshake.remote_advertised_protocol(),
            Self::Legacy(handshake) => handshake.server_greeting().advertised_protocol(),
        }
    }

    /// Reports whether the remote advertisement had to be clamped to the supported range.
    #[must_use]
    pub fn remote_protocol_was_clamped(&self) -> bool {
        match self {
            Self::Binary(handshake) => handshake.remote_protocol_was_clamped(),
            Self::Legacy(handshake) => {
                let advertised = handshake.server_greeting().advertised_protocol();
                let advertised_byte = u8::try_from(advertised).unwrap_or(u8::MAX);
                advertised_byte > handshake.server_protocol().as_u8()
            }
        }
    }

    /// Returns a shared reference to the replaying stream regardless of variant.
    #[must_use]
    pub fn stream(&self) -> &NegotiatedStream<R> {
        match self {
            Self::Binary(handshake) => handshake.stream(),
            Self::Legacy(handshake) => handshake.stream(),
        }
    }

    /// Returns a mutable reference to the replaying stream regardless of variant.
    #[must_use]
    pub fn stream_mut(&mut self) -> &mut NegotiatedStream<R> {
        match self {
            Self::Binary(handshake) => handshake.stream_mut(),
            Self::Legacy(handshake) => handshake.stream_mut(),
        }
    }

    /// Releases the wrapper and returns the replaying stream.
    #[must_use]
    pub fn into_stream(self) -> NegotiatedStream<R> {
        match self {
            Self::Binary(handshake) => handshake.into_stream(),
            Self::Legacy(handshake) => handshake.into_stream(),
        }
    }

    /// Maps the inner transport while preserving the negotiated metadata.
    #[must_use]
    pub fn map_stream_inner<F, T>(self, map: F) -> SessionHandshake<T>
    where
        F: FnOnce(R) -> T,
    {
        match self {
            Self::Binary(handshake) => SessionHandshake::Binary(handshake.map_stream_inner(map)),
            Self::Legacy(handshake) => SessionHandshake::Legacy(handshake.map_stream_inner(map)),
        }
    }

    /// Returns the underlying binary handshake if the negotiation used that style.
    #[must_use]
    pub fn as_binary(&self) -> Option<&BinaryHandshake<R>> {
        match self {
            Self::Binary(handshake) => Some(handshake),
            Self::Legacy(_) => None,
        }
    }

    /// Returns the underlying legacy daemon handshake if the negotiation used that style.
    #[must_use]
    pub fn as_legacy(&self) -> Option<&LegacyDaemonHandshake<R>> {
        match self {
            Self::Binary(_) => None,
            Self::Legacy(handshake) => Some(handshake),
        }
    }

    /// Consumes the wrapper, returning the binary handshake when applicable.
    pub fn into_binary(self) -> Result<BinaryHandshake<R>, SessionHandshake<R>> {
        match self {
            Self::Binary(handshake) => Ok(handshake),
            Self::Legacy(_) => Err(self),
        }
    }

    /// Consumes the wrapper, returning the legacy daemon handshake when applicable.
    pub fn into_legacy(self) -> Result<LegacyDaemonHandshake<R>, SessionHandshake<R>> {
        match self {
            Self::Binary(_) => Err(self),
            Self::Legacy(handshake) => Ok(handshake),
        }
    }

    /// Decomposes the handshake into variant-specific metadata and replaying stream parts.
    ///
    /// The returned [`SessionHandshakeParts`] mirrors the helpers exposed by the variant-specific
    /// handshakes while allowing higher layers to stage the buffered negotiation bytes and
    /// negotiated metadata without matching on [`SessionHandshake`] immediately. This is useful
    /// when temporary ownership of the underlying transport is required (for example to wrap it
    /// with instrumentation) before resuming the rsync protocol exchange.
    #[must_use]
    pub fn into_stream_parts(self) -> SessionHandshakeParts<R> {
        match self {
            SessionHandshake::Binary(handshake) => {
                let (remote_advertised_protocol, remote_protocol, negotiated_protocol, parts) =
                    handshake.into_stream_parts();
                SessionHandshakeParts::Binary {
                    remote_advertised_protocol,
                    remote_protocol,
                    negotiated_protocol,
                    stream: parts,
                }
            }
            SessionHandshake::Legacy(handshake) => {
                let (server_greeting, negotiated_protocol, parts) = handshake.into_stream_parts();
                SessionHandshakeParts::Legacy {
                    server_greeting,
                    negotiated_protocol,
                    stream: parts,
                }
            }
        }
    }

    /// Reassembles a [`SessionHandshake`] from the variant-specific stream parts previously
    /// extracted via [`Self::into_stream_parts`].
    #[must_use]
    pub fn from_stream_parts(parts: SessionHandshakeParts<R>) -> Self {
        match parts {
            SessionHandshakeParts::Binary {
                remote_advertised_protocol,
                remote_protocol,
                negotiated_protocol,
                stream,
            } => SessionHandshake::Binary(BinaryHandshake::from_stream_parts(
                remote_advertised_protocol,
                remote_protocol,
                negotiated_protocol,
                stream,
            )),
            SessionHandshakeParts::Legacy {
                server_greeting,
                negotiated_protocol,
                stream,
            } => SessionHandshake::Legacy(LegacyDaemonHandshake::from_stream_parts(
                server_greeting,
                negotiated_protocol,
                stream,
            )),
        }
    }
}

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

/// Components extracted from a [`SessionHandshake`].
#[derive(Debug)]
pub enum SessionHandshakeParts<R> {
    /// Binary handshake metadata and replaying stream parts.
    Binary {
        /// Protocol number advertised by the remote peer before clamping.
        remote_advertised_protocol: u32,
        /// Protocol advertised by the remote peer.
        remote_protocol: ProtocolVersion,
        /// Protocol negotiated after applying the caller cap.
        negotiated_protocol: ProtocolVersion,
        /// Replaying stream parts containing the sniffed negotiation bytes.
        stream: NegotiatedStreamParts<R>,
    },
    /// Legacy daemon handshake metadata and replaying stream parts.
    Legacy {
        /// Parsed legacy daemon greeting announced by the server.
        server_greeting: LegacyDaemonGreetingOwned,
        /// Protocol negotiated after applying the caller cap.
        negotiated_protocol: ProtocolVersion,
        /// Replaying stream parts containing the sniffed negotiation bytes.
        stream: NegotiatedStreamParts<R>,
    },
}

impl<R> SessionHandshakeParts<R> {
    /// Returns the negotiation style associated with the extracted handshake.
    #[must_use]
    pub const fn decision(&self) -> NegotiationPrologue {
        match self {
            SessionHandshakeParts::Binary { .. } => NegotiationPrologue::Binary,
            SessionHandshakeParts::Legacy { .. } => NegotiationPrologue::LegacyAscii,
        }
    }

    /// Returns the negotiated protocol version retained by the parts structure.
    #[must_use]
    pub fn negotiated_protocol(&self) -> ProtocolVersion {
        match self {
            SessionHandshakeParts::Binary {
                negotiated_protocol,
                ..
            }
            | SessionHandshakeParts::Legacy {
                negotiated_protocol,
                ..
            } => *negotiated_protocol,
        }
    }

    /// Returns the protocol advertised by the remote peer when it can be derived from
    /// the captured handshake metadata.
    #[must_use]
    pub fn remote_protocol(&self) -> Option<ProtocolVersion> {
        match self {
            SessionHandshakeParts::Binary {
                remote_protocol, ..
            } => Some(*remote_protocol),
            SessionHandshakeParts::Legacy {
                server_greeting, ..
            } => Some(server_greeting.protocol()),
        }
    }

    /// Returns the raw protocol number advertised by the remote peer.
    #[must_use]
    pub fn remote_advertised_protocol(&self) -> u32 {
        match self {
            SessionHandshakeParts::Binary {
                remote_advertised_protocol,
                ..
            } => *remote_advertised_protocol,
            SessionHandshakeParts::Legacy {
                server_greeting, ..
            } => server_greeting.advertised_protocol(),
        }
    }

    /// Returns the legacy daemon greeting advertised by the server when available.
    #[must_use]
    pub fn server_greeting(&self) -> Option<&LegacyDaemonGreetingOwned> {
        match self {
            SessionHandshakeParts::Binary { .. } => None,
            SessionHandshakeParts::Legacy {
                server_greeting, ..
            } => Some(server_greeting),
        }
    }

    /// Returns a shared reference to the replaying stream parts.
    #[must_use]
    pub fn stream(&self) -> &NegotiatedStreamParts<R> {
        match self {
            SessionHandshakeParts::Binary { stream, .. }
            | SessionHandshakeParts::Legacy { stream, .. } => stream,
        }
    }

    /// Returns a mutable reference to the replaying stream parts.
    #[must_use]
    pub fn stream_mut(&mut self) -> &mut NegotiatedStreamParts<R> {
        match self {
            SessionHandshakeParts::Binary { stream, .. }
            | SessionHandshakeParts::Legacy { stream, .. } => stream,
        }
    }

    /// Releases the parts structure and reconstructs the replaying stream.
    #[must_use]
    pub fn into_stream(self) -> NegotiatedStream<R> {
        match self {
            SessionHandshakeParts::Binary { stream, .. }
            | SessionHandshakeParts::Legacy { stream, .. } => stream.into_stream(),
        }
    }

    /// Maps the inner transport for both variants while preserving the negotiated metadata.
    #[must_use]
    pub fn map_stream_inner<F, T>(self, map: F) -> SessionHandshakeParts<T>
    where
        F: FnOnce(R) -> T,
    {
        match self {
            SessionHandshakeParts::Binary {
                remote_advertised_protocol,
                remote_protocol,
                negotiated_protocol,
                stream,
            } => SessionHandshakeParts::Binary {
                remote_advertised_protocol,
                remote_protocol,
                negotiated_protocol,
                stream: stream.map_inner(map),
            },
            SessionHandshakeParts::Legacy {
                server_greeting,
                negotiated_protocol,
                stream,
            } => SessionHandshakeParts::Legacy {
                server_greeting,
                negotiated_protocol,
                stream: stream.map_inner(map),
            },
        }
    }

    /// Consumes the parts structure, returning the binary handshake components when available.
    pub fn into_binary(
        self,
    ) -> Result<
        (
            u32,
            ProtocolVersion,
            ProtocolVersion,
            NegotiatedStreamParts<R>,
        ),
        SessionHandshakeParts<R>,
    > {
        match self {
            SessionHandshakeParts::Binary {
                remote_advertised_protocol,
                remote_protocol,
                negotiated_protocol,
                stream,
            } => Ok((
                remote_advertised_protocol,
                remote_protocol,
                negotiated_protocol,
                stream,
            )),
            SessionHandshakeParts::Legacy { .. } => Err(self),
        }
    }

    /// Consumes the parts structure, returning the legacy handshake components when available.
    pub fn into_legacy(
        self,
    ) -> Result<
        (
            LegacyDaemonGreetingOwned,
            ProtocolVersion,
            NegotiatedStreamParts<R>,
        ),
        SessionHandshakeParts<R>,
    > {
        match self {
            SessionHandshakeParts::Binary { .. } => Err(self),
            SessionHandshakeParts::Legacy {
                server_greeting,
                negotiated_protocol,
                stream,
            } => Ok((server_greeting, negotiated_protocol, stream)),
        }
    }

    /// Reports whether the remote advertisement had to be clamped to the supported range.
    #[must_use]
    pub fn remote_protocol_was_clamped(&self) -> bool {
        match self {
            SessionHandshakeParts::Binary {
                remote_advertised_protocol,
                remote_protocol,
                ..
            } => {
                let advertised_byte = u8::try_from(*remote_advertised_protocol).unwrap_or(u8::MAX);
                advertised_byte > remote_protocol.as_u8()
            }
            SessionHandshakeParts::Legacy {
                server_greeting, ..
            } => {
                let advertised = server_greeting.advertised_protocol();
                let advertised_byte = u8::try_from(advertised).unwrap_or(u8::MAX);
                advertised_byte > server_greeting.protocol().as_u8()
            }
        }
    }

    /// Reassembles a [`SessionHandshake`] from the stored components.
    #[must_use]
    pub fn into_handshake(self) -> SessionHandshake<R> {
        SessionHandshake::from_stream_parts(self)
    }
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

fn negotiate_session_from_stream<R>(
    stream: NegotiatedStream<R>,
    desired_protocol: ProtocolVersion,
) -> io::Result<SessionHandshake<R>>
where
    R: Read + Write,
{
    match stream.decision() {
        NegotiationPrologue::Binary => {
            negotiate_binary_session_from_stream(stream, desired_protocol)
                .map(SessionHandshake::Binary)
        }
        NegotiationPrologue::LegacyAscii => {
            negotiate_legacy_daemon_session_from_stream(stream, desired_protocol)
                .map(SessionHandshake::Legacy)
        }
        NegotiationPrologue::NeedMoreData => {
            unreachable!("negotiation sniffer fully classifies the prologue")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsync_protocol::{NegotiationPrologueSniffer, ProtocolVersion};
    use std::io::{self, Cursor, Read, Write};

    #[derive(Debug)]
    struct MemoryTransport {
        reader: Cursor<Vec<u8>>,
        writes: Vec<u8>,
        flushes: usize,
    }

    impl MemoryTransport {
        fn new(input: &[u8]) -> Self {
            Self {
                reader: Cursor::new(input.to_vec()),
                writes: Vec::new(),
                flushes: 0,
            }
        }

        fn writes(&self) -> &[u8] {
            &self.writes
        }

        fn flushes(&self) -> usize {
            self.flushes
        }
    }

    impl Read for MemoryTransport {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.reader.read(buf)
        }
    }

    impl Write for MemoryTransport {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.writes.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            Ok(())
        }
    }

    fn binary_handshake_bytes(version: ProtocolVersion) -> [u8; 4] {
        u32::from(version.as_u8()).to_le_bytes()
    }

    #[test]
    fn negotiate_session_detects_binary_transport() {
        let remote_version = ProtocolVersion::from_supported(31).expect("protocol 31 supported");
        let transport = MemoryTransport::new(&binary_handshake_bytes(remote_version));

        let handshake = negotiate_session(transport, ProtocolVersion::NEWEST)
            .expect("binary handshake succeeds");

        assert_eq!(handshake.decision(), NegotiationPrologue::Binary);
        assert_eq!(handshake.negotiated_protocol(), remote_version);
        assert_eq!(handshake.remote_protocol(), remote_version);
        assert_eq!(
            handshake.remote_advertised_protocol(),
            u32::from(remote_version.as_u8())
        );
        assert!(!handshake.remote_protocol_was_clamped());

        let transport = match handshake.into_binary() {
            Ok(handshake) => {
                assert_eq!(handshake.remote_protocol(), remote_version);
                handshake.into_stream().into_inner()
            }
            Err(_) => panic!("binary handshake expected"),
        };

        assert_eq!(
            transport.writes(),
            &binary_handshake_bytes(ProtocolVersion::NEWEST)
        );
        assert_eq!(transport.flushes(), 1);
    }

    #[test]
    fn negotiate_session_detects_legacy_transport() {
        let transport = MemoryTransport::new(b"@RSYNCD: 31.0\n");

        let handshake = negotiate_session(transport, ProtocolVersion::NEWEST)
            .expect("legacy handshake succeeds");

        assert_eq!(handshake.decision(), NegotiationPrologue::LegacyAscii);
        assert_eq!(
            handshake.negotiated_protocol(),
            ProtocolVersion::from_supported(31).expect("protocol 31 supported"),
        );
        assert_eq!(
            handshake.remote_protocol(),
            ProtocolVersion::from_supported(31).expect("protocol 31 supported"),
        );

        let transport = match handshake.into_legacy() {
            Ok(handshake) => {
                assert_eq!(handshake.server_greeting().advertised_protocol(), 31);
                handshake.into_stream().into_inner()
            }
            Err(_) => panic!("legacy handshake expected"),
        };

        assert_eq!(transport.writes(), b"@RSYNCD: 31.0\n");
        assert_eq!(transport.flushes(), 1);
    }

    #[test]
    fn negotiate_session_with_sniffer_supports_reuse() {
        let remote_version = ProtocolVersion::from_supported(31).expect("protocol 31 supported");
        let transport1 = MemoryTransport::new(&binary_handshake_bytes(remote_version));
        let transport2 = MemoryTransport::new(b"@RSYNCD: 31.0\n");
        let mut sniffer = NegotiationPrologueSniffer::new();

        let handshake1 =
            negotiate_session_with_sniffer(transport1, ProtocolVersion::NEWEST, &mut sniffer)
                .expect("binary handshake succeeds");
        assert!(matches!(handshake1.decision(), NegotiationPrologue::Binary));
        assert_eq!(handshake1.remote_protocol(), remote_version);

        let transport1 = handshake1
            .into_binary()
            .expect("first handshake is binary")
            .into_stream()
            .into_inner();
        assert_eq!(
            transport1.writes(),
            &binary_handshake_bytes(ProtocolVersion::NEWEST)
        );

        let handshake2 =
            negotiate_session_with_sniffer(transport2, ProtocolVersion::NEWEST, &mut sniffer)
                .expect("legacy handshake succeeds");
        assert!(matches!(
            handshake2.decision(),
            NegotiationPrologue::LegacyAscii
        ));
        assert_eq!(
            handshake2.remote_protocol(),
            ProtocolVersion::from_supported(31).expect("protocol 31 supported"),
        );

        let transport2 = handshake2
            .into_legacy()
            .expect("second handshake is legacy")
            .into_stream()
            .into_inner();
        assert_eq!(transport2.writes(), b"@RSYNCD: 31.0\n");
        assert_eq!(transport2.flushes(), 1);
    }

    #[test]
    fn map_stream_inner_preserves_variant_and_metadata() {
        let remote_version = ProtocolVersion::from_supported(31).expect("protocol 31 supported");
        let transport = MemoryTransport::new(&binary_handshake_bytes(remote_version));

        let handshake = negotiate_session(transport, ProtocolVersion::NEWEST)
            .expect("binary handshake succeeds");

        let mut handshake = handshake.map_stream_inner(InstrumentedTransport::new);
        assert_eq!(handshake.decision(), NegotiationPrologue::Binary);
        assert!(handshake.as_binary().is_some());

        handshake
            .stream_mut()
            .write_all(b"payload")
            .expect("write succeeds");

        assert_eq!(
            handshake.remote_advertised_protocol(),
            u32::from(remote_version.as_u8())
        );
        assert!(!handshake.remote_protocol_was_clamped());

        let transport = handshake
            .into_binary()
            .expect("variant remains binary")
            .into_stream()
            .into_inner();

        let mut expected = binary_handshake_bytes(ProtocolVersion::NEWEST).to_vec();
        expected.extend_from_slice(b"payload");
        assert_eq!(transport.writes(), expected.as_slice());
        assert_eq!(transport.flushes(), 1);
    }

    #[test]
    fn session_reports_clamped_binary_future_version() {
        let future_version = 40u32;
        let transport = MemoryTransport::new(&future_version.to_le_bytes());

        let handshake = negotiate_session(transport, ProtocolVersion::NEWEST)
            .expect("binary handshake clamps future versions");

        assert_eq!(handshake.decision(), NegotiationPrologue::Binary);
        assert_eq!(handshake.remote_protocol(), ProtocolVersion::NEWEST);
        assert_eq!(handshake.remote_advertised_protocol(), future_version);
        assert!(handshake.remote_protocol_was_clamped());

        let parts = handshake.into_stream_parts();
        assert_eq!(parts.decision(), NegotiationPrologue::Binary);
        assert_eq!(parts.remote_protocol(), Some(ProtocolVersion::NEWEST));
        assert_eq!(parts.remote_advertised_protocol(), future_version);
        assert!(parts.remote_protocol_was_clamped());
    }

    #[test]
    fn session_handshake_parts_round_trip_binary_handshake() {
        let remote_version = ProtocolVersion::from_supported(31).expect("protocol 31 supported");
        let transport = MemoryTransport::new(&binary_handshake_bytes(remote_version));

        let handshake = negotiate_session(transport, ProtocolVersion::NEWEST)
            .expect("binary handshake succeeds");

        let parts = handshake.into_stream_parts();
        assert_eq!(parts.decision(), NegotiationPrologue::Binary);
        assert_eq!(parts.negotiated_protocol(), remote_version);
        assert_eq!(parts.remote_protocol(), Some(remote_version));
        assert_eq!(
            parts.remote_advertised_protocol(),
            u32::from(remote_version.as_u8())
        );
        assert!(!parts.remote_protocol_was_clamped());
        assert!(parts.server_greeting().is_none());
        assert_eq!(parts.stream().decision(), NegotiationPrologue::Binary);

        let (remote_advertised_protocol, remote_protocol, negotiated_protocol, stream_parts) =
            parts.into_binary().expect("binary parts available");

        let parts = SessionHandshakeParts::Binary {
            remote_advertised_protocol,
            remote_protocol,
            negotiated_protocol,
            stream: stream_parts.map_inner(InstrumentedTransport::new),
        };

        let handshake = SessionHandshake::from_stream_parts(parts);
        let mut binary = handshake
            .into_binary()
            .expect("parts reconstruct binary handshake");

        assert_eq!(
            binary.remote_advertised_protocol(),
            u32::from(remote_version.as_u8())
        );
        assert!(!binary.remote_protocol_was_clamped());

        binary
            .stream_mut()
            .write_all(b"payload")
            .expect("write succeeds");

        let transport = binary.into_stream().into_inner();
        let mut expected = binary_handshake_bytes(ProtocolVersion::NEWEST).to_vec();
        expected.extend_from_slice(b"payload");
        assert_eq!(transport.writes(), expected.as_slice());
        assert_eq!(transport.flushes(), 1);
    }

    #[test]
    fn session_handshake_parts_round_trip_legacy_handshake() {
        let transport = MemoryTransport::new(b"@RSYNCD: 31.0\n");

        let handshake = negotiate_session(transport, ProtocolVersion::NEWEST)
            .expect("legacy handshake succeeds");

        let parts = handshake.into_stream_parts();
        assert_eq!(parts.decision(), NegotiationPrologue::LegacyAscii);
        let negotiated = ProtocolVersion::from_supported(31).expect("protocol 31 supported");
        assert_eq!(parts.negotiated_protocol(), negotiated);
        assert_eq!(parts.remote_protocol(), Some(negotiated));
        assert_eq!(parts.remote_advertised_protocol(), 31);
        assert!(!parts.remote_protocol_was_clamped());
        let server = parts.server_greeting().expect("server greeting retained");
        assert_eq!(server.advertised_protocol(), 31);
        assert_eq!(parts.stream().decision(), NegotiationPrologue::LegacyAscii);

        let (server_greeting, negotiated_protocol, stream_parts) =
            parts.into_legacy().expect("legacy parts available");

        let parts = SessionHandshakeParts::Legacy {
            server_greeting,
            negotiated_protocol,
            stream: stream_parts.map_inner(InstrumentedTransport::new),
        };

        let handshake = SessionHandshake::from_stream_parts(parts);
        let mut legacy = handshake
            .into_legacy()
            .expect("parts reconstruct legacy handshake");

        legacy
            .stream_mut()
            .write_all(b"module\n")
            .expect("write succeeds");

        let transport = legacy.into_stream().into_inner();
        let mut expected = b"@RSYNCD: 31.0\n".to_vec();
        expected.extend_from_slice(b"module\n");
        assert_eq!(transport.writes(), expected.as_slice());
        assert_eq!(transport.flushes(), 1);
    }

    #[test]
    fn session_reports_clamped_future_legacy_version() {
        let transport = MemoryTransport::new(b"@RSYNCD: 40.0\n");

        let handshake = negotiate_session(transport, ProtocolVersion::NEWEST)
            .expect("legacy handshake clamps future advertisement");

        assert_eq!(handshake.decision(), NegotiationPrologue::LegacyAscii);
        assert_eq!(handshake.remote_protocol(), ProtocolVersion::NEWEST);
        assert_eq!(handshake.remote_advertised_protocol(), 40);
        assert!(handshake.remote_protocol_was_clamped());

        let parts = handshake.into_stream_parts();
        assert_eq!(parts.decision(), NegotiationPrologue::LegacyAscii);
        assert_eq!(parts.remote_protocol(), Some(ProtocolVersion::NEWEST));
        assert_eq!(parts.remote_advertised_protocol(), 40);
        assert!(parts.remote_protocol_was_clamped());
    }

    #[test]
    fn session_handshake_parts_preserve_remote_protocol_for_legacy_caps() {
        let desired = ProtocolVersion::from_supported(30).expect("protocol 30 supported");
        let transport = MemoryTransport::new(b"@RSYNCD: 32.0\n");

        let handshake = negotiate_session(transport, desired)
            .expect("legacy handshake succeeds with future advertisement");

        let parts = handshake.into_stream_parts();
        let remote = ProtocolVersion::from_supported(32).expect("protocol 32 supported");
        assert_eq!(parts.negotiated_protocol(), desired);
        assert_eq!(parts.remote_protocol(), Some(remote));
        assert_eq!(parts.remote_advertised_protocol(), 32);
        assert!(!parts.remote_protocol_was_clamped());
        let server = parts.server_greeting().expect("server greeting retained");
        assert_eq!(server.protocol(), remote);
        assert_eq!(server.advertised_protocol(), 32);
    }

    #[derive(Debug)]
    struct InstrumentedTransport {
        inner: MemoryTransport,
    }

    impl InstrumentedTransport {
        fn new(inner: MemoryTransport) -> Self {
            Self { inner }
        }

        fn writes(&self) -> &[u8] {
            self.inner.writes()
        }

        fn flushes(&self) -> usize {
            self.inner.flushes()
        }
    }

    impl Read for InstrumentedTransport {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.inner.read(buf)
        }
    }

    impl Write for InstrumentedTransport {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.inner.write(buf)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.inner.flush()
        }
    }
}
