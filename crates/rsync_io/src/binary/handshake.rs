use crate::handshake_util::{RemoteProtocolAdvertisement, local_cap_reduced_protocol};
use crate::negotiation::{NegotiatedStream, NegotiatedStreamParts, TryMapInnerError};
use protocol::{
    CompatibilityFlags, NegotiationPrologue, NegotiationPrologueSniffer, ProtocolVersion,
};
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
    remote_compatibility_flags: CompatibilityFlags,
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

    /// Returns the compatibility flags advertised by the remote peer.
    ///
    /// Compatibility flags are exchanged after the protocol negotiation when
    /// both sides speak the binary handshake (protocol 30 or newer). They
    /// describe optional behaviours supported by the sender. Upstream rsync
    /// propagates future bits even when the local build does not understand
    /// their semantics; callers can use [`CompatibilityFlags::has_unknown_bits`]
    /// to detect that condition and surface downgraded diagnostics.
    #[must_use]
    pub const fn remote_compatibility_flags(&self) -> CompatibilityFlags {
        self.remote_compatibility_flags
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
    /// use rsync_io::negotiate_binary_session;
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
    pub const fn stream_mut(&mut self) -> &mut NegotiatedStream<R> {
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
        CompatibilityFlags,
        NegotiatedStream<R>,
    ) {
        (
            self.remote_advertisement.advertised(),
            self.remote_advertisement.negotiated(),
            self.local_advertised,
            self.negotiated_protocol,
            self.remote_compatibility_flags,
            self.stream,
        )
    }

    /// Decomposes the handshake into a [`BinaryHandshakeParts`] structure.
    #[must_use]
    pub fn into_parts(self) -> BinaryHandshakeParts<R> {
        let (
            remote_advertised,
            remote_protocol,
            local_advertised,
            negotiated_protocol,
            remote_compatibility_flags,
            stream,
        ) = self.into_stream_parts();
        let remote_advertisement =
            RemoteProtocolAdvertisement::from_raw(remote_advertised, remote_protocol);
        BinaryHandshakeParts::from_components(
            remote_advertisement,
            negotiated_protocol,
            local_advertised,
            remote_compatibility_flags,
            stream,
        )
    }

    /// Reconstructs a [`BinaryHandshake`] from previously extracted parts.
    #[must_use]
    pub fn from_parts(parts: BinaryHandshakeParts<R>) -> Self {
        let (
            remote_advertised,
            remote_protocol,
            local_advertised,
            negotiated_protocol,
            remote_compatibility_flags,
            stream,
        ) = parts.into_components();
        Self::from_stream_parts(
            remote_advertised,
            remote_protocol,
            local_advertised,
            negotiated_protocol,
            remote_compatibility_flags,
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
        remote_compatibility_flags: CompatibilityFlags,
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
            remote_compatibility_flags,
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
            remote_compatibility_flags,
        } = self;

        BinaryHandshake {
            stream: stream.map_inner(map),
            remote_advertisement,
            negotiated_protocol,
            local_advertised,
            remote_compatibility_flags,
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
            remote_compatibility_flags,
        } = self;

        stream
            .try_map_inner(map)
            .map(|stream| BinaryHandshake {
                stream,
                remote_advertisement,
                negotiated_protocol,
                local_advertised,
                remote_compatibility_flags,
            })
            .map_err(|err| {
                err.map_original(|stream| BinaryHandshake {
                    stream,
                    remote_advertisement,
                    negotiated_protocol,
                    local_advertised,
                    remote_compatibility_flags,
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
        CompatibilityFlags,
        NegotiatedStreamParts<R>,
    ) {
        let Self {
            stream,
            remote_advertisement,
            negotiated_protocol,
            local_advertised,
            remote_compatibility_flags,
        } = self;

        (
            remote_advertisement.advertised(),
            remote_advertisement.negotiated(),
            local_advertised,
            negotiated_protocol,
            remote_compatibility_flags,
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
        remote_compatibility_flags: CompatibilityFlags,
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
            remote_compatibility_flags,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sniff_negotiation_stream;
    use std::io::{self, Cursor};

    fn create_test_handshake() -> BinaryHandshake<Cursor<Vec<u8>>> {
        // Binary negotiation is triggered by first byte != '@'
        // Protocol 31 as BE u32: 0x00 0x00 0x00 0x1f
        let stream = sniff_negotiation_stream(Cursor::new(vec![0x00, 0x00, 0x00, 0x1f]))
            .expect("sniff succeeds");
        let proto31 = ProtocolVersion::from_supported(31).unwrap();
        BinaryHandshake::from_components(
            31,
            proto31,
            proto31,
            proto31,
            CompatibilityFlags::EMPTY,
            stream,
        )
    }

    // ==== Protocol accessors ====

    #[test]
    fn negotiated_protocol_returns_version() {
        let hs = create_test_handshake();
        assert_eq!(hs.negotiated_protocol().as_u8(), 31);
    }

    #[test]
    fn remote_protocol_returns_clamped_version() {
        let hs = create_test_handshake();
        assert_eq!(hs.remote_protocol().as_u8(), 31);
    }

    #[test]
    fn remote_advertised_protocol_returns_raw() {
        let hs = create_test_handshake();
        assert_eq!(hs.remote_advertised_protocol(), 31);
    }

    #[test]
    fn local_advertised_protocol_returns_version() {
        let hs = create_test_handshake();
        assert_eq!(hs.local_advertised_protocol().as_u8(), 31);
    }

    #[test]
    fn remote_compatibility_flags_empty() {
        let hs = create_test_handshake();
        assert_eq!(hs.remote_compatibility_flags(), CompatibilityFlags::EMPTY);
    }

    // ==== Protocol clamping ====

    #[test]
    fn remote_protocol_was_clamped_false_when_supported() {
        let hs = create_test_handshake();
        assert!(!hs.remote_protocol_was_clamped());
    }

    #[test]
    fn remote_advertisement_returns_classification() {
        let hs = create_test_handshake();
        let adv = hs.remote_advertisement();
        assert!(!adv.was_clamped());
        assert_eq!(adv.negotiated().as_u8(), 31);
    }

    #[test]
    fn local_protocol_was_capped_true_when_reduced() {
        let stream = sniff_negotiation_stream(Cursor::new(vec![0x00, 0x00, 0x00, 0x1f]))
            .expect("sniff succeeds");
        let proto31 = ProtocolVersion::from_supported(31).unwrap();
        let proto29 = ProtocolVersion::from_supported(29).unwrap();
        let hs = BinaryHandshake::from_components(
            31,
            proto31,
            proto29,
            proto29,
            CompatibilityFlags::EMPTY,
            stream,
        );
        assert!(hs.local_protocol_was_capped());
    }

    #[test]
    fn local_protocol_was_capped_false_when_not_reduced() {
        let hs = create_test_handshake();
        assert!(!hs.local_protocol_was_capped());
    }

    // ==== Stream accessors ====

    #[test]
    fn stream_returns_shared_reference() {
        let hs = create_test_handshake();
        let stream = hs.stream();
        assert_eq!(stream.decision(), NegotiationPrologue::Binary);
    }

    #[test]
    fn stream_mut_returns_mutable_reference() {
        let mut hs = create_test_handshake();
        let stream = hs.stream_mut();
        assert_eq!(stream.decision(), NegotiationPrologue::Binary);
    }

    #[test]
    fn into_stream_returns_owned_stream() {
        let hs = create_test_handshake();
        let stream = hs.into_stream();
        assert_eq!(stream.decision(), NegotiationPrologue::Binary);
    }

    // ==== Decomposition ====

    #[test]
    fn into_components_returns_all_parts() {
        let hs = create_test_handshake();
        let (remote_adv, remote_proto, local_adv, negotiated, flags, stream) = hs.into_components();
        assert_eq!(remote_adv, 31);
        assert_eq!(remote_proto.as_u8(), 31);
        assert_eq!(local_adv.as_u8(), 31);
        assert_eq!(negotiated.as_u8(), 31);
        assert_eq!(flags, CompatibilityFlags::EMPTY);
        assert_eq!(stream.decision(), NegotiationPrologue::Binary);
    }

    #[test]
    fn into_parts_returns_handshake_parts() {
        let hs = create_test_handshake();
        let parts = hs.into_parts();
        assert_eq!(parts.negotiated_protocol().as_u8(), 31);
    }

    #[test]
    fn into_stream_parts_returns_components() {
        let hs = create_test_handshake();
        let (remote_adv, remote_proto, local_adv, negotiated, flags, parts) =
            hs.into_stream_parts();
        assert_eq!(remote_adv, 31);
        assert_eq!(remote_proto.as_u8(), 31);
        assert_eq!(local_adv.as_u8(), 31);
        assert_eq!(negotiated.as_u8(), 31);
        assert_eq!(flags, CompatibilityFlags::EMPTY);
        assert_eq!(parts.decision(), NegotiationPrologue::Binary);
    }

    // ==== Reconstruction ====

    #[test]
    fn from_components_reconstructs_handshake() {
        let stream = sniff_negotiation_stream(Cursor::new(vec![0x00, 0x00, 0x00, 0x1f]))
            .expect("sniff succeeds");
        let proto31 = ProtocolVersion::from_supported(31).unwrap();
        let hs = BinaryHandshake::from_components(
            31,
            proto31,
            proto31,
            proto31,
            CompatibilityFlags::EMPTY,
            stream,
        );
        assert_eq!(hs.negotiated_protocol().as_u8(), 31);
    }

    #[test]
    fn from_parts_reconstructs_handshake() {
        let hs = create_test_handshake();
        let parts = hs.into_parts();
        let reconstructed = BinaryHandshake::from_parts(parts);
        assert_eq!(reconstructed.negotiated_protocol().as_u8(), 31);
    }

    #[test]
    fn from_stream_parts_reconstructs_handshake() {
        let hs = create_test_handshake();
        let (remote_adv, remote_proto, local_adv, negotiated, flags, parts) =
            hs.into_stream_parts();
        let reconstructed = BinaryHandshake::from_stream_parts(
            remote_adv,
            remote_proto,
            local_adv,
            negotiated,
            flags,
            parts,
        );
        assert_eq!(reconstructed.negotiated_protocol().as_u8(), 31);
    }

    // ==== Mapping ====

    #[test]
    fn map_stream_inner_transforms_transport() {
        let hs = create_test_handshake();
        let mapped = hs.map_stream_inner(|cursor| {
            let pos = cursor.position();
            let mut new_cursor = Cursor::new(cursor.into_inner());
            new_cursor.set_position(pos);
            new_cursor
        });
        assert_eq!(mapped.negotiated_protocol().as_u8(), 31);
    }

    #[test]
    fn try_map_stream_inner_succeeds() {
        let hs = create_test_handshake();
        let result = hs.try_map_stream_inner(|cursor| -> Result<_, (io::Error, _)> { Ok(cursor) });
        assert!(result.is_ok());
        let mapped = result.unwrap();
        assert_eq!(mapped.negotiated_protocol().as_u8(), 31);
    }

    #[test]
    fn try_map_stream_inner_fails_preserves_handshake() {
        let hs = create_test_handshake();
        let result = hs.try_map_stream_inner(|cursor| -> Result<Cursor<Vec<u8>>, _> {
            Err((io::Error::other("test error"), cursor))
        });
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.error().kind(), io::ErrorKind::Other);
        let recovered = err.into_original();
        assert_eq!(recovered.negotiated_protocol().as_u8(), 31);
    }

    // ==== Rehydrate sniffer ====

    #[test]
    fn rehydrate_sniffer_succeeds() {
        let hs = create_test_handshake();
        let mut sniffer = NegotiationPrologueSniffer::new();
        let result = hs.rehydrate_sniffer(&mut sniffer);
        assert!(result.is_ok());
    }

    // ==== Clone and Debug ====

    #[test]
    fn clone_produces_independent_copy() {
        let hs = create_test_handshake();
        let cloned = hs.clone();
        assert_eq!(hs.negotiated_protocol(), cloned.negotiated_protocol());
    }

    #[test]
    fn debug_format_contains_type_name() {
        let hs = create_test_handshake();
        let debug = format!("{hs:?}");
        assert!(debug.contains("BinaryHandshake"));
    }
}
