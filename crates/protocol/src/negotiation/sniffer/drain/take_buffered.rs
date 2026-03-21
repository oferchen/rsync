use ::core::mem;
use std::collections::TryReserveError;
use std::io::{self, IoSliceMut, Write};

use crate::legacy::LEGACY_DAEMON_PREFIX_LEN;
use crate::negotiation::BufferedPrefixTooSmall;

use super::super::NegotiationPrologueSniffer;
use super::super::util::{copy_into_vectored, ensure_vec_capacity};

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

}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, IoSliceMut};

    use crate::negotiation::sniffer::NegotiationPrologueSniffer;

    fn create_binary_sniffer() -> NegotiationPrologueSniffer {
        let mut sniffer = NegotiationPrologueSniffer::new();
        sniffer.observe(b"\x00\x00\x00\x1f").unwrap();
        sniffer
    }

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

    // ==== Edge cases ====

    #[test]
    fn take_operations_on_partial_legacy_prefix() {
        let mut sniffer = NegotiationPrologueSniffer::new();
        sniffer.observe(b"@RSY").unwrap();
        assert!(sniffer.requires_more_data());
        let buffered = sniffer.take_buffered();
        assert!(buffered.is_empty());
    }

    #[test]
    fn take_buffered_resets_buffer_for_reuse() {
        let mut sniffer = create_legacy_sniffer();
        let _ = sniffer.take_buffered();
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
