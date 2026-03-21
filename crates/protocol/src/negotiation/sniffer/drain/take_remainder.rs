use std::collections::TryReserveError;
use std::io::{self, IoSliceMut, Write};

use crate::legacy::LEGACY_DAEMON_PREFIX_LEN;
use crate::negotiation::BufferedPrefixTooSmall;

use super::super::NegotiationPrologueSniffer;
use super::super::util::{copy_into_vectored, ensure_vec_capacity};

impl NegotiationPrologueSniffer {
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
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, IoSliceMut};

    use crate::negotiation::sniffer::NegotiationPrologueSniffer;

    fn create_legacy_sniffer() -> NegotiationPrologueSniffer {
        let mut sniffer = NegotiationPrologueSniffer::new();
        sniffer.observe(b"@RSYNCD:").unwrap();
        sniffer
    }

    fn create_sniffer_with_remainder() -> NegotiationPrologueSniffer {
        let mut sniffer = NegotiationPrologueSniffer::new();
        sniffer.observe(b"@RSYNCD:").unwrap();
        sniffer.buffered_storage_mut().extend_from_slice(b" 31.0\n");
        sniffer
    }

    fn create_undecided_sniffer() -> NegotiationPrologueSniffer {
        NegotiationPrologueSniffer::new()
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

    // ==== Cross-submodule edge case ====

    #[test]
    fn multiple_drain_calls_work_correctly() {
        let mut sniffer = create_sniffer_with_remainder();
        let prefix = sniffer.take_sniffed_prefix();
        assert_eq!(&prefix, b"@RSYNCD:");
        let remainder = sniffer.take_buffered_remainder();
        assert!(remainder.starts_with(b" 31.0"));
        assert!(sniffer.buffered().is_empty());
    }
}
