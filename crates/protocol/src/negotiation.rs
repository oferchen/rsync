use core::{mem, slice};
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
#[derive(Debug)]
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

    /// Returns the number of bytes retained while sniffing the negotiation prologue.
    ///
    /// Higher layers that forward the captured prefix to the legacy ASCII parser often only
    /// need to know how many bytes should be replayed without inspecting the raw slice. Providing
    /// the length mirrors [`NegotiationPrologueDetector::buffered_len`] and keeps the sniffer's
    /// API aligned with the lower-level helper while avoiding repeated `len()` calls at the call
    /// site.
    #[must_use]
    pub fn buffered_len(&self) -> usize {
        self.buffered.len()
    }

    /// Consumes the sniffer and returns the owned buffer containing the bytes
    /// that were read while determining the negotiation style.
    #[must_use]
    pub fn into_buffered(self) -> Vec<u8> {
        self.buffered
    }

    /// Drains the buffered bytes while keeping the sniffer available for reuse.
    ///
    /// Callers that need to replay the captured prefix into the legacy greeting
    /// parser (or feed the initial binary byte back into the negotiation
    /// handler) can drain the buffer without relinquishing ownership of the
    /// sniffer. The internal storage is replaced with an empty vector whose
    /// capacity is capped at the canonical legacy prefix length so subsequent
    /// detections do not retain unbounded allocations while still satisfying the
    /// workspace's buffer reuse guidance.
    #[must_use]
    pub fn take_buffered(&mut self) -> Vec<u8> {
        let target_capacity = self.buffered.capacity().min(LEGACY_DAEMON_PREFIX_LEN);
        let mut drained = Vec::with_capacity(target_capacity);
        mem::swap(&mut self.buffered, &mut drained);

        if self.buffered.capacity() < LEGACY_DAEMON_PREFIX_LEN {
            self.buffered
                .reserve_exact(LEGACY_DAEMON_PREFIX_LEN - self.buffered.capacity());
        }

        // Defensively cap the returned capacity at the canonical prefix length so callers never
        // retain an excessively large allocation even if the sniffer previously observed a
        // malformed banner that forced the buffer to grow. The buffered length never exceeds the
        // prefix length, making the shrink operation a no-op for successful detections while
        // mirroring upstream's fixed-size peek storage.
        drained.shrink_to(LEGACY_DAEMON_PREFIX_LEN);

        drained
    }

    /// Drains the buffered bytes into an existing vector supplied by the caller.
    ///
    /// The helper mirrors [`take_buffered`] but avoids allocating a new vector when the
    /// caller already owns a reusable buffer. The destination vector is cleared before the
    /// captured prefix is copied into it, ensuring the slice matches the bytes that were
    /// consumed during negotiation sniffing. The returned length mirrors the number of bytes
    /// that were replayed into `target`, keeping the API consistent with the I/O traits used
    /// throughout the transport layer. After the transfer the sniffer retains an empty
    /// buffer whose capacity is clamped to the canonical legacy prefix length so repeated
    /// connections continue to benefit from buffer reuse.
    #[must_use = "negotiation prefix length is required to replay the handshake"]
    pub fn take_buffered_into(&mut self, target: &mut Vec<u8>) -> usize {
        target.clear();
        target.reserve_exact(self.buffered.len());
        target.extend_from_slice(&self.buffered);
        let drained = target.len();

        if self.buffered.capacity() > LEGACY_DAEMON_PREFIX_LEN {
            let mut replacement = Vec::with_capacity(LEGACY_DAEMON_PREFIX_LEN);
            mem::swap(&mut self.buffered, &mut replacement);
        } else {
            self.buffered.clear();
            if self.buffered.capacity() < LEGACY_DAEMON_PREFIX_LEN {
                self.buffered
                    .reserve_exact(LEGACY_DAEMON_PREFIX_LEN - self.buffered.capacity());
            }
        }

        drained
    }

    /// Reports the cached negotiation decision, if any.
    #[must_use]
    pub fn decision(&self) -> Option<NegotiationPrologue> {
        self.detector.decision()
    }

    /// Observes bytes that have already been read from the transport while tracking how
    /// many of them were required to determine the negotiation style.
    ///
    /// Callers that perform buffered reads or speculative peeks can forward the captured
    /// bytes to this helper instead of re-reading from the underlying transport. The sniffer
    /// mirrors [`NegotiationPrologueDetector::observe`] by consuming bytes until a definitive
    /// decision is available or the canonical legacy prefix (`@RSYNCD:`) has been fully
    /// buffered. Any remaining data in `chunk` is left untouched so higher layers can process
    /// it according to the negotiated protocol.
    #[must_use]
    pub fn observe(&mut self, chunk: &[u8]) -> (NegotiationPrologue, usize) {
        let cached = self.detector.decision();
        let needs_more_prefix_bytes = cached == Some(NegotiationPrologue::LegacyAscii)
            && !self.detector.legacy_prefix_complete();

        if chunk.is_empty() || (cached.is_some() && !needs_more_prefix_bytes) {
            return (cached.unwrap_or(NegotiationPrologue::NeedMoreData), 0);
        }

        let mut consumed = 0;

        for &byte in chunk {
            self.buffered.push(byte);
            consumed += 1;

            let decision = self.detector.observe_byte(byte);
            let needs_more_prefix_bytes = decision == NegotiationPrologue::LegacyAscii
                && !self.detector.legacy_prefix_complete();

            if decision != NegotiationPrologue::NeedMoreData && !needs_more_prefix_bytes {
                return (decision, consumed);
            }
        }

        (
            self.detector
                .decision()
                .unwrap_or(NegotiationPrologue::NeedMoreData),
            consumed,
        )
    }

    /// Observes a single byte that has already been read from the transport.
    ///
    /// The helper mirrors [`observe`](Self::observe) but keeps the common
    /// "one-octet-at-a-time" call pattern used by upstream rsync ergonomic.
    /// Callers can therefore forward individual bytes without allocating a
    /// temporary slice. The returned decision matches the value that would be
    /// produced by [`observe`](Self::observe) while ensuring at most a single
    /// byte is accounted for as consumed.
    #[must_use]
    #[inline]
    pub fn observe_byte(&mut self, byte: u8) -> NegotiationPrologue {
        let (decision, consumed) = self.observe(slice::from_ref(&byte));
        debug_assert!(consumed <= 1);
        decision
    }

    /// Clears the buffered prefix and resets the negotiation detector so the
    /// sniffer can be reused for another connection attempt.
    ///
    /// The internal buffer retains its allocation when it already matches the
    /// canonical legacy prefix length so that back-to-back legacy negotiations
    /// do not pay repeated allocations. If the buffer had previously grown
    /// beyond that size—for instance when an attacker sent a very large
    /// malformed banner before the session was aborted—the capacity is trimmed
    /// back to the prefix length to avoid carrying an unnecessarily large
    /// allocation into subsequent connections.
    pub fn reset(&mut self) {
        self.detector.reset();
        if self.buffered.capacity() > LEGACY_DAEMON_PREFIX_LEN {
            self.buffered = Vec::with_capacity(LEGACY_DAEMON_PREFIX_LEN);
        } else {
            self.buffered.clear();
        }
    }

    /// Reads from `reader` until the negotiation style can be determined.
    ///
    /// Bytes consumed during detection are appended to the internal buffer so
    /// callers can replay them into the legacy greeting parser if necessary.
    /// Once a decision has been cached, subsequent calls return immediately
    /// without performing additional I/O **unless** the exchange has been
    /// classified as legacy ASCII and the canonical `@RSYNCD:` prefix still
    /// needs to be buffered. This mirrors upstream rsync, which keeps reading
    /// until the marker has been captured so the greeting parser can reuse the
    /// already consumed bytes.
    pub fn read_from<R: Read>(&mut self, reader: &mut R) -> io::Result<NegotiationPrologue> {
        if let Some(decision) = self.detector.decision() {
            let needs_more_prefix_bytes = decision == NegotiationPrologue::LegacyAscii
                && !self.detector.legacy_prefix_complete();
            if !needs_more_prefix_bytes {
                return Ok(decision);
            }
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
                    let (decision, consumed) = self.observe(observed);
                    debug_assert_eq!(consumed, observed.len());
                    let needs_more_prefix_bytes = decision == NegotiationPrologue::LegacyAscii
                        && !self.detector.legacy_prefix_complete();
                    if decision != NegotiationPrologue::NeedMoreData && !needs_more_prefix_bytes {
                        return Ok(decision);
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => return Err(err),
            }
        }
    }

    /// Reports whether the canonical legacy prefix (`@RSYNCD:`) has already
    /// been fully observed.
    ///
    /// Legacy ASCII negotiations reuse the bytes captured during detection when
    /// parsing the daemon greeting. Higher layers therefore need to know when
    /// the marker has been buffered or ruled out so they can decide whether to
    /// keep reading from the transport before handing the accumulated bytes to
    /// the legacy greeting parser. The helper simply forwards to
    /// [`NegotiationPrologueDetector::legacy_prefix_complete`], keeping the
    /// sniffer's API in sync with the lower-level detector without exposing the
    /// internal field directly.
    #[must_use]
    pub fn legacy_prefix_complete(&self) -> bool {
        self.detector.legacy_prefix_complete()
    }

    /// Reports how many additional bytes are still required to finish buffering
    /// the canonical legacy prefix.
    ///
    /// When the detector has already classified the stream as legacy ASCII but
    /// the full `@RSYNCD:` prefix has not yet been captured, callers can use the
    /// returned count to decide whether another read is necessary before
    /// replaying the buffered bytes into the legacy greeting parser. Once the
    /// prefix has been fully observed—or when the exchange is binary—the helper
    /// yields `None`, mirroring
    /// [`NegotiationPrologueDetector::legacy_prefix_remaining`].
    #[must_use]
    pub fn legacy_prefix_remaining(&self) -> Option<usize> {
        self.detector.legacy_prefix_remaining()
    }
}

impl Default for NegotiationPrologueSniffer {
    fn default() -> Self {
        Self {
            detector: NegotiationPrologueDetector::new(),
            buffered: Vec::with_capacity(LEGACY_DAEMON_PREFIX_LEN),
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
    use std::io::{self, Cursor, Read};

    struct InterruptedOnceReader {
        inner: Cursor<Vec<u8>>,
        interrupted: bool,
    }

    impl InterruptedOnceReader {
        fn new(data: Vec<u8>) -> Self {
            Self {
                inner: Cursor::new(data),
                interrupted: false,
            }
        }

        fn was_interrupted(&self) -> bool {
            self.interrupted
        }

        fn into_inner(self) -> Cursor<Vec<u8>> {
            self.inner
        }
    }

    impl Read for InterruptedOnceReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if !self.interrupted {
                self.interrupted = true;
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "simulated EINTR during negotiation sniff",
                ));
            }

            self.inner.read(buf)
        }
    }

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
    fn observe_byte_after_reset_restarts_detection() {
        let mut detector = NegotiationPrologueDetector::new();

        for &byte in LEGACY_DAEMON_PREFIX.as_bytes() {
            assert_eq!(
                detector.observe_byte(byte),
                NegotiationPrologue::LegacyAscii
            );
        }
        assert!(detector.legacy_prefix_complete());

        detector.reset();

        assert_eq!(
            detector.observe_byte(b'@'),
            NegotiationPrologue::LegacyAscii
        );
        assert_eq!(detector.buffered_prefix(), b"@");
        assert_eq!(detector.buffered_len(), 1);
        assert_eq!(
            detector.legacy_prefix_remaining(),
            Some(LEGACY_DAEMON_PREFIX_LEN - 1)
        );
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
        assert_eq!(detector.buffered_len(), 0);
        assert_eq!(detector.legacy_prefix_remaining(), None);

        assert_eq!(detector.observe(&[0x00]), NegotiationPrologue::Binary);
        assert_eq!(detector.decision(), Some(NegotiationPrologue::Binary));
        assert_eq!(detector.legacy_prefix_remaining(), None);
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

    proptest! {
        #[test]
        fn prologue_sniffer_stays_in_lockstep_with_detector(
            chunks in prop::collection::vec(
                prop::collection::vec(any::<u8>(), 0..=LEGACY_DAEMON_PREFIX_LEN + 2),
                0..=6
            )
        ) {
            let mut detector = NegotiationPrologueDetector::new();
            let mut sniffer = NegotiationPrologueSniffer::new();

            for chunk in &chunks {
                let (sniffer_decision, consumed) = sniffer.observe(chunk);
                prop_assert!(consumed <= chunk.len());

                let detector_decision = if consumed != 0 {
                    detector.observe(&chunk[..consumed])
                } else {
                    detector
                        .decision()
                        .unwrap_or(NegotiationPrologue::NeedMoreData)
                };

                prop_assert_eq!(sniffer_decision, detector_decision);

                let detector_buffer = detector.buffered_prefix();
                prop_assert!(sniffer.buffered().starts_with(detector_buffer));
                prop_assert!(sniffer.buffered_len() >= detector.buffered_len());

                match sniffer_decision {
                    NegotiationPrologue::LegacyAscii => {
                        if detector.legacy_prefix_complete() {
                            prop_assert!(sniffer.legacy_prefix_complete());
                            prop_assert_eq!(sniffer.legacy_prefix_remaining(), None);
                            prop_assert_eq!(detector.legacy_prefix_remaining(), None);
                        } else {
                            prop_assert!(!sniffer.legacy_prefix_complete());
                            prop_assert_eq!(
                                sniffer.legacy_prefix_remaining(),
                                detector.legacy_prefix_remaining()
                            );
                        }
                    }
                    NegotiationPrologue::Binary => {
                        prop_assert!(!sniffer.legacy_prefix_complete());
                        prop_assert!(!detector.legacy_prefix_complete());
                        prop_assert_eq!(sniffer.legacy_prefix_remaining(), None);
                        prop_assert_eq!(detector.legacy_prefix_remaining(), None);
                    }
                    NegotiationPrologue::NeedMoreData => {
                        prop_assert_eq!(
                            sniffer.legacy_prefix_complete(),
                            detector.legacy_prefix_complete()
                        );
                        prop_assert_eq!(
                            sniffer.legacy_prefix_remaining(),
                            detector.legacy_prefix_remaining()
                        );
                    }
                }
            }

            prop_assert_eq!(sniffer.decision(), detector.decision());
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
    fn prologue_sniffer_preallocates_legacy_prefix_capacity() {
        let buffered = NegotiationPrologueSniffer::new().into_buffered();
        assert!(buffered.capacity() >= LEGACY_DAEMON_PREFIX_LEN);
        assert!(buffered.is_empty());
    }

    #[test]
    fn prologue_sniffer_reports_legacy_negotiation() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        let mut cursor = Cursor::new(b"@RSYNCD: 31.0\n".to_vec());

        let decision = sniffer
            .read_from(&mut cursor)
            .expect("legacy negotiation should succeed");
        assert_eq!(decision, NegotiationPrologue::LegacyAscii);
        assert_eq!(sniffer.buffered(), LEGACY_DAEMON_PREFIX.as_bytes());
        assert_eq!(sniffer.buffered_len(), LEGACY_DAEMON_PREFIX_LEN);

        assert!(sniffer.legacy_prefix_complete());
        assert_eq!(sniffer.legacy_prefix_remaining(), None);

        let mut remaining = Vec::new();
        cursor.read_to_end(&mut remaining).expect("read remainder");
        let mut replay = sniffer.into_buffered();
        replay.extend_from_slice(&remaining);
        assert_eq!(replay, b"@RSYNCD: 31.0\n");
    }

    #[test]
    fn prologue_sniffer_reports_buffered_length() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        assert_eq!(sniffer.buffered_len(), 0);

        let (decision, consumed) = sniffer.observe(b"@RS");
        assert_eq!(decision, NegotiationPrologue::LegacyAscii);
        assert_eq!(consumed, 3);
        assert_eq!(sniffer.buffered_len(), 3);

        let (decision, consumed) = sniffer.observe(b"YN");
        assert_eq!(decision, NegotiationPrologue::LegacyAscii);
        assert_eq!(consumed, 2);
        assert_eq!(sniffer.buffered_len(), 5);

        let buffered = sniffer.take_buffered();
        assert_eq!(buffered, b"@RSYN");
        assert_eq!(sniffer.buffered_len(), 0);
        assert_eq!(sniffer.buffered(), b"");
    }

    #[test]
    fn prologue_sniffer_observe_consumes_only_required_bytes() {
        let mut sniffer = NegotiationPrologueSniffer::new();

        let (decision, consumed) = sniffer.observe(b"@RS");
        assert_eq!(decision, NegotiationPrologue::LegacyAscii);
        assert_eq!(consumed, 3);
        assert_eq!(sniffer.buffered(), b"@RS");
        assert_eq!(
            sniffer.legacy_prefix_remaining(),
            Some(LEGACY_DAEMON_PREFIX_LEN - 3)
        );

        let (decision, consumed) = sniffer.observe(b"YNCD: remainder");
        assert_eq!(decision, NegotiationPrologue::LegacyAscii);
        assert_eq!(consumed, LEGACY_DAEMON_PREFIX_LEN - 3);
        assert_eq!(sniffer.buffered(), LEGACY_DAEMON_PREFIX.as_bytes());
        assert!(sniffer.legacy_prefix_complete());
        assert_eq!(sniffer.legacy_prefix_remaining(), None);

        let (decision, consumed) = sniffer.observe(b" trailing data");
        assert_eq!(decision, NegotiationPrologue::LegacyAscii);
        assert_eq!(consumed, 0);
        assert_eq!(sniffer.buffered(), LEGACY_DAEMON_PREFIX.as_bytes());
    }

    #[test]
    fn prologue_sniffer_observe_handles_prefix_mismatches() {
        let mut sniffer = NegotiationPrologueSniffer::new();

        let (decision, consumed) = sniffer.observe(b"@X remainder");
        assert_eq!(decision, NegotiationPrologue::LegacyAscii);
        assert_eq!(consumed, 2);
        assert_eq!(sniffer.buffered(), b"@X");
        assert!(sniffer.legacy_prefix_complete());
        assert_eq!(sniffer.legacy_prefix_remaining(), None);

        let (decision, consumed) = sniffer.observe(b"anything else");
        assert_eq!(decision, NegotiationPrologue::LegacyAscii);
        assert_eq!(consumed, 0);
        assert_eq!(sniffer.buffered(), b"@X");

        let replay = sniffer.into_buffered();
        assert_eq!(replay, b"@X");
    }

    #[test]
    fn prologue_sniffer_observe_byte_matches_slice_behavior() {
        let mut slice_sniffer = NegotiationPrologueSniffer::new();
        let mut byte_sniffer = NegotiationPrologueSniffer::new();

        let stream = b"@RSYNCD: 31.0\n";

        for &byte in stream {
            let (expected, consumed) = slice_sniffer.observe(slice::from_ref(&byte));
            assert!(consumed <= 1);
            let observed = byte_sniffer.observe_byte(byte);
            assert_eq!(observed, expected);
            assert_eq!(byte_sniffer.buffered(), slice_sniffer.buffered());
            assert_eq!(
                byte_sniffer.legacy_prefix_remaining(),
                slice_sniffer.legacy_prefix_remaining()
            );
        }

        assert_eq!(byte_sniffer.decision(), slice_sniffer.decision());
        assert_eq!(
            byte_sniffer.legacy_prefix_complete(),
            slice_sniffer.legacy_prefix_complete()
        );
    }

    #[test]
    fn prologue_sniffer_observe_returns_need_more_data_for_empty_chunk() {
        let mut sniffer = NegotiationPrologueSniffer::new();

        let (decision, consumed) = sniffer.observe(b"");
        assert_eq!(decision, NegotiationPrologue::NeedMoreData);
        assert_eq!(consumed, 0);
        assert!(sniffer.buffered().is_empty());
        assert_eq!(sniffer.decision(), None);

        let (decision, consumed) = sniffer.observe(b"");
        assert_eq!(decision, NegotiationPrologue::NeedMoreData);
        assert_eq!(consumed, 0);
        assert!(sniffer.buffered().is_empty());
        assert_eq!(sniffer.decision(), None);
    }

    #[test]
    fn prologue_sniffer_observe_handles_binary_detection() {
        let mut sniffer = NegotiationPrologueSniffer::new();

        let (decision, consumed) = sniffer.observe(&[0x42, 0x99, 0x00]);
        assert_eq!(decision, NegotiationPrologue::Binary);
        assert_eq!(consumed, 1);
        assert_eq!(sniffer.buffered(), &[0x42]);

        let (decision, consumed) = sniffer.observe(&[0x00]);
        assert_eq!(decision, NegotiationPrologue::Binary);
        assert_eq!(consumed, 0);
        assert_eq!(sniffer.buffered(), &[0x42]);
    }

    #[test]
    fn prologue_sniffer_reads_until_canonical_prefix_is_buffered() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        let mut cursor = Cursor::new(b"@RSYNCD: 31.0\n".to_vec());

        let decision = sniffer
            .read_from(&mut cursor)
            .expect("first byte should classify legacy negotiation");
        assert_eq!(decision, NegotiationPrologue::LegacyAscii);
        assert_eq!(sniffer.buffered(), LEGACY_DAEMON_PREFIX.as_bytes());
        assert!(sniffer.legacy_prefix_complete());
        assert_eq!(sniffer.legacy_prefix_remaining(), None);

        let position_after_prefix = cursor.position();

        let decision = sniffer
            .read_from(&mut cursor)
            .expect("cached decision should avoid extra reads once prefix buffered");
        assert_eq!(decision, NegotiationPrologue::LegacyAscii);
        assert_eq!(cursor.position(), position_after_prefix);

        let mut remaining = Vec::new();
        cursor.read_to_end(&mut remaining).expect("read remainder");
        assert_eq!(remaining, b" 31.0\n");
    }

    #[test]
    fn prologue_sniffer_take_buffered_drains_accumulated_prefix() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        let (decision, consumed) = sniffer.observe(LEGACY_DAEMON_PREFIX.as_bytes());
        assert_eq!(decision, NegotiationPrologue::LegacyAscii);
        assert_eq!(consumed, LEGACY_DAEMON_PREFIX_LEN);
        assert!(sniffer.legacy_prefix_complete());

        let buffered = sniffer.take_buffered();
        assert_eq!(buffered, LEGACY_DAEMON_PREFIX.as_bytes());
        assert!(buffered.capacity() <= LEGACY_DAEMON_PREFIX_LEN);
        assert!(sniffer.buffered().is_empty());
        assert_eq!(sniffer.decision(), Some(NegotiationPrologue::LegacyAscii));
        assert_eq!(sniffer.legacy_prefix_remaining(), None);

        sniffer.reset();
        assert!(sniffer.buffered().is_empty());
        assert_eq!(sniffer.decision(), None);
    }

    #[test]
    fn prologue_sniffer_take_buffered_into_drains_accumulated_prefix() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        let (decision, consumed) = sniffer.observe(LEGACY_DAEMON_PREFIX.as_bytes());
        assert_eq!(decision, NegotiationPrologue::LegacyAscii);
        assert_eq!(consumed, LEGACY_DAEMON_PREFIX_LEN);
        assert!(sniffer.legacy_prefix_complete());

        let mut reused = b"placeholder".to_vec();
        let drained = sniffer.take_buffered_into(&mut reused);

        assert_eq!(reused, LEGACY_DAEMON_PREFIX.as_bytes());
        assert_eq!(drained, LEGACY_DAEMON_PREFIX_LEN);
        assert!(sniffer.buffered().is_empty());
        assert_eq!(sniffer.decision(), Some(NegotiationPrologue::LegacyAscii));
        assert_eq!(sniffer.legacy_prefix_remaining(), None);

        sniffer.reset();
        assert!(sniffer.buffered().is_empty());
        assert_eq!(sniffer.decision(), None);
    }

    #[test]
    fn prologue_sniffer_take_buffered_returns_initial_binary_byte() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        let (decision, consumed) = sniffer.observe(&[0x80, 0x81, 0x82]);
        assert_eq!(decision, NegotiationPrologue::Binary);
        assert_eq!(consumed, 1);

        let buffered = sniffer.take_buffered();
        assert_eq!(buffered, [0x80]);
        assert!(buffered.capacity() <= LEGACY_DAEMON_PREFIX_LEN);
        assert!(sniffer.buffered().is_empty());
        assert_eq!(sniffer.decision(), Some(NegotiationPrologue::Binary));
    }

    #[test]
    fn prologue_sniffer_take_buffered_into_returns_initial_binary_byte() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        let (decision, consumed) = sniffer.observe(&[0x80, 0x81, 0x82]);
        assert_eq!(decision, NegotiationPrologue::Binary);
        assert_eq!(consumed, 1);

        let mut reused = Vec::with_capacity(16);
        let drained = sniffer.take_buffered_into(&mut reused);

        assert_eq!(reused, [0x80]);
        assert_eq!(drained, 1);
        assert!(sniffer.buffered().is_empty());
        assert_eq!(sniffer.decision(), Some(NegotiationPrologue::Binary));
    }

    #[test]
    fn prologue_sniffer_take_buffered_clamps_replacement_capacity() {
        let mut sniffer = NegotiationPrologueSniffer::new();

        sniffer.buffered.reserve(1024);
        assert!(sniffer.buffered.capacity() > LEGACY_DAEMON_PREFIX_LEN);

        let _ = sniffer.take_buffered();

        assert_eq!(sniffer.buffered.capacity(), LEGACY_DAEMON_PREFIX_LEN);
    }

    #[test]
    fn prologue_sniffer_take_buffered_into_clamps_replacement_capacity() {
        let mut sniffer = NegotiationPrologueSniffer::new();

        sniffer.buffered.reserve(1024);
        assert!(sniffer.buffered.capacity() > LEGACY_DAEMON_PREFIX_LEN);

        let mut reused = Vec::new();
        let drained = sniffer.take_buffered_into(&mut reused);

        assert!(reused.is_empty());
        assert_eq!(drained, 0);
        assert_eq!(sniffer.buffered.capacity(), LEGACY_DAEMON_PREFIX_LEN);
    }

    #[test]
    fn prologue_sniffer_take_buffered_into_reuses_destination_capacity() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        let (decision, _) = sniffer.observe(LEGACY_DAEMON_PREFIX.as_bytes());
        assert_eq!(decision, NegotiationPrologue::LegacyAscii);

        let mut reused = Vec::with_capacity(64);
        reused.extend_from_slice(b"some prior contents");
        let ptr = reused.as_ptr();
        let capacity_before = reused.capacity();

        let drained = sniffer.take_buffered_into(&mut reused);

        assert_eq!(reused, LEGACY_DAEMON_PREFIX.as_bytes());
        assert_eq!(reused.as_ptr(), ptr);
        assert_eq!(reused.capacity(), capacity_before);
        assert_eq!(drained, LEGACY_DAEMON_PREFIX_LEN);
    }

    #[test]
    fn prologue_sniffer_reports_binary_prefix_state() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        let mut cursor = Cursor::new(vec![0x00, 0x20, 0x00]);

        let decision = sniffer
            .read_from(&mut cursor)
            .expect("binary negotiation should succeed");
        assert_eq!(decision, NegotiationPrologue::Binary);

        assert!(!sniffer.legacy_prefix_complete());
        assert_eq!(sniffer.legacy_prefix_remaining(), None);
    }

    #[test]
    fn prologue_sniffer_reset_clears_buffer_and_state() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        let mut cursor = Cursor::new(LEGACY_DAEMON_PREFIX.as_bytes().to_vec());
        let _ = sniffer
            .read_from(&mut cursor)
            .expect("legacy negotiation should succeed");

        assert_eq!(sniffer.buffered(), LEGACY_DAEMON_PREFIX.as_bytes());
        assert_eq!(sniffer.decision(), Some(NegotiationPrologue::LegacyAscii));

        sniffer.reset();
        assert!(sniffer.buffered().is_empty());
        assert_eq!(sniffer.decision(), None);
    }

    #[test]
    fn prologue_sniffer_reset_trims_excess_capacity() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        // Inflate the backing allocation to simulate a previous oversized prefix capture.
        sniffer.buffered = Vec::with_capacity(128);
        sniffer
            .buffered
            .extend_from_slice(LEGACY_DAEMON_PREFIX.as_bytes());
        assert!(sniffer.buffered.capacity() > LEGACY_DAEMON_PREFIX_LEN);

        sniffer.reset();
        assert_eq!(sniffer.buffered.capacity(), LEGACY_DAEMON_PREFIX_LEN);
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

    #[test]
    fn prologue_sniffer_retries_after_interrupted_read() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        let mut reader = InterruptedOnceReader::new(b"@RSYNCD: 31.0\n".to_vec());

        let decision = sniffer
            .read_from(&mut reader)
            .expect("sniffer should retry after EINTR");

        assert_eq!(decision, NegotiationPrologue::LegacyAscii);
        assert!(
            reader.was_interrupted(),
            "the reader must report an interruption"
        );
        assert_eq!(sniffer.buffered(), LEGACY_DAEMON_PREFIX.as_bytes());
        assert!(sniffer.legacy_prefix_complete());

        let mut cursor = reader.into_inner();
        let mut remainder = Vec::new();
        cursor.read_to_end(&mut remainder).expect("read remainder");
        assert_eq!(remainder, b" 31.0\n");
    }
}
