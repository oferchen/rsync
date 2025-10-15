use std::io::{self, Read};

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

/// Incrementally reads bytes from a [`Read`] implementation until the
/// negotiation style can be determined.
///
/// Upstream rsync only needs to observe the very first octet to decide between
/// the legacy ASCII negotiation (`@RSYNCD:`) and the modern binary handshake.
/// Real transports, however, may deliver that byte in small fragments or after
/// transient `EINTR` interruptions. This helper mirrors upstream behavior while
/// providing a higher level interface that owns the buffered prefix so callers
/// can replay the bytes into the legacy greeting parser without reallocating.
#[derive(Debug, Default)]
pub struct NegotiationPrologueSniffer {
    detector: NegotiationPrologueDetector,
    buffered: Vec<u8>,
}

impl NegotiationPrologueSniffer {
    /// Creates a sniffer with an empty buffer and undecided negotiation state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the buffered bytes that were consumed while detecting the
    /// negotiation style.
    #[must_use]
    pub fn buffered(&self) -> &[u8] {
        &self.buffered
    }

    /// Consumes the sniffer and returns the owned buffer containing the bytes
    /// that were read while determining the negotiation style.
    #[must_use]
    pub fn into_buffered(self) -> Vec<u8> {
        self.buffered
    }

    /// Reports the cached negotiation decision, if any.
    #[must_use]
    pub fn decision(&self) -> Option<NegotiationPrologue> {
        self.detector.decision()
    }

    /// Clears the buffered prefix and resets the negotiation detector so the
    /// sniffer can be reused for another connection attempt.
    pub fn reset(&mut self) {
        self.detector.reset();
        self.buffered.clear();
    }

    /// Reads from `reader` until the negotiation style can be determined.
    ///
    /// Bytes consumed during detection are appended to the internal buffer so
    /// callers can replay them into the legacy greeting parser if necessary.
    /// Once a decision has been cached, subsequent calls return immediately
    /// without performing additional I/O.
    pub fn read_from<R: Read>(&mut self, reader: &mut R) -> io::Result<NegotiationPrologue> {
        if let Some(decision) = self.detector.decision() {
            return Ok(decision);
        }

        let mut byte = [0u8; 1];

        loop {
            match reader.read(&mut byte) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "connection closed before rsync negotiation prologue was determined",
                    ));
                }
                Ok(read) => {
                    let observed = &byte[..read];
                    self.buffered.extend_from_slice(observed);
                    let decision = self.detector.observe(observed);
                    if decision != NegotiationPrologue::NeedMoreData {
                        return Ok(decision);
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => return Err(err),
            }
        }
    }
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
    prefix_complete: bool,
}

impl NegotiationPrologueDetector {
    /// Creates a fresh detector that has not yet observed any bytes.
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
    /// Once a non-`NeedMoreData` classification is returned, subsequent calls
    /// will keep producing the same value without inspecting further input.
    #[must_use]
    pub fn observe(&mut self, chunk: &[u8]) -> NegotiationPrologue {
        if let Some(decided) = self.decided {
            let needs_more_prefix_bytes =
                decided == NegotiationPrologue::LegacyAscii && !self.prefix_complete;
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

            if self.buffer[..self.len] != prefix[..self.len] {
                self.prefix_complete = true;
                decision = Some(self.decide(NegotiationPrologue::LegacyAscii));
                break;
            }

            if self.len >= LEGACY_DAEMON_PREFIX_LEN {
                self.prefix_complete = true;
                decision = Some(self.decide(NegotiationPrologue::LegacyAscii));
                break;
            }
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
        self.observe(core::slice::from_ref(&byte))
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

    /// Reports whether the canonical legacy prefix (`@RSYNCD:`) has been fully
    /// observed (or ruled out due to a mismatch) after classifying the stream
    /// as [`NegotiationPrologue::LegacyAscii`].
    ///
    /// Legacy negotiations reuse the bytes that triggered the legacy
    /// classification when parsing the full greeting line. Upstream rsync marks
    /// the prefix handling as complete once the canonical marker is buffered or
    /// a divergence is detected. This helper mirrors that behavior so higher
    /// layers can determine when it is safe to hand the accumulated bytes to
    /// [`parse_legacy_daemon_greeting_bytes`]
    /// (`crate::legacy::parse_legacy_daemon_greeting_bytes`) without peeking at
    /// the detector's internal fields.
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

    /// Returns the number of bytes retained in the prefix buffer.
    ///
    /// The detector only stores bytes while it is still determining whether
    /// the exchange uses the legacy ASCII greeting. Once the binary path has
    /// been selected the buffer remains empty. Higher layers that want to
    /// mirror upstream rsync's peek logic can query this helper to decide how
    /// many bytes should be replayed into the legacy greeting parser without
    /// inspecting the raw slice returned by [`buffered_prefix`].
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
    pub fn reset(&mut self) {
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
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::io::{Cursor, Read};

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
    fn prologue_detector_default_matches_initial_state() {
        let detector = NegotiationPrologueDetector::default();

        assert_eq!(detector.decision(), None);
        assert_eq!(detector.buffered_prefix(), b"");
        assert_eq!(detector.buffered_len(), 0);
        assert!(!detector.legacy_prefix_complete());
        assert_eq!(detector.legacy_prefix_remaining(), None);
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
    fn prologue_detector_handles_mismatch_at_last_prefix_byte() {
        let mut detector = NegotiationPrologueDetector::new();
        assert_eq!(
            detector.observe(b"@RSYNCD;"),
            NegotiationPrologue::LegacyAscii
        );
        assert_eq!(detector.buffered_prefix(), b"@RSYNCD;");

        // Subsequent bytes keep replaying the cached decision without extending
        // the buffered prefix because the canonical marker has already been
        // ruled out by the mismatch in the final position.
        assert_eq!(
            detector.observe(b": more"),
            NegotiationPrologue::LegacyAscii
        );
        assert_eq!(detector.buffered_prefix(), b"@RSYNCD;");
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
    fn prologue_detector_handles_empty_chunk_while_waiting_for_prefix_completion() {
        let mut detector = NegotiationPrologueDetector::new();
        assert_eq!(detector.observe(b"@"), NegotiationPrologue::LegacyAscii);
        assert_eq!(detector.buffered_prefix(), b"@");
        assert_eq!(
            detector.legacy_prefix_remaining(),
            Some(LEGACY_DAEMON_PREFIX_LEN - 1)
        );

        // Feeding an empty chunk while still collecting the canonical legacy
        // prefix must replay the cached decision without mutating the
        // buffered bytes. Upstream's detector simply waits for additional data
        // while treating the exchange as legacy after the leading '@' is
        // observed.
        assert_eq!(detector.observe(b""), NegotiationPrologue::LegacyAscii);
        assert_eq!(detector.buffered_prefix(), b"@");
        assert_eq!(
            detector.legacy_prefix_remaining(),
            Some(LEGACY_DAEMON_PREFIX_LEN - 1)
        );

        assert_eq!(
            detector.observe(b"RSYNCD:"),
            NegotiationPrologue::LegacyAscii
        );
        assert_eq!(detector.buffered_prefix(), b"@RSYNCD:");
        assert_eq!(detector.legacy_prefix_remaining(), None);
    }

    #[test]
    fn prologue_detector_reports_buffered_length() {
        let mut detector = NegotiationPrologueDetector::new();
        assert_eq!(detector.buffered_len(), 0);

        assert_eq!(detector.observe(b"@RS"), NegotiationPrologue::LegacyAscii);
        assert_eq!(detector.buffered_len(), 3);
        assert_eq!(
            detector.legacy_prefix_remaining(),
            Some(LEGACY_DAEMON_PREFIX_LEN - 3)
        );

        assert_eq!(detector.observe(b"YNCD:"), NegotiationPrologue::LegacyAscii);
        assert_eq!(detector.buffered_len(), LEGACY_DAEMON_PREFIX_LEN);
        assert_eq!(detector.legacy_prefix_remaining(), None);

        assert_eq!(
            detector.observe(b" 31.0\n"),
            NegotiationPrologue::LegacyAscii
        );
        assert_eq!(detector.buffered_len(), LEGACY_DAEMON_PREFIX_LEN);
        assert_eq!(detector.legacy_prefix_remaining(), None);

        let mut binary = NegotiationPrologueDetector::new();
        assert_eq!(binary.observe(b"modern"), NegotiationPrologue::Binary);
        assert_eq!(binary.buffered_len(), 0);
        assert_eq!(binary.legacy_prefix_remaining(), None);
    }

    #[test]
    fn prologue_detector_caches_decision() {
        let mut detector = NegotiationPrologueDetector::new();
        assert_eq!(detector.observe(b"@X"), NegotiationPrologue::LegacyAscii);
        assert_eq!(
            detector.observe(b"anything"),
            NegotiationPrologue::LegacyAscii
        );
        assert_eq!(detector.legacy_prefix_remaining(), None);
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
    fn legacy_prefix_completion_reports_state_before_decision() {
        let detector = NegotiationPrologueDetector::new();
        assert!(!detector.legacy_prefix_complete());
    }

    #[test]
    fn legacy_prefix_completion_tracks_partial_prefix() {
        let mut detector = NegotiationPrologueDetector::new();
        assert_eq!(detector.observe(b"@"), NegotiationPrologue::LegacyAscii);
        assert!(!detector.legacy_prefix_complete());

        assert_eq!(detector.observe(b"RSYN"), NegotiationPrologue::LegacyAscii);
        assert!(!detector.legacy_prefix_complete());

        assert_eq!(detector.observe(b"CD:"), NegotiationPrologue::LegacyAscii);
        assert!(detector.legacy_prefix_complete());
    }

    #[test]
    fn legacy_prefix_completion_handles_mismatch() {
        let mut detector = NegotiationPrologueDetector::new();
        assert_eq!(detector.observe(b"@X"), NegotiationPrologue::LegacyAscii);
        assert!(detector.legacy_prefix_complete());
    }

    #[test]
    fn legacy_prefix_completion_stays_false_for_binary_detection() {
        let mut detector = NegotiationPrologueDetector::new();
        assert_eq!(detector.observe(&[0x00]), NegotiationPrologue::Binary);
        assert!(!detector.legacy_prefix_complete());
    }

    #[test]
    fn legacy_prefix_completion_resets_with_detector() {
        let mut detector = NegotiationPrologueDetector::new();
        assert_eq!(
            detector.observe(b"@RSYNCD:"),
            NegotiationPrologue::LegacyAscii
        );
        assert!(detector.legacy_prefix_complete());

        detector.reset();
        assert!(!detector.legacy_prefix_complete());
    }

    #[test]
    fn legacy_prefix_remaining_reports_none_before_decision() {
        let detector = NegotiationPrologueDetector::new();
        assert_eq!(detector.legacy_prefix_remaining(), None);
    }

    #[test]
    fn legacy_prefix_remaining_tracks_mismatch_completion() {
        let mut detector = NegotiationPrologueDetector::new();
        assert_eq!(detector.observe(b"@RS"), NegotiationPrologue::LegacyAscii);
        assert_eq!(
            detector.legacy_prefix_remaining(),
            Some(LEGACY_DAEMON_PREFIX_LEN - 3)
        );

        // Diverging from the canonical marker completes the prefix handling
        // immediately, mirroring upstream's behavior. The helper should report
        // that no additional bytes are required once the mismatch has been
        // observed.
        assert_eq!(detector.observe(b"YNXD"), NegotiationPrologue::LegacyAscii);
        assert_eq!(detector.legacy_prefix_remaining(), None);
    }

    #[test]
    fn legacy_prefix_remaining_counts_down_through_canonical_prefix() {
        let mut detector = NegotiationPrologueDetector::new();
        assert_eq!(detector.legacy_prefix_remaining(), None);

        for (idx, &byte) in LEGACY_DAEMON_PREFIX.as_bytes().iter().enumerate() {
            let observed = detector.observe_byte(byte);
            assert_eq!(observed, NegotiationPrologue::LegacyAscii);

            let expected_remaining = if idx + 1 < LEGACY_DAEMON_PREFIX_LEN {
                Some(LEGACY_DAEMON_PREFIX_LEN - idx - 1)
            } else {
                None
            };

            assert_eq!(detector.legacy_prefix_remaining(), expected_remaining);
            assert_eq!(detector.buffered_len(), idx + 1);
            assert_eq!(
                detector.buffered_prefix(),
                &LEGACY_DAEMON_PREFIX.as_bytes()[..idx + 1]
            );
        }

        assert!(detector.legacy_prefix_complete());
        assert_eq!(detector.buffered_prefix(), LEGACY_DAEMON_PREFIX.as_bytes());
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

    #[test]
    fn buffered_prefix_stops_growing_after_mismatch_with_long_chunk() {
        let mut detector = NegotiationPrologueDetector::new();

        // Feed a chunk that starts with the legacy marker but diverges on the
        // second byte. The detector should record the observed prefix up to
        // the mismatch and ignore the remainder of the chunk, mirroring
        // upstream's behavior of replaying the legacy decision without
        // extending the buffered slice past the canonical marker length.
        let mut chunk = Vec::new();
        chunk.push(b'@');
        chunk.extend_from_slice(&[b'X'; 32]);

        assert_eq!(detector.observe(&chunk), NegotiationPrologue::LegacyAscii,);
        assert_eq!(detector.buffered_prefix(), b"@X");
        assert_eq!(detector.buffered_prefix().len(), 2);

        // Additional bytes keep replaying the cached decision without mutating
        // the buffered prefix that was captured before the mismatch.
        assert_eq!(detector.observe(b"more"), NegotiationPrologue::LegacyAscii);
        assert_eq!(detector.buffered_prefix(), b"@X");
    }

    #[test]
    fn prologue_detector_can_be_reset_for_reuse() {
        let mut detector = NegotiationPrologueDetector::new();
        assert_eq!(detector.observe(b"@RS"), NegotiationPrologue::LegacyAscii);
        assert_eq!(detector.buffered_prefix(), b"@RS");
        assert_eq!(detector.decision(), Some(NegotiationPrologue::LegacyAscii));

        detector.reset();
        assert_eq!(detector.decision(), None);
        assert_eq!(detector.buffered_prefix(), b"");

        assert_eq!(detector.observe(&[0x00]), NegotiationPrologue::Binary);
        assert_eq!(detector.decision(), Some(NegotiationPrologue::Binary));
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

    #[test]
    fn prologue_detector_observe_byte_matches_slice_behavior() {
        fn run_case(data: &[u8]) {
            let mut slice_detector = NegotiationPrologueDetector::new();
            let slice_result = slice_detector.observe(data);

            let mut byte_detector = NegotiationPrologueDetector::new();
            let byte_result = if data.is_empty() {
                byte_detector.observe(data)
            } else {
                let mut last = NegotiationPrologue::NeedMoreData;
                for &byte in data {
                    last = byte_detector.observe_byte(byte);
                }
                last
            };

            assert_eq!(
                byte_result, slice_result,
                "decision mismatch for {:?}",
                data
            );
            assert_eq!(
                byte_detector.decision(),
                slice_detector.decision(),
                "cached decision mismatch for {:?}",
                data
            );
            assert_eq!(
                byte_detector.legacy_prefix_complete(),
                slice_detector.legacy_prefix_complete(),
                "prefix completion mismatch for {:?}",
                data
            );
            assert_eq!(
                byte_detector.buffered_prefix(),
                slice_detector.buffered_prefix(),
                "buffered prefix mismatch for {:?}",
                data
            );
        }

        run_case(b"");
        run_case(b"@");
        run_case(b"@RS");
        run_case(b"@RSYNCD:");
        run_case(b"@RSYNCD: 31.0\n");
        run_case(b"@RSYNCX");
        run_case(b"modern");
        run_case(&[0x00, 0x20, 0x00, 0x00]);
    }

    proptest! {
        #[test]
        fn prologue_detector_matches_stateless_detection_for_random_chunks(
            chunks in prop::collection::vec(
                prop::collection::vec(any::<u8>(), 0..=LEGACY_DAEMON_PREFIX_LEN + 2),
                0..=4
            )
        ) {
            let concatenated: Vec<u8> = chunks.iter().flatten().copied().collect();
            let expected = detect_negotiation_prologue(&concatenated);

            let mut detector = NegotiationPrologueDetector::new();
            let mut last = NegotiationPrologue::NeedMoreData;

            for chunk in &chunks {
                last = detector.observe(chunk);
            }

            prop_assert_eq!(last, expected);

            match expected {
                NegotiationPrologue::NeedMoreData => {
                    prop_assert_eq!(detector.decision(), None);
                }
                decision => {
                    prop_assert_eq!(detector.decision(), Some(decision));
                }
            }

            let buffered = detector.buffered_prefix();
            prop_assert_eq!(buffered.len(), detector.buffered_len());

            match detector.decision() {
                Some(NegotiationPrologue::LegacyAscii) => {
                    if let Some(remaining) = detector.legacy_prefix_remaining() {
                        prop_assert!(remaining > 0);
                        prop_assert!(!detector.legacy_prefix_complete());
                    } else {
                        prop_assert!(detector.legacy_prefix_complete());
                    }
                }
                _ => {
                    prop_assert_eq!(detector.legacy_prefix_remaining(), None);
                    prop_assert!(!detector.legacy_prefix_complete());
                    prop_assert!(buffered.is_empty());
                }
            }
        }
    }

    #[test]
    fn prologue_sniffer_reports_binary_negotiation() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        let mut cursor = Cursor::new(vec![0x00, 0x20, 0x00]);

        let decision = sniffer
            .read_from(&mut cursor)
            .expect("binary negotiation should succeed");
        assert_eq!(decision, NegotiationPrologue::Binary);
        assert_eq!(sniffer.buffered(), &[0x00]);

        // Subsequent calls reuse the cached decision and avoid additional I/O.
        let decision = sniffer
            .read_from(&mut cursor)
            .expect("cached decision should be returned");
        assert_eq!(decision, NegotiationPrologue::Binary);
        assert_eq!(cursor.position(), 1);
    }

    #[test]
    fn prologue_sniffer_reports_legacy_negotiation() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        let mut cursor = Cursor::new(b"@RSYNCD: 31.0\n".to_vec());

        let decision = sniffer
            .read_from(&mut cursor)
            .expect("legacy negotiation should succeed");
        assert_eq!(decision, NegotiationPrologue::LegacyAscii);
        assert_eq!(sniffer.buffered(), b"@");

        let mut remaining = Vec::new();
        cursor.read_to_end(&mut remaining).expect("read remainder");
        let mut replay = sniffer.into_buffered();
        replay.extend_from_slice(&remaining);
        assert_eq!(replay, b"@RSYNCD: 31.0\n");
    }

    #[test]
    fn prologue_sniffer_reset_clears_buffer_and_state() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        let mut cursor = Cursor::new(b"@".to_vec());
        let _ = sniffer
            .read_from(&mut cursor)
            .expect("legacy negotiation should succeed");

        assert_eq!(sniffer.buffered(), b"@");
        assert_eq!(sniffer.decision(), Some(NegotiationPrologue::LegacyAscii));

        sniffer.reset();
        assert!(sniffer.buffered().is_empty());
        assert_eq!(sniffer.decision(), None);
    }

    #[test]
    fn prologue_sniffer_handles_unexpected_eof() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        let mut cursor = Cursor::new(Vec::<u8>::new());
        let err = sniffer.read_from(&mut cursor).expect_err("EOF should fail");
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }
}
