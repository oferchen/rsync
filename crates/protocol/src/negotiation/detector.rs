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
