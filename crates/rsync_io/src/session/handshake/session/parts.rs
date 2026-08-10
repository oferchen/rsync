//! Parts decomposition and trait implementations for [`SessionHandshake`].

use ::core::convert::TryFrom;

use crate::binary::BinaryHandshake;
use crate::daemon::LegacyDaemonHandshake;

use super::super::super::parts::SessionHandshakeParts;
use super::SessionHandshake;

impl<R> SessionHandshake<R> {
    /// Decomposes the handshake into variant-specific metadata and replaying stream parts.
    ///
    /// The returned [`SessionHandshakeParts`] mirrors the helpers exposed by the variant-specific
    /// handshakes while allowing higher layers to stage the buffered negotiation bytes and
    /// negotiated metadata without matching on [`SessionHandshake`] immediately. This is useful
    /// when temporary ownership of the underlying transport is required (for example to wrap it
    /// with instrumentation) before resuming the rsync protocol exchange.
    #[must_use]
    pub fn into_parts(self) -> SessionHandshakeParts<R> {
        match self {
            SessionHandshake::Binary(handshake) => {
                SessionHandshakeParts::Binary(handshake.into_parts())
            }
            SessionHandshake::Legacy(handshake) => {
                SessionHandshakeParts::Legacy(handshake.into_parts())
            }
        }
    }

    /// Decomposes the handshake into variant-specific metadata and replaying stream parts.
    ///
    /// This is an alias for [`SessionHandshake::into_parts`] retained for historical parity with
    /// earlier drafts of the transport API where the method carried the `into_stream_parts` name.
    /// Keeping the shim avoids churn for downstream users while allowing the documentation and
    /// examples to reference the more succinct terminology shared by variant-specific handshakes.
    #[must_use]
    #[doc(alias = "into_parts")]
    pub fn into_stream_parts(self) -> SessionHandshakeParts<R> {
        self.into_parts()
    }

    /// Reassembles a [`SessionHandshake`] from the variant-specific parts previously extracted via
    /// [`Self::into_parts`].
    ///
    /// Callers can invoke this helper directly or rely on the [`From`] conversion implemented for
    /// [`SessionHandshakeParts`], which internally delegates to this constructor. The explicit
    /// method remains available for situations where type inference benefits from naming the
    /// conversion target.
    #[must_use]
    pub fn from_parts(parts: SessionHandshakeParts<R>) -> Self {
        match parts {
            SessionHandshakeParts::Binary(parts) => {
                SessionHandshake::Binary(BinaryHandshake::from_parts(parts))
            }
            SessionHandshakeParts::Legacy(parts) => {
                SessionHandshake::Legacy(LegacyDaemonHandshake::from_parts(parts))
            }
        }
    }

    /// Reassembles a [`SessionHandshake`] from the variant-specific stream parts previously
    /// extracted via [`Self::into_stream_parts`].
    ///
    /// This method aliases [`SessionHandshake::from_parts`]; it remains available for symmetry with
    /// older code that referred to the split representation as "stream parts". New call sites should
    /// prefer [`SessionHandshake::from_parts`] for consistency with the variant-specific helpers.
    #[must_use]
    #[doc(alias = "from_parts")]
    pub fn from_stream_parts(parts: SessionHandshakeParts<R>) -> Self {
        SessionHandshake::from_parts(parts)
    }
}

impl<R> From<SessionHandshakeParts<R>> for SessionHandshake<R> {
    fn from(parts: SessionHandshakeParts<R>) -> Self {
        SessionHandshake::from_parts(parts)
    }
}

impl<R> From<SessionHandshake<R>> for SessionHandshakeParts<R> {
    fn from(handshake: SessionHandshake<R>) -> Self {
        handshake.into_stream_parts()
    }
}

impl<R> From<BinaryHandshake<R>> for SessionHandshake<R> {
    fn from(handshake: BinaryHandshake<R>) -> Self {
        SessionHandshake::Binary(handshake)
    }
}

impl<R> From<LegacyDaemonHandshake<R>> for SessionHandshake<R> {
    fn from(handshake: LegacyDaemonHandshake<R>) -> Self {
        SessionHandshake::Legacy(handshake)
    }
}

impl<R> TryFrom<SessionHandshake<R>> for BinaryHandshake<R> {
    type Error = SessionHandshake<R>;

    fn try_from(handshake: SessionHandshake<R>) -> Result<Self, Self::Error> {
        handshake.into_binary()
    }
}

impl<R> TryFrom<SessionHandshake<R>> for LegacyDaemonHandshake<R> {
    type Error = SessionHandshake<R>;

    fn try_from(handshake: SessionHandshake<R>) -> Result<Self, Self::Error> {
        handshake.into_legacy()
    }
}
