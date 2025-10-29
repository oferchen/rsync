use std::collections::TryReserveError;
use std::io::{self, IoSliceMut, Write};

use rsync_protocol::LEGACY_DAEMON_PREFIX_LEN;

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

    pub(crate) fn buffered_len(&self) -> usize {
        self.buffered.len()
    }

    pub(crate) fn buffered_consumed(&self) -> usize {
        self.buffered_pos
    }

    pub(crate) fn buffered_remaining(&self) -> usize {
        self.buffered.len().saturating_sub(self.buffered_pos)
    }

    pub(crate) fn sniffed_prefix_remaining(&self) -> usize {
        let consumed_prefix = self.buffered_pos.min(self.sniffed_prefix_len);
        self.sniffed_prefix_len.saturating_sub(consumed_prefix)
    }

    pub(crate) fn legacy_prefix_complete(&self) -> bool {
        self.sniffed_prefix_len >= LEGACY_DAEMON_PREFIX_LEN
    }

    pub(crate) fn has_remaining(&self) -> bool {
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

    pub(crate) fn consume(&mut self, amt: usize) -> usize {
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
