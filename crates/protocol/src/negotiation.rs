use crate::legacy::{LEGACY_DAEMON_PREFIX, LEGACY_DAEMON_PREFIX_LEN};

/// Classification of the negotiation prologue received from a peer.
///
/// Upstream rsync distinguishes between two negotiation styles:
///
/// * Legacy ASCII greetings that begin with `@RSYNCD:`. These are produced by
///   peers that only understand protocols older than 30.
/// * Binary handshakes used by newer clients and daemons.
///
/// The detection helper mirrors upstream's lightweight peek: if the very first
/// byte equals `b'@'`, the stream is treated as a legacy greeting (subject to
/// later validation). Otherwise the exchange proceeds in binary mode. When the
/// caller has not yet accumulated enough bytes to decide, the helper reports
/// [`NegotiationPrologue::NeedMoreData`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NegotiationPrologue {
    /// There is not enough buffered data to determine the negotiation style.
    NeedMoreData,
    /// The peer is speaking the legacy ASCII `@RSYNCD:` protocol.
    LegacyAscii,
    /// The peer is speaking the modern binary negotiation protocol.
    Binary,
}

/// Determines whether the peer is performing the legacy ASCII negotiation or
/// the modern binary handshake.
///
/// The caller provides the initial bytes read from the transport without
/// consuming them. The helper follows upstream rsync's logic:
///
/// * If no data has been received yet, more bytes are required before a
///   decision can be made.
/// * If the first byte is `b'@'`, the peer is assumed to speak the legacy
///   protocol. Callers should then parse the banner via
///   [`parse_legacy_daemon_greeting_bytes`](crate::parse_legacy_daemon_greeting_bytes),
///   which will surface malformed input as
///   [`NegotiationError::MalformedLegacyGreeting`](crate::NegotiationError::MalformedLegacyGreeting).
/// * Otherwise, the exchange uses the binary negotiation.
#[must_use]
pub fn detect_negotiation_prologue(buffer: &[u8]) -> NegotiationPrologue {
    if buffer.is_empty() {
        return NegotiationPrologue::NeedMoreData;
    }

    if buffer[0] != b'@' {
        return NegotiationPrologue::Binary;
    }

    let prefix = LEGACY_DAEMON_PREFIX.as_bytes();
    if buffer.len() < prefix.len() && &prefix[..buffer.len()] == buffer {
        return NegotiationPrologue::NeedMoreData;
    }

    NegotiationPrologue::LegacyAscii
}

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
}

impl NegotiationPrologueDetector {
    /// Creates a fresh detector that has not yet observed any bytes.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            buffer: [0; LEGACY_DAEMON_PREFIX_LEN],
            len: 0,
            decided: None,
        }
    }

    /// Observes the next chunk of bytes from the transport and reports the
    /// negotiation style chosen so far.
    ///
    /// Once a non-`NeedMoreData` classification is returned, subsequent calls
    /// will keep producing the same value without inspecting further input.
    #[must_use]
    pub fn observe(&mut self, chunk: &[u8]) -> NegotiationPrologue {
        if let Some(decided) = self.decided {
            return decided;
        }

        if chunk.is_empty() {
            return NegotiationPrologue::NeedMoreData;
        }

        let prefix = LEGACY_DAEMON_PREFIX.as_bytes();

        for &byte in chunk {
            if self.len == 0 {
                if byte != b'@' {
                    return self.decide(NegotiationPrologue::Binary);
                }

                self.buffer[0] = byte;
                self.len = 1;
                continue;
            }

            if self.len >= LEGACY_DAEMON_PREFIX_LEN {
                return self.decide(NegotiationPrologue::LegacyAscii);
            }

            self.buffer[self.len] = byte;
            self.len += 1;

            if &self.buffer[..self.len] == &prefix[..self.len] {
                if self.len == LEGACY_DAEMON_PREFIX_LEN {
                    return self.decide(NegotiationPrologue::LegacyAscii);
                }
                continue;
            }

            return self.decide(NegotiationPrologue::LegacyAscii);
        }

        NegotiationPrologue::NeedMoreData
    }

    /// Reports the finalized negotiation style, if one has been established.
    ///
    /// Callers that feed data incrementally can use this accessor to check
    /// whether a definitive classification has already been produced without
    /// issuing another `observe` call. This mirrors upstream rsync's approach
    /// where the decision is sticky after the first non-`NeedMoreData`
    /// determination.
    #[must_use]
    pub const fn decision(&self) -> Option<NegotiationPrologue> {
        self.decided
    }

    fn decide(&mut self, decision: NegotiationPrologue) -> NegotiationPrologue {
        self.decided = Some(decision);
        decision
    }
}

impl Default for NegotiationPrologueDetector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_negotiation_prologue_requires_data() {
        assert_eq!(
            detect_negotiation_prologue(b""),
            NegotiationPrologue::NeedMoreData
        );
    }

    #[test]
    fn detect_negotiation_prologue_waits_for_full_prefix() {
        assert_eq!(
            detect_negotiation_prologue(b"@RS"),
            NegotiationPrologue::NeedMoreData
        );
    }

    #[test]
    fn detect_negotiation_prologue_flags_legacy_ascii() {
        assert_eq!(
            detect_negotiation_prologue(b"@RSYNCD: 31.0\n"),
            NegotiationPrologue::LegacyAscii
        );
    }

    #[test]
    fn detect_negotiation_prologue_flags_malformed_ascii_as_legacy() {
        assert_eq!(
            detect_negotiation_prologue(b"@RSYNCX"),
            NegotiationPrologue::LegacyAscii
        );
    }

    #[test]
    fn detect_negotiation_prologue_detects_binary() {
        assert_eq!(
            detect_negotiation_prologue(&[0x00, 0x20, 0x00, 0x00]),
            NegotiationPrologue::Binary
        );
    }

    #[test]
    fn prologue_detector_requires_data() {
        let mut detector = NegotiationPrologueDetector::new();
        assert_eq!(detector.observe(b""), NegotiationPrologue::NeedMoreData);
        assert_eq!(detector.observe(b"@"), NegotiationPrologue::NeedMoreData);
        assert_eq!(
            detector.observe(b"RSYNCD: 31.0\n"),
            NegotiationPrologue::LegacyAscii
        );
    }

    #[test]
    fn prologue_detector_detects_binary_immediately() {
        let mut detector = NegotiationPrologueDetector::default();
        assert_eq!(detector.observe(b"x"), NegotiationPrologue::Binary);
        assert_eq!(detector.observe(b"@"), NegotiationPrologue::Binary);
    }

    #[test]
    fn prologue_detector_handles_prefix_mismatch() {
        let mut detector = NegotiationPrologueDetector::new();
        assert_eq!(
            detector.observe(b"@RSYNCD"),
            NegotiationPrologue::NeedMoreData
        );
        assert_eq!(detector.observe(b"X"), NegotiationPrologue::LegacyAscii);
        assert_eq!(
            detector.observe(b"additional"),
            NegotiationPrologue::LegacyAscii
        );
    }

    #[test]
    fn prologue_detector_handles_split_prefix_chunks() {
        let mut detector = NegotiationPrologueDetector::new();
        assert_eq!(detector.observe(b"@RS"), NegotiationPrologue::NeedMoreData);
        assert_eq!(detector.observe(b"YN"), NegotiationPrologue::NeedMoreData);
        assert_eq!(
            detector.observe(b"CD: 32"),
            NegotiationPrologue::LegacyAscii
        );
    }

    #[test]
    fn prologue_detector_caches_decision() {
        let mut detector = NegotiationPrologueDetector::new();
        assert_eq!(detector.observe(b"@X"), NegotiationPrologue::LegacyAscii);
        assert_eq!(
            detector.observe(b"anything"),
            NegotiationPrologue::LegacyAscii
        );
    }

    #[test]
    fn prologue_detector_exposes_decision_state() {
        let mut detector = NegotiationPrologueDetector::new();
        assert_eq!(detector.decision(), None);
        assert_eq!(detector.observe(b""), NegotiationPrologue::NeedMoreData);
        assert_eq!(detector.decision(), None);

        assert_eq!(detector.observe(b"x"), NegotiationPrologue::Binary);
        assert_eq!(detector.decision(), Some(NegotiationPrologue::Binary));
    }
}
