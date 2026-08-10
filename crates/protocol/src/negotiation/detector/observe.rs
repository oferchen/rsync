//! Core observation logic for feeding transport bytes into the detector.

use ::core::slice;

use crate::legacy::{LEGACY_DAEMON_PREFIX_BYTES, LEGACY_DAEMON_PREFIX_LEN};

use super::NegotiationPrologueDetector;
use crate::negotiation::NegotiationPrologue;

impl NegotiationPrologueDetector {
    /// Observes the next chunk of bytes from the transport and reports the
    /// negotiation style chosen so far.
    ///
    /// Once the detector has observed enough bytes to classify the exchange, the
    /// decision is cached. Further calls that provide additional data replay the
    /// cached classification without re-reading earlier input. When the legacy
    /// ASCII path has been selected but more prefix bytes are still required and
    /// the caller supplies an empty chunk, the method returns
    /// [`NegotiationPrologue::NeedMoreData`] to signal that more I/O is
    /// necessary; the cached decision remains available via
    /// [`NegotiationPrologueDetector::decision`] and
    /// [`NegotiationPrologueDetector::is_legacy`].
    #[must_use]
    pub fn observe(&mut self, chunk: &[u8]) -> NegotiationPrologue {
        let needs_more_prefix_bytes = matches!(
            self.decided,
            Some(NegotiationPrologue::LegacyAscii) if !self.prefix_complete
        );

        if let Some(decided) = self.decided.filter(|_| !needs_more_prefix_bytes) {
            return decided;
        }

        if chunk.is_empty() {
            return if needs_more_prefix_bytes {
                NegotiationPrologue::NeedMoreData
            } else {
                self.decided.unwrap_or(NegotiationPrologue::NeedMoreData)
            };
        }

        let prefix = LEGACY_DAEMON_PREFIX_BYTES.as_slice();
        let mut decision = None;

        for &byte in chunk {
            if self.len == 0 {
                // upstream: clientserver.c - first byte determines negotiation style
                let classification = NegotiationPrologue::from_initial_byte(byte);

                if classification == NegotiationPrologue::Binary {
                    return self.decide(classification);
                }

                self.buffer[0] = byte;
                self.len = 1;
                decision = Some(self.decide(classification));
                continue;
            }

            if self.len < LEGACY_DAEMON_PREFIX_LEN {
                let expected = prefix[self.len];
                self.buffer[self.len] = byte;
                self.len += 1;

                if byte != expected {
                    self.prefix_complete = true;
                    decision = Some(self.decide(NegotiationPrologue::LegacyAscii));
                    break;
                }

                if self.len == LEGACY_DAEMON_PREFIX_LEN {
                    self.prefix_complete = true;
                    decision = Some(self.decide(NegotiationPrologue::LegacyAscii));
                    break;
                }
                continue;
            }

            self.prefix_complete = true;
            decision = Some(self.decide(NegotiationPrologue::LegacyAscii));
            break;
        }

        if let Some(decision) = decision {
            return decision;
        }

        self.decided.unwrap_or(NegotiationPrologue::NeedMoreData)
    }

    /// Observes a single byte from the transport and updates the negotiation state.
    ///
    /// Upstream rsync often peeks at one octet at a time while deciding whether the
    /// peer is speaking the legacy ASCII or binary handshake. This convenience
    /// wrapper keeps that call pattern expressive without forcing callers to
    /// allocate temporary one-byte slices.
    #[must_use]
    #[inline]
    pub fn observe_byte(&mut self, byte: u8) -> NegotiationPrologue {
        self.observe(slice::from_ref(&byte))
    }
}
