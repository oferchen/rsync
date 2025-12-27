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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    // Helper to create a sniffer that has observed binary negotiation (first byte != '@')
    fn create_binary_sniffer() -> NegotiationPrologueSniffer {
        let mut sniffer = NegotiationPrologueSniffer::new();
        sniffer.observe(b"\x00\x00\x00\x1f").unwrap();
        sniffer
    }

    // Helper to create a sniffer that has observed legacy negotiation prefix
    fn create_legacy_sniffer() -> NegotiationPrologueSniffer {
        let mut sniffer = NegotiationPrologueSniffer::new();
        sniffer.observe(b"@RSYNCD:").unwrap();
        sniffer
    }

    // Helper to create a sniffer with remainder beyond prefix
    // Note: observe() only buffers the prefix, so we manually extend the buffer
    fn create_sniffer_with_remainder() -> NegotiationPrologueSniffer {
        let mut sniffer = NegotiationPrologueSniffer::new();
        sniffer.observe(b"@RSYNCD:").unwrap();
        // Manually add remainder beyond the prefix (simulates buffered unconsumed data)
        sniffer.buffered_storage_mut().extend_from_slice(b" 31.0\n");
        sniffer
    }

    // Helper to create undecided sniffer (no data observed)
    fn create_undecided_sniffer() -> NegotiationPrologueSniffer {
        NegotiationPrologueSniffer::new()
    }

    // ==== take_buffered tests ====

    #[test]
    fn take_buffered_returns_vec_for_binary() {
        let mut sniffer = create_binary_sniffer();
        let buffered = sniffer.take_buffered();
        assert!(!buffered.is_empty());
        assert_eq!(buffered[0], 0x00);
    }

    #[test]
    fn take_buffered_returns_vec_for_legacy() {
        let mut sniffer = create_legacy_sniffer();
        let buffered = sniffer.take_buffered();
        assert_eq!(&buffered, b"@RSYNCD:");
    }

    #[test]
    fn take_buffered_returns_empty_when_undecided() {
        let mut sniffer = create_undecided_sniffer();
        let buffered = sniffer.take_buffered();
        assert!(buffered.is_empty());
    }

    #[test]
    fn take_buffered_clears_internal_buffer() {
        let mut sniffer = create_binary_sniffer();
        let _ = sniffer.take_buffered();
        assert!(sniffer.buffered().is_empty());
    }

    #[test]
    fn take_buffered_includes_remainder() {
        let mut sniffer = create_sniffer_with_remainder();
        let buffered = sniffer.take_buffered();
        assert!(buffered.starts_with(b"@RSYNCD:"));
        assert!(buffered.len() > 8);
    }

    // ==== take_sniffed_prefix tests ====

    #[test]
    fn take_sniffed_prefix_returns_prefix_only() {
        let mut sniffer = create_sniffer_with_remainder();
        let prefix = sniffer.take_sniffed_prefix();
        assert_eq!(&prefix, b"@RSYNCD:");
    }

    #[test]
    fn take_sniffed_prefix_preserves_remainder() {
        let mut sniffer = create_sniffer_with_remainder();
        let _ = sniffer.take_sniffed_prefix();
        // Remainder should still be accessible
        let remainder = sniffer.buffered();
        assert!(remainder.starts_with(b" 31.0"));
    }

    #[test]
    fn take_sniffed_prefix_returns_empty_when_undecided() {
        let mut sniffer = create_undecided_sniffer();
        let prefix = sniffer.take_sniffed_prefix();
        assert!(prefix.is_empty());
    }

    #[test]
    fn take_sniffed_prefix_returns_single_byte_for_binary() {
        let mut sniffer = create_binary_sniffer();
        let prefix = sniffer.take_sniffed_prefix();
        assert_eq!(prefix.len(), 1);
        assert_eq!(prefix[0], 0x00);
    }

    // ==== take_sniffed_prefix_into tests ====

    #[test]
    fn take_sniffed_prefix_into_fills_target() {
        let mut sniffer = create_legacy_sniffer();
        let mut target = Vec::new();
        let len = sniffer.take_sniffed_prefix_into(&mut target).unwrap();
        assert_eq!(len, 8);
        assert_eq!(&target, b"@RSYNCD:");
    }

    #[test]
    fn take_sniffed_prefix_into_returns_zero_when_undecided() {
        let mut sniffer = create_undecided_sniffer();
        let mut target = Vec::new();
        let len = sniffer.take_sniffed_prefix_into(&mut target).unwrap();
        assert_eq!(len, 0);
    }

    #[test]
    fn take_sniffed_prefix_into_clears_target() {
        let mut sniffer = create_legacy_sniffer();
        let mut target = vec![1, 2, 3, 4, 5];
        let _ = sniffer.take_sniffed_prefix_into(&mut target).unwrap();
        assert_eq!(&target, b"@RSYNCD:");
    }

    // ==== take_sniffed_prefix_into_slice tests ====

    #[test]
    fn take_sniffed_prefix_into_slice_copies_to_slice() {
        let mut sniffer = create_legacy_sniffer();
        let mut buf = [0u8; 16];
        let len = sniffer.take_sniffed_prefix_into_slice(&mut buf).unwrap();
        assert_eq!(len, 8);
        assert_eq!(&buf[..8], b"@RSYNCD:");
    }

    #[test]
    fn take_sniffed_prefix_into_slice_fails_on_small_buffer() {
        let mut sniffer = create_legacy_sniffer();
        let mut buf = [0u8; 4];
        let result = sniffer.take_sniffed_prefix_into_slice(&mut buf);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.required(), 8);
        assert_eq!(err.available(), 4);
    }

    #[test]
    fn take_sniffed_prefix_into_slice_returns_zero_when_undecided() {
        let mut sniffer = create_undecided_sniffer();
        let mut buf = [0u8; 16];
        let len = sniffer.take_sniffed_prefix_into_slice(&mut buf).unwrap();
        assert_eq!(len, 0);
    }

    // ==== take_sniffed_prefix_into_array tests ====

    #[test]
    fn take_sniffed_prefix_into_array_copies_to_array() {
        let mut sniffer = create_legacy_sniffer();
        let mut buf = [0u8; 16];
        let len = sniffer.take_sniffed_prefix_into_array(&mut buf).unwrap();
        assert_eq!(len, 8);
        assert_eq!(&buf[..8], b"@RSYNCD:");
    }

    #[test]
    fn take_sniffed_prefix_into_array_fails_on_small_array() {
        let mut sniffer = create_legacy_sniffer();
        let mut buf = [0u8; 4];
        let result = sniffer.take_sniffed_prefix_into_array(&mut buf);
        assert!(result.is_err());
    }

    // ==== take_sniffed_prefix_into_writer tests ====

    #[test]
    fn take_sniffed_prefix_into_writer_writes_prefix() {
        let mut sniffer = create_legacy_sniffer();
        let mut writer = Cursor::new(Vec::new());
        let len = sniffer
            .take_sniffed_prefix_into_writer(&mut writer)
            .unwrap();
        assert_eq!(len, 8);
        assert_eq!(writer.get_ref(), b"@RSYNCD:");
    }

    #[test]
    fn take_sniffed_prefix_into_writer_returns_zero_when_undecided() {
        let mut sniffer = create_undecided_sniffer();
        let mut writer = Cursor::new(Vec::new());
        let len = sniffer
            .take_sniffed_prefix_into_writer(&mut writer)
            .unwrap();
        assert_eq!(len, 0);
        assert!(writer.get_ref().is_empty());
    }

    // ==== take_buffered_into tests ====

    #[test]
    fn take_buffered_into_fills_target() {
        let mut sniffer = create_binary_sniffer();
        let mut target = Vec::new();
        let len = sniffer.take_buffered_into(&mut target).unwrap();
        assert!(len > 0);
        assert!(!target.is_empty());
    }

    #[test]
    fn take_buffered_into_returns_zero_when_undecided() {
        let mut sniffer = create_undecided_sniffer();
        let mut target = Vec::new();
        let len = sniffer.take_buffered_into(&mut target).unwrap();
        assert_eq!(len, 0);
    }

    #[test]
    fn take_buffered_into_swaps_when_target_smaller() {
        let mut sniffer = create_sniffer_with_remainder();
        let mut target = Vec::new();
        let len = sniffer.take_buffered_into(&mut target).unwrap();
        assert!(len > 8);
        assert!(target.starts_with(b"@RSYNCD:"));
    }

    #[test]
    fn take_buffered_into_copies_when_target_larger() {
        let mut sniffer = create_sniffer_with_remainder();
        let mut target = Vec::with_capacity(100);
        let len = sniffer.take_buffered_into(&mut target).unwrap();
        assert!(len > 8);
        assert!(target.starts_with(b"@RSYNCD:"));
    }

    // ==== take_buffered_into_slice tests ====

    #[test]
    fn take_buffered_into_slice_copies_all() {
        let mut sniffer = create_binary_sniffer();
        let mut buf = [0u8; 32];
        let len = sniffer.take_buffered_into_slice(&mut buf).unwrap();
        assert!(len > 0);
    }

    #[test]
    fn take_buffered_into_slice_fails_on_small_buffer() {
        let mut sniffer = create_legacy_sniffer();
        let mut buf = [0u8; 4];
        let result = sniffer.take_buffered_into_slice(&mut buf);
        assert!(result.is_err());
    }

    #[test]
    fn take_buffered_into_slice_returns_zero_when_undecided() {
        let mut sniffer = create_undecided_sniffer();
        let mut buf = [0u8; 32];
        let len = sniffer.take_buffered_into_slice(&mut buf).unwrap();
        assert_eq!(len, 0);
    }

    // ==== take_buffered_into_vectored tests ====

    #[test]
    fn take_buffered_into_vectored_copies_across_slices() {
        let mut sniffer = create_legacy_sniffer();
        let mut buf1 = [0u8; 4];
        let mut buf2 = [0u8; 8];
        let mut slices = [IoSliceMut::new(&mut buf1), IoSliceMut::new(&mut buf2)];
        let len = sniffer.take_buffered_into_vectored(&mut slices).unwrap();
        assert_eq!(len, 8);
        assert_eq!(&buf1, b"@RSY");
        assert_eq!(&buf2[..4], b"NCD:");
    }

    #[test]
    fn take_buffered_into_vectored_fails_on_insufficient_space() {
        let mut sniffer = create_legacy_sniffer();
        let mut buf = [0u8; 2];
        let mut slices = [IoSliceMut::new(&mut buf)];
        let result = sniffer.take_buffered_into_vectored(&mut slices);
        assert!(result.is_err());
    }

    #[test]
    fn take_buffered_into_vectored_returns_zero_when_undecided() {
        let mut sniffer = create_undecided_sniffer();
        let mut buf = [0u8; 32];
        let mut slices = [IoSliceMut::new(&mut buf)];
        let len = sniffer.take_buffered_into_vectored(&mut slices).unwrap();
        assert_eq!(len, 0);
    }

    // ==== take_buffered_into_array tests ====

    #[test]
    fn take_buffered_into_array_copies_all() {
        let mut sniffer = create_binary_sniffer();
        let mut buf = [0u8; 32];
        let len = sniffer.take_buffered_into_array(&mut buf).unwrap();
        assert!(len > 0);
    }

    // ==== take_buffered_into_writer tests ====

    #[test]
    fn take_buffered_into_writer_writes_all() {
        let mut sniffer = create_legacy_sniffer();
        let mut writer = Cursor::new(Vec::new());
        let len = sniffer.take_buffered_into_writer(&mut writer).unwrap();
        assert_eq!(len, 8);
        assert_eq!(writer.get_ref(), b"@RSYNCD:");
    }

    #[test]
    fn take_buffered_into_writer_returns_zero_when_undecided() {
        let mut sniffer = create_undecided_sniffer();
        let mut writer = Cursor::new(Vec::new());
        let len = sniffer.take_buffered_into_writer(&mut writer).unwrap();
        assert_eq!(len, 0);
    }

    // ==== take_buffered_split_into tests ====

    #[test]
    fn take_buffered_split_into_separates_prefix_and_remainder() {
        let mut sniffer = create_sniffer_with_remainder();
        let mut prefix = Vec::new();
        let mut remainder = Vec::new();
        let (prefix_len, remainder_len) = sniffer
            .take_buffered_split_into(&mut prefix, &mut remainder)
            .unwrap();
        assert_eq!(prefix_len, 8);
        assert_eq!(&prefix, b"@RSYNCD:");
        assert!(remainder_len > 0);
        assert!(remainder.starts_with(b" 31.0"));
    }

    #[test]
    fn take_buffered_split_into_returns_zeros_when_undecided() {
        let mut sniffer = create_undecided_sniffer();
        let mut prefix = Vec::new();
        let mut remainder = Vec::new();
        let (prefix_len, remainder_len) = sniffer
            .take_buffered_split_into(&mut prefix, &mut remainder)
            .unwrap();
        assert_eq!(prefix_len, 0);
        assert_eq!(remainder_len, 0);
    }

    #[test]
    fn take_buffered_split_into_empty_remainder_for_prefix_only() {
        let mut sniffer = create_legacy_sniffer();
        let mut prefix = Vec::new();
        let mut remainder = Vec::new();
        let (prefix_len, remainder_len) = sniffer
            .take_buffered_split_into(&mut prefix, &mut remainder)
            .unwrap();
        assert_eq!(prefix_len, 8);
        assert_eq!(remainder_len, 0);
        assert!(remainder.is_empty());
    }

    // ==== take_buffered_remainder tests ====

    #[test]
    fn take_buffered_remainder_returns_remainder_only() {
        let mut sniffer = create_sniffer_with_remainder();
        let remainder = sniffer.take_buffered_remainder();
        assert!(remainder.starts_with(b" 31.0"));
    }

    #[test]
    fn take_buffered_remainder_returns_empty_when_no_remainder() {
        let mut sniffer = create_legacy_sniffer();
        let remainder = sniffer.take_buffered_remainder();
        assert!(remainder.is_empty());
    }

    #[test]
    fn take_buffered_remainder_preserves_prefix() {
        let mut sniffer = create_sniffer_with_remainder();
        let _ = sniffer.take_buffered_remainder();
        assert_eq!(sniffer.buffered(), b"@RSYNCD:");
    }

    // ==== take_buffered_remainder_into tests ====

    #[test]
    fn take_buffered_remainder_into_fills_target() {
        let mut sniffer = create_sniffer_with_remainder();
        let mut target = Vec::new();
        let len = sniffer.take_buffered_remainder_into(&mut target).unwrap();
        assert!(len > 0);
        assert!(target.starts_with(b" 31.0"));
    }

    #[test]
    fn take_buffered_remainder_into_returns_zero_when_no_remainder() {
        let mut sniffer = create_legacy_sniffer();
        let mut target = Vec::new();
        let len = sniffer.take_buffered_remainder_into(&mut target).unwrap();
        assert_eq!(len, 0);
    }

    // ==== take_buffered_remainder_into_slice tests ====

    #[test]
    fn take_buffered_remainder_into_slice_copies_remainder() {
        let mut sniffer = create_sniffer_with_remainder();
        let mut buf = [0u8; 32];
        let len = sniffer
            .take_buffered_remainder_into_slice(&mut buf)
            .unwrap();
        assert!(len > 0);
        assert!(buf.starts_with(b" 31.0"));
    }

    #[test]
    fn take_buffered_remainder_into_slice_fails_on_small_buffer() {
        let mut sniffer = create_sniffer_with_remainder();
        let mut buf = [0u8; 2];
        let result = sniffer.take_buffered_remainder_into_slice(&mut buf);
        assert!(result.is_err());
    }

    #[test]
    fn take_buffered_remainder_into_slice_returns_zero_when_no_remainder() {
        let mut sniffer = create_legacy_sniffer();
        let mut buf = [0u8; 32];
        let len = sniffer
            .take_buffered_remainder_into_slice(&mut buf)
            .unwrap();
        assert_eq!(len, 0);
    }

    // ==== take_buffered_remainder_into_vectored tests ====

    #[test]
    fn take_buffered_remainder_into_vectored_copies_remainder() {
        let mut sniffer = create_sniffer_with_remainder();
        let mut buf = [0u8; 32];
        let mut slices = [IoSliceMut::new(&mut buf)];
        let len = sniffer
            .take_buffered_remainder_into_vectored(&mut slices)
            .unwrap();
        assert!(len > 0);
    }

    #[test]
    fn take_buffered_remainder_into_vectored_returns_zero_when_no_remainder() {
        let mut sniffer = create_legacy_sniffer();
        let mut buf = [0u8; 32];
        let mut slices = [IoSliceMut::new(&mut buf)];
        let len = sniffer
            .take_buffered_remainder_into_vectored(&mut slices)
            .unwrap();
        assert_eq!(len, 0);
    }

    // ==== take_buffered_remainder_into_array tests ====

    #[test]
    fn take_buffered_remainder_into_array_copies_remainder() {
        let mut sniffer = create_sniffer_with_remainder();
        let mut buf = [0u8; 32];
        let len = sniffer
            .take_buffered_remainder_into_array(&mut buf)
            .unwrap();
        assert!(len > 0);
    }

    // ==== take_buffered_remainder_into_writer tests ====

    #[test]
    fn take_buffered_remainder_into_writer_writes_remainder() {
        let mut sniffer = create_sniffer_with_remainder();
        let mut writer = Cursor::new(Vec::new());
        let len = sniffer
            .take_buffered_remainder_into_writer(&mut writer)
            .unwrap();
        assert!(len > 0);
        assert!(writer.get_ref().starts_with(b" 31.0"));
    }

    #[test]
    fn take_buffered_remainder_into_writer_returns_zero_when_no_remainder() {
        let mut sniffer = create_legacy_sniffer();
        let mut writer = Cursor::new(Vec::new());
        let len = sniffer
            .take_buffered_remainder_into_writer(&mut writer)
            .unwrap();
        assert_eq!(len, 0);
    }

    // ==== discard_sniffed_prefix tests ====

    #[test]
    fn discard_sniffed_prefix_returns_prefix_length() {
        let mut sniffer = create_sniffer_with_remainder();
        let discarded = sniffer.discard_sniffed_prefix();
        assert_eq!(discarded, 8);
    }

    #[test]
    fn discard_sniffed_prefix_removes_prefix_from_buffer() {
        let mut sniffer = create_sniffer_with_remainder();
        let _ = sniffer.discard_sniffed_prefix();
        assert!(sniffer.buffered().starts_with(b" 31.0"));
    }

    #[test]
    fn discard_sniffed_prefix_returns_zero_when_no_prefix() {
        let mut sniffer = create_undecided_sniffer();
        let discarded = sniffer.discard_sniffed_prefix();
        assert_eq!(discarded, 0);
    }

    #[test]
    fn discard_sniffed_prefix_zeroes_prefix_bytes_retained() {
        let mut sniffer = create_legacy_sniffer();
        assert!(sniffer.sniffed_prefix_len() > 0);
        let _ = sniffer.discard_sniffed_prefix();
        assert_eq!(sniffer.sniffed_prefix_len(), 0);
    }

    // ==== Edge cases ====

    #[test]
    fn take_operations_on_partial_legacy_prefix() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        sniffer.observe(b"@RSY").unwrap();
        // Still undecided because not enough bytes for full prefix
        assert!(sniffer.requires_more_data());
        let buffered = sniffer.take_buffered();
        assert!(buffered.is_empty());
    }

    #[test]
    fn take_sniffed_prefix_updates_prefix_bytes_retained() {
        let mut sniffer = create_sniffer_with_remainder();
        assert_eq!(sniffer.sniffed_prefix_len(), 8);
        let _ = sniffer.take_sniffed_prefix();
        assert_eq!(sniffer.sniffed_prefix_len(), 0);
    }

    #[test]
    fn multiple_drain_calls_work_correctly() {
        let mut sniffer = create_sniffer_with_remainder();
        // First take prefix
        let prefix = sniffer.take_sniffed_prefix();
        assert_eq!(&prefix, b"@RSYNCD:");
        // Then take remainder
        let remainder = sniffer.take_buffered_remainder();
        assert!(remainder.starts_with(b" 31.0"));
        // Buffer should be empty now
        assert!(sniffer.buffered().is_empty());
    }

    #[test]
    fn take_buffered_resets_buffer_for_reuse() {
        let mut sniffer = create_legacy_sniffer();
        let _ = sniffer.take_buffered();
        // Sniffer should be ready for reuse
        assert!(sniffer.buffered().is_empty());
    }

    #[test]
    fn binary_decision_has_single_byte_prefix() {
        let sniffer = create_binary_sniffer();
        assert!(sniffer.is_binary());
        assert_eq!(sniffer.sniffed_prefix_len(), 1);
    }

    #[test]
    fn legacy_decision_has_full_prefix() {
        let sniffer = create_legacy_sniffer();
        assert!(sniffer.is_legacy());
        assert_eq!(sniffer.sniffed_prefix_len(), 8);
    }
}
