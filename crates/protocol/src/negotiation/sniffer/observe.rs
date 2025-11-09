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
