use crate::binary::{BinaryHandshake, negotiate_binary_session_from_stream};
use crate::daemon::{LegacyDaemonHandshake, negotiate_legacy_daemon_session_from_stream};
use crate::negotiation::{
    NegotiatedStream, sniff_negotiation_stream, sniff_negotiation_stream_with_sniffer,
};
use rsync_protocol::{NegotiationPrologue, NegotiationPrologueSniffer, ProtocolVersion};
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
        assert_eq!(transport.flushes(), 0);
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

        let transport = handshake
            .into_binary()
            .expect("variant remains binary")
            .into_stream()
            .into_inner();

        let mut expected = binary_handshake_bytes(ProtocolVersion::NEWEST).to_vec();
        expected.extend_from_slice(b"payload");
        assert_eq!(transport.writes(), expected.as_slice());
        assert_eq!(transport.flushes(), 0);
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
