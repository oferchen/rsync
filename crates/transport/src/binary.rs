use crate::negotiation::{
    NegotiatedStream, NegotiatedStreamParts, TryMapInnerError, sniff_negotiation_stream,
    sniff_negotiation_stream_with_sniffer,
};
use core::convert::TryFrom;
use rsync_protocol::{NegotiationPrologue, NegotiationPrologueSniffer, ProtocolVersion};
use std::cmp;
use std::io::{self, Read, Write};

/// Result of completing the binary rsync protocol negotiation.
///
/// The structure mirrors the legacy daemon helper but targets transports that
/// use the binary handshake (e.g. remote-shell sessions). It exposes the
/// negotiated protocol version together with the remote peer's advertisement
/// while retaining the replaying stream so higher layers can continue the
/// exchange without losing buffered bytes consumed during negotiation
/// detection.
#[derive(Debug)]
pub struct BinaryHandshake<R> {
    stream: NegotiatedStream<R>,
    remote_advertised: u32,
    remote_protocol: ProtocolVersion,
    negotiated_protocol: ProtocolVersion,
}

impl<R> BinaryHandshake<R> {
    /// Returns the negotiated protocol version after clamping to the caller's
    /// desired cap and the remote peer's advertisement.
    #[must_use]
    pub const fn negotiated_protocol(&self) -> ProtocolVersion {
        self.negotiated_protocol
    }

    /// Returns the protocol version advertised by the remote peer.
    #[must_use]
    pub const fn remote_protocol(&self) -> ProtocolVersion {
        self.remote_protocol
    }

    /// Returns the protocol byte advertised by the remote peer before clamping.
    #[must_use]
    pub const fn remote_advertised_protocol(&self) -> u32 {
        self.remote_advertised
    }

    /// Reports whether the remote peer advertised a protocol newer than we support.
    #[must_use]
    pub fn remote_protocol_was_clamped(&self) -> bool {
        let advertised = self.remote_advertised_protocol();
        let advertised_byte = u8::try_from(advertised).unwrap_or(u8::MAX);
        advertised_byte > self.remote_protocol.as_u8()
    }

    /// Returns a shared reference to the replaying stream.
    #[must_use]
    pub const fn stream(&self) -> &NegotiatedStream<R> {
        &self.stream
    }

    /// Returns a mutable reference to the replaying stream.
    #[must_use]
    pub fn stream_mut(&mut self) -> &mut NegotiatedStream<R> {
        &mut self.stream
    }

    /// Releases the handshake wrapper and returns the replaying stream.
    #[must_use]
    pub fn into_stream(self) -> NegotiatedStream<R> {
        self.stream
    }

    /// Decomposes the handshake into its components.
    #[must_use]
    pub fn into_components(self) -> (u32, ProtocolVersion, ProtocolVersion, NegotiatedStream<R>) {
        (
            self.remote_advertised,
            self.remote_protocol,
            self.negotiated_protocol,
            self.stream,
        )
    }

    /// Maps the inner transport while preserving the negotiated metadata.
    ///
    /// This helper forwards to [`NegotiatedStream::map_inner`], allowing callers to
    /// install additional instrumentation or adapters around the underlying
    /// transport without losing the negotiated protocol versions. The replay
    /// buffer captured during negotiation is retained so higher layers can
    /// resume reading or writing immediately after the transformation.
    #[must_use]
    pub fn map_stream_inner<F, T>(self, map: F) -> BinaryHandshake<T>
    where
        F: FnOnce(R) -> T,
    {
        let Self {
            stream,
            remote_advertised,
            remote_protocol,
            negotiated_protocol,
        } = self;

        BinaryHandshake {
            stream: stream.map_inner(map),
            remote_advertised,
            remote_protocol,
            negotiated_protocol,
        }
    }

    /// Attempts to transform the inner transport while preserving the negotiated metadata.
    ///
    /// The closure returns the replacement reader on success or a tuple containing the error and
    /// original reader on failure, mirroring [`NegotiatedStream::try_map_inner`].
    pub fn try_map_stream_inner<F, T, E>(
        self,
        map: F,
    ) -> Result<BinaryHandshake<T>, TryMapInnerError<BinaryHandshake<R>, E>>
    where
        F: FnOnce(R) -> Result<T, (E, R)>,
    {
        let Self {
            stream,
            remote_advertised,
            remote_protocol,
            negotiated_protocol,
        } = self;

        stream
            .try_map_inner(map)
            .map(|stream| BinaryHandshake {
                stream,
                remote_advertised,
                remote_protocol,
                negotiated_protocol,
            })
            .map_err(|err| {
                err.map_original(|stream| BinaryHandshake {
                    stream,
                    remote_advertised,
                    remote_protocol,
                    negotiated_protocol,
                })
            })
    }

    /// Decomposes the handshake into the negotiated protocol metadata and replaying stream parts.
    ///
    /// Returning [`NegotiatedStreamParts`] allows higher layers to temporarily take ownership of
    /// the buffered negotiation bytes (for example to wrap the underlying transport) without
    /// dropping the recorded remote advertisement. The tuple mirrors
    /// [`Self::into_components`], but hands back the split representation so callers can inspect or
    /// transform the inner reader before reassembling a [`NegotiatedStream`].
    #[must_use]
    pub fn into_stream_parts(
        self,
    ) -> (
        u32,
        ProtocolVersion,
        ProtocolVersion,
        NegotiatedStreamParts<R>,
    ) {
        let Self {
            stream,
            remote_advertised,
            remote_protocol,
            negotiated_protocol,
        } = self;

        (
            remote_advertised,
            remote_protocol,
            negotiated_protocol,
            stream.into_parts(),
        )
    }

    /// Reconstructs a [`BinaryHandshake`] from previously extracted stream parts.
    ///
    /// Higher layers occasionally need to stash the negotiated protocol metadata while they wrap the
    /// underlying transport with instrumentation or adapters. This helper accepts the values returned
    /// by [`Self::into_stream_parts`] and rebuilds the handshake without rerunning the negotiation or
    /// replaying buffered bytes. The negotiation decision is asserted in debug builds so binary and
    /// legacy parts cannot be mixed inadvertently.
    #[must_use]
    pub fn from_stream_parts(
        remote_advertised: u32,
        remote_protocol: ProtocolVersion,
        negotiated_protocol: ProtocolVersion,
        parts: NegotiatedStreamParts<R>,
    ) -> Self {
        debug_assert_eq!(parts.decision(), NegotiationPrologue::Binary);

        Self {
            stream: parts.into_stream(),
            remote_advertised,
            remote_protocol,
            negotiated_protocol,
        }
    }
}

/// Performs the binary rsync protocol negotiation.
///
/// The helper mirrors upstream rsync's behaviour when establishing a
/// remote-shell session: it sniffs the transport to ensure the connection is
/// using the binary handshake, writes the caller's desired protocol version,
/// reads the peer's advertisement, clamps the negotiated value, and returns
/// the replaying stream together with the negotiated protocol information.
///
/// # Errors
///
/// - [`io::ErrorKind::InvalidData`] if the transport advertises the legacy
///   `@RSYNCD:` negotiation or if the peer reports a protocol outside the
///   supported range.
/// - Any I/O error reported while sniffing the prologue, writing the client's
///   advertisement, or reading the peer's response.
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

pub(crate) fn negotiate_binary_session_from_stream<R>(
    mut stream: NegotiatedStream<R>,
    desired_protocol: ProtocolVersion,
) -> io::Result<BinaryHandshake<R>>
where
    R: Read + Write,
{
    match stream.decision() {
        NegotiationPrologue::Binary => {}
        NegotiationPrologue::LegacyAscii => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "binary negotiation requires binary prologue",
            ));
        }
        NegotiationPrologue::NeedMoreData => {
            unreachable!("sniffer fully classifies the negotiation prologue");
        }
    }

    let mut advertisement = [0u8; 4];
    let desired = desired_protocol.as_u8();
    advertisement.copy_from_slice(&u32::from(desired).to_le_bytes());
    {
        let inner = stream.inner_mut();
        inner.write_all(&advertisement)?;
        inner.flush()?;
    }

    let mut remote_buf = [0u8; 4];
    stream.read_exact(&mut remote_buf)?;
    let remote_advertised = u32::from_le_bytes(remote_buf);

    let remote_byte = u8::try_from(remote_advertised).unwrap_or(u8::MAX);

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

    Ok(BinaryHandshake {
        stream,
        remote_advertised,
        remote_protocol,
        negotiated_protocol,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsync_protocol::{NegotiationPrologue, NegotiationPrologueSniffer};
    use std::io::{self, Cursor, Read, Write};

    #[derive(Debug)]
    struct MemoryTransport {
        reader: Cursor<Vec<u8>>,
        written: Vec<u8>,
    }

    impl MemoryTransport {
        fn new(input: &[u8]) -> Self {
            Self {
                reader: Cursor::new(input.to_vec()),
                written: Vec::new(),
            }
        }

        fn written(&self) -> &[u8] {
            &self.written
        }
    }

    impl Read for MemoryTransport {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.reader.read(buf)
        }
    }

    impl Write for MemoryTransport {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.written.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[derive(Debug)]
    struct InstrumentedTransport {
        inner: MemoryTransport,
        observed_writes: Vec<u8>,
        flushes: usize,
    }

    impl InstrumentedTransport {
        fn new(inner: MemoryTransport) -> Self {
            Self {
                inner,
                observed_writes: Vec::new(),
                flushes: 0,
            }
        }

        fn writes(&self) -> &[u8] {
            &self.observed_writes
        }

        fn flushes(&self) -> usize {
            self.flushes
        }

        fn into_inner(self) -> MemoryTransport {
            self.inner
        }
    }

    impl Read for InstrumentedTransport {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.inner.read(buf)
        }
    }

    impl Write for InstrumentedTransport {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.observed_writes.extend_from_slice(buf);
            self.inner.write(buf)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            self.inner.flush()
        }
    }

    #[derive(Debug)]
    struct CountingTransport {
        inner: MemoryTransport,
        flushes: usize,
    }

    impl CountingTransport {
        fn new(input: &[u8]) -> Self {
            Self {
                inner: MemoryTransport::new(input),
                flushes: 0,
            }
        }

        fn written(&self) -> &[u8] {
            self.inner.written()
        }

        fn flushes(&self) -> usize {
            self.flushes
        }
    }

    impl Read for CountingTransport {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.inner.read(buf)
        }
    }

    impl Write for CountingTransport {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.inner.write(buf)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            self.inner.flush()
        }
    }

    fn handshake_bytes(version: ProtocolVersion) -> [u8; 4] {
        u32::from(version.as_u8()).to_le_bytes()
    }

    #[test]
    fn negotiate_binary_session_exchanges_versions() {
        let remote_version = ProtocolVersion::from_supported(31).expect("31 supported");
        let transport = MemoryTransport::new(&handshake_bytes(remote_version));

        let handshake = negotiate_binary_session(transport, ProtocolVersion::NEWEST)
            .expect("handshake succeeds");

        assert_eq!(handshake.remote_protocol(), remote_version);
        assert_eq!(handshake.negotiated_protocol(), remote_version);
        assert_eq!(
            handshake.remote_advertised_protocol(),
            u32::from(remote_version.as_u8())
        );
        assert!(!handshake.remote_protocol_was_clamped());

        let transport = handshake.into_stream().into_inner();
        assert_eq!(
            transport.written(),
            &handshake_bytes(ProtocolVersion::NEWEST)
        );
    }

    #[test]
    fn map_stream_inner_preserves_protocols_and_replays_transport() {
        let remote_version = ProtocolVersion::from_supported(31).expect("31 supported");
        let transport = MemoryTransport::new(&handshake_bytes(remote_version));

        let handshake = negotiate_binary_session(transport, ProtocolVersion::NEWEST)
            .expect("handshake succeeds");

        let mut handshake = handshake.map_stream_inner(InstrumentedTransport::new);
        handshake
            .stream_mut()
            .write_all(b"payload")
            .expect("write propagates");
        handshake.stream_mut().flush().expect("flush propagates");

        assert_eq!(handshake.remote_protocol(), remote_version);
        assert_eq!(handshake.negotiated_protocol(), remote_version);
        assert_eq!(
            handshake.remote_advertised_protocol(),
            u32::from(remote_version.as_u8())
        );
        assert!(!handshake.remote_protocol_was_clamped());

        let instrumented = handshake.into_stream().into_inner();
        assert_eq!(instrumented.writes(), b"payload");
        assert_eq!(instrumented.flushes(), 1);

        let inner = instrumented.into_inner();
        let mut expected = handshake_bytes(ProtocolVersion::NEWEST).to_vec();
        expected.extend_from_slice(b"payload");
        assert_eq!(inner.written(), expected.as_slice());
    }

    #[test]
    fn try_map_stream_inner_transforms_transport() {
        let remote_version = ProtocolVersion::from_supported(31).expect("31 supported");
        let transport = MemoryTransport::new(&handshake_bytes(remote_version));

        let handshake = negotiate_binary_session(transport, ProtocolVersion::NEWEST)
            .expect("handshake succeeds");

        let mut handshake = handshake
            .try_map_stream_inner(
                |inner| -> Result<InstrumentedTransport, (io::Error, MemoryTransport)> {
                    Ok(InstrumentedTransport::new(inner))
                },
            )
            .expect("mapping succeeds");

        handshake
            .stream_mut()
            .write_all(b"payload")
            .expect("write propagates");
        handshake.stream_mut().flush().expect("flush propagates");
        assert_eq!(handshake.remote_protocol(), remote_version);

        let instrumented = handshake.into_stream().into_inner();
        assert_eq!(instrumented.writes(), b"payload");
        assert_eq!(instrumented.flushes(), 1);
    }

    #[test]
    fn try_map_stream_inner_preserves_original_on_error() {
        let remote_version = ProtocolVersion::from_supported(31).expect("31 supported");
        let transport = MemoryTransport::new(&handshake_bytes(remote_version));

        let handshake = negotiate_binary_session(transport, ProtocolVersion::NEWEST)
            .expect("handshake succeeds");

        let err = handshake
            .try_map_stream_inner(
                |inner| -> Result<InstrumentedTransport, (io::Error, MemoryTransport)> {
                    Err((io::Error::new(io::ErrorKind::Other, "boom"), inner))
                },
            )
            .expect_err("mapping fails");

        assert_eq!(err.error().kind(), io::ErrorKind::Other);
        let original = err.into_original();
        assert_eq!(original.remote_protocol(), remote_version);
        let transport = original.into_stream().into_inner();
        assert_eq!(
            transport.written(),
            &handshake_bytes(ProtocolVersion::NEWEST)
        );
    }

    #[test]
    fn negotiate_binary_session_clamps_future_protocols() {
        let future_version = 40u32;
        let transport = MemoryTransport::new(&future_version.to_le_bytes());

        let desired = ProtocolVersion::from_supported(29).expect("29 supported");
        let handshake =
            negotiate_binary_session(transport, desired).expect("future versions clamp");

        assert_eq!(handshake.remote_protocol(), ProtocolVersion::NEWEST);
        assert_eq!(handshake.negotiated_protocol(), desired);
        assert_eq!(handshake.remote_advertised_protocol(), future_version);
        assert!(handshake.remote_protocol_was_clamped());

        let transport = handshake.into_stream().into_inner();
        assert_eq!(transport.written(), &handshake_bytes(desired));
    }

    #[test]
    fn negotiate_binary_session_accepts_protocols_beyond_u8() {
        let future_version = 0x0001_0200u32;
        let transport = MemoryTransport::new(&future_version.to_le_bytes());

        let handshake = negotiate_binary_session(transport, ProtocolVersion::NEWEST)
            .expect("future ints clamp");

        assert_eq!(handshake.remote_protocol(), ProtocolVersion::NEWEST);
        assert_eq!(handshake.negotiated_protocol(), ProtocolVersion::NEWEST);
        assert_eq!(handshake.remote_advertised_protocol(), future_version);
        assert!(handshake.remote_protocol_was_clamped());
    }

    #[test]
    fn negotiate_binary_session_applies_cap() {
        let remote_version = ProtocolVersion::NEWEST;
        let desired = ProtocolVersion::from_supported(30).expect("30 supported");
        let transport = MemoryTransport::new(&handshake_bytes(remote_version));

        let handshake = negotiate_binary_session(transport, desired).expect("handshake succeeds");

        assert_eq!(handshake.remote_protocol(), remote_version);
        assert_eq!(handshake.negotiated_protocol(), desired);
        assert_eq!(
            handshake.remote_advertised_protocol(),
            u32::from(remote_version.as_u8())
        );
        assert!(!handshake.remote_protocol_was_clamped());
    }

    #[test]
    fn negotiate_binary_session_rejects_legacy_prefix() {
        let transport = MemoryTransport::new(b"@RSYNCD: 31.0\n");
        let err = negotiate_binary_session(transport, ProtocolVersion::NEWEST)
            .expect_err("legacy prefix must fail");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn negotiate_binary_session_rejects_out_of_range_version() {
        let mut bytes = [0u8; 4];
        bytes.copy_from_slice(&u32::from(27u32).to_le_bytes());
        let transport = MemoryTransport::new(&bytes);
        let err = negotiate_binary_session(transport, ProtocolVersion::NEWEST)
            .expect_err("unsupported protocol must fail");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn into_stream_parts_exposes_negotiation_state() {
        let remote_version = ProtocolVersion::from_supported(31).expect("31 supported");
        let transport = MemoryTransport::new(&handshake_bytes(remote_version));

        let handshake = negotiate_binary_session(transport, ProtocolVersion::NEWEST)
            .expect("handshake succeeds");

        let (remote_adv, remote, negotiated, parts) = handshake.into_stream_parts();
        assert_eq!(remote_adv, u32::from(remote_version.as_u8()));
        assert_eq!(remote, remote_version);
        assert_eq!(negotiated, remote_version);
        assert_eq!(parts.decision(), NegotiationPrologue::Binary);
        assert_eq!(parts.sniffed_prefix(), &[remote_version.as_u8()]);
        assert_eq!(parts.buffered_remaining(), 0);
        assert_eq!(parts.sniffed_prefix_len(), 1);

        let mut stream = parts.into_stream();
        let mut remainder = Vec::new();
        stream
            .read_to_end(&mut remainder)
            .expect("no additional bytes remain after handshake");
        assert!(remainder.is_empty());

        let transport = stream.into_inner();
        assert_eq!(
            transport.written(),
            &handshake_bytes(ProtocolVersion::NEWEST)
        );
    }

    #[test]
    fn from_stream_parts_rehydrates_binary_handshake() {
        let remote_version = ProtocolVersion::from_supported(31).expect("31 supported");
        let transport = CountingTransport::new(&handshake_bytes(remote_version));

        let handshake = negotiate_binary_session(transport, ProtocolVersion::NEWEST)
            .expect("handshake succeeds");

        let (remote_adv, remote, negotiated, parts) = handshake.into_stream_parts();
        assert_eq!(remote_adv, u32::from(remote_version.as_u8()));
        assert_eq!(remote, remote_version);
        assert_eq!(negotiated, remote_version);
        assert_eq!(parts.decision(), NegotiationPrologue::Binary);

        let mut rehydrated =
            BinaryHandshake::from_stream_parts(remote_adv, remote, negotiated, parts);

        assert_eq!(rehydrated.remote_protocol(), remote_version);
        assert_eq!(rehydrated.negotiated_protocol(), remote_version);
        assert_eq!(rehydrated.stream().decision(), NegotiationPrologue::Binary);

        rehydrated
            .stream_mut()
            .write_all(b"payload")
            .expect("write propagates");
        rehydrated.stream_mut().flush().expect("flush propagates");

        let transport = rehydrated.into_stream().into_inner();
        assert_eq!(transport.flushes(), 2);

        let mut expected = handshake_bytes(ProtocolVersion::NEWEST).to_vec();
        expected.extend_from_slice(b"payload");
        assert_eq!(transport.written(), expected.as_slice());
    }

    #[test]
    fn negotiate_binary_session_flushes_advertisement() {
        let transport = CountingTransport::new(&handshake_bytes(ProtocolVersion::NEWEST));

        let handshake = negotiate_binary_session(transport, ProtocolVersion::NEWEST)
            .expect("handshake succeeds");

        let transport = handshake.into_stream().into_inner();
        assert_eq!(transport.flushes(), 1);
        assert_eq!(
            transport.written(),
            &handshake_bytes(ProtocolVersion::NEWEST)
        );
    }

    #[test]
    fn negotiate_binary_session_with_sniffer_reuses_instance() {
        let remote_version = ProtocolVersion::from_supported(31).expect("31 supported");
        let transport1 = MemoryTransport::new(&handshake_bytes(remote_version));
        let transport2 = MemoryTransport::new(&handshake_bytes(remote_version));

        let mut sniffer = NegotiationPrologueSniffer::new();

        let handshake1 = negotiate_binary_session_with_sniffer(
            transport1,
            ProtocolVersion::NEWEST,
            &mut sniffer,
        )
        .expect("handshake succeeds with supplied sniffer");
        assert_eq!(handshake1.remote_protocol(), remote_version);
        assert_eq!(handshake1.negotiated_protocol(), remote_version);

        drop(handshake1);

        let handshake2 = negotiate_binary_session_with_sniffer(
            transport2,
            ProtocolVersion::NEWEST,
            &mut sniffer,
        )
        .expect("sniffer can be reused for subsequent sessions");
        assert_eq!(handshake2.remote_protocol(), remote_version);
        assert_eq!(handshake2.negotiated_protocol(), remote_version);
    }
}
