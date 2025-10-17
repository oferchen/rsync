use crate::negotiation::{NegotiatedStream, sniff_negotiation_stream};
use rsync_protocol::{NegotiationPrologue, ProtocolVersion};
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
    pub fn into_components(self) -> (ProtocolVersion, ProtocolVersion, NegotiatedStream<R>) {
        (self.remote_protocol, self.negotiated_protocol, self.stream)
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
            remote_protocol,
            negotiated_protocol,
        } = self;

        BinaryHandshake {
            stream: stream.map_inner(map),
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

fn negotiate_binary_session_from_stream<R>(
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
    stream.inner_mut().write_all(&advertisement)?;

    let mut remote_buf = [0u8; 4];
    stream.read_exact(&mut remote_buf)?;
    let remote_raw = u32::from_le_bytes(remote_buf);

    let remote_byte = remote_raw.try_into().unwrap_or(u8::MAX);

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
        remote_protocol,
        negotiated_protocol,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
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
    }

    impl InstrumentedTransport {
        fn new(inner: MemoryTransport) -> Self {
            Self {
                inner,
                observed_writes: Vec::new(),
            }
        }

        fn writes(&self) -> &[u8] {
            &self.observed_writes
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

        let instrumented = handshake.into_stream().into_inner();
        assert_eq!(instrumented.writes(), b"payload");

        let inner = instrumented.into_inner();
        let mut expected = handshake_bytes(ProtocolVersion::NEWEST).to_vec();
        expected.extend_from_slice(b"payload");
        assert_eq!(inner.written(), expected.as_slice());
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
    }

    #[test]
    fn negotiate_binary_session_applies_cap() {
        let remote_version = ProtocolVersion::NEWEST;
        let desired = ProtocolVersion::from_supported(30).expect("30 supported");
        let transport = MemoryTransport::new(&handshake_bytes(remote_version));

        let handshake = negotiate_binary_session(transport, desired).expect("handshake succeeds");

        assert_eq!(handshake.remote_protocol(), remote_version);
        assert_eq!(handshake.negotiated_protocol(), desired);
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
}
