use std::collections::TryReserveError;
use std::io::{self, Write};

use crate::negotiation::BufferedPrefixTooSmall;

use super::super::NegotiationPrologueSniffer;
use super::super::util::ensure_vec_capacity;

impl NegotiationPrologueSniffer {
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
    use std::io::Cursor;

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
    fn take_sniffed_prefix_updates_prefix_bytes_retained() {
        let mut sniffer = create_sniffer_with_remainder();
        assert_eq!(sniffer.sniffed_prefix_len(), 8);
        let _ = sniffer.take_sniffed_prefix();
        assert_eq!(sniffer.sniffed_prefix_len(), 0);
    }
}
