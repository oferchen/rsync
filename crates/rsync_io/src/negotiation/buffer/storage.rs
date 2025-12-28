use std::collections::TryReserveError;
use std::io::{self, IoSliceMut, Write};

use protocol::LEGACY_DAEMON_PREFIX_LEN;

use super::errors::BufferedCopyTooSmall;
use super::slices::NegotiationBufferedSlices;

#[derive(Clone, Debug)]
pub(crate) struct NegotiationBuffer {
    sniffed_prefix_len: usize,
    buffered_pos: usize,
    buffered: Vec<u8>,
}

impl NegotiationBuffer {
    pub(crate) fn new(sniffed_prefix_len: usize, buffered_pos: usize, buffered: Vec<u8>) -> Self {
        let clamped_prefix_len = sniffed_prefix_len.min(buffered.len());
        let clamped_pos = buffered_pos.min(buffered.len());

        Self {
            sniffed_prefix_len: clamped_prefix_len,
            buffered_pos: clamped_pos,
            buffered,
        }
    }

    pub(crate) fn sniffed_prefix(&self) -> &[u8] {
        &self.buffered[..self.sniffed_prefix_len]
    }

    pub(crate) fn buffered_remainder(&self) -> &[u8] {
        let start = self
            .buffered_pos
            .max(self.sniffed_prefix_len())
            .min(self.buffered.len());
        &self.buffered[start..]
    }

    pub(crate) fn buffered(&self) -> &[u8] {
        &self.buffered
    }

    pub(crate) fn buffered_consumed_slice(&self) -> &[u8] {
        let consumed = self.buffered_pos.min(self.buffered.len());
        &self.buffered[..consumed]
    }

    pub(crate) fn buffered_vectored(&self) -> NegotiationBufferedSlices<'_> {
        let prefix = &self.buffered[..self.sniffed_prefix_len];
        let remainder = &self.buffered[self.sniffed_prefix_len..];
        NegotiationBufferedSlices::new(prefix, remainder)
    }

    pub(crate) fn buffered_to_vec(&self) -> Result<Vec<u8>, TryReserveError> {
        self.buffered_vectored().to_vec()
    }

    pub(crate) fn buffered_split(&self) -> (&[u8], &[u8]) {
        let prefix_len = self.sniffed_prefix_len();
        debug_assert!(prefix_len <= self.buffered.len());

        let consumed_prefix = self.buffered_pos.min(prefix_len);
        let prefix_start = consumed_prefix;
        let prefix_slice = &self.buffered[prefix_start..prefix_len];

        let remainder_start = self.buffered_pos.max(prefix_len).min(self.buffered.len());
        let remainder_slice = &self.buffered[remainder_start..];

        (prefix_slice, remainder_slice)
    }

    pub(crate) fn buffered_remaining_vectored(&self) -> NegotiationBufferedSlices<'_> {
        let (prefix, remainder) = self.buffered_split();
        NegotiationBufferedSlices::new(prefix, remainder)
    }

    pub(crate) fn buffered_remaining_to_vec(&self) -> Result<Vec<u8>, TryReserveError> {
        let remainder = self.buffered_remainder();
        if remainder.is_empty() {
            return Ok(Vec::new());
        }

        let mut buffer = Vec::new();
        buffer.try_reserve_exact(remainder.len())?;
        buffer.extend_from_slice(remainder);
        Ok(buffer)
    }

    pub(crate) const fn sniffed_prefix_len(&self) -> usize {
        self.sniffed_prefix_len
    }

    pub(crate) const fn buffered_len(&self) -> usize {
        self.buffered.len()
    }

    pub(crate) const fn buffered_consumed(&self) -> usize {
        self.buffered_pos
    }

    pub(crate) const fn buffered_remaining(&self) -> usize {
        self.buffered.len().saturating_sub(self.buffered_pos)
    }

    pub(crate) fn sniffed_prefix_remaining(&self) -> usize {
        let consumed_prefix = self.buffered_pos.min(self.sniffed_prefix_len);
        self.sniffed_prefix_len.saturating_sub(consumed_prefix)
    }

    pub(crate) const fn legacy_prefix_complete(&self) -> bool {
        self.sniffed_prefix_len >= LEGACY_DAEMON_PREFIX_LEN
    }

    pub(crate) const fn has_remaining(&self) -> bool {
        self.buffered_pos < self.buffered.len()
    }

    pub(crate) fn remaining_slice(&self) -> &[u8] {
        &self.buffered[self.buffered_pos..]
    }

    pub(crate) fn buffered_remaining_slice(&self) -> &[u8] {
        self.remaining_slice()
    }

    pub(crate) fn copy_into(&mut self, buf: &mut [u8]) -> usize {
        if buf.is_empty() || !self.has_remaining() {
            return 0;
        }

        let available = &self.buffered[self.buffered_pos..];
        let to_copy = available.len().min(buf.len());
        buf[..to_copy].copy_from_slice(&available[..to_copy]);
        self.buffered_pos += to_copy;
        to_copy
    }

    pub(crate) fn copy_into_vec(&self, target: &mut Vec<u8>) -> Result<usize, TryReserveError> {
        Self::copy_bytes_into_vec(target, &self.buffered)
    }

    pub(crate) fn extend_into_vec(&self, target: &mut Vec<u8>) -> Result<usize, TryReserveError> {
        Self::extend_bytes_into_vec(target, &self.buffered)
    }

    pub(crate) fn extend_remaining_into_vec(
        &self,
        target: &mut Vec<u8>,
    ) -> Result<usize, TryReserveError> {
        Self::extend_bytes_into_vec(target, self.remaining_slice())
    }

    pub(crate) fn copy_remaining_into_vec(
        &self,
        target: &mut Vec<u8>,
    ) -> Result<usize, TryReserveError> {
        Self::copy_bytes_into_vec(target, self.remaining_slice())
    }

    pub(crate) fn copy_all_into_slice(
        &self,
        target: &mut [u8],
    ) -> Result<usize, BufferedCopyTooSmall> {
        let required = self.buffered.len();

        if target.len() < required {
            return Err(BufferedCopyTooSmall::new(required, target.len()));
        }

        target[..required].copy_from_slice(&self.buffered);
        Ok(required)
    }

    pub(crate) fn copy_remaining_into_slice(
        &self,
        target: &mut [u8],
    ) -> Result<usize, BufferedCopyTooSmall> {
        let remaining = self.remaining_slice();
        if target.len() < remaining.len() {
            return Err(BufferedCopyTooSmall::new(remaining.len(), target.len()));
        }

        target[..remaining.len()].copy_from_slice(remaining);
        Ok(remaining.len())
    }

    pub(crate) fn copy_all_into_array<const N: usize>(
        &self,
        target: &mut [u8; N],
    ) -> Result<usize, BufferedCopyTooSmall> {
        self.copy_all_into_slice(target.as_mut_slice())
    }

    pub(crate) fn copy_remaining_into_array<const N: usize>(
        &self,
        target: &mut [u8; N],
    ) -> Result<usize, BufferedCopyTooSmall> {
        self.copy_remaining_into_slice(target.as_mut_slice())
    }

    pub(crate) fn copy_all_into_writer<W: Write>(&self, target: &mut W) -> io::Result<usize> {
        target.write_all(&self.buffered)?;
        Ok(self.buffered.len())
    }

    pub(crate) fn copy_remaining_into_writer<W: Write>(&self, target: &mut W) -> io::Result<usize> {
        let remaining = self.remaining_slice();
        if remaining.is_empty() {
            return Ok(0);
        }

        target.write_all(remaining)?;
        Ok(remaining.len())
    }

    pub(crate) fn copy_remaining_into_vectored(
        &self,
        bufs: &mut [IoSliceMut<'_>],
    ) -> Result<usize, BufferedCopyTooSmall> {
        let remaining = self.remaining_slice();
        if remaining.is_empty() {
            return Ok(0);
        }

        let mut provided = 0usize;
        for buf in bufs.iter() {
            provided = provided.saturating_add(buf.len());
            if provided >= remaining.len() {
                break;
            }
        }

        if provided < remaining.len() {
            return Err(BufferedCopyTooSmall::new(remaining.len(), provided));
        }

        let mut written = 0usize;
        for buf in bufs.iter_mut() {
            if written == remaining.len() {
                break;
            }

            let slice = buf.as_mut();
            if slice.is_empty() {
                continue;
            }

            let to_copy = (remaining.len() - written).min(slice.len());
            slice[..to_copy].copy_from_slice(&remaining[written..written + to_copy]);
            written += to_copy;
        }

        debug_assert_eq!(written, remaining.len());
        Ok(remaining.len())
    }

    pub(crate) fn copy_all_into_vectored(
        &self,
        bufs: &mut [IoSliceMut<'_>],
    ) -> Result<usize, BufferedCopyTooSmall> {
        let required = self.buffered.len();
        if required == 0 {
            return Ok(0);
        }

        let mut provided = 0usize;
        for buf in bufs.iter() {
            provided = provided.saturating_add(buf.len());
            if provided >= required {
                break;
            }
        }

        if provided < required {
            return Err(BufferedCopyTooSmall::new(required, provided));
        }

        let mut written = 0usize;
        for buf in bufs.iter_mut() {
            if written == required {
                break;
            }

            let slice = buf.as_mut();
            if slice.is_empty() {
                continue;
            }

            let to_copy = (required - written).min(slice.len());
            slice[..to_copy].copy_from_slice(&self.buffered[written..written + to_copy]);
            written += to_copy;
        }

        debug_assert_eq!(written, required);
        Ok(required)
    }

    fn copy_bytes_into_vec(target: &mut Vec<u8>, bytes: &[u8]) -> Result<usize, TryReserveError> {
        let len = target.len();
        target.try_reserve(bytes.len().saturating_sub(len))?;
        target.clear();

        if bytes.is_empty() {
            return Ok(0);
        }

        target.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn extend_bytes_into_vec(target: &mut Vec<u8>, bytes: &[u8]) -> Result<usize, TryReserveError> {
        if bytes.is_empty() {
            return Ok(0);
        }

        target.try_reserve(bytes.len())?;
        target.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    pub(crate) fn copy_into_vectored(&mut self, bufs: &mut [IoSliceMut<'_>]) -> usize {
        if bufs.is_empty() || !self.has_remaining() {
            return 0;
        }

        let available = &self.buffered[self.buffered_pos..];
        let mut copied = 0;

        for buf in bufs.iter_mut() {
            if copied == available.len() {
                break;
            }

            let target = buf.as_mut();
            if target.is_empty() {
                continue;
            }

            let remaining = available.len() - copied;
            let to_copy = remaining.min(target.len());
            target[..to_copy].copy_from_slice(&available[copied..copied + to_copy]);
            copied += to_copy;
        }

        self.buffered_pos += copied;
        copied
    }

    pub(crate) const fn consume(&mut self, amt: usize) -> usize {
        if !self.has_remaining() {
            return amt;
        }

        let available = self.buffered_remaining();
        if amt < available {
            self.buffered_pos += amt;
            0
        } else {
            self.buffered_pos = self.buffered.len();
            amt - available
        }
    }

    pub(crate) fn into_raw_parts(self) -> (usize, usize, Vec<u8>) {
        let Self {
            sniffed_prefix_len,
            buffered_pos,
            buffered,
        } = self;
        (sniffed_prefix_len, buffered_pos, buffered)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::IoSliceMut;

    // ==== Construction and clamping ====

    #[test]
    fn new_stores_values() {
        let data = vec![1, 2, 3, 4, 5];
        let buf = NegotiationBuffer::new(3, 1, data.clone());
        assert_eq!(buf.sniffed_prefix_len(), 3);
        assert_eq!(buf.buffered_consumed(), 1);
        assert_eq!(buf.buffered(), &data[..]);
    }

    #[test]
    fn new_clamps_prefix_len_to_buffer_len() {
        let data = vec![1, 2, 3];
        let buf = NegotiationBuffer::new(100, 0, data);
        // prefix_len should be clamped to buffer length (3)
        assert_eq!(buf.sniffed_prefix_len(), 3);
    }

    #[test]
    fn new_clamps_pos_to_buffer_len() {
        let data = vec![1, 2, 3];
        let buf = NegotiationBuffer::new(0, 100, data);
        // pos should be clamped to buffer length (3)
        assert_eq!(buf.buffered_consumed(), 3);
    }

    #[test]
    fn new_with_empty_buffer() {
        let buf = NegotiationBuffer::new(5, 5, Vec::new());
        assert_eq!(buf.sniffed_prefix_len(), 0);
        assert_eq!(buf.buffered_consumed(), 0);
        assert!(buf.buffered().is_empty());
    }

    // ==== Accessor methods ====

    #[test]
    fn sniffed_prefix_returns_correct_slice() {
        let data = vec![1, 2, 3, 4, 5];
        let buf = NegotiationBuffer::new(3, 0, data);
        assert_eq!(buf.sniffed_prefix(), &[1, 2, 3]);
    }

    #[test]
    fn buffered_remainder_returns_unconsumed_bytes() {
        let data = vec![1, 2, 3, 4, 5];
        let buf = NegotiationBuffer::new(2, 3, data);
        // buffered_remainder starts after max(pos, prefix_len) = max(3, 2) = 3
        assert_eq!(buf.buffered_remainder(), &[4, 5]);
    }

    #[test]
    fn buffered_len_returns_total_length() {
        let data = vec![1, 2, 3, 4, 5];
        let buf = NegotiationBuffer::new(2, 1, data);
        assert_eq!(buf.buffered_len(), 5);
    }

    #[test]
    fn buffered_remaining_returns_bytes_left() {
        let data = vec![1, 2, 3, 4, 5];
        let buf = NegotiationBuffer::new(2, 2, data);
        assert_eq!(buf.buffered_remaining(), 3); // 5 - 2 = 3
    }

    #[test]
    fn has_remaining_true_when_not_consumed() {
        let data = vec![1, 2, 3];
        let buf = NegotiationBuffer::new(0, 0, data);
        assert!(buf.has_remaining());
    }

    #[test]
    fn has_remaining_false_when_fully_consumed() {
        let data = vec![1, 2, 3];
        let buf = NegotiationBuffer::new(0, 3, data);
        assert!(!buf.has_remaining());
    }

    #[test]
    fn remaining_slice_returns_unconsumed() {
        let data = vec![1, 2, 3, 4, 5];
        let buf = NegotiationBuffer::new(0, 2, data);
        assert_eq!(buf.remaining_slice(), &[3, 4, 5]);
    }

    #[test]
    fn buffered_consumed_slice_returns_consumed() {
        let data = vec![1, 2, 3, 4, 5];
        let buf = NegotiationBuffer::new(0, 3, data);
        assert_eq!(buf.buffered_consumed_slice(), &[1, 2, 3]);
    }

    #[test]
    fn sniffed_prefix_remaining_accounts_for_consumption() {
        let data = vec![1, 2, 3, 4, 5];
        let buf = NegotiationBuffer::new(3, 1, data);
        // prefix_len=3, consumed=1, so 2 prefix bytes remain
        assert_eq!(buf.sniffed_prefix_remaining(), 2);
    }

    #[test]
    fn sniffed_prefix_remaining_zero_when_past_prefix() {
        let data = vec![1, 2, 3, 4, 5];
        let buf = NegotiationBuffer::new(2, 5, data);
        assert_eq!(buf.sniffed_prefix_remaining(), 0);
    }

    // ==== copy_into ====

    #[test]
    fn copy_into_copies_and_advances_position() {
        let data = vec![1, 2, 3, 4, 5];
        let mut buf = NegotiationBuffer::new(0, 0, data);
        let mut dest = [0u8; 3];
        let copied = buf.copy_into(&mut dest);
        assert_eq!(copied, 3);
        assert_eq!(dest, [1, 2, 3]);
        assert_eq!(buf.buffered_consumed(), 3);
    }

    #[test]
    fn copy_into_partial_when_dest_small() {
        let data = vec![1, 2, 3, 4, 5];
        let mut buf = NegotiationBuffer::new(0, 0, data);
        let mut dest = [0u8; 2];
        let copied = buf.copy_into(&mut dest);
        assert_eq!(copied, 2);
        assert_eq!(dest, [1, 2]);
    }

    #[test]
    fn copy_into_returns_zero_when_empty_dest() {
        let data = vec![1, 2, 3];
        let mut buf = NegotiationBuffer::new(0, 0, data);
        let mut dest = [0u8; 0];
        assert_eq!(buf.copy_into(&mut dest), 0);
    }

    #[test]
    fn copy_into_returns_zero_when_fully_consumed() {
        let data = vec![1, 2, 3];
        let mut buf = NegotiationBuffer::new(0, 3, data);
        let mut dest = [0u8; 10];
        assert_eq!(buf.copy_into(&mut dest), 0);
    }

    // ==== consume ====

    #[test]
    fn consume_advances_position() {
        let data = vec![1, 2, 3, 4, 5];
        let mut buf = NegotiationBuffer::new(0, 0, data);
        let leftover = buf.consume(2);
        assert_eq!(leftover, 0);
        assert_eq!(buf.buffered_consumed(), 2);
    }

    #[test]
    fn consume_returns_excess_when_consuming_more() {
        let data = vec![1, 2, 3];
        let mut buf = NegotiationBuffer::new(0, 0, data);
        let leftover = buf.consume(5);
        assert_eq!(leftover, 2); // consumed 3, leftover 2
        assert_eq!(buf.buffered_consumed(), 3);
    }

    #[test]
    fn consume_returns_full_amount_when_already_consumed() {
        let data = vec![1, 2, 3];
        let mut buf = NegotiationBuffer::new(0, 3, data);
        let leftover = buf.consume(10);
        assert_eq!(leftover, 10);
    }

    // ==== copy methods ====

    #[test]
    fn copy_into_vec_copies_all_buffered() {
        let data = vec![1, 2, 3, 4, 5];
        let buf = NegotiationBuffer::new(2, 1, data.clone());
        let mut target = Vec::new();
        let copied = buf.copy_into_vec(&mut target).unwrap();
        assert_eq!(copied, 5);
        assert_eq!(target, data);
    }

    #[test]
    fn extend_into_vec_appends_to_existing() {
        let data = vec![1, 2, 3];
        let buf = NegotiationBuffer::new(0, 0, data);
        let mut target = vec![9, 8, 7];
        let extended = buf.extend_into_vec(&mut target).unwrap();
        assert_eq!(extended, 3);
        assert_eq!(target, vec![9, 8, 7, 1, 2, 3]);
    }

    #[test]
    fn copy_all_into_slice_succeeds() {
        let data = vec![1, 2, 3, 4, 5];
        let buf = NegotiationBuffer::new(0, 0, data);
        let mut target = [0u8; 10];
        let copied = buf.copy_all_into_slice(&mut target).unwrap();
        assert_eq!(copied, 5);
        assert_eq!(&target[..5], &[1, 2, 3, 4, 5]);
    }

    #[test]
    fn copy_all_into_slice_fails_when_too_small() {
        let data = vec![1, 2, 3, 4, 5];
        let buf = NegotiationBuffer::new(0, 0, data);
        let mut target = [0u8; 3];
        let err = buf.copy_all_into_slice(&mut target).unwrap_err();
        assert_eq!(err.required(), 5);
        assert_eq!(err.provided(), 3);
    }

    #[test]
    fn copy_remaining_into_slice_succeeds() {
        let data = vec![1, 2, 3, 4, 5];
        let buf = NegotiationBuffer::new(0, 2, data);
        let mut target = [0u8; 10];
        let copied = buf.copy_remaining_into_slice(&mut target).unwrap();
        assert_eq!(copied, 3);
        assert_eq!(&target[..3], &[3, 4, 5]);
    }

    #[test]
    fn copy_all_into_writer_succeeds() {
        let data = vec![1, 2, 3, 4, 5];
        let buf = NegotiationBuffer::new(0, 0, data.clone());
        let mut target = Vec::new();
        let written = buf.copy_all_into_writer(&mut target).unwrap();
        assert_eq!(written, 5);
        assert_eq!(target, data);
    }

    #[test]
    fn copy_remaining_into_writer_succeeds() {
        let data = vec![1, 2, 3, 4, 5];
        let buf = NegotiationBuffer::new(0, 3, data);
        let mut target = Vec::new();
        let written = buf.copy_remaining_into_writer(&mut target).unwrap();
        assert_eq!(written, 2);
        assert_eq!(target, vec![4, 5]);
    }

    // ==== vectored copy ====

    #[test]
    fn copy_into_vectored_spans_multiple_slices() {
        let data = vec![1, 2, 3, 4, 5, 6];
        let mut buf = NegotiationBuffer::new(0, 0, data);
        let mut buf1 = [0u8; 2];
        let mut buf2 = [0u8; 2];
        let mut buf3 = [0u8; 2];
        let mut slices = [
            IoSliceMut::new(&mut buf1),
            IoSliceMut::new(&mut buf2),
            IoSliceMut::new(&mut buf3),
        ];
        let copied = buf.copy_into_vectored(&mut slices);
        assert_eq!(copied, 6);
        assert_eq!(buf1, [1, 2]);
        assert_eq!(buf2, [3, 4]);
        assert_eq!(buf3, [5, 6]);
    }

    #[test]
    fn copy_remaining_into_vectored_fails_when_insufficient() {
        let data = vec![1, 2, 3, 4, 5];
        let buf = NegotiationBuffer::new(0, 0, data);
        let mut buf1 = [0u8; 2];
        let mut slices = [IoSliceMut::new(&mut buf1)];
        let err = buf.copy_remaining_into_vectored(&mut slices).unwrap_err();
        assert_eq!(err.required(), 5);
        assert_eq!(err.provided(), 2);
    }

    // ==== into_raw_parts ====

    #[test]
    fn into_raw_parts_returns_components() {
        let data = vec![1, 2, 3, 4, 5];
        let buf = NegotiationBuffer::new(3, 2, data.clone());
        let (prefix_len, pos, buffer) = buf.into_raw_parts();
        assert_eq!(prefix_len, 3);
        assert_eq!(pos, 2);
        assert_eq!(buffer, data);
    }

    // ==== buffered_split ====

    #[test]
    fn buffered_split_returns_prefix_and_remainder() {
        let data = vec![1, 2, 3, 4, 5];
        let buf = NegotiationBuffer::new(2, 0, data);
        let (prefix, remainder) = buf.buffered_split();
        assert_eq!(prefix, &[1, 2]);
        assert_eq!(remainder, &[3, 4, 5]);
    }

    #[test]
    fn buffered_split_accounts_for_consumed_prefix() {
        let data = vec![1, 2, 3, 4, 5];
        let buf = NegotiationBuffer::new(3, 1, data);
        let (prefix, remainder) = buf.buffered_split();
        // prefix starts at pos 1, goes to prefix_len 3
        assert_eq!(prefix, &[2, 3]);
        assert_eq!(remainder, &[4, 5]);
    }

    // ==== legacy prefix complete ====

    #[test]
    fn legacy_prefix_complete_true_when_enough() {
        let data = vec![0u8; 20];
        let buf = NegotiationBuffer::new(LEGACY_DAEMON_PREFIX_LEN, 0, data);
        assert!(buf.legacy_prefix_complete());
    }

    #[test]
    fn legacy_prefix_complete_false_when_short() {
        let data = vec![0u8; 5];
        let buf = NegotiationBuffer::new(3, 0, data);
        assert!(!buf.legacy_prefix_complete());
    }

    // ==== Clone and Debug ====

    #[test]
    fn clone_produces_independent_copy() {
        let data = vec![1, 2, 3];
        let buf = NegotiationBuffer::new(1, 0, data);
        let cloned = buf.clone();
        assert_eq!(buf.buffered(), cloned.buffered());
        assert_eq!(buf.sniffed_prefix_len(), cloned.sniffed_prefix_len());
    }

    #[test]
    fn debug_format_contains_type_name() {
        let buf = NegotiationBuffer::new(0, 0, vec![1, 2, 3]);
        let debug = format!("{buf:?}");
        assert!(debug.contains("NegotiationBuffer"));
    }

    // ==== Edge cases ====

    #[test]
    fn extend_remaining_into_vec_with_partial_consumption() {
        let data = vec![1, 2, 3, 4, 5];
        let buf = NegotiationBuffer::new(0, 3, data);
        let mut target = vec![9];
        let extended = buf.extend_remaining_into_vec(&mut target).unwrap();
        assert_eq!(extended, 2);
        assert_eq!(target, vec![9, 4, 5]);
    }

    #[test]
    fn buffered_to_vec_returns_all_buffered() {
        let data = vec![1, 2, 3, 4, 5];
        let buf = NegotiationBuffer::new(2, 0, data.clone());
        let result = buf.buffered_to_vec().unwrap();
        assert_eq!(result, data);
    }

    #[test]
    fn buffered_remaining_to_vec_with_empty_remainder() {
        let data = vec![1, 2, 3];
        let buf = NegotiationBuffer::new(0, 3, data);
        let result = buf.buffered_remaining_to_vec().unwrap();
        assert!(result.is_empty());
    }
}
