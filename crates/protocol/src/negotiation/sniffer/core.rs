use std::collections::TryReserveError;

use crate::legacy::LEGACY_DAEMON_PREFIX_LEN;

use super::super::{NegotiationPrologue, NegotiationPrologueDetector};

/// Incrementally reads bytes from a [`Read`](std::io::Read) implementation until the
/// negotiation style can be determined.
///
/// Upstream rsync only needs to observe the very first octet to decide between
/// the legacy ASCII negotiation (`@RSYNCD:`) and the modern binary handshake.
/// Real transports, however, may deliver that byte in small fragments or after
/// transient `EINTR` interruptions. This helper mirrors upstream behavior while
/// providing a higher level interface that owns the buffered prefix so callers
/// can replay the bytes into the legacy greeting parser without reallocating.
#[derive(Clone, Debug)]
pub struct NegotiationPrologueSniffer {
    pub(super) detector: NegotiationPrologueDetector,
    pub(super) buffered: Vec<u8>,
    pub(super) prefix_bytes_retained: usize,
}

impl NegotiationPrologueSniffer {
    /// Creates a sniffer with an empty buffer and undecided negotiation state.
    #[must_use]
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a sniffer that reuses the caller-provided buffer for prefix storage.
    ///
    /// The allocation is cleared and its capacity is normalized to the canonical
    /// legacy prefix length so the resulting sniffer mirrors the behavior of
    /// [`Self::new`].
    #[must_use]
    #[inline]
    pub fn with_buffer(buffer: Vec<u8>) -> Self {
        let mut sniffer = Self {
            detector: NegotiationPrologueDetector::new(),
            buffered: buffer,
            prefix_bytes_retained: 0,
        };
        sniffer.reset();
        sniffer
    }

    /// Returns the buffered bytes that were consumed while detecting the
    /// negotiation style.
    #[must_use]
    #[inline]
    pub fn buffered(&self) -> &[u8] {
        &self.buffered
    }

    /// Returns the bytes buffered beyond the sniffed negotiation prefix.
    #[must_use]
    #[inline]
    pub fn buffered_remainder(&self) -> &[u8] {
        let prefix_len = self.sniffed_prefix_len();
        debug_assert!(prefix_len <= self.buffered.len());
        &self.buffered[prefix_len..]
    }

    /// Splits the buffered bytes into the sniffed prefix and the trailing remainder.
    #[must_use]
    #[inline]
    pub fn buffered_split(&self) -> (&[u8], &[u8]) {
        let prefix_len = self.sniffed_prefix_len();
        debug_assert!(prefix_len <= self.buffered.len());
        self.buffered.split_at(prefix_len)
    }

    /// Reports whether a negotiation decision has already been cached.
    #[must_use]
    #[inline]
    pub fn is_decided(&self) -> bool {
        self.detector
            .decision()
            .is_some_and(|decision| decision != NegotiationPrologue::NeedMoreData)
    }

    /// Reports whether additional bytes are required to classify the negotiation.
    #[must_use]
    #[inline]
    pub fn requires_more_data(&self) -> bool {
        self.detector
            .decision()
            .is_none_or(|decision| self.needs_more_legacy_prefix_bytes(decision))
    }

    /// Returns the number of bytes that have been buffered while sniffing the negotiation.
    #[must_use]
    #[inline]
    pub const fn buffered_len(&self) -> usize {
        self.buffered.len()
    }

    /// Attempts to reserve additional capacity for the buffered transcript.
    pub fn try_reserve_buffered(&mut self, additional: usize) -> Result<(), TryReserveError> {
        self.buffered.try_reserve(additional)
    }

    /// Returns the length of the sniffed negotiation prefix in bytes.
    #[must_use]
    #[inline]
    pub const fn sniffed_prefix_len(&self) -> usize {
        self.prefix_bytes_retained
    }

    /// Returns the canonical negotiation prefix observed so far.
    #[must_use]
    #[inline]
    pub fn sniffed_prefix(&self) -> &[u8] {
        let prefix_len = self.sniffed_prefix_len();
        &self.buffered[..prefix_len]
    }

    #[cfg(test)]
    pub(crate) const fn buffered_storage(&self) -> &Vec<u8> {
        &self.buffered
    }

    pub(crate) const fn buffered_storage_mut(&mut self) -> &mut Vec<u8> {
        &mut self.buffered
    }

    /// Rehydrates the sniffer from a previously captured negotiation snapshot.
    pub fn rehydrate_from_parts(
        &mut self,
        decision: NegotiationPrologue,
        sniffed_prefix_len: usize,
        buffered: &[u8],
    ) -> Result<(), TryReserveError> {
        self.reset();

        self.buffered.try_reserve(buffered.len())?;
        self.buffered.extend_from_slice(buffered);

        let clamped_prefix = sniffed_prefix_len
            .min(self.buffered.len())
            .min(LEGACY_DAEMON_PREFIX_LEN);
        if clamped_prefix > 0 {
            let prefix = &self.buffered[..clamped_prefix];
            let observed = self.detector.observe(prefix);
            debug_assert!(
                !decision.is_decided()
                    || observed == decision
                    || (decision.is_legacy() && observed == NegotiationPrologue::NeedMoreData),
                "rehydrated decision {observed:?} does not match snapshot {decision:?}"
            );
        } else {
            debug_assert!(
                !decision.is_decided(),
                "non-empty decision {decision:?} requires at least one sniffed byte"
            );
        }

        self.prefix_bytes_retained = clamped_prefix;
        Ok(())
    }

    /// Consumes the sniffer and returns the buffered transcript.
    #[must_use]
    pub fn into_buffered(mut self) -> Vec<u8> {
        if self.buffered.capacity() > LEGACY_DAEMON_PREFIX_LEN {
            self.buffered.shrink_to(LEGACY_DAEMON_PREFIX_LEN);
        }

        self.buffered
    }

    /// Consumes the sniffer and returns the cached decision, prefix length, and transcript.
    #[must_use]
    pub fn into_parts(mut self) -> (NegotiationPrologue, usize, Vec<u8>) {
        let decision = self
            .detector
            .decision()
            .unwrap_or(NegotiationPrologue::NeedMoreData);
        let prefix_len = self.sniffed_prefix_len();

        if self.buffered.capacity() > LEGACY_DAEMON_PREFIX_LEN {
            self.buffered.shrink_to(LEGACY_DAEMON_PREFIX_LEN);
        }

        (decision, prefix_len, self.buffered)
    }

    /// Returns the cached negotiation decision, if any.
    #[must_use]
    #[inline]
    pub const fn decision(&self) -> Option<NegotiationPrologue> {
        self.detector.decision()
    }

    /// Reports whether the exchange has been classified as legacy ASCII.
    #[must_use]
    #[inline]
    pub fn is_legacy(&self) -> bool {
        self.detector
            .decision()
            .is_some_and(|decision| decision == NegotiationPrologue::LegacyAscii)
    }

    /// Reports whether the exchange has been classified as binary.
    #[must_use]
    #[inline]
    pub fn is_binary(&self) -> bool {
        self.detector
            .decision()
            .is_some_and(|decision| decision == NegotiationPrologue::Binary)
    }
}

impl Default for NegotiationPrologueSniffer {
    fn default() -> Self {
        Self {
            detector: NegotiationPrologueDetector::new(),
            buffered: Vec::with_capacity(LEGACY_DAEMON_PREFIX_LEN),
            prefix_bytes_retained: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ==== new() tests ====

    #[test]
    fn new_creates_empty_sniffer() {
        let sniffer = NegotiationPrologueSniffer::new();
        assert!(sniffer.buffered().is_empty());
    }

    #[test]
    fn new_has_undecided_state() {
        let sniffer = NegotiationPrologueSniffer::new();
        assert!(sniffer.decision().is_none());
    }

    #[test]
    fn new_requires_more_data() {
        let sniffer = NegotiationPrologueSniffer::new();
        assert!(sniffer.requires_more_data());
    }

    #[test]
    fn new_has_capacity_for_legacy_prefix() {
        let sniffer = NegotiationPrologueSniffer::new();
        assert!(sniffer.buffered_storage().capacity() >= LEGACY_DAEMON_PREFIX_LEN);
    }

    // ==== with_buffer() tests ====

    #[test]
    fn with_buffer_reuses_allocation() {
        // Use exactly LEGACY_DAEMON_PREFIX_LEN capacity to avoid triggering
        // shrink_to() in reset_buffer_for_reuse(), which may reallocate on
        // some platforms (e.g., macOS).
        let mut buffer = Vec::with_capacity(LEGACY_DAEMON_PREFIX_LEN);
        buffer.extend_from_slice(b"data");
        let original_ptr = buffer.as_ptr();
        let sniffer = NegotiationPrologueSniffer::with_buffer(buffer);
        // Buffer is cleared but allocation reused
        assert!(sniffer.buffered().is_empty());
        // Pointer should be the same since capacity wasn't normalized
        assert_eq!(sniffer.buffered_storage().as_ptr(), original_ptr);
    }

    #[test]
    fn with_buffer_clears_content() {
        let buffer = b"@RSYNCD:31.0".to_vec();
        let sniffer = NegotiationPrologueSniffer::with_buffer(buffer);
        assert!(sniffer.buffered().is_empty());
        assert!(sniffer.decision().is_none());
    }

    #[test]
    fn with_buffer_normalizes_capacity() {
        let buffer = Vec::with_capacity(1024);
        let sniffer = NegotiationPrologueSniffer::with_buffer(buffer);
        // Capacity should be normalized (shrunk to reasonable size)
        assert!(sniffer.buffered_storage().capacity() <= LEGACY_DAEMON_PREFIX_LEN * 2);
    }

    // ==== buffered() tests ====

    #[test]
    fn buffered_empty_initially() {
        let sniffer = NegotiationPrologueSniffer::new();
        assert!(sniffer.buffered().is_empty());
    }

    #[test]
    fn buffered_returns_observed_bytes() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        sniffer.observe(b"@RSY").unwrap();
        assert_eq!(sniffer.buffered(), b"@RSY");
    }

    // ==== buffered_remainder() tests ====

    #[test]
    fn buffered_remainder_empty_when_no_remainder() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        sniffer.observe(b"@RSYNCD:").unwrap();
        // Only prefix, no remainder added
        assert!(sniffer.buffered_remainder().is_empty());
    }

    #[test]
    fn buffered_remainder_returns_bytes_beyond_prefix() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        sniffer.observe(b"@RSYNCD:").unwrap();
        sniffer.buffered_storage_mut().extend_from_slice(b" 31.0\n");
        assert_eq!(sniffer.buffered_remainder(), b" 31.0\n");
    }

    #[test]
    fn buffered_remainder_binary_prefix_is_single_byte() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        sniffer.observe(b"\x1f\x00\x00\x00rest").unwrap();
        // Binary prefix is 1 byte, remainder is the rest of buffered data
        assert_eq!(sniffer.sniffed_prefix_len(), 1);
    }

    // ==== buffered_split() tests ====

    #[test]
    fn buffered_split_with_remainder() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        sniffer.observe(b"@RSYNCD:").unwrap();
        sniffer.buffered_storage_mut().extend_from_slice(b" 31.0\n");
        let (prefix, remainder) = sniffer.buffered_split();
        assert_eq!(prefix, b"@RSYNCD:");
        assert_eq!(remainder, b" 31.0\n");
    }

    #[test]
    fn buffered_split_no_remainder() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        sniffer.observe(b"@RSYNCD:").unwrap();
        let (prefix, remainder) = sniffer.buffered_split();
        assert_eq!(prefix, b"@RSYNCD:");
        assert!(remainder.is_empty());
    }

    #[test]
    fn buffered_split_empty_before_observation() {
        let sniffer = NegotiationPrologueSniffer::new();
        let (prefix, remainder) = sniffer.buffered_split();
        assert!(prefix.is_empty());
        assert!(remainder.is_empty());
    }

    // ==== is_decided() tests ====

    #[test]
    fn is_decided_false_initially() {
        let sniffer = NegotiationPrologueSniffer::new();
        assert!(!sniffer.is_decided());
    }

    #[test]
    fn is_decided_true_after_binary() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        sniffer.observe(b"\x00").unwrap();
        assert!(sniffer.is_decided());
    }

    #[test]
    fn is_decided_true_after_legacy_prefix() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        sniffer.observe(b"@RSYNCD:").unwrap();
        assert!(sniffer.is_decided());
    }

    // ==== requires_more_data() tests ====

    #[test]
    fn requires_more_data_true_initially() {
        let sniffer = NegotiationPrologueSniffer::new();
        assert!(sniffer.requires_more_data());
    }

    #[test]
    fn requires_more_data_false_after_binary() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        sniffer.observe(b"\x00").unwrap();
        assert!(!sniffer.requires_more_data());
    }

    #[test]
    fn requires_more_data_false_after_complete_legacy() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        sniffer.observe(b"@RSYNCD:").unwrap();
        assert!(!sniffer.requires_more_data());
    }

    #[test]
    fn requires_more_data_true_for_partial_legacy() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        sniffer.observe(b"@RSY").unwrap();
        assert!(sniffer.requires_more_data());
    }

    // ==== buffered_len() tests ====

    #[test]
    fn buffered_len_zero_initially() {
        let sniffer = NegotiationPrologueSniffer::new();
        assert_eq!(sniffer.buffered_len(), 0);
    }

    #[test]
    fn buffered_len_reflects_observed_bytes() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        sniffer.observe(b"@RSY").unwrap();
        assert_eq!(sniffer.buffered_len(), 4);
    }

    #[test]
    fn buffered_len_includes_manually_added_bytes() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        sniffer.observe(b"@RSYNCD:").unwrap();
        sniffer.buffered_storage_mut().extend_from_slice(b" 31.0");
        assert_eq!(sniffer.buffered_len(), 13);
    }

    // ==== try_reserve_buffered() tests ====

    #[test]
    fn try_reserve_buffered_ensures_capacity() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        sniffer.try_reserve_buffered(100).unwrap();
        // After reserving, capacity should be at least 100
        assert!(sniffer.buffered_storage().capacity() >= 100);
    }

    #[test]
    fn try_reserve_buffered_succeeds_when_already_large() {
        let buffer = Vec::with_capacity(1024);
        let mut sniffer = NegotiationPrologueSniffer::with_buffer(buffer);
        // Even after capacity normalization, should be able to reserve more
        sniffer.try_reserve_buffered(50).unwrap();
    }

    // ==== sniffed_prefix_len() tests ====

    #[test]
    fn sniffed_prefix_len_zero_initially() {
        let sniffer = NegotiationPrologueSniffer::new();
        assert_eq!(sniffer.sniffed_prefix_len(), 0);
    }

    #[test]
    fn sniffed_prefix_len_one_for_binary() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        sniffer.observe(b"\x1f").unwrap();
        assert_eq!(sniffer.sniffed_prefix_len(), 1);
    }

    #[test]
    fn sniffed_prefix_len_eight_for_complete_legacy() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        sniffer.observe(b"@RSYNCD:").unwrap();
        assert_eq!(sniffer.sniffed_prefix_len(), LEGACY_DAEMON_PREFIX_LEN);
    }

    #[test]
    fn sniffed_prefix_len_partial_legacy() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        sniffer.observe(b"@RSY").unwrap();
        assert_eq!(sniffer.sniffed_prefix_len(), 4);
    }

    // ==== sniffed_prefix() tests ====

    #[test]
    fn sniffed_prefix_empty_initially() {
        let sniffer = NegotiationPrologueSniffer::new();
        assert!(sniffer.sniffed_prefix().is_empty());
    }

    #[test]
    fn sniffed_prefix_returns_prefix_bytes() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        sniffer.observe(b"@RSYNCD:").unwrap();
        assert_eq!(sniffer.sniffed_prefix(), b"@RSYNCD:");
    }

    #[test]
    fn sniffed_prefix_excludes_remainder() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        sniffer.observe(b"@RSYNCD:").unwrap();
        sniffer.buffered_storage_mut().extend_from_slice(b" 31.0\n");
        // sniffed_prefix() should not include the remainder
        assert_eq!(sniffer.sniffed_prefix(), b"@RSYNCD:");
    }

    // ==== rehydrate_from_parts() tests ====

    #[test]
    fn rehydrate_from_parts_restores_binary() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        sniffer
            .rehydrate_from_parts(NegotiationPrologue::Binary, 1, b"\x1f\x00\x00\x00")
            .unwrap();
        assert!(sniffer.is_binary());
        assert_eq!(sniffer.sniffed_prefix_len(), 1);
        assert_eq!(sniffer.buffered(), b"\x1f\x00\x00\x00");
    }

    #[test]
    fn rehydrate_from_parts_restores_legacy() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        sniffer
            .rehydrate_from_parts(NegotiationPrologue::LegacyAscii, 8, b"@RSYNCD: 31.0\n")
            .unwrap();
        assert!(sniffer.is_legacy());
        assert_eq!(sniffer.sniffed_prefix_len(), 8);
        assert_eq!(sniffer.buffered(), b"@RSYNCD: 31.0\n");
    }

    #[test]
    fn rehydrate_from_parts_clamps_prefix_len_to_buffer() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        // prefix_len larger than buffer length
        sniffer
            .rehydrate_from_parts(NegotiationPrologue::LegacyAscii, 100, b"@RSY")
            .unwrap();
        assert_eq!(sniffer.sniffed_prefix_len(), 4);
    }

    #[test]
    fn rehydrate_from_parts_clamps_prefix_len_to_max() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        // prefix_len larger than LEGACY_DAEMON_PREFIX_LEN
        sniffer
            .rehydrate_from_parts(
                NegotiationPrologue::LegacyAscii,
                100,
                b"@RSYNCD: 31.0\nmore data",
            )
            .unwrap();
        assert_eq!(sniffer.sniffed_prefix_len(), LEGACY_DAEMON_PREFIX_LEN);
    }

    #[test]
    fn rehydrate_from_parts_resets_first() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        sniffer.observe(b"\x00").unwrap();
        assert!(sniffer.is_binary());
        // Rehydrating with legacy should reset first
        sniffer
            .rehydrate_from_parts(NegotiationPrologue::LegacyAscii, 8, b"@RSYNCD:")
            .unwrap();
        assert!(sniffer.is_legacy());
    }

    // ==== into_buffered() tests ====

    #[test]
    fn into_buffered_returns_buffer() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        sniffer.observe(b"@RSYNCD:").unwrap();
        sniffer.buffered_storage_mut().extend_from_slice(b" 31.0\n");
        let buffer = sniffer.into_buffered();
        assert_eq!(buffer, b"@RSYNCD: 31.0\n");
    }

    #[test]
    fn into_buffered_shrinks_large_capacity() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        sniffer.try_reserve_buffered(1024).unwrap();
        sniffer.observe(b"@").unwrap();
        let buffer = sniffer.into_buffered();
        // Should shrink to reasonable capacity
        assert!(buffer.capacity() <= LEGACY_DAEMON_PREFIX_LEN * 2);
    }

    // ==== into_parts() tests ====

    #[test]
    fn into_parts_returns_binary_decision() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        sniffer.observe(b"\x1f").unwrap();
        let (decision, prefix_len, buffer) = sniffer.into_parts();
        assert_eq!(decision, NegotiationPrologue::Binary);
        assert_eq!(prefix_len, 1);
        assert_eq!(buffer, b"\x1f");
    }

    #[test]
    fn into_parts_returns_legacy_decision() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        sniffer.observe(b"@RSYNCD:").unwrap();
        let (decision, prefix_len, buffer) = sniffer.into_parts();
        assert_eq!(decision, NegotiationPrologue::LegacyAscii);
        assert_eq!(prefix_len, 8);
        assert_eq!(buffer, b"@RSYNCD:");
    }

    #[test]
    fn into_parts_returns_need_more_data_initially() {
        let sniffer = NegotiationPrologueSniffer::new();
        let (decision, prefix_len, buffer) = sniffer.into_parts();
        assert_eq!(decision, NegotiationPrologue::NeedMoreData);
        assert_eq!(prefix_len, 0);
        assert!(buffer.is_empty());
    }

    // ==== decision() tests ====

    #[test]
    fn decision_none_initially() {
        let sniffer = NegotiationPrologueSniffer::new();
        assert!(sniffer.decision().is_none());
    }

    #[test]
    fn decision_binary_after_non_at_byte() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        sniffer.observe(b"\x00").unwrap();
        assert_eq!(sniffer.decision(), Some(NegotiationPrologue::Binary));
    }

    #[test]
    fn decision_legacy_after_at_byte() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        sniffer.observe(b"@").unwrap();
        assert_eq!(sniffer.decision(), Some(NegotiationPrologue::LegacyAscii));
    }

    // ==== is_legacy() tests ====

    #[test]
    fn is_legacy_false_initially() {
        let sniffer = NegotiationPrologueSniffer::new();
        assert!(!sniffer.is_legacy());
    }

    #[test]
    fn is_legacy_true_after_at_byte() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        sniffer.observe(b"@").unwrap();
        assert!(sniffer.is_legacy());
    }

    #[test]
    fn is_legacy_false_after_binary() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        sniffer.observe(b"\x00").unwrap();
        assert!(!sniffer.is_legacy());
    }

    // ==== is_binary() tests ====

    #[test]
    fn is_binary_false_initially() {
        let sniffer = NegotiationPrologueSniffer::new();
        assert!(!sniffer.is_binary());
    }

    #[test]
    fn is_binary_true_after_non_at_byte() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        sniffer.observe(b"\x1f").unwrap();
        assert!(sniffer.is_binary());
    }

    #[test]
    fn is_binary_false_after_legacy() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        sniffer.observe(b"@").unwrap();
        assert!(!sniffer.is_binary());
    }

    // ==== Default impl tests ====

    #[test]
    fn default_matches_new() {
        let default_sniffer = NegotiationPrologueSniffer::default();
        let new_sniffer = NegotiationPrologueSniffer::new();
        assert_eq!(
            default_sniffer.buffered().len(),
            new_sniffer.buffered().len()
        );
        assert!(default_sniffer.decision().is_none());
        assert!(new_sniffer.decision().is_none());
    }

    // ==== Clone/Debug tests ====

    #[test]
    fn clone_preserves_state() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        sniffer.observe(b"@RSY").unwrap();
        let cloned = sniffer.clone();
        assert_eq!(cloned.buffered(), sniffer.buffered());
        assert_eq!(cloned.decision(), sniffer.decision());
        assert_eq!(cloned.sniffed_prefix_len(), sniffer.sniffed_prefix_len());
    }

    #[test]
    fn debug_format_contains_expected_info() {
        let sniffer = NegotiationPrologueSniffer::new();
        let debug = format!("{sniffer:?}");
        assert!(debug.contains("NegotiationPrologueSniffer"));
    }
}
