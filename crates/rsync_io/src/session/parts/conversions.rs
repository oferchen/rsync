use crate::binary::{BinaryHandshake, BinaryHandshakeParts};
use crate::daemon::{LegacyDaemonHandshake, LegacyDaemonHandshakeParts};

use super::SessionHandshakeParts;

impl<R> From<BinaryHandshake<R>> for SessionHandshakeParts<R> {
    fn from(handshake: BinaryHandshake<R>) -> Self {
        SessionHandshakeParts::Binary(handshake.into_parts())
    }
}

impl<R> From<LegacyDaemonHandshake<R>> for SessionHandshakeParts<R> {
    fn from(handshake: LegacyDaemonHandshake<R>) -> Self {
        SessionHandshakeParts::Legacy(handshake.into_parts())
    }
}

impl<R> TryFrom<SessionHandshakeParts<R>> for BinaryHandshake<R> {
    type Error = SessionHandshakeParts<R>;

    fn try_from(parts: SessionHandshakeParts<R>) -> Result<Self, Self::Error> {
        match parts {
            SessionHandshakeParts::Binary(parts) => Ok(BinaryHandshake::from_parts(parts)),
            SessionHandshakeParts::Legacy(parts) => Err(SessionHandshakeParts::Legacy(parts)),
        }
    }
}

impl<R> TryFrom<SessionHandshakeParts<R>> for LegacyDaemonHandshake<R> {
    type Error = SessionHandshakeParts<R>;

    fn try_from(parts: SessionHandshakeParts<R>) -> Result<Self, Self::Error> {
        match parts {
            SessionHandshakeParts::Legacy(parts) => Ok(LegacyDaemonHandshake::from_parts(parts)),
            SessionHandshakeParts::Binary(parts) => Err(SessionHandshakeParts::Binary(parts)),
        }
    }
}

impl<R> TryFrom<SessionHandshakeParts<R>> for BinaryHandshakeParts<R> {
    type Error = SessionHandshakeParts<R>;

    fn try_from(parts: SessionHandshakeParts<R>) -> Result<Self, Self::Error> {
        parts.into_binary_parts()
    }
}

impl<R> TryFrom<SessionHandshakeParts<R>> for LegacyDaemonHandshakeParts<R> {
    type Error = SessionHandshakeParts<R>;

    fn try_from(parts: SessionHandshakeParts<R>) -> Result<Self, Self::Error> {
        parts.into_legacy_parts()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sniff_negotiation_stream;
    use protocol::{CompatibilityFlags, LegacyDaemonGreetingOwned, ProtocolVersion};
    use std::io::Cursor;

    // Helper to create a BinaryHandshake
    fn create_binary_handshake() -> BinaryHandshake<Cursor<Vec<u8>>> {
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

    // Helper to create a LegacyDaemonHandshake
    fn create_legacy_handshake() -> LegacyDaemonHandshake<Cursor<Vec<u8>>> {
        let stream = sniff_negotiation_stream(Cursor::new(b"@RSYNCD: 31.0\n".to_vec()))
            .expect("sniff succeeds");
        let greeting =
            LegacyDaemonGreetingOwned::from_parts(31, Some(0), None).expect("valid greeting");
        let proto31 = ProtocolVersion::from_supported(31).unwrap();
        LegacyDaemonHandshake::from_components(greeting, proto31, stream)
    }

    // Helper to create SessionHandshakeParts (binary)
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

    // Helper to create SessionHandshakeParts (legacy)
    fn create_legacy_parts() -> SessionHandshakeParts<Cursor<Vec<u8>>> {
        let stream = sniff_negotiation_stream(Cursor::new(b"@RSYNCD: 31.0\n".to_vec()))
            .expect("sniff succeeds");
        let greeting =
            LegacyDaemonGreetingOwned::from_parts(31, Some(0), None).expect("valid greeting");
        let proto31 = ProtocolVersion::from_supported(31).unwrap();
        SessionHandshakeParts::from_legacy_components(greeting, proto31, stream.into_parts())
    }

    // ==== From<BinaryHandshake> tests ====

    #[test]
    fn from_binary_handshake_creates_binary_parts() {
        let handshake = create_binary_handshake();
        let parts: SessionHandshakeParts<_> = handshake.into();
        assert!(matches!(parts, SessionHandshakeParts::Binary(_)));
    }

    #[test]
    fn from_binary_handshake_preserves_negotiated_protocol() {
        let handshake = create_binary_handshake();
        let negotiated = handshake.negotiated_protocol();
        let parts: SessionHandshakeParts<_> = handshake.into();
        assert_eq!(parts.negotiated_protocol(), negotiated);
    }

    // ==== From<LegacyDaemonHandshake> tests ====

    #[test]
    fn from_legacy_handshake_creates_legacy_parts() {
        let handshake = create_legacy_handshake();
        let parts: SessionHandshakeParts<_> = handshake.into();
        assert!(matches!(parts, SessionHandshakeParts::Legacy(_)));
    }

    #[test]
    fn from_legacy_handshake_preserves_negotiated_protocol() {
        let handshake = create_legacy_handshake();
        let negotiated = handshake.negotiated_protocol();
        let parts: SessionHandshakeParts<_> = handshake.into();
        assert_eq!(parts.negotiated_protocol(), negotiated);
    }

    // ==== TryFrom<SessionHandshakeParts> for BinaryHandshake tests ====

    #[test]
    fn try_from_parts_to_binary_handshake_succeeds_for_binary() {
        let parts = create_binary_parts();
        let result: Result<BinaryHandshake<_>, _> = parts.try_into();
        assert!(result.is_ok());
    }

    #[test]
    fn try_from_parts_to_binary_handshake_fails_for_legacy() {
        let parts = create_legacy_parts();
        let result: Result<BinaryHandshake<_>, _> = parts.try_into();
        assert!(result.is_err());
        assert!(result.unwrap_err().is_legacy());
    }

    #[test]
    fn try_from_parts_to_binary_handshake_preserves_protocol() {
        let parts = create_binary_parts();
        let negotiated = parts.negotiated_protocol();
        let handshake: BinaryHandshake<_> = parts.try_into().unwrap();
        assert_eq!(handshake.negotiated_protocol(), negotiated);
    }

    // ==== TryFrom<SessionHandshakeParts> for LegacyDaemonHandshake tests ====

    #[test]
    fn try_from_parts_to_legacy_handshake_succeeds_for_legacy() {
        let parts = create_legacy_parts();
        let result: Result<LegacyDaemonHandshake<_>, _> = parts.try_into();
        assert!(result.is_ok());
    }

    #[test]
    fn try_from_parts_to_legacy_handshake_fails_for_binary() {
        let parts = create_binary_parts();
        let result: Result<LegacyDaemonHandshake<_>, _> = parts.try_into();
        assert!(result.is_err());
        assert!(result.unwrap_err().is_binary());
    }

    #[test]
    fn try_from_parts_to_legacy_handshake_preserves_protocol() {
        let parts = create_legacy_parts();
        let negotiated = parts.negotiated_protocol();
        let handshake: LegacyDaemonHandshake<_> = parts.try_into().unwrap();
        assert_eq!(handshake.negotiated_protocol(), negotiated);
    }

    // ==== TryFrom<SessionHandshakeParts> for BinaryHandshakeParts tests ====

    #[test]
    fn try_from_parts_to_binary_parts_succeeds_for_binary() {
        let parts = create_binary_parts();
        let result: Result<BinaryHandshakeParts<_>, _> = parts.try_into();
        assert!(result.is_ok());
    }

    #[test]
    fn try_from_parts_to_binary_parts_fails_for_legacy() {
        let parts = create_legacy_parts();
        let result: Result<BinaryHandshakeParts<_>, _> = parts.try_into();
        assert!(result.is_err());
    }

    // ==== TryFrom<SessionHandshakeParts> for LegacyDaemonHandshakeParts tests ====

    #[test]
    fn try_from_parts_to_legacy_parts_succeeds_for_legacy() {
        let parts = create_legacy_parts();
        let result: Result<LegacyDaemonHandshakeParts<_>, _> = parts.try_into();
        assert!(result.is_ok());
    }

    #[test]
    fn try_from_parts_to_legacy_parts_fails_for_binary() {
        let parts = create_binary_parts();
        let result: Result<LegacyDaemonHandshakeParts<_>, _> = parts.try_into();
        assert!(result.is_err());
    }

    // ==== Round-trip tests ====

    #[test]
    fn binary_handshake_round_trip_preserves_data() {
        let original = create_binary_handshake();
        let negotiated = original.negotiated_protocol();
        let remote = original.remote_protocol();

        let parts: SessionHandshakeParts<_> = original.into();
        let recovered: BinaryHandshake<_> = parts.try_into().unwrap();

        assert_eq!(recovered.negotiated_protocol(), negotiated);
        assert_eq!(recovered.remote_protocol(), remote);
    }

    #[test]
    fn legacy_handshake_round_trip_preserves_data() {
        let original = create_legacy_handshake();
        let negotiated = original.negotiated_protocol();

        let parts: SessionHandshakeParts<_> = original.into();
        let recovered: LegacyDaemonHandshake<_> = parts.try_into().unwrap();

        assert_eq!(recovered.negotiated_protocol(), negotiated);
    }
}
