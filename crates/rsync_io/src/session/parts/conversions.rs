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
    // Conversion tests require creating BinaryHandshake and LegacyDaemonHandshake instances,
    // which have complex construction requirements. Basic type conversions are verified
    // through integration tests. Here we ensure the module compiles correctly with
    // its From/TryFrom implementations.

    #[test]
    fn module_compiles() {
        // This test ensures the module with its trait implementations compiles
        // The actual conversions are tested through higher-level integration tests
    }
}
