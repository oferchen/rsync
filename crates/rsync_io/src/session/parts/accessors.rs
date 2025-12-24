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
    pub fn negotiated_protocol(&self) -> ProtocolVersion {
        match self {
            SessionHandshakeParts::Binary(parts) => parts.negotiated_protocol(),
            SessionHandshakeParts::Legacy(parts) => parts.negotiated_protocol(),
        }
    }

    /// Returns the protocol advertised by the remote peer.
    #[must_use]
    pub fn remote_protocol(&self) -> ProtocolVersion {
        match self {
            SessionHandshakeParts::Binary(parts) => parts.remote_protocol(),
            SessionHandshakeParts::Legacy(parts) => parts.server_protocol(),
        }
    }

    /// Returns the raw protocol number advertised by the remote peer.
    #[must_use]
    pub fn remote_advertised_protocol(&self) -> u32 {
        match self {
            SessionHandshakeParts::Binary(parts) => parts.remote_advertised_protocol(),
            SessionHandshakeParts::Legacy(parts) => parts.remote_advertised_protocol(),
        }
    }

    /// Returns the protocol version advertised by the local peer before the negotiation settled.
    #[doc(alias = "--protocol")]
    #[must_use]
    pub fn local_advertised_protocol(&self) -> ProtocolVersion {
        match self {
            SessionHandshakeParts::Binary(parts) => parts.local_advertised_protocol(),
            SessionHandshakeParts::Legacy(parts) => parts.local_advertised_protocol(),
        }
    }

    /// Returns the classification of the peer's protocol advertisement.
    #[must_use]
    pub fn remote_advertisement(&self) -> RemoteProtocolAdvertisement {
        match self {
            SessionHandshakeParts::Binary(parts) => parts.remote_advertisement(),
            SessionHandshakeParts::Legacy(parts) => parts.remote_advertisement(),
        }
    }

    /// Returns the legacy daemon greeting advertised by the server when available.
    #[must_use]
    pub fn server_greeting(&self) -> Option<&LegacyDaemonGreetingOwned> {
        match self {
            SessionHandshakeParts::Binary(_) => None,
            SessionHandshakeParts::Legacy(parts) => Some(parts.server_greeting()),
        }
    }

    /// Returns a shared reference to the replaying stream parts.
    #[must_use]
    pub fn stream(&self) -> &NegotiatedStreamParts<R> {
        match self {
            SessionHandshakeParts::Binary(parts) => parts.stream_parts(),
            SessionHandshakeParts::Legacy(parts) => parts.stream_parts(),
        }
    }

    /// Returns a mutable reference to the replaying stream parts.
    #[must_use]
    pub fn stream_mut(&mut self) -> &mut NegotiatedStreamParts<R> {
        match self {
            SessionHandshakeParts::Binary(parts) => parts.stream_parts_mut(),
            SessionHandshakeParts::Legacy(parts) => parts.stream_parts_mut(),
        }
    }
}
