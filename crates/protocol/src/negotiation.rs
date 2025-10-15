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
/// later validation). Otherwise the exchange proceeds in binary mode. When no
/// data has been observed yet, the helper reports
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
            let needs_more_prefix_bytes =
                decided == NegotiationPrologue::LegacyAscii && self.len < LEGACY_DAEMON_PREFIX_LEN;
            if !needs_more_prefix_bytes {
                return decided;
            }
        }

        if chunk.is_empty() {
            return self.decided.unwrap_or(NegotiationPrologue::NeedMoreData);
        }

        let prefix = LEGACY_DAEMON_PREFIX.as_bytes();
        let mut decision = None;

        for &byte in chunk {
            if self.len == 0 {
                if byte != b'@' {
                    return self.decide(NegotiationPrologue::Binary);
                }

                self.buffer[0] = byte;
                self.len = 1;
                decision = Some(self.decide(NegotiationPrologue::LegacyAscii));
                continue;
            }

            if self.len < LEGACY_DAEMON_PREFIX_LEN {
                self.buffer[self.len] = byte;
                self.len += 1;
            }

            if self.len >= LEGACY_DAEMON_PREFIX_LEN || self.buffer[..self.len] != prefix[..self.len]
            {
                decision = Some(self.decide(NegotiationPrologue::LegacyAscii));
                break;
            }
        }

        if let Some(decision) = decision {
            return decision;
        }

        self.decided.unwrap_or(NegotiationPrologue::NeedMoreData)
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

    /// Returns the prefix bytes buffered while deciding on the negotiation style.
    ///
    /// When the detector concludes that the peer is using the legacy ASCII
    /// greeting, the already consumed bytes must be included when parsing the
    /// full banner. Upstream rsync accomplishes this by reusing the peeked
    /// prefix. Callers of this Rust implementation can mirror that behavior by
    /// reading the buffered prefix through this accessor instead of re-reading
    /// from the underlying transport. The buffer continues to grow across
    /// subsequent [`observe`] calls until the canonical `@RSYNCD:` prefix has
    /// been captured or a mismatch forces the legacy classification. For binary
    /// negotiations, no bytes are retained and this method returns an empty
    /// slice.
    #[must_use]
    #[inline]
    pub fn buffered_prefix(&self) -> &[u8] {
        &self.buffer[..self.len]
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
    fn detect_negotiation_prologue_classifies_partial_prefix_as_legacy() {
        assert_eq!(
            detect_negotiation_prologue(b"@RS"),
            NegotiationPrologue::LegacyAscii
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
        assert_eq!(detector.observe(b"@"), NegotiationPrologue::LegacyAscii);
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
            NegotiationPrologue::LegacyAscii
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
        assert_eq!(detector.observe(b"@RS"), NegotiationPrologue::LegacyAscii);
        assert_eq!(detector.observe(b"YN"), NegotiationPrologue::LegacyAscii);
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

    #[test]
    fn prologue_detector_exposes_legacy_decision_state() {
        let mut detector = NegotiationPrologueDetector::new();
        assert_eq!(detector.decision(), None);

        assert_eq!(detector.observe(b"@"), NegotiationPrologue::LegacyAscii);
        assert_eq!(detector.decision(), Some(NegotiationPrologue::LegacyAscii));

        // Additional observations keep reporting the cached decision, matching
        // upstream's handling once the legacy path has been chosen.
        assert_eq!(detector.observe(b"RSYN"), NegotiationPrologue::LegacyAscii);
        assert_eq!(detector.decision(), Some(NegotiationPrologue::LegacyAscii));
    }

    #[test]
    fn buffered_prefix_tracks_bytes_consumed_for_legacy_detection() {
        let mut detector = NegotiationPrologueDetector::new();
        assert_eq!(detector.buffered_prefix(), b"");

        assert_eq!(detector.observe(b"@RS"), NegotiationPrologue::LegacyAscii);
        assert_eq!(detector.buffered_prefix(), b"@RS");

        // Additional observations extend the buffered prefix until the full
        // legacy marker is buffered.
        assert_eq!(detector.observe(b"YNCD"), NegotiationPrologue::LegacyAscii);
        assert_eq!(detector.buffered_prefix(), b"@RSYNCD");

        // Feeding an empty chunk after the decision simply replays the cached
        // classification and leaves the buffered prefix intact.
        assert_eq!(detector.observe(b""), NegotiationPrologue::LegacyAscii);
        assert_eq!(detector.buffered_prefix(), b"@RSYNCD");
    }

    #[test]
    fn buffered_prefix_captures_full_marker_when_present_in_single_chunk() {
        let mut detector = NegotiationPrologueDetector::new();
        assert_eq!(
            detector.observe(b"@RSYNCD: 31.0\n"),
            NegotiationPrologue::LegacyAscii
        );
        assert_eq!(detector.buffered_prefix(), b"@RSYNCD:");
    }

    #[test]
    fn buffered_prefix_is_empty_for_binary_detection() {
        let mut detector = NegotiationPrologueDetector::new();
        assert_eq!(detector.observe(&[0x00]), NegotiationPrologue::Binary);
        assert_eq!(detector.buffered_prefix(), b"");
    }

    fn assert_detector_matches_across_partitions(data: &[u8]) {
        let expected = detect_negotiation_prologue(data);

        for first_end in 0..=data.len() {
            for second_end in first_end..=data.len() {
                let mut detector = NegotiationPrologueDetector::new();
                let _ = detector.observe(&data[..first_end]);
                let _ = detector.observe(&data[first_end..second_end]);
                let result = detector.observe(&data[second_end..]);

                assert_eq!(
                    result, expected,
                    "segmented detection mismatch for {:?} with splits ({}, {})",
                    data, first_end, second_end
                );

                match expected {
                    NegotiationPrologue::NeedMoreData => {
                        assert_eq!(detector.decision(), None);
                    }
                    decision => {
                        assert_eq!(detector.decision(), Some(decision));
                    }
                }
            }
        }
    }

    #[test]
    fn prologue_detector_matches_stateless_detection_across_partitions() {
        assert_detector_matches_across_partitions(b"");
        assert_detector_matches_across_partitions(b"@");
        assert_detector_matches_across_partitions(b"@RS");
        assert_detector_matches_across_partitions(b"@RSYNCD: 31.0\n");
        assert_detector_matches_across_partitions(b"@RSYNCX");
        assert_detector_matches_across_partitions(&[0x00, 0x20, 0x00, 0x00]);
        assert_detector_matches_across_partitions(b"modern");
    }
}
