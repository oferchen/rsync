use crate::handshake_util::RemoteProtocolAdvertisement;
use crate::negotiation::NegotiatedStreamParts;
use protocol::{LegacyDaemonGreetingOwned, NegotiationPrologue, ProtocolVersion};

use super::SessionHandshakeParts;

impl<R> SessionHandshakeParts<R> {
    /// Returns the negotiation style associated with the extracted handshake.
    #[must_use]
    pub const fn decision(&self) -> NegotiationPrologue {
        match self {
            SessionHandshakeParts::Binary(_) => NegotiationPrologue::Binary,
            SessionHandshakeParts::Legacy(_) => NegotiationPrologue::LegacyAscii,
        }
    }

    /// Reports whether the extracted handshake originated from a binary negotiation.
    ///
    /// The helper mirrors [`crate::session::SessionHandshake::is_binary`], keeping the
    /// convenience available even after the handshake has been decomposed into
    /// its parts.
    #[must_use]
    pub const fn is_binary(&self) -> bool {
        matches!(self, SessionHandshakeParts::Binary(_))
    }

    /// Reports whether the extracted handshake originated from the legacy ASCII negotiation.
    ///
    /// This mirrors [`crate::session::SessionHandshake::is_legacy`] and returns `true` when the
    /// parts were produced from a legacy `@RSYNCD:` daemon exchange.
    #[must_use]
    pub const fn is_legacy(&self) -> bool {
        matches!(self, SessionHandshakeParts::Legacy(_))
    }

    /// Returns the negotiated protocol version retained by the parts structure.
    #[must_use]
    pub const fn negotiated_protocol(&self) -> ProtocolVersion {
        match self {
            SessionHandshakeParts::Binary(parts) => parts.negotiated_protocol(),
            SessionHandshakeParts::Legacy(parts) => parts.negotiated_protocol(),
        }
    }

    /// Returns the protocol advertised by the remote peer.
    #[must_use]
    pub const fn remote_protocol(&self) -> ProtocolVersion {
        match self {
            SessionHandshakeParts::Binary(parts) => parts.remote_protocol(),
            SessionHandshakeParts::Legacy(parts) => parts.server_protocol(),
        }
    }

    /// Returns the raw protocol number advertised by the remote peer.
    #[must_use]
    pub const fn remote_advertised_protocol(&self) -> u32 {
        match self {
            SessionHandshakeParts::Binary(parts) => parts.remote_advertised_protocol(),
            SessionHandshakeParts::Legacy(parts) => parts.remote_advertised_protocol(),
        }
    }

    /// Returns the protocol version advertised by the local peer before the negotiation settled.
    #[doc(alias = "--protocol")]
    #[must_use]
    pub const fn local_advertised_protocol(&self) -> ProtocolVersion {
        match self {
            SessionHandshakeParts::Binary(parts) => parts.local_advertised_protocol(),
            SessionHandshakeParts::Legacy(parts) => parts.local_advertised_protocol(),
        }
    }

    /// Returns the classification of the peer's protocol advertisement.
    #[must_use]
    pub const fn remote_advertisement(&self) -> RemoteProtocolAdvertisement {
        match self {
            SessionHandshakeParts::Binary(parts) => parts.remote_advertisement(),
            SessionHandshakeParts::Legacy(parts) => parts.remote_advertisement(),
        }
    }

    /// Returns the legacy daemon greeting advertised by the server when available.
    #[must_use]
    pub const fn server_greeting(&self) -> Option<&LegacyDaemonGreetingOwned> {
        match self {
            SessionHandshakeParts::Binary(_) => None,
            SessionHandshakeParts::Legacy(parts) => Some(parts.server_greeting()),
        }
    }

    /// Returns a shared reference to the replaying stream parts.
    #[must_use]
    pub const fn stream(&self) -> &NegotiatedStreamParts<R> {
        match self {
            SessionHandshakeParts::Binary(parts) => parts.stream_parts(),
            SessionHandshakeParts::Legacy(parts) => parts.stream_parts(),
        }
    }

    /// Returns a mutable reference to the replaying stream parts.
    #[must_use]
    pub const fn stream_mut(&mut self) -> &mut NegotiatedStreamParts<R> {
        match self {
            SessionHandshakeParts::Binary(parts) => parts.stream_parts_mut(),
            SessionHandshakeParts::Legacy(parts) => parts.stream_parts_mut(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sniff_negotiation_stream;
    use protocol::CompatibilityFlags;
    use std::io::Cursor;

    // Helper to create binary handshake parts
    fn create_binary_parts() -> SessionHandshakeParts<Cursor<Vec<u8>>> {
        let stream = sniff_negotiation_stream(Cursor::new(vec![0x00, 0x00, 0x00, 0x1f]))
            .expect("sniff succeeds");
        let proto31 = ProtocolVersion::from_supported(31).unwrap();
        SessionHandshakeParts::from_binary_components(
            31,
            proto31,
            proto31,
            proto31,
            CompatibilityFlags::EMPTY,
            stream.into_parts(),
        )
    }

    // Helper to create legacy handshake parts
    fn create_legacy_parts() -> SessionHandshakeParts<Cursor<Vec<u8>>> {
        let stream = sniff_negotiation_stream(Cursor::new(b"@RSYNCD: 31.0\n".to_vec()))
            .expect("sniff succeeds");
        let greeting =
            LegacyDaemonGreetingOwned::from_parts(31, Some(0), None).expect("valid greeting");
        let proto31 = ProtocolVersion::from_supported(31).unwrap();
        SessionHandshakeParts::from_legacy_components(greeting, proto31, stream.into_parts())
    }

    // ==== decision tests ====

    #[test]
    fn decision_returns_binary_for_binary_variant() {
        let parts = create_binary_parts();
        assert_eq!(parts.decision(), NegotiationPrologue::Binary);
    }

    #[test]
    fn decision_returns_legacy_ascii_for_legacy_variant() {
        let parts = create_legacy_parts();
        assert_eq!(parts.decision(), NegotiationPrologue::LegacyAscii);
    }

    // ==== is_binary tests ====

    #[test]
    fn is_binary_true_for_binary_variant() {
        let parts = create_binary_parts();
        assert!(parts.is_binary());
    }

    #[test]
    fn is_binary_false_for_legacy_variant() {
        let parts = create_legacy_parts();
        assert!(!parts.is_binary());
    }

    // ==== is_legacy tests ====

    #[test]
    fn is_legacy_true_for_legacy_variant() {
        let parts = create_legacy_parts();
        assert!(parts.is_legacy());
    }

    #[test]
    fn is_legacy_false_for_binary_variant() {
        let parts = create_binary_parts();
        assert!(!parts.is_legacy());
    }

    // ==== negotiated_protocol tests ====

    #[test]
    fn negotiated_protocol_returns_correct_value_for_binary() {
        let parts = create_binary_parts();
        assert_eq!(parts.negotiated_protocol().as_u8(), 31);
    }

    #[test]
    fn negotiated_protocol_returns_correct_value_for_legacy() {
        let parts = create_legacy_parts();
        assert_eq!(parts.negotiated_protocol().as_u8(), 31);
    }

    #[test]
    fn negotiated_protocol_respects_clamping() {
        let stream = sniff_negotiation_stream(Cursor::new(vec![0x00, 0x00, 0x00, 0x1f]))
            .expect("sniff succeeds");
        let proto30 = ProtocolVersion::from_supported(30).unwrap();
        let proto31 = ProtocolVersion::from_supported(31).unwrap();
        let parts = SessionHandshakeParts::from_binary_components(
            31,
            proto31,
            proto31,
            proto30, // negotiated at 30
            CompatibilityFlags::EMPTY,
            stream.into_parts(),
        );
        assert_eq!(parts.negotiated_protocol().as_u8(), 30);
    }

    // ==== remote_protocol tests ====

    #[test]
    fn remote_protocol_returns_clamped_value_for_binary() {
        let parts = create_binary_parts();
        assert_eq!(parts.remote_protocol().as_u8(), 31);
    }

    #[test]
    fn remote_protocol_returns_server_protocol_for_legacy() {
        let parts = create_legacy_parts();
        assert_eq!(parts.remote_protocol().as_u8(), 31);
    }

    // ==== remote_advertised_protocol tests ====

    #[test]
    fn remote_advertised_protocol_returns_raw_value_for_binary() {
        let stream = sniff_negotiation_stream(Cursor::new(vec![0x00, 0x00, 0x00, 0x1f]))
            .expect("sniff succeeds");
        let proto31 = ProtocolVersion::from_supported(31).unwrap();
        let parts = SessionHandshakeParts::from_binary_components(
            999, // raw unsupported value
            proto31,
            proto31,
            proto31,
            CompatibilityFlags::EMPTY,
            stream.into_parts(),
        );
        assert_eq!(parts.remote_advertised_protocol(), 999);
    }

    #[test]
    fn remote_advertised_protocol_returns_greeting_protocol_for_legacy() {
        let parts = create_legacy_parts();
        assert_eq!(parts.remote_advertised_protocol(), 31);
    }

    // ==== local_advertised_protocol tests ====

    #[test]
    fn local_advertised_protocol_returns_value_for_binary() {
        let stream = sniff_negotiation_stream(Cursor::new(vec![0x00, 0x00, 0x00, 0x1f]))
            .expect("sniff succeeds");
        let proto30 = ProtocolVersion::from_supported(30).unwrap();
        let proto31 = ProtocolVersion::from_supported(31).unwrap();
        let parts = SessionHandshakeParts::from_binary_components(
            31,
            proto31,
            proto30, // local advertised at 30
            proto30,
            CompatibilityFlags::EMPTY,
            stream.into_parts(),
        );
        assert_eq!(parts.local_advertised_protocol().as_u8(), 30);
    }

    #[test]
    fn local_advertised_protocol_returns_value_for_legacy() {
        let parts = create_legacy_parts();
        // Legacy uses negotiated protocol as local advertised
        assert_eq!(parts.local_advertised_protocol().as_u8(), 31);
    }

    // ==== remote_advertisement tests ====

    #[test]
    fn remote_advertisement_returns_classification_for_binary() {
        let parts = create_binary_parts();
        let adv = parts.remote_advertisement();
        // Protocol 31 is supported, so clamped() returns None
        assert!(adv.supported().is_some());
        assert_eq!(adv.supported().unwrap().as_u8(), 31);
    }

    #[test]
    fn remote_advertisement_returns_classification_for_legacy() {
        let parts = create_legacy_parts();
        let adv = parts.remote_advertisement();
        // Protocol 31 is supported, so clamped() returns None
        assert!(adv.supported().is_some());
        assert_eq!(adv.supported().unwrap().as_u8(), 31);
    }

    // ==== server_greeting tests ====

    #[test]
    fn server_greeting_none_for_binary() {
        let parts = create_binary_parts();
        assert!(parts.server_greeting().is_none());
    }

    #[test]
    fn server_greeting_some_for_legacy() {
        let parts = create_legacy_parts();
        let greeting = parts.server_greeting().expect("legacy has greeting");
        assert_eq!(greeting.advertised_protocol(), 31);
    }

    #[test]
    fn server_greeting_includes_subprotocol() {
        let parts = create_legacy_parts();
        let greeting = parts.server_greeting().expect("legacy has greeting");
        // Subprotocol is a u32 (0 in our test case)
        assert_eq!(greeting.subprotocol(), 0);
    }

    // ==== stream tests ====

    #[test]
    fn stream_returns_reference_for_binary() {
        let parts = create_binary_parts();
        let stream = parts.stream();
        assert!(!stream.buffered().is_empty());
    }

    #[test]
    fn stream_returns_reference_for_legacy() {
        let parts = create_legacy_parts();
        let stream = parts.stream();
        assert!(stream.buffered().starts_with(b"@RSYNCD:"));
    }

    #[test]
    fn stream_decision_matches_parts_decision() {
        let binary = create_binary_parts();
        assert_eq!(binary.stream().decision(), binary.decision());

        let legacy = create_legacy_parts();
        assert_eq!(legacy.stream().decision(), legacy.decision());
    }

    // ==== stream_mut tests ====

    #[test]
    fn stream_mut_returns_mutable_reference_for_binary() {
        let mut parts = create_binary_parts();
        let stream = parts.stream_mut();
        assert!(!stream.buffered().is_empty());
    }

    #[test]
    fn stream_mut_returns_mutable_reference_for_legacy() {
        let mut parts = create_legacy_parts();
        let stream = parts.stream_mut();
        assert!(stream.buffered().starts_with(b"@RSYNCD:"));
    }
}
