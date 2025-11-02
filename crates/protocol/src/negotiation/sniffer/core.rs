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
    pub fn buffered_len(&self) -> usize {
        self.buffered.len()
    }

    /// Attempts to reserve additional capacity for the buffered transcript.
    pub fn try_reserve_buffered(&mut self, additional: usize) -> Result<(), TryReserveError> {
        self.buffered.try_reserve(additional)
    }

    /// Returns the length of the sniffed negotiation prefix in bytes.
    #[must_use]
    #[inline]
    pub fn sniffed_prefix_len(&self) -> usize {
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
    pub(crate) fn buffered_storage(&self) -> &Vec<u8> {
        &self.buffered
    }

    pub(crate) fn buffered_storage_mut(&mut self) -> &mut Vec<u8> {
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
    pub fn decision(&self) -> Option<NegotiationPrologue> {
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
