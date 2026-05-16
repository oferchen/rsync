use super::handshake::LegacyDaemonHandshake;
use crate::handshake_util::{RemoteProtocolAdvertisement, local_cap_reduced_protocol};
use crate::negotiation::{NegotiatedStreamParts, TryMapInnerError};
use protocol::{LegacyDaemonGreetingOwned, ProtocolVersion};

/// Decomposed components of a [`LegacyDaemonHandshake`].
///
/// Groups the parsed greeting, negotiated protocol, and replaying stream parts
/// so callers can temporarily take ownership of the components while
/// instrumenting the transport.
#[doc(alias = "@RSYNCD")]
#[derive(Clone, Debug)]
pub struct LegacyDaemonHandshakeParts<R> {
    server_greeting: LegacyDaemonGreetingOwned,
    negotiated_protocol: ProtocolVersion,
    stream: NegotiatedStreamParts<R>,
}

impl<R> LegacyDaemonHandshakeParts<R> {
    const fn new(
        server_greeting: LegacyDaemonGreetingOwned,
        negotiated_protocol: ProtocolVersion,
        stream: NegotiatedStreamParts<R>,
    ) -> Self {
        Self {
            server_greeting,
            negotiated_protocol,
            stream,
        }
    }

    /// Returns the parsed daemon greeting advertised by the server.
    #[must_use]
    pub const fn server_greeting(&self) -> &LegacyDaemonGreetingOwned {
        &self.server_greeting
    }

    /// Returns the server protocol after clamping future advertisements.
    #[must_use]
    pub const fn server_protocol(&self) -> ProtocolVersion {
        self.server_greeting.protocol()
    }

    /// Returns the raw protocol number advertised by the daemon.
    #[must_use]
    pub const fn remote_advertised_protocol(&self) -> u32 {
        self.server_greeting.advertised_protocol()
    }

    /// Returns the negotiated protocol after applying the caller's cap.
    #[must_use]
    pub const fn negotiated_protocol(&self) -> ProtocolVersion {
        self.negotiated_protocol
    }

    /// Returns the protocol version the client advertised to the daemon.
    ///
    /// Mirrors [`Self::negotiated_protocol`] (the legacy handshake echoes the
    /// final value); exposed explicitly to match the binary helper shape.
    #[doc(alias = "--protocol")]
    #[must_use]
    pub const fn local_advertised_protocol(&self) -> ProtocolVersion {
        self.negotiated_protocol
    }

    /// Reports whether the daemon advertised a protocol newer than the supported range.
    #[must_use]
    pub const fn remote_protocol_was_clamped(&self) -> bool {
        self.remote_advertisement().was_clamped()
    }

    /// Returns the classification of the daemon's protocol advertisement.
    #[must_use]
    pub const fn remote_advertisement(&self) -> RemoteProtocolAdvertisement {
        RemoteProtocolAdvertisement::from_raw(
            self.remote_advertised_protocol(),
            self.server_protocol(),
        )
    }

    /// Reports whether the negotiated protocol was reduced by the caller-provided cap.
    ///
    /// Mirrors `rsync --protocol=<version>`: when the caller requests an older
    /// protocol than the daemon advertised the session is forced to downgrade.
    #[doc(alias = "--protocol")]
    #[must_use]
    pub const fn local_protocol_was_capped(&self) -> bool {
        local_cap_reduced_protocol(self.server_protocol(), self.negotiated_protocol())
    }

    /// Returns the replaying stream parts captured during negotiation.
    #[must_use]
    pub const fn stream_parts(&self) -> &NegotiatedStreamParts<R> {
        &self.stream
    }

    /// Returns a mutable reference to the replaying stream parts.
    #[must_use]
    pub const fn stream_parts_mut(&mut self) -> &mut NegotiatedStreamParts<R> {
        &mut self.stream
    }

    /// Releases the structure and returns the replaying stream parts.
    #[must_use]
    pub fn into_stream_parts(self) -> NegotiatedStreamParts<R> {
        self.stream
    }

    /// Maps the inner transport while preserving the negotiated metadata and greeting.
    ///
    /// Counterpart to [`LegacyDaemonHandshake::map_stream_inner`] operating on
    /// the decomposed parts. The replay buffer and parsed greeting are retained.
    #[must_use]
    pub fn map_stream_inner<F, T>(self, map: F) -> LegacyDaemonHandshakeParts<T>
    where
        F: FnOnce(R) -> T,
    {
        let Self {
            server_greeting,
            negotiated_protocol,
            stream,
        } = self;

        LegacyDaemonHandshakeParts::from_components(
            server_greeting,
            negotiated_protocol,
            stream.map_inner(map),
        )
    }

    /// Attempts to transform the inner transport while preserving the negotiated metadata.
    ///
    /// On failure the original parts structure is returned alongside the error
    /// so callers can continue using the negotiated session.
    pub fn try_map_stream_inner<F, T, E>(
        self,
        map: F,
    ) -> Result<LegacyDaemonHandshakeParts<T>, TryMapInnerError<LegacyDaemonHandshakeParts<R>, E>>
    where
        F: FnOnce(R) -> Result<T, (E, R)>,
    {
        let Self {
            server_greeting,
            negotiated_protocol,
            stream,
        } = self;

        match (stream.try_map_inner(map), server_greeting) {
            (Ok(stream), greeting) => Ok(LegacyDaemonHandshakeParts::from_components(
                greeting,
                negotiated_protocol,
                stream,
            )),
            (Err(err), greeting) => Err(err.map_original(|stream| {
                LegacyDaemonHandshakeParts::from_components(greeting, negotiated_protocol, stream)
            })),
        }
    }

    /// Decomposes the parts structure into the parsed greeting, negotiated protocol, and
    /// replaying stream.
    ///
    /// Owned counterpart to [`LegacyDaemonHandshake::into_components`].
    #[must_use]
    pub fn into_components(
        self,
    ) -> (
        LegacyDaemonGreetingOwned,
        ProtocolVersion,
        NegotiatedStreamParts<R>,
    ) {
        (self.server_greeting, self.negotiated_protocol, self.stream)
    }

    pub(super) const fn from_components(
        server_greeting: LegacyDaemonGreetingOwned,
        negotiated_protocol: ProtocolVersion,
        stream: NegotiatedStreamParts<R>,
    ) -> Self {
        Self::new(server_greeting, negotiated_protocol, stream)
    }

    /// Rebuilds a [`LegacyDaemonHandshake`] from the preserved components.
    #[must_use]
    pub fn into_handshake(self) -> LegacyDaemonHandshake<R> {
        LegacyDaemonHandshake::from_parts(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sniff_negotiation_stream;
    use protocol::{LegacyDaemonGreetingOwned, NegotiationPrologue};
    use std::io::{self, Cursor};

    fn create_test_parts() -> LegacyDaemonHandshakeParts<Cursor<Vec<u8>>> {
        let greeting =
            LegacyDaemonGreetingOwned::from_parts(31, Some(0), None).expect("valid greeting");
        let proto = ProtocolVersion::from_supported(31).unwrap();
        let stream = sniff_negotiation_stream(Cursor::new(b"@RSYNCD: 31.0\n".to_vec()))
            .expect("sniff succeeds");
        LegacyDaemonHandshakeParts::from_components(greeting, proto, stream.into_parts())
    }

    #[test]
    fn server_greeting_returns_reference() {
        let parts = create_test_parts();
        let greeting = parts.server_greeting();
        assert_eq!(greeting.protocol().as_u8(), 31);
    }

    #[test]
    fn server_protocol_returns_clamped_version() {
        let parts = create_test_parts();
        assert_eq!(parts.server_protocol().as_u8(), 31);
    }

    #[test]
    fn remote_advertised_protocol_returns_raw() {
        let parts = create_test_parts();
        assert_eq!(parts.remote_advertised_protocol(), 31);
    }

    #[test]
    fn negotiated_protocol_returns_version() {
        let parts = create_test_parts();
        assert_eq!(parts.negotiated_protocol().as_u8(), 31);
    }

    #[test]
    fn local_advertised_protocol_mirrors_negotiated() {
        let parts = create_test_parts();
        assert_eq!(
            parts.local_advertised_protocol(),
            parts.negotiated_protocol()
        );
    }

    #[test]
    fn remote_protocol_was_clamped_false_when_supported() {
        let parts = create_test_parts();
        assert!(!parts.remote_protocol_was_clamped());
    }

    #[test]
    fn remote_advertisement_returns_classification() {
        let parts = create_test_parts();
        let adv = parts.remote_advertisement();
        assert!(!adv.was_clamped());
    }

    #[test]
    fn local_protocol_was_capped_true_when_reduced() {
        let greeting =
            LegacyDaemonGreetingOwned::from_parts(31, Some(0), None).expect("valid greeting");
        let proto = ProtocolVersion::from_supported(29).unwrap();
        let stream = sniff_negotiation_stream(Cursor::new(b"@RSYNCD: 31.0\n".to_vec()))
            .expect("sniff succeeds");
        let parts =
            LegacyDaemonHandshakeParts::from_components(greeting, proto, stream.into_parts());
        assert!(parts.local_protocol_was_capped());
    }

    #[test]
    fn local_protocol_was_capped_false_when_not_reduced() {
        let parts = create_test_parts();
        assert!(!parts.local_protocol_was_capped());
    }

    #[test]
    fn stream_parts_returns_reference() {
        let parts = create_test_parts();
        let stream = parts.stream_parts();
        assert_eq!(stream.decision(), NegotiationPrologue::LegacyAscii);
    }

    #[test]
    fn stream_parts_mut_returns_mutable_reference() {
        let mut parts = create_test_parts();
        let stream = parts.stream_parts_mut();
        assert_eq!(stream.decision(), NegotiationPrologue::LegacyAscii);
    }

    #[test]
    fn into_stream_parts_returns_owned() {
        let parts = create_test_parts();
        let stream = parts.into_stream_parts();
        assert_eq!(stream.decision(), NegotiationPrologue::LegacyAscii);
    }

    #[test]
    fn into_components_returns_all_parts() {
        let parts = create_test_parts();
        let (greeting, proto, stream) = parts.into_components();
        assert_eq!(greeting.protocol().as_u8(), 31);
        assert_eq!(proto.as_u8(), 31);
        assert_eq!(stream.decision(), NegotiationPrologue::LegacyAscii);
    }

    #[test]
    fn into_handshake_rebuilds_wrapper() {
        let parts = create_test_parts();
        let handshake = parts.into_handshake();
        assert_eq!(handshake.negotiated_protocol().as_u8(), 31);
    }

    #[test]
    fn roundtrip_through_handshake_preserves_state() {
        let parts = create_test_parts();
        let expected_proto = parts.negotiated_protocol();
        let handshake = parts.into_handshake();
        let restored_parts = handshake.into_parts();
        assert_eq!(restored_parts.negotiated_protocol(), expected_proto);
    }

    #[test]
    fn map_stream_inner_transforms_transport() {
        let parts = create_test_parts();
        let mapped = parts.map_stream_inner(|cursor| {
            let pos = cursor.position();
            let mut new_cursor = Cursor::new(cursor.into_inner());
            new_cursor.set_position(pos);
            new_cursor
        });
        assert_eq!(mapped.negotiated_protocol().as_u8(), 31);
    }

    #[test]
    fn try_map_stream_inner_succeeds() {
        let parts = create_test_parts();
        let result =
            parts.try_map_stream_inner(|cursor| -> Result<_, (io::Error, _)> { Ok(cursor) });
        assert!(result.is_ok());
        let mapped = result.unwrap();
        assert_eq!(mapped.negotiated_protocol().as_u8(), 31);
    }

    #[test]
    fn try_map_stream_inner_fails_preserves_parts() {
        let parts = create_test_parts();
        let result = parts.try_map_stream_inner(|cursor| -> Result<Cursor<Vec<u8>>, _> {
            Err((io::Error::other("test error"), cursor))
        });
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.error().kind(), io::ErrorKind::Other);
        let recovered = err.into_original();
        assert_eq!(recovered.negotiated_protocol().as_u8(), 31);
    }

    #[test]
    fn clone_produces_independent_copy() {
        let parts = create_test_parts();
        let cloned = parts.clone();
        assert_eq!(parts.negotiated_protocol(), cloned.negotiated_protocol());
    }

    #[test]
    fn debug_format_contains_type_name() {
        let parts = create_test_parts();
        let debug = format!("{parts:?}");
        assert!(debug.contains("LegacyDaemonHandshakeParts"));
    }
}
