mod access;
mod copy;
mod transform;

use rsync_protocol::NegotiationPrologue;

use super::super::{NegotiationBuffer, NegotiationBufferAccess};

/// Components extracted from a [`crate::negotiation::NegotiatedStream`].
///
/// # Examples
///
/// Decompose the replaying stream into its constituent pieces and resume consumption once any
/// inspection or wrapping is complete.
///
/// ```
/// use rsync_protocol::NegotiationPrologue;
/// use rsync_transport::sniff_negotiation_stream;
/// use std::io::{Cursor, Read};
///
/// let stream = sniff_negotiation_stream(Cursor::new(b"@RSYNCD: 31.0\nreply".to_vec()))
///     .expect("sniff succeeds");
/// let parts = stream.into_parts();
/// assert_eq!(parts.decision(), NegotiationPrologue::LegacyAscii);
///
/// let mut rebuilt = parts.into_stream();
/// let mut replay = Vec::new();
/// rebuilt
///     .read_to_end(&mut replay)
///     .expect("replayed bytes remain available");
/// assert_eq!(replay, b"@RSYNCD: 31.0\nreply");
/// ```
#[derive(Clone, Debug)]
pub struct NegotiatedStreamParts<R> {
    decision: NegotiationPrologue,
    buffer: NegotiationBuffer,
    inner: R,
}

impl<R> NegotiatedStreamParts<R> {
    pub(crate) fn new(decision: NegotiationPrologue, buffer: NegotiationBuffer, inner: R) -> Self {
        Self {
            decision,
            buffer,
            inner,
        }
    }

    pub(crate) fn into_components(self) -> (NegotiationPrologue, NegotiationBuffer, R) {
        let Self {
            decision,
            buffer,
            inner,
        } = self;
        (decision, buffer, inner)
    }
}

impl<R> NegotiationBufferAccess for NegotiatedStreamParts<R> {
    #[inline]
    fn buffer_ref(&self) -> &NegotiationBuffer {
        &self.buffer
    }
}

impl<R> NegotiatedStreamParts<R> {
    /// Returns the negotiation style that was detected.
    #[must_use]
    pub const fn decision(&self) -> NegotiationPrologue {
        self.decision
    }

    /// Reports whether the decomposed stream originated from a binary negotiation.
    ///
    /// This mirrors [`crate::negotiation::NegotiatedStream::is_binary`], allowing callers that work
    /// with [`NegotiatedStreamParts`] to branch on the handshake style without
    /// reconstructing the wrapper or inspecting [`Self::decision`] manually.
    #[must_use]
    pub const fn is_binary(&self) -> bool {
        self.decision.is_binary()
    }

    /// Reports whether the decomposed stream originated from the legacy ASCII negotiation.
    ///
    /// The helper mirrors [`crate::negotiation::NegotiatedStream::is_legacy`], exposing the same
    /// convenience for code that operates on [`NegotiatedStreamParts`]. It
    /// returns `true` when the captured negotiation began with the canonical
    /// `@RSYNCD:` prefix.
    #[must_use]
    pub const fn is_legacy(&self) -> bool {
        self.decision.is_legacy()
    }
}
