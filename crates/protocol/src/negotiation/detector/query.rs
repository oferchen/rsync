//! Query and predicate methods for inspecting the detector's classification.

use crate::legacy::LEGACY_DAEMON_PREFIX_LEN;

use super::NegotiationPrologueDetector;
use crate::negotiation::NegotiationPrologue;

impl NegotiationPrologueDetector {
    /// Reports the finalized negotiation style, if one has been established.
    ///
    /// Callers that feed data incrementally can use this accessor to check
    /// whether a definitive classification has already been produced without
    /// issuing another `observe` call. This mirrors upstream rsync's approach
    /// where the decision is sticky after the first non-`NeedMoreData`
    /// determination.
    pub const fn decision(&self) -> Option<NegotiationPrologue> {
        self.decided
    }

    /// Reports whether the negotiation style has been determined.
    ///
    /// Returns `false` until the first byte is observed, mirroring upstream
    /// rsync's behavior where the detection logic remains pending until the
    /// transport yields data.
    #[must_use]
    pub const fn is_decided(&self) -> bool {
        matches!(self.decided, Some(decision) if decision.is_decided())
    }

    /// Reports whether the detector has determined that the peer selected the
    /// legacy ASCII negotiation.
    ///
    /// The predicate remains `true` even when additional prefix bytes still
    /// need to be buffered, matching upstream rsync's behavior where the legacy
    /// decision is sticky once the initial `@` byte has been observed.
    #[must_use = "check whether the detector classified the exchange as legacy ASCII"]
    pub const fn is_legacy(&self) -> bool {
        matches!(self.decided, Some(decision) if decision.is_legacy())
    }

    /// Reports whether the detector has determined that the peer selected the
    /// binary negotiation path.
    ///
    /// Becomes `true` as soon as the first byte rules out the legacy ASCII
    /// negotiation, allowing call sites to react immediately without awaiting
    /// further I/O.
    #[must_use = "check whether the detector classified the exchange as binary"]
    pub const fn is_binary(&self) -> bool {
        matches!(self.decided, Some(decision) if decision.is_binary())
    }

    /// Reports whether additional bytes must be read before the negotiation
    /// prologue is fully understood.
    ///
    /// Binary negotiations require only the leading byte, so the helper flips
    /// to `false` immediately once the first non-`@` octet is observed. Legacy
    /// exchanges remain in the "needs more" state until the canonical `@RSYNCD:`
    /// prefix has been buffered. When no data has been observed yet the method
    /// also returns `true`, matching the semantics of
    /// [`NegotiationPrologue::requires_more_data`].
    #[must_use]
    pub const fn requires_more_data(&self) -> bool {
        match self.decided {
            Some(NegotiationPrologue::LegacyAscii) => !self.prefix_complete,
            Some(NegotiationPrologue::Binary) => false,
            Some(NegotiationPrologue::NeedMoreData) | None => true,
        }
    }

    /// Reports whether the canonical legacy prefix (`@RSYNCD:`) has been fully
    /// observed (or ruled out due to a mismatch) after classifying the stream
    /// as [`NegotiationPrologue::LegacyAscii`].
    ///
    /// Upstream rsync marks the prefix handling as complete once the canonical
    /// marker is buffered or a divergence is detected. Higher layers can use
    /// this to determine when it is safe to hand the accumulated bytes to the
    /// legacy greeting parser.
    #[must_use]
    pub const fn legacy_prefix_complete(&self) -> bool {
        matches!(self.decided, Some(NegotiationPrologue::LegacyAscii)) && self.prefix_complete
    }

    /// Reports how many additional bytes are required to capture the canonical
    /// legacy prefix when the detector has already classified the stream as
    /// [`NegotiationPrologue::LegacyAscii`].
    ///
    /// Returns `Some(n)` when `n` more bytes are needed to finish buffering the
    /// canonical prefix. Returns `None` once the prefix has been completed or
    /// when the detector decides the exchange is binary.
    pub const fn legacy_prefix_remaining(&self) -> Option<usize> {
        match (self.decided, self.prefix_complete) {
            (Some(NegotiationPrologue::LegacyAscii), false) => {
                Some(LEGACY_DAEMON_PREFIX_LEN - self.len)
            }
            _ => None,
        }
    }
}
