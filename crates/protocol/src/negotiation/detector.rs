use ::core::slice;

use crate::legacy::{LEGACY_DAEMON_PREFIX_BYTES, LEGACY_DAEMON_PREFIX_LEN};

use super::{BufferedPrefixTooSmall, NegotiationPrologue};

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
    /// This constructor is equivalent to [`Self::default()`] but remains available
    /// as a `const` so compile-time contexts—such as other `const fn` initialisers—
    /// can instantiate detectors without going through trait dispatch.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            buffer: [0; LEGACY_DAEMON_PREFIX_LEN],
            len: 0,
            decided: None,
            prefix_complete: false,
        }
    }

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
    /// [`Self::decision`] and [`Self::is_legacy`].
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
    /// peer is speaking the legacy ASCII or binary handshake. Providing a
    /// convenience wrapper keeps that call pattern expressive without forcing
    /// callers to allocate temporary one-byte slices.
    #[must_use]
    #[inline]
    pub fn observe_byte(&mut self, byte: u8) -> NegotiationPrologue {
        self.observe(slice::from_ref(&byte))
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

    /// Reports whether the negotiation style has been determined.
    ///
    /// The helper mirrors [`NegotiationPrologue::is_decided`] but operates on the cached
    /// decision maintained by the detector. Callers that only need to know whether the
    /// initial byte has already selected the binary or legacy ASCII handshake can rely on
    /// this predicate instead of matching on [`Self::decision`]. The method returns `false`
    /// until the first byte is observed, mirroring upstream rsync's behavior where the
    /// detection logic remains pending until the transport yields data.
    #[must_use]
    pub const fn is_decided(&self) -> bool {
        matches!(self.decided, Some(decision) if decision.is_decided())
    }

    /// Reports whether the detector has determined that the peer selected the legacy
    /// ASCII negotiation.
    ///
    /// The helper mirrors [`NegotiationPrologue::is_legacy`] while accounting for the
    /// possibility that a decision has not yet been cached. Higher layers that only need
    /// a boolean view of the cached state can therefore rely on this method instead of
    /// matching on [`Self::decision`]. The predicate remains `true` even when additional
    /// prefix bytes still need to be buffered, matching upstream rsync's behavior where
    /// the legacy decision is sticky once the initial `@` byte has been observed.
    #[must_use = "check whether the detector classified the exchange as legacy ASCII"]
    pub const fn is_legacy(&self) -> bool {
        matches!(self.decided, Some(decision) if decision.is_legacy())
    }

    /// Reports whether the detector has determined that the peer selected the binary
    /// negotiation path.
    ///
    /// The helper mirrors [`NegotiationPrologue::is_binary`] while tolerating undecided
    /// states. It becomes `true` as soon as the first byte rules out the legacy ASCII
    /// negotiation, allowing call sites to react immediately without awaiting further I/O.
    #[must_use = "check whether the detector classified the exchange as binary"]
    pub const fn is_binary(&self) -> bool {
        matches!(self.decided, Some(decision) if decision.is_binary())
    }

    /// Reports whether additional bytes must be read before the negotiation prologue is
    /// fully understood.
    ///
    /// Binary negotiations require only the leading byte, so the helper flips to `false`
    /// immediately once the first non-`@` octet is observed. Legacy exchanges remain in the
    /// "needs more" state until the canonical `@RSYNCD:` prefix has been buffered, allowing
    /// higher layers to keep reading until the greeting parser can replay the captured
    /// bytes. When no data has been observed yet the method also returns `true`, matching the
    /// semantics of [`NegotiationPrologue::requires_more_data`].
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
    /// Legacy negotiations reuse the bytes that triggered the legacy
    /// classification when parsing the full greeting line. Upstream rsync marks
    /// the prefix handling as complete once the canonical marker is buffered or
    /// a divergence is detected. This helper mirrors that behavior so higher
    /// layers can determine when it is safe to hand the accumulated bytes to
    /// [`crate::parse_legacy_daemon_greeting_bytes`] without peeking at the
    /// detector's internal fields.
    #[must_use]
    pub const fn legacy_prefix_complete(&self) -> bool {
        matches!(self.decided, Some(NegotiationPrologue::LegacyAscii)) && self.prefix_complete
    }

    /// Reports how many additional bytes are required to capture the canonical
    /// legacy prefix when the detector has already classified the stream as
    /// [`NegotiationPrologue::LegacyAscii`].
    ///
    /// Upstream rsync keeps reading from the transport until the full
    /// `@RSYNCD:` marker has been buffered or a mismatch forces the legacy
    /// classification. Higher layers often need the same information to decide
    /// whether another blocking read is necessary before parsing the full
    /// greeting line. Returning `Some(n)` indicates that `n` more bytes are
    /// required to finish buffering the canonical prefix. Once the prefix has
    /// been completed—or when the detector decides the exchange is binary—the
    /// method returns `None`.
    #[must_use]
    pub const fn legacy_prefix_remaining(&self) -> Option<usize> {
        match (self.decided, self.prefix_complete) {
            (Some(NegotiationPrologue::LegacyAscii), false) => {
                Some(LEGACY_DAEMON_PREFIX_LEN - self.len)
            }
            _ => None,
        }
    }

    const fn decide(&mut self, decision: NegotiationPrologue) -> NegotiationPrologue {
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
    /// subsequent [`Self::observe`] calls until the canonical `@RSYNCD:` prefix has
    /// been captured or a mismatch forces the legacy classification. For binary
    /// negotiations, no bytes are retained and this method returns an empty
    /// slice.
    #[must_use]
    #[inline]
    pub fn buffered_prefix(&self) -> &[u8] {
        &self.buffer[..self.len]
    }

    /// Copies the buffered prefix into the caller-provided slice.
    ///
    /// Legacy negotiations require replaying the already-consumed bytes into the
    /// greeting parser once enough data has been read from the transport. Higher
    /// layers that reuse stack-allocated scratch space can avoid temporary
    /// vectors by copying the buffered prefix into an existing slice instead of
    /// borrowing it directly. When the destination slice is too small to hold
    /// the buffered prefix, a [`BufferedPrefixTooSmall`] error is returned and no
    /// data is written to the provided slice.
    #[must_use = "process the copy result so the buffered prefix can be replayed or the size error handled"]
    pub fn copy_buffered_prefix_into(
        &self,
        target: &mut [u8],
    ) -> Result<usize, BufferedPrefixTooSmall> {
        let required = self.len;

        if target.len() < required {
            return Err(BufferedPrefixTooSmall::new(required, target.len()));
        }

        target[..required].copy_from_slice(&self.buffer[..required]);
        Ok(required)
    }

    /// Copies the buffered prefix into a caller-provided array without allocation.
    ///
    /// This convenience wrapper mirrors
    /// [`copy_buffered_prefix_into`](Self::copy_buffered_prefix_into) but accepts a
    /// fixed-size array directly. Callers that keep a stack-allocated
    /// `LEGACY_DAEMON_PREFIX_LEN` scratch buffer can therefore avoid the
    /// additional `.as_mut_slice()` boilerplate while still receiving the copied
    /// byte count. When the array cannot hold the buffered prefix a
    /// [`BufferedPrefixTooSmall`] error is returned and no bytes are written,
    /// matching upstream rsync's behavior where short buffers are reported to
    /// the caller without mutating the destination.
    #[must_use = "process the copy result or handle the insufficient capacity error"]
    pub fn copy_buffered_prefix_into_array<const N: usize>(
        &self,
        target: &mut [u8; N],
    ) -> Result<usize, BufferedPrefixTooSmall> {
        self.copy_buffered_prefix_into(target.as_mut_slice())
    }

    /// Returns the number of bytes retained in the prefix buffer.
    ///
    /// The detector only stores bytes while it is still determining whether
    /// the exchange uses the legacy ASCII greeting. Once the binary path has
    /// been selected the buffer remains empty. Higher layers that want to
    /// mirror upstream rsync's peek logic can query this helper to decide how
    /// many bytes should be replayed into the legacy greeting parser without
    /// inspecting the raw slice returned by [`Self::buffered_prefix`].
    #[must_use]
    #[inline]
    pub const fn buffered_len(&self) -> usize {
        self.len
    }

    /// Resets the detector to its initial state so it can be reused for a new
    /// connection attempt.
    ///
    /// Higher layers often keep a detector instance around while reading from a
    /// transport in small increments. Once a negotiation completes (success or
    /// failure), the same buffer can be recycled by clearing the buffered
    /// prefix and any cached decision rather than allocating a new detector.
    /// The method restores the struct to the state produced by
    /// [`NegotiationPrologueDetector::new`], mirroring upstream rsync's
    /// practice of zeroing its detection state before accepting another
    /// connection.
    pub const fn reset(&mut self) {
        self.buffer = [0; LEGACY_DAEMON_PREFIX_LEN];
        self.len = 0;
        self.decided = None;
        self.prefix_complete = false;
    }
}

impl Default for NegotiationPrologueDetector {
    /// Creates a detector that has not yet observed any bytes.
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[allow(unused_must_use, clippy::uninlined_format_args)]
mod tests {
    use super::*;

    #[test]
    fn new_detector_has_no_decision() {
        let detector = NegotiationPrologueDetector::new();
        assert!(detector.decision().is_none());
        assert!(!detector.is_decided());
    }

    #[test]
    fn default_equals_new() {
        let default = NegotiationPrologueDetector::default();
        let new = NegotiationPrologueDetector::new();
        assert_eq!(default.buffered_len(), new.buffered_len());
        assert_eq!(default.decision(), new.decision());
    }

    #[test]
    fn binary_detected_on_non_at_byte() {
        let mut detector = NegotiationPrologueDetector::new();
        let result = detector.observe_byte(0x00);
        assert_eq!(result, NegotiationPrologue::Binary);
        assert!(detector.is_binary());
        assert!(!detector.is_legacy());
    }

    #[test]
    fn legacy_detected_on_at_byte() {
        let mut detector = NegotiationPrologueDetector::new();
        let result = detector.observe_byte(b'@');
        assert!(detector.is_legacy());
        assert!(!detector.is_binary());
        assert!(!detector.legacy_prefix_complete());
        assert!(
            result == NegotiationPrologue::LegacyAscii
                || result == NegotiationPrologue::NeedMoreData
        );
    }

    #[test]
    fn decision_is_sticky() {
        let mut detector = NegotiationPrologueDetector::new();
        detector.observe_byte(0x00);
        assert!(detector.is_binary());

        // Even with more data, the decision remains binary
        detector.observe(b"@RSYNCD:");
        assert!(detector.is_binary());
    }

    #[test]
    fn buffered_prefix_empty_for_binary() {
        let mut detector = NegotiationPrologueDetector::new();
        detector.observe_byte(0x00);
        assert!(detector.buffered_prefix().is_empty());
        assert_eq!(detector.buffered_len(), 0);
    }

    #[test]
    fn buffered_prefix_grows_for_legacy() {
        let mut detector = NegotiationPrologueDetector::new();
        detector.observe(b"@RSY");
        assert!(detector.is_legacy());
        assert_eq!(detector.buffered_len(), 4);
        assert_eq!(detector.buffered_prefix(), b"@RSY");
    }

    #[test]
    fn legacy_prefix_remaining_tracks_bytes() {
        let mut detector = NegotiationPrologueDetector::new();
        detector.observe_byte(b'@');
        let remaining = detector.legacy_prefix_remaining();
        assert!(remaining.is_some());
        assert!(remaining.unwrap() < LEGACY_DAEMON_PREFIX_LEN);
    }

    #[test]
    fn legacy_prefix_complete_after_full_prefix() {
        let mut detector = NegotiationPrologueDetector::new();
        detector.observe(b"@RSYNCD:");
        assert!(detector.is_legacy());
        assert!(detector.legacy_prefix_complete());
    }

    #[test]
    fn requires_more_data_when_empty() {
        let detector = NegotiationPrologueDetector::new();
        assert!(detector.requires_more_data());
    }

    #[test]
    fn requires_more_data_false_for_binary() {
        let mut detector = NegotiationPrologueDetector::new();
        detector.observe_byte(0x00);
        assert!(!detector.requires_more_data());
    }

    #[test]
    fn copy_buffered_prefix_into_success() {
        let mut detector = NegotiationPrologueDetector::new();
        detector.observe(b"@RSY");

        let mut buffer = [0u8; 10];
        let copied = detector.copy_buffered_prefix_into(&mut buffer).unwrap();
        assert_eq!(copied, 4);
        assert_eq!(&buffer[..4], b"@RSY");
    }

    #[test]
    fn copy_buffered_prefix_into_too_small() {
        let mut detector = NegotiationPrologueDetector::new();
        detector.observe(b"@RSYNCD:");

        let mut buffer = [0u8; 2];
        let result = detector.copy_buffered_prefix_into(&mut buffer);
        assert!(result.is_err());
    }

    #[test]
    fn copy_buffered_prefix_into_array_success() {
        let mut detector = NegotiationPrologueDetector::new();
        detector.observe(b"@RSY");

        let mut buffer = [0u8; LEGACY_DAEMON_PREFIX_LEN];
        let copied = detector
            .copy_buffered_prefix_into_array(&mut buffer)
            .unwrap();
        assert_eq!(copied, 4);
    }

    #[test]
    fn reset_clears_state() {
        let mut detector = NegotiationPrologueDetector::new();
        detector.observe(b"@RSYNCD:");
        assert!(detector.is_legacy());
        assert!(detector.legacy_prefix_complete());

        detector.reset();
        assert!(detector.decision().is_none());
        assert!(!detector.is_decided());
        assert_eq!(detector.buffered_len(), 0);
    }

    #[test]
    fn observe_empty_chunk_returns_need_more() {
        let mut detector = NegotiationPrologueDetector::new();
        let result = detector.observe(&[]);
        assert_eq!(result, NegotiationPrologue::NeedMoreData);
    }

    #[test]
    fn is_decided_true_after_classification() {
        let mut detector = NegotiationPrologueDetector::new();
        detector.observe_byte(0x00);
        assert!(detector.is_decided());
    }

    // ========================================================================
    // Comprehensive Binary Detection Tests
    // ========================================================================

    #[test]
    fn binary_detected_for_all_non_at_bytes() {
        // All byte values except '@' (0x40) should trigger binary detection
        for byte in 0u8..=255 {
            if byte == b'@' {
                continue;
            }
            let mut detector = NegotiationPrologueDetector::new();
            let result = detector.observe_byte(byte);
            assert_eq!(
                result,
                NegotiationPrologue::Binary,
                "byte {:#04X} should be binary",
                byte
            );
            assert!(detector.is_binary());
            assert!(!detector.is_legacy());
        }
    }

    #[test]
    fn binary_detection_immediate() {
        let mut detector = NegotiationPrologueDetector::new();

        // First non-@ byte should immediately decide binary
        let result = detector.observe_byte(0x1F);
        assert_eq!(result, NegotiationPrologue::Binary);
        assert!(detector.is_decided());
        assert!(!detector.requires_more_data());
    }

    // ========================================================================
    // Comprehensive Legacy Detection Tests
    // ========================================================================

    #[test]
    fn legacy_full_prefix_match() {
        let mut detector = NegotiationPrologueDetector::new();
        let result = detector.observe(b"@RSYNCD:");

        assert!(detector.is_legacy());
        assert!(detector.legacy_prefix_complete());
        assert!(!detector.requires_more_data());
        assert_eq!(detector.buffered_len(), LEGACY_DAEMON_PREFIX_LEN);
        assert_eq!(detector.buffered_prefix(), b"@RSYNCD:");
        assert!(result == NegotiationPrologue::LegacyAscii);
    }

    #[test]
    fn legacy_incremental_byte_by_byte() {
        let prefix = b"@RSYNCD:";
        let mut detector = NegotiationPrologueDetector::new();

        for (i, &byte) in prefix.iter().enumerate() {
            let _result = detector.observe_byte(byte);
            assert!(detector.is_legacy(), "should be legacy at byte {i}");

            if i < prefix.len() - 1 {
                assert!(!detector.legacy_prefix_complete());
                assert!(detector.requires_more_data());
            } else {
                assert!(detector.legacy_prefix_complete());
                assert!(!detector.requires_more_data());
            }

            // Check progress matches upstream expectation
            let remaining = detector.legacy_prefix_remaining();
            if i < prefix.len() - 1 {
                assert_eq!(remaining, Some(prefix.len() - 1 - i));
            } else {
                assert!(remaining.is_none() || remaining == Some(0));
            }
        }
    }

    #[test]
    fn legacy_incremental_chunks() {
        let mut detector = NegotiationPrologueDetector::new();

        // Feed in multiple chunks
        detector.observe(b"@RSY");
        assert!(detector.is_legacy());
        assert!(!detector.legacy_prefix_complete());
        assert_eq!(detector.buffered_len(), 4);

        detector.observe(b"NCD:");
        assert!(detector.is_legacy());
        assert!(detector.legacy_prefix_complete());
        assert_eq!(detector.buffered_len(), 8);
    }

    #[test]
    fn legacy_mismatch_early() {
        // Prefix starts with @ but doesn't match @RSYNCD:
        let mut detector = NegotiationPrologueDetector::new();
        detector.observe(b"@WRONG:");

        assert!(detector.is_legacy());
        // Should still be marked as legacy but with complete prefix (mismatch)
        assert!(detector.legacy_prefix_complete());
        assert!(!detector.requires_more_data());
    }

    #[test]
    fn legacy_mismatch_at_second_byte() {
        let mut detector = NegotiationPrologueDetector::new();
        detector.observe(b"@X");

        // Still legacy (started with @) but mismatch detected
        assert!(detector.is_legacy());
        assert!(detector.legacy_prefix_complete());
    }

    // ========================================================================
    // State Persistence Tests
    // ========================================================================

    #[test]
    fn decision_sticky_after_binary() {
        let mut detector = NegotiationPrologueDetector::new();
        detector.observe_byte(0x00);
        assert!(detector.is_binary());

        // Feeding @ after binary decision doesn't change anything
        detector.observe(b"@RSYNCD:");
        assert!(detector.is_binary());
        assert!(!detector.is_legacy());

        // Buffer should remain empty for binary
        assert_eq!(detector.buffered_len(), 0);
    }

    #[test]
    fn decision_sticky_after_legacy() {
        let mut detector = NegotiationPrologueDetector::new();
        detector.observe(b"@RSYNCD:");
        assert!(detector.is_legacy());

        // Feeding binary data after legacy decision doesn't change anything
        detector.observe(&[0x00, 0x01, 0x02]);
        assert!(detector.is_legacy());
        assert!(!detector.is_binary());
    }

    // ========================================================================
    // Reset Tests
    // ========================================================================

    #[test]
    fn reset_after_binary() {
        let mut detector = NegotiationPrologueDetector::new();
        detector.observe_byte(0x00);
        assert!(detector.is_binary());

        detector.reset();
        assert!(!detector.is_decided());
        assert!(detector.decision().is_none());
        assert_eq!(detector.buffered_len(), 0);
        assert!(detector.requires_more_data());
    }

    #[test]
    fn reset_after_legacy() {
        let mut detector = NegotiationPrologueDetector::new();
        detector.observe(b"@RSYNCD:");
        assert!(detector.is_legacy());
        assert_eq!(detector.buffered_len(), 8);

        detector.reset();
        assert!(!detector.is_decided());
        assert!(detector.decision().is_none());
        assert_eq!(detector.buffered_len(), 0);
        assert!(detector.requires_more_data());
    }

    #[test]
    fn reset_allows_reuse() {
        let mut detector = NegotiationPrologueDetector::new();

        // First detection: binary
        detector.observe_byte(0x00);
        assert!(detector.is_binary());

        // Reset and reuse
        detector.reset();

        // Second detection: legacy
        detector.observe(b"@RSYNCD:");
        assert!(detector.is_legacy());
    }

    // ========================================================================
    // Copy Prefix Tests
    // ========================================================================

    #[test]
    fn copy_buffered_prefix_exact_size() {
        let mut detector = NegotiationPrologueDetector::new();
        detector.observe(b"@RSYNCD:");

        let mut buffer = [0u8; LEGACY_DAEMON_PREFIX_LEN];
        let copied = detector
            .copy_buffered_prefix_into_array(&mut buffer)
            .unwrap();

        assert_eq!(copied, 8);
        assert_eq!(&buffer[..8], b"@RSYNCD:");
    }

    #[test]
    fn copy_buffered_prefix_larger_buffer() {
        let mut detector = NegotiationPrologueDetector::new();
        detector.observe(b"@RSY");

        let mut buffer = [0u8; 100];
        let copied = detector.copy_buffered_prefix_into(&mut buffer).unwrap();

        assert_eq!(copied, 4);
        assert_eq!(&buffer[..4], b"@RSY");
    }

    #[test]
    fn copy_buffered_prefix_binary_returns_zero() {
        let mut detector = NegotiationPrologueDetector::new();
        detector.observe_byte(0x00);

        let mut buffer = [0u8; 10];
        let copied = detector.copy_buffered_prefix_into(&mut buffer).unwrap();

        assert_eq!(copied, 0);
    }

    // ========================================================================
    // Edge Cases
    // ========================================================================

    #[test]
    fn observe_empty_before_any_data() {
        let mut detector = NegotiationPrologueDetector::new();
        let result = detector.observe(&[]);

        assert_eq!(result, NegotiationPrologue::NeedMoreData);
        assert!(!detector.is_decided());
    }

    #[test]
    fn observe_empty_after_partial_legacy() {
        let mut detector = NegotiationPrologueDetector::new();
        detector.observe(b"@RSY");

        let result = detector.observe(&[]);
        assert_eq!(result, NegotiationPrologue::NeedMoreData);
        assert!(detector.is_legacy());
        assert!(!detector.legacy_prefix_complete());
    }

    #[test]
    fn observe_empty_after_complete_legacy() {
        let mut detector = NegotiationPrologueDetector::new();
        detector.observe(b"@RSYNCD:");

        let result = detector.observe(&[]);
        // After complete, empty observe returns cached decision
        assert!(result == NegotiationPrologue::LegacyAscii);
    }

    #[test]
    fn observe_empty_after_binary() {
        let mut detector = NegotiationPrologueDetector::new();
        detector.observe_byte(0x00);

        let result = detector.observe(&[]);
        assert_eq!(result, NegotiationPrologue::Binary);
    }

    #[test]
    fn observe_byte_is_equivalent_to_single_byte_slice() {
        let test_bytes = [0x00, b'@', 0xFF, 0x7F];

        for &byte in &test_bytes {
            let mut detector1 = NegotiationPrologueDetector::new();
            let mut detector2 = NegotiationPrologueDetector::new();

            let result1 = detector1.observe_byte(byte);
            let result2 = detector2.observe(&[byte]);

            assert_eq!(result1, result2, "mismatch for byte {:#04X}", byte);
            assert_eq!(detector1.is_binary(), detector2.is_binary());
            assert_eq!(detector1.is_legacy(), detector2.is_legacy());
        }
    }

    // ========================================================================
    // Clone Tests
    // ========================================================================

    #[test]
    fn clone_preserves_state() {
        let mut original = NegotiationPrologueDetector::new();
        original.observe(b"@RSY");

        let cloned = original.clone();

        assert_eq!(original.buffered_len(), cloned.buffered_len());
        assert_eq!(original.decision(), cloned.decision());
        assert_eq!(original.is_legacy(), cloned.is_legacy());
        assert_eq!(
            original.legacy_prefix_complete(),
            cloned.legacy_prefix_complete()
        );
        assert_eq!(original.buffered_prefix(), cloned.buffered_prefix());
    }

    #[test]
    fn clone_independence() {
        let mut original = NegotiationPrologueDetector::new();
        original.observe(b"@RSY");

        let cloned = original.clone();

        // Advance original further
        original.observe(b"NCD:");
        assert!(original.legacy_prefix_complete());

        // Cloned should not be affected
        assert!(!cloned.legacy_prefix_complete());
        assert_eq!(cloned.buffered_len(), 4);
    }

    // ========================================================================
    // Debug Trait Tests
    // ========================================================================

    #[test]
    fn debug_format_new() {
        let detector = NegotiationPrologueDetector::new();
        let debug = format!("{:?}", detector);
        assert!(debug.contains("NegotiationPrologueDetector"));
    }

    #[test]
    fn debug_format_after_binary() {
        let mut detector = NegotiationPrologueDetector::new();
        detector.observe_byte(0x00);
        let debug = format!("{:?}", detector);
        assert!(debug.contains("Binary") || debug.contains("Some"));
    }

    #[test]
    fn debug_format_after_legacy() {
        let mut detector = NegotiationPrologueDetector::new();
        detector.observe(b"@RSYNCD:");
        let debug = format!("{:?}", detector);
        assert!(debug.contains("LegacyAscii") || debug.contains("Some"));
    }
}
