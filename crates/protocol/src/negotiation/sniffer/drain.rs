use ::core::mem;
use std::collections::TryReserveError;
use std::io::{self, IoSliceMut, Write};

use crate::legacy::LEGACY_DAEMON_PREFIX_LEN;

use super::super::BufferedPrefixTooSmall;
use super::NegotiationPrologueSniffer;
use super::util::{copy_into_vectored, ensure_vec_capacity};

impl NegotiationPrologueSniffer {
    /// Drains the buffered bytes while keeping the sniffer available for reuse.
    #[must_use = "buffered negotiation bytes must be replayed"]
    pub fn take_buffered(&mut self) -> Vec<u8> {
        if self.requires_more_data() {
            if self.buffered.capacity() > LEGACY_DAEMON_PREFIX_LEN {
                self.buffered.shrink_to(LEGACY_DAEMON_PREFIX_LEN);
            }
            return Vec::new();
        }

        let target_capacity = self.buffered.capacity().min(LEGACY_DAEMON_PREFIX_LEN);
        let mut drained = Vec::with_capacity(target_capacity);
        mem::swap(&mut self.buffered, &mut drained);
        self.reset_buffer_for_reuse();

        drained.shrink_to(LEGACY_DAEMON_PREFIX_LEN);
        drained
    }

    /// Drains only the sniffed negotiation prefix into an existing vector while preserving the
    /// buffered remainder.
    pub fn take_sniffed_prefix_into(
        &mut self,
        target: &mut Vec<u8>,
    ) -> Result<usize, TryReserveError> {
        if self.requires_more_data() {
            return Ok(0);
        }

        let prefix_len = self.sniffed_prefix_len();
        if prefix_len == 0 {
            target.clear();
            return Ok(0);
        }

        ensure_vec_capacity(target, prefix_len)?;

        target.clear();
        target.extend_from_slice(&self.buffered[..prefix_len]);
        self.buffered.drain(..prefix_len);
        self.prefix_bytes_retained = 0;

        Ok(prefix_len)
    }

    /// Returns the sniffed negotiation prefix as an owned vector while preserving any buffered
    /// remainder.
    #[must_use = "the drained negotiation prefix must be replayed"]
    pub fn take_sniffed_prefix(&mut self) -> Vec<u8> {
        if self.requires_more_data() {
            return Vec::new();
        }

        let prefix_len = self.sniffed_prefix_len();
        if prefix_len == 0 {
            return Vec::new();
        }

        let mut drained = Vec::with_capacity(prefix_len);
        drained.extend_from_slice(&self.buffered[..prefix_len]);
        self.buffered.drain(..prefix_len);
        self.prefix_bytes_retained = 0;

        drained
    }

    /// Copies the sniffed negotiation prefix into a caller-provided slice while preserving the
    /// buffered remainder.
    pub fn take_sniffed_prefix_into_slice(
        &mut self,
        target: &mut [u8],
    ) -> Result<usize, BufferedPrefixTooSmall> {
        if self.requires_more_data() {
            return Ok(0);
        }

        let prefix_len = self.sniffed_prefix_len();
        if prefix_len == 0 {
            return Ok(0);
        }

        if target.len() < prefix_len {
            return Err(BufferedPrefixTooSmall::new(prefix_len, target.len()));
        }

        target[..prefix_len].copy_from_slice(&self.buffered[..prefix_len]);
        self.buffered.drain(..prefix_len);
        self.prefix_bytes_retained = 0;

        Ok(prefix_len)
    }

    /// Copies the sniffed negotiation prefix into a caller-provided array while preserving any
    /// buffered remainder.
    pub fn take_sniffed_prefix_into_array<const N: usize>(
        &mut self,
        target: &mut [u8; N],
    ) -> Result<usize, BufferedPrefixTooSmall> {
        self.take_sniffed_prefix_into_slice(target.as_mut_slice())
    }

    /// Writes the sniffed negotiation prefix to the provided [`Write`] implementation while
    /// preserving any buffered remainder.
    pub fn take_sniffed_prefix_into_writer<W: Write>(
        &mut self,
        target: &mut W,
    ) -> io::Result<usize> {
        if self.requires_more_data() {
            return Ok(0);
        }

        let prefix_len = self.sniffed_prefix_len();
        if prefix_len == 0 {
            return Ok(0);
        }

        target.write_all(&self.buffered[..prefix_len])?;
        self.buffered.drain(..prefix_len);
        self.prefix_bytes_retained = 0;

        Ok(prefix_len)
    }

    /// Drains the buffered bytes (including any remainder beyond the detection prefix) into an
    /// existing vector supplied by the caller.
    pub fn take_buffered_into(&mut self, target: &mut Vec<u8>) -> Result<usize, TryReserveError> {
        if self.requires_more_data() {
            if self.buffered.capacity() > LEGACY_DAEMON_PREFIX_LEN {
                self.buffered.shrink_to(LEGACY_DAEMON_PREFIX_LEN);
            }
            return Ok(0);
        }

        let required = self.buffered.len();

        if target.capacity() < required {
            target.clear();
            mem::swap(&mut self.buffered, target);
            let drained = target.len();
            self.reset_buffer_for_reuse();

            return Ok(drained);
        }

        ensure_vec_capacity(target, required)?;
        target.clear();
        target.extend_from_slice(&self.buffered);
        let drained = target.len();
        self.reset_buffer_for_reuse();

        Ok(drained)
    }

    /// Drains the buffered bytes (prefix and any captured remainder) into the caller-provided
    /// slice without allocating.
    pub fn take_buffered_into_slice(
        &mut self,
        target: &mut [u8],
    ) -> Result<usize, BufferedPrefixTooSmall> {
        if self.requires_more_data() {
            return Ok(0);
        }

        let required = self.buffered.len();
        if target.len() < required {
            return Err(BufferedPrefixTooSmall::new(required, target.len()));
        }

        target[..required].copy_from_slice(&self.buffered);
        self.reset_buffer_for_reuse();

        Ok(required)
    }

    /// Drains the buffered bytes into a vectored slice supplied by the caller without allocating.
    #[inline]
    pub fn take_buffered_into_vectored(
        &mut self,
        targets: &mut [IoSliceMut<'_>],
    ) -> Result<usize, BufferedPrefixTooSmall> {
        if self.requires_more_data() {
            return Ok(0);
        }

        let required = self.buffered.len();
        copy_into_vectored(&self.buffered, targets)?;
        self.reset_buffer_for_reuse();

        Ok(required)
    }

    /// Drains the buffered bytes into an array supplied by the caller without allocating.
    pub fn take_buffered_into_array<const N: usize>(
        &mut self,
        target: &mut [u8; N],
    ) -> Result<usize, BufferedPrefixTooSmall> {
        self.take_buffered_into_slice(target.as_mut_slice())
    }

    /// Drains the buffered bytes into an arbitrary [`Write`] implementation without allocating.
    pub fn take_buffered_into_writer<W: Write>(&mut self, target: &mut W) -> io::Result<usize> {
        if self.requires_more_data() {
            return Ok(0);
        }

        target.write_all(&self.buffered)?;
        let written = self.buffered.len();
        self.reset_buffer_for_reuse();

        Ok(written)
    }

    /// Drains the sniffed negotiation prefix and any buffered remainder into two separate vectors.
    pub fn take_buffered_split_into(
        &mut self,
        prefix: &mut Vec<u8>,
        remainder: &mut Vec<u8>,
    ) -> Result<(usize, usize), TryReserveError> {
        if self.requires_more_data() {
            if self.buffered.capacity() > LEGACY_DAEMON_PREFIX_LEN {
                self.buffered.shrink_to(LEGACY_DAEMON_PREFIX_LEN);
            }
            return Ok((0, 0));
        }

        let prefix_len = self.sniffed_prefix_len();
        let remainder_len = self.buffered.len().saturating_sub(prefix_len);

        ensure_vec_capacity(prefix, prefix_len)?;
        ensure_vec_capacity(remainder, remainder_len)?;

        prefix.clear();
        prefix.extend_from_slice(&self.buffered[..prefix_len]);

        remainder.clear();
        remainder.extend_from_slice(&self.buffered[prefix_len..]);

        self.reset_buffer_for_reuse();

        Ok((prefix_len, remainder_len))
    }

    /// Drains the buffered remainder while retaining the sniffed negotiation prefix.
    #[inline]
    pub fn take_buffered_remainder(&mut self) -> Vec<u8> {
        let prefix_len = self.sniffed_prefix_len();
        if self.buffered.len() <= prefix_len {
            return Vec::new();
        }

        let remainder = self.buffered.split_off(prefix_len);
        self.update_prefix_retention();

        remainder
    }

    /// Drains the buffered remainder into an existing vector supplied by the caller.
    #[inline]
    pub fn take_buffered_remainder_into(
        &mut self,
        target: &mut Vec<u8>,
    ) -> Result<usize, TryReserveError> {
        let prefix_len = self.sniffed_prefix_len();
        if self.buffered.len() <= prefix_len {
            target.clear();
            return Ok(0);
        }

        let remainder_len = self.buffered.len() - prefix_len;
        ensure_vec_capacity(target, remainder_len)?;
        target.clear();
        target.extend_from_slice(&self.buffered[prefix_len..]);
        self.buffered.truncate(prefix_len);
        self.update_prefix_retention();

        Ok(remainder_len)
    }

    /// Drains the buffered remainder into the provided slice while retaining the sniffed prefix.
    #[inline]
    pub fn take_buffered_remainder_into_slice(
        &mut self,
        target: &mut [u8],
    ) -> Result<usize, BufferedPrefixTooSmall> {
        let prefix_len = self.sniffed_prefix_len();
        if self.buffered.len() <= prefix_len {
            return Ok(0);
        }

        let remainder_len = self.buffered.len() - prefix_len;
        if target.len() < remainder_len {
            return Err(BufferedPrefixTooSmall::new(remainder_len, target.len()));
        }

        target[..remainder_len].copy_from_slice(&self.buffered[prefix_len..]);
        self.buffered.truncate(prefix_len);
        self.update_prefix_retention();

        Ok(remainder_len)
    }

    /// Drains the buffered remainder into a vectored slice supplied by the caller.
    #[inline]
    pub fn take_buffered_remainder_into_vectored(
        &mut self,
        targets: &mut [IoSliceMut<'_>],
    ) -> Result<usize, BufferedPrefixTooSmall> {
        let prefix_len = self.sniffed_prefix_len();
        if self.buffered.len() <= prefix_len {
            return Ok(0);
        }

        let remainder = &self.buffered[prefix_len..];
        copy_into_vectored(remainder, targets)?;
        let remainder_len = remainder.len();
        self.buffered.truncate(prefix_len);
        self.update_prefix_retention();

        Ok(remainder_len)
    }

    /// Drains the buffered remainder into a caller-provided array while retaining the sniffed prefix.
    pub fn take_buffered_remainder_into_array<const N: usize>(
        &mut self,
        target: &mut [u8; N],
    ) -> Result<usize, BufferedPrefixTooSmall> {
        self.take_buffered_remainder_into_slice(target.as_mut_slice())
    }

    /// Drains the buffered remainder into the provided [`Write`] implementation while retaining the
    /// sniffed negotiation prefix.
    #[inline]
    pub fn take_buffered_remainder_into_writer<W: Write>(
        &mut self,
        target: &mut W,
    ) -> io::Result<usize> {
        let prefix_len = self.sniffed_prefix_len();
        if self.buffered.len() <= prefix_len {
            return Ok(0);
        }

        let remainder_len = self.buffered.len() - prefix_len;
        target.write_all(&self.buffered[prefix_len..])?;
        self.buffered.truncate(prefix_len);
        self.update_prefix_retention();

        Ok(remainder_len)
    }

    /// Drops the sniffed negotiation prefix while retaining any buffered remainder.
    #[must_use]
    #[inline]
    pub fn discard_sniffed_prefix(&mut self) -> usize {
        let prefix_len = self.sniffed_prefix_len();
        if prefix_len == 0 {
            return 0;
        }

        self.buffered.drain(..prefix_len);
        self.prefix_bytes_retained = 0;

        prefix_len
    }
}
