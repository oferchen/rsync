use ::core::slice;
use std::collections::TryReserveError;
use std::io::{self, Read};

use crate::legacy::LEGACY_DAEMON_PREFIX_LEN;

use super::super::NegotiationPrologue;
use super::{NegotiationPrologueSniffer, map_reserve_error_for_io};

impl NegotiationPrologueSniffer {
    /// Observes a chunk of bytes that were read from the transport.
    pub fn observe(
        &mut self,
        chunk: &[u8],
    ) -> Result<(NegotiationPrologue, usize), TryReserveError> {
        let cached = self.detector.decision();
        let needs_more_prefix_bytes =
            cached.is_some_and(|decision| self.needs_more_legacy_prefix_bytes(decision));

        if chunk.is_empty() {
            if needs_more_prefix_bytes {
                return Ok((NegotiationPrologue::NeedMoreData, 0));
            }

            return Ok((cached.unwrap_or(NegotiationPrologue::NeedMoreData), 0));
        }

        if let Some(decision) = cached.filter(|_| !needs_more_prefix_bytes) {
            return Ok((decision, 0));
        }

        let planned = self.planned_prefix_bytes_for_observation(cached, chunk.len());
        if planned > 0 {
            self.buffered.try_reserve(planned)?;
        }

        let mut consumed = 0;

        for &byte in chunk {
            self.buffered.push(byte);
            consumed += 1;

            let decision = self.detector.observe_byte(byte);
            self.update_prefix_retention();
            let needs_more_prefix_bytes = self.needs_more_legacy_prefix_bytes(decision);

            if decision != NegotiationPrologue::NeedMoreData && !needs_more_prefix_bytes {
                return Ok((decision, consumed));
            }
        }

        let final_decision = self.detector.decision();
        if final_decision.is_some_and(|decision| self.needs_more_legacy_prefix_bytes(decision)) {
            Ok((NegotiationPrologue::NeedMoreData, consumed))
        } else {
            Ok((
                final_decision.unwrap_or(NegotiationPrologue::NeedMoreData),
                consumed,
            ))
        }
    }

    /// Observes a single byte that has already been read from the transport.
    #[inline]
    pub fn observe_byte(&mut self, byte: u8) -> Result<NegotiationPrologue, TryReserveError> {
        let (decision, consumed) = self.observe(slice::from_ref(&byte))?;
        debug_assert!(consumed <= 1);
        Ok(decision)
    }

    /// Clears the buffered prefix and resets the negotiation detector so the sniffer can be reused.
    pub fn reset(&mut self) {
        self.detector.reset();
        self.reset_buffer_for_reuse();
    }

    /// Reads from `reader` until the negotiation style can be determined.
    pub fn read_from<R: Read>(&mut self, reader: &mut R) -> io::Result<NegotiationPrologue> {
        match self.detector.decision() {
            Some(decision) if !self.needs_more_legacy_prefix_bytes(decision) => {
                return Ok(decision);
            }
            _ => {}
        }

        let mut scratch = [0u8; LEGACY_DAEMON_PREFIX_LEN];

        loop {
            let cached = self.detector.decision();
            let needs_more_prefix_bytes =
                cached.is_some_and(|decision| self.needs_more_legacy_prefix_bytes(decision));
            if let Some(decision) = cached.filter(|_| !needs_more_prefix_bytes) {
                return Ok(decision);
            }

            let bytes_to_read = if needs_more_prefix_bytes {
                LEGACY_DAEMON_PREFIX_LEN - self.detector.buffered_len()
            } else {
                1
            };

            match reader.read(&mut scratch[..bytes_to_read]) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "connection closed before rsync negotiation prologue was determined",
                    ));
                }
                Ok(read) => {
                    let observed = &scratch[..read];
                    let (decision, consumed) =
                        self.observe(observed).map_err(map_reserve_error_for_io)?;
                    debug_assert!(consumed <= observed.len());

                    if consumed < observed.len() {
                        let remainder = &observed[consumed..];
                        if !remainder.is_empty() {
                            self.buffered
                                .try_reserve_exact(remainder.len())
                                .map_err(map_reserve_error_for_io)?;
                            self.buffered.extend_from_slice(remainder);
                        }
                    }
                    let needs_more_prefix_bytes = self.needs_more_legacy_prefix_bytes(decision);
                    if decision != NegotiationPrologue::NeedMoreData && !needs_more_prefix_bytes {
                        return Ok(decision);
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => return Err(err),
            }
        }
    }

    /// Reports whether the canonical legacy prefix (`@RSYNCD:`) has already been fully observed.
    #[must_use]
    #[inline]
    pub fn legacy_prefix_complete(&self) -> bool {
        self.detector.legacy_prefix_complete()
    }

    /// Reports how many additional bytes are still required to finish buffering the canonical prefix.
    #[must_use]
    #[inline]
    pub fn legacy_prefix_remaining(&self) -> Option<usize> {
        self.detector.legacy_prefix_remaining()
    }

    #[inline]
    pub(super) fn needs_more_legacy_prefix_bytes(&self, decision: NegotiationPrologue) -> bool {
        decision == NegotiationPrologue::LegacyAscii && !self.detector.legacy_prefix_complete()
    }

    pub(super) fn reset_buffer_for_reuse(&mut self) {
        self.buffered.clear();
        self.prefix_bytes_retained = 0;

        if self.buffered.capacity() > LEGACY_DAEMON_PREFIX_LEN {
            self.buffered.shrink_to(LEGACY_DAEMON_PREFIX_LEN);
        }

        if self.buffered.capacity() < LEGACY_DAEMON_PREFIX_LEN {
            let required = LEGACY_DAEMON_PREFIX_LEN.saturating_sub(self.buffered.len());
            if required > 0 {
                let _ = self.buffered.try_reserve_exact(required);
            }
        }
    }

    #[inline]
    fn planned_prefix_bytes_for_observation(
        &self,
        cached: Option<NegotiationPrologue>,
        chunk_len: usize,
    ) -> usize {
        match cached {
            None | Some(NegotiationPrologue::NeedMoreData) => chunk_len,
            Some(NegotiationPrologue::LegacyAscii) => {
                let observed = self.detector.buffered_len();
                let remaining = LEGACY_DAEMON_PREFIX_LEN.saturating_sub(observed);
                remaining.min(chunk_len)
            }
            Some(NegotiationPrologue::Binary) => {
                let chunk_len = chunk_len.min(LEGACY_DAEMON_PREFIX_LEN);
                let observed = self.detector.buffered_len();
                let remaining = LEGACY_DAEMON_PREFIX_LEN.saturating_sub(observed);
                chunk_len.min(remaining)
            }
        }
    }

    pub(super) fn update_prefix_retention(&mut self) {
        self.prefix_bytes_retained = match self.detector.decision() {
            Some(NegotiationPrologue::LegacyAscii) | Some(NegotiationPrologue::NeedMoreData) => {
                self.detector
                    .buffered_len()
                    .min(self.buffered.len())
                    .min(LEGACY_DAEMON_PREFIX_LEN)
            }
            Some(NegotiationPrologue::Binary) => self.buffered.len().min(1),
            None => 0,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    // ==== observe tests ====

    #[test]
    fn observe_binary_first_byte() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        let (decision, consumed) = sniffer.observe(b"\x00").unwrap();
        assert_eq!(decision, NegotiationPrologue::Binary);
        assert_eq!(consumed, 1);
    }

    #[test]
    fn observe_legacy_first_byte() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        let (decision, consumed) = sniffer.observe(b"@").unwrap();
        // After just '@', we need more data to confirm legacy
        assert_eq!(decision, NegotiationPrologue::NeedMoreData);
        assert_eq!(consumed, 1);
    }

    #[test]
    fn observe_full_legacy_prefix() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        let (decision, consumed) = sniffer.observe(b"@RSYNCD:").unwrap();
        assert_eq!(decision, NegotiationPrologue::LegacyAscii);
        assert_eq!(consumed, 8);
    }

    #[test]
    fn observe_empty_chunk_returns_need_more_data() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        let (decision, consumed) = sniffer.observe(b"").unwrap();
        assert_eq!(decision, NegotiationPrologue::NeedMoreData);
        assert_eq!(consumed, 0);
    }

    #[test]
    fn observe_empty_chunk_after_decision_returns_cached() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        sniffer.observe(b"\x00\x00\x00\x1f").unwrap();
        let (decision, consumed) = sniffer.observe(b"").unwrap();
        assert_eq!(decision, NegotiationPrologue::Binary);
        assert_eq!(consumed, 0);
    }

    #[test]
    fn observe_incremental_legacy_detection() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        // Feed bytes one at a time
        for (i, byte) in b"@RSYNCD:".iter().enumerate() {
            let (decision, consumed) = sniffer.observe(slice::from_ref(byte)).unwrap();
            assert_eq!(consumed, 1);
            if i < 7 {
                assert_eq!(decision, NegotiationPrologue::NeedMoreData);
            } else {
                assert_eq!(decision, NegotiationPrologue::LegacyAscii);
            }
        }
    }

    #[test]
    fn observe_consumes_only_needed_bytes_for_binary() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        // Binary decision is made on first non-'@' byte
        let (decision, consumed) = sniffer.observe(b"\x01extra_data").unwrap();
        assert_eq!(decision, NegotiationPrologue::Binary);
        assert_eq!(consumed, 1);
    }

    #[test]
    fn observe_consumes_full_prefix_for_legacy() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        let (decision, consumed) = sniffer.observe(b"@RSYNCD: 31.0\n").unwrap();
        assert_eq!(decision, NegotiationPrologue::LegacyAscii);
        assert_eq!(consumed, 8);
    }

    #[test]
    fn observe_does_not_consume_after_decision() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        sniffer.observe(b"\x00").unwrap();
        let (decision, consumed) = sniffer.observe(b"more_data").unwrap();
        assert_eq!(decision, NegotiationPrologue::Binary);
        assert_eq!(consumed, 0);
    }

    // ==== observe_byte tests ====

    #[test]
    fn observe_byte_binary() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        let decision = sniffer.observe_byte(0x00).unwrap();
        assert_eq!(decision, NegotiationPrologue::Binary);
    }

    #[test]
    fn observe_byte_legacy_start() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        let decision = sniffer.observe_byte(b'@').unwrap();
        assert_eq!(decision, NegotiationPrologue::NeedMoreData);
    }

    #[test]
    fn observe_byte_incremental() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        for &byte in b"@RSYNC" {
            let decision = sniffer.observe_byte(byte).unwrap();
            assert_eq!(decision, NegotiationPrologue::NeedMoreData);
        }
        sniffer.observe_byte(b'D').unwrap();
        let decision = sniffer.observe_byte(b':').unwrap();
        assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    }

    // ==== reset tests ====

    #[test]
    fn reset_clears_state() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        sniffer.observe(b"\x00\x00\x00\x1f").unwrap();
        assert!(sniffer.is_decided());
        sniffer.reset();
        assert!(!sniffer.is_decided());
        assert!(sniffer.buffered().is_empty());
    }

    #[test]
    fn reset_allows_reuse() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        sniffer.observe(b"\x00").unwrap();
        assert!(sniffer.is_binary());
        sniffer.reset();
        sniffer.observe(b"@RSYNCD:").unwrap();
        assert!(sniffer.is_legacy());
    }

    // ==== read_from tests ====

    #[test]
    fn read_from_binary() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        let mut reader = Cursor::new(b"\x00\x00\x00\x1f".to_vec());
        let decision = sniffer.read_from(&mut reader).unwrap();
        assert_eq!(decision, NegotiationPrologue::Binary);
    }

    #[test]
    fn read_from_legacy() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        let mut reader = Cursor::new(b"@RSYNCD: 31.0\n".to_vec());
        let decision = sniffer.read_from(&mut reader).unwrap();
        assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    }

    #[test]
    fn read_from_eof_before_decision() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        let mut reader = Cursor::new(Vec::new());
        let result = sniffer.read_from(&mut reader);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn read_from_partial_legacy_eof() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        let mut reader = Cursor::new(b"@RSY".to_vec());
        let result = sniffer.read_from(&mut reader);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn read_from_returns_cached_decision() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        sniffer.observe(b"\x00").unwrap();
        let mut reader = Cursor::new(Vec::new());
        let decision = sniffer.read_from(&mut reader).unwrap();
        assert_eq!(decision, NegotiationPrologue::Binary);
    }

    // ==== legacy_prefix_complete tests ====

    #[test]
    fn legacy_prefix_complete_false_initially() {
        let sniffer = NegotiationPrologueSniffer::new();
        assert!(!sniffer.legacy_prefix_complete());
    }

    #[test]
    fn legacy_prefix_complete_false_partial() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        sniffer.observe(b"@RSY").unwrap();
        assert!(!sniffer.legacy_prefix_complete());
    }

    #[test]
    fn legacy_prefix_complete_true_full() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        sniffer.observe(b"@RSYNCD:").unwrap();
        assert!(sniffer.legacy_prefix_complete());
    }

    // ==== legacy_prefix_remaining tests ====

    #[test]
    fn legacy_prefix_remaining_none_before_start() {
        let sniffer = NegotiationPrologueSniffer::new();
        assert!(sniffer.legacy_prefix_remaining().is_none());
    }

    #[test]
    fn legacy_prefix_remaining_some_partial() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        sniffer.observe(b"@RSY").unwrap();
        let remaining = sniffer.legacy_prefix_remaining();
        assert!(remaining.is_some());
        assert_eq!(remaining.unwrap(), 4);
    }

    #[test]
    fn legacy_prefix_remaining_none_after_complete() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        sniffer.observe(b"@RSYNCD:").unwrap();
        // Once prefix is complete, legacy_prefix_remaining() returns None
        // (it only returns Some(n) while prefix is incomplete)
        assert!(sniffer.legacy_prefix_remaining().is_none());
    }

    // ==== State transition tests ====

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
    fn is_decided_true_after_legacy() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        sniffer.observe(b"@RSYNCD:").unwrap();
        assert!(sniffer.is_decided());
    }

    #[test]
    fn is_decided_partial_legacy_is_technically_decided() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        sniffer.observe(b"@RSY").unwrap();
        // After seeing '@', detector decides it's legacy even though more bytes needed
        // is_decided() returns true because decision is LegacyAscii, not NeedMoreData
        // But requires_more_data() returns true because prefix is incomplete
        assert!(sniffer.is_decided());
        assert!(sniffer.requires_more_data());
    }

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
    fn requires_more_data_false_after_full_legacy() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        sniffer.observe(b"@RSYNCD:").unwrap();
        assert!(!sniffer.requires_more_data());
    }

    #[test]
    fn requires_more_data_true_partial_legacy() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        sniffer.observe(b"@RSY").unwrap();
        assert!(sniffer.requires_more_data());
    }
}
