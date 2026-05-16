use crate::handshake_util::{RemoteProtocolAdvertisement, local_cap_reduced_protocol};
use crate::negotiation::{NegotiatedStream, NegotiatedStreamParts, TryMapInnerError};
use protocol::{
    CompatibilityFlags, NegotiationPrologue, NegotiationPrologueSniffer, ProtocolVersion,
};
use std::collections::TryReserveError;

use super::BinaryHandshakeParts;

/// Result of completing the binary rsync protocol negotiation.
///
/// Targets transports that use the binary handshake (e.g. remote-shell
/// sessions). Exposes the negotiated protocol version and the remote peer's
/// advertisement while retaining the replaying stream so higher layers can
/// continue the exchange without losing buffered bytes.
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
    /// Captured here so diagnostics can reference both sides of the negotiation
    /// (subject to `--protocol` caps).
    #[doc(alias = "--protocol")]
    #[must_use]
    pub const fn local_advertised_protocol(&self) -> ProtocolVersion {
        self.local_advertised
    }

    /// Returns the compatibility flags advertised by the remote peer.
    ///
    /// Exchanged after protocol negotiation when both sides speak the binary
    /// handshake (protocol 30+). Upstream rsync propagates future bits even
    /// when their semantics are unknown; use [`CompatibilityFlags::has_unknown_bits`]
    /// to detect that condition.
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
    /// Upstream rsync clamps the negotiated protocol to the minimum of the
    /// peer's advertisement and the caller's `--protocol` cap. Returns `true`
    /// when the cap forced a downgrade.
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
    /// Invokes [`NegotiationPrologueSniffer::rehydrate_from_parts`] without
    /// unpacking the parts structure or replaying the underlying transport.
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
    /// Complements [`Self::into_components`]. Debug builds assert that the
    /// supplied stream captured a binary negotiation.
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
    /// Forwards to [`NegotiatedStream::map_inner`]; the replay buffer captured
    /// during negotiation is retained.
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
    /// On failure the closure returns `(error, original_reader)`, mirroring
    /// [`NegotiatedStream::try_map_inner`].
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
    /// Hands back a [`NegotiatedStreamParts`] so callers can inspect or
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
    /// Accepts values returned by [`Self::into_stream_parts`]. Debug builds
    /// assert the negotiation decision so binary and legacy parts cannot mix.
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

    /// Builds a binary handshake fixture at protocol 31. The first byte differs
    /// from `'@'` so the sniffer chooses the binary prologue, and the remaining
    /// bytes encode protocol 31 as a little-endian u32.
    fn create_test_handshake() -> BinaryHandshake<Cursor<Vec<u8>>> {
        let stream = sniff_negotiation_stream(Cursor::new(vec![0x1f, 0x00, 0x00, 0x00]))
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
        let stream = sniff_negotiation_stream(Cursor::new(vec![0x1f, 0x00, 0x00, 0x00]))
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

    #[test]
    fn from_components_reconstructs_handshake() {
        let stream = sniff_negotiation_stream(Cursor::new(vec![0x1f, 0x00, 0x00, 0x00]))
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

    #[test]
    fn rehydrate_sniffer_succeeds() {
        let hs = create_test_handshake();
        let mut sniffer = NegotiationPrologueSniffer::new();
        let result = hs.rehydrate_sniffer(&mut sniffer);
        assert!(result.is_ok());
    }

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
