//! Incremental detector for the negotiation prologue style.
//!
//! Upstream rsync classifies the binary-vs-legacy ASCII decision from the very
//! first byte read from the transport. Real transports often deliver data in
//! small bursts, so the detector accumulates bytes across multiple calls until
//! a definitive classification is available.

mod buffer;
mod observe;
mod query;

#[cfg(test)]
mod tests;

use crate::legacy::LEGACY_DAEMON_PREFIX_LEN;

use super::NegotiationPrologue;

/// Incremental detector for the negotiation prologue style.
///
/// The binary vs. legacy ASCII decision in upstream rsync is based on the very
/// first byte read from the transport. However, real transports often deliver
/// data in small bursts, meaning the caller may need to feed multiple chunks
/// before a definitive answer is available. This helper maintains a small
/// amount of state so that `detect_negotiation_prologue` parity can be achieved
/// without repeatedly re-buffering the prefix.
#[derive(Clone, Debug)]
pub struct NegotiationPrologueDetector {
    buffer: [u8; LEGACY_DAEMON_PREFIX_LEN],
    len: usize,
    decided: Option<NegotiationPrologue>,
    prefix_complete: bool,
}

impl NegotiationPrologueDetector {
    /// Creates a fresh detector that has not yet observed any bytes.
    ///
    /// Available as `const` so compile-time contexts can instantiate detectors
    /// without going through trait dispatch.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            buffer: [0; LEGACY_DAEMON_PREFIX_LEN],
            len: 0,
            decided: None,
            prefix_complete: false,
        }
    }

    /// Caches a decision and returns it.
    const fn decide(&mut self, decision: NegotiationPrologue) -> NegotiationPrologue {
        self.decided = Some(decision);
        decision
    }
}

impl Default for NegotiationPrologueDetector {
    /// Creates a detector that has not yet observed any bytes.
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}
