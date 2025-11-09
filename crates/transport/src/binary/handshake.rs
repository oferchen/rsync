use crate::handshake_util::{RemoteProtocolAdvertisement, local_cap_reduced_protocol};
use crate::negotiation::{NegotiatedStream, NegotiatedStreamParts, TryMapInnerError};
use protocol::{NegotiationPrologue, NegotiationPrologueSniffer, ProtocolVersion};
use std::collections::TryReserveError;

use super::BinaryHandshakeParts;

/// Result of completing the binary rsync protocol negotiation.
///
/// The structure mirrors the legacy daemon helper but targets transports that
/// use the binary handshake (e.g. remote-shell sessions). It exposes the
/// negotiated protocol version together with the remote peer's advertisement
/// while retaining the replaying stream so higher layers can continue the
/// exchange without losing buffered bytes consumed during negotiation
/// detection.
///
/// When the underlying transport implements [`Clone`], the handshake can be
/// cloned to stage multiple views of the same negotiated session. The cloned
/// value retains the replay buffer and metadata so both instances continue in
/// lockstep without rereading from the transport.
#[derive(Clone, Debug)]
pub struct BinaryHandshake<R> {
    stream: NegotiatedStream<R>,
    remote_advertisement: RemoteProtocolAdvertisement,
    negotiated_protocol: ProtocolVersion,
    local_advertised: ProtocolVersion,
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
        self.remote_advertisement.negotiated()
    }

    /// Returns the protocol byte advertised by the remote peer before clamping.
    #[must_use]
    pub const fn remote_advertised_protocol(&self) -> u32 {
        self.remote_advertisement.advertised()
    }

    /// Returns the protocol version the local peer advertised to the remote side.
    ///
    /// Binary negotiations transmit the caller's desired protocol (subject to `--protocol` caps) before
    /// the remote advertisement is read. Capturing the value allows diagnostics to reference both sides
    /// of the negotiation without requiring the original caller to stash the requested version.
    #[doc(alias = "--protocol")]
    #[must_use]
    pub const fn local_advertised_protocol(&self) -> ProtocolVersion {
        self.local_advertised
    }

    /// Reports whether the remote peer advertised a protocol newer than we support.
    #[must_use]
    pub const fn remote_protocol_was_clamped(&self) -> bool {
        self.remote_advertisement.was_clamped()
    }

    /// Returns the classification of the peer's protocol advertisement.
    ///
    /// The helper mirrors [`BinaryHandshakeParts::remote_advertisement`] so the
    /// wrapper and its decomposed form remain in sync.
    #[must_use]
    pub const fn remote_advertisement(&self) -> RemoteProtocolAdvertisement {
        self.remote_advertisement
    }

    /// Reports whether the caller's desired cap reduced the negotiated protocol version.
    ///
    /// Upstream rsync clamps the negotiated protocol to the minimum of the peer's advertisement and
    /// the caller's requested cap (as configured via `--protocol`). When the requested value is
    /// lower than the remote protocol, the transfer is forced to speak the older version. This
    /// helper exposes that condition so higher layers can surface diagnostics or adjust feature
    /// negotiation in parity with the C implementation.
    ///
    /// # Examples
    ///
    /// Force the negotiation to protocol 29 even though the peer advertises 31. The helper reports
    /// that the user-imposed cap took effect, mirroring the observable behaviour of
    /// `rsync --protocol=29`.
    ///
    /// ```
    /// use protocol::ProtocolVersion;
    /// use transport::negotiate_binary_session;
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
    /// let desired = ProtocolVersion::from_supported(29).unwrap();
    /// let transport = Loopback::new(remote);
    /// let handshake = negotiate_binary_session(transport, desired).unwrap();
    ///
    /// assert!(handshake.local_protocol_was_capped());
    /// assert_eq!(handshake.negotiated_protocol(), desired);
    /// ```
    #[doc(alias = "--protocol")]
    #[must_use]
    pub const fn local_protocol_was_capped(&self) -> bool {
        local_cap_reduced_protocol(self.remote_protocol(), self.negotiated_protocol)
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

    /// Rehydrates a [`NegotiationPrologueSniffer`] using the captured negotiation snapshot.
    ///
    /// The helper invokes [`NegotiationPrologueSniffer::rehydrate_from_parts`] with the buffered
    /// transcript captured during negotiation, mirroring the functionality available via
    /// [`BinaryHandshakeParts::stream_parts`]. Callers that retain the handshake wrapper can
    /// therefore rebuild sniffers without unpacking the parts structure or replaying the underlying
    /// transport, matching the ergonomics provided by the session-level helpers.
    pub fn rehydrate_sniffer(
        &self,
        sniffer: &mut NegotiationPrologueSniffer,
    ) -> Result<(), TryReserveError> {
        sniffer.rehydrate_from_parts(
            self.stream.decision(),
            self.stream.sniffed_prefix_len(),
            self.stream.buffered(),
        )
    }

    /// Decomposes the handshake into its components.
    #[must_use]
    pub fn into_components(
        self,
    ) -> (
        u32,
        ProtocolVersion,
        ProtocolVersion,
        ProtocolVersion,
        NegotiatedStream<R>,
    ) {
        (
            self.remote_advertisement.advertised(),
            self.remote_advertisement.negotiated(),
            self.local_advertised,
            self.negotiated_protocol,
            self.stream,
        )
    }

    /// Decomposes the handshake into a [`BinaryHandshakeParts`] structure.
    #[must_use]
    pub fn into_parts(self) -> BinaryHandshakeParts<R> {
        let (remote_advertised, remote_protocol, local_advertised, negotiated_protocol, stream) =
            self.into_stream_parts();
        let remote_advertisement =
            RemoteProtocolAdvertisement::from_raw(remote_advertised, remote_protocol);
        BinaryHandshakeParts::from_components(
            remote_advertisement,
            negotiated_protocol,
            local_advertised,
            stream,
        )
    }

    /// Reconstructs a [`BinaryHandshake`] from previously extracted parts.
    #[must_use]
    pub fn from_parts(parts: BinaryHandshakeParts<R>) -> Self {
        let (remote_advertised, remote_protocol, local_advertised, negotiated_protocol, stream) =
            parts.into_components();
        Self::from_stream_parts(
            remote_advertised,
            remote_protocol,
            local_advertised,
            negotiated_protocol,
            stream,
        )
    }

    /// Reconstructs a [`BinaryHandshake`] from its components.
    ///
    /// The helper complements [`Self::into_components`] by allowing callers to temporarily extract
    /// the handshake metadata and replaying stream and later rebuild the wrapper without rerunning
    /// the negotiation. Debug builds assert that the supplied stream captured a binary negotiation
    /// so mismatched variants are detected early.
    #[must_use]
    pub fn from_components(
        remote_advertised: u32,
        remote_protocol: ProtocolVersion,
        local_advertised: ProtocolVersion,
        negotiated_protocol: ProtocolVersion,
        stream: NegotiatedStream<R>,
    ) -> Self {
        debug_assert_eq!(stream.decision(), NegotiationPrologue::Binary);

        Self {
            stream,
            remote_advertisement: RemoteProtocolAdvertisement::from_raw(
                remote_advertised,
                remote_protocol,
            ),
            local_advertised,
            negotiated_protocol,
        }
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
            remote_advertisement,
            negotiated_protocol,
            local_advertised,
        } = self;

        BinaryHandshake {
            stream: stream.map_inner(map),
            remote_advertisement,
            negotiated_protocol,
            local_advertised,
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
            remote_advertisement,
            negotiated_protocol,
            local_advertised,
        } = self;

        stream
            .try_map_inner(map)
            .map(|stream| BinaryHandshake {
                stream,
                remote_advertisement,
                negotiated_protocol,
                local_advertised,
            })
            .map_err(|err| {
                err.map_original(|stream| BinaryHandshake {
                    stream,
                    remote_advertisement,
                    negotiated_protocol,
                    local_advertised,
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
        ProtocolVersion,
        NegotiatedStreamParts<R>,
    ) {
        let Self {
            stream,
            remote_advertisement,
            negotiated_protocol,
            local_advertised,
        } = self;

        (
            remote_advertisement.advertised(),
            remote_advertisement.negotiated(),
            local_advertised,
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
        local_advertised: ProtocolVersion,
        negotiated_protocol: ProtocolVersion,
        parts: NegotiatedStreamParts<R>,
    ) -> Self {
        debug_assert_eq!(parts.decision(), NegotiationPrologue::Binary);

        Self {
            stream: parts.into_stream(),
            remote_advertisement: RemoteProtocolAdvertisement::from_raw(
                remote_advertised,
                remote_protocol,
            ),
            local_advertised,
            negotiated_protocol,
        }
    }
}
