use std::collections::TryReserveError;
use std::io::{self, IoSliceMut, Write};

use super::errors::BufferedCopyTooSmall;
use super::slices::NegotiationBufferedSlices;
use super::storage::NegotiationBuffer;

/// Shared accessors for buffered negotiation data.
pub(crate) trait NegotiationBufferAccess {
    fn buffer_ref(&self) -> &NegotiationBuffer;

    #[inline]
    fn buffered(&self) -> &[u8] {
        self.buffer_ref().buffered()
    }

    #[inline]
    fn sniffed_prefix(&self) -> &[u8] {
        self.buffer_ref().sniffed_prefix()
    }

    #[inline]
    fn buffered_remainder(&self) -> &[u8] {
        self.buffer_ref().buffered_remainder()
    }

    #[inline]
    fn buffered_vectored(&self) -> NegotiationBufferedSlices<'_> {
        self.buffer_ref().buffered_vectored()
    }

    #[inline]
    fn buffered_to_vec(&self) -> Result<Vec<u8>, TryReserveError> {
        self.buffer_ref().buffered_to_vec()
    }

    #[inline]
    fn copy_buffered_into_vec(&self, target: &mut Vec<u8>) -> Result<usize, TryReserveError> {
        self.buffer_ref().copy_into_vec(target)
    }

    #[inline]
    fn copy_buffered_remaining_into_vec(
        &self,
        target: &mut Vec<u8>,
    ) -> Result<usize, TryReserveError> {
        self.buffer_ref().copy_remaining_into_vec(target)
    }

    #[inline]
    fn extend_buffered_into_vec(&self, target: &mut Vec<u8>) -> Result<usize, TryReserveError> {
        self.buffer_ref().extend_into_vec(target)
    }

    #[inline]
    fn extend_buffered_remaining_into_vec(
        &self,
        target: &mut Vec<u8>,
    ) -> Result<usize, TryReserveError> {
        self.buffer_ref().extend_remaining_into_vec(target)
    }

    #[inline]
    fn buffered_split(&self) -> (&[u8], &[u8]) {
        self.buffer_ref().buffered_split()
    }

    #[inline]
    fn buffered_len(&self) -> usize {
        self.buffer_ref().buffered_len()
    }

    #[inline]
    fn buffered_consumed(&self) -> usize {
        self.buffer_ref().buffered_consumed()
    }

    #[inline]
    fn buffered_consumed_slice(&self) -> &[u8] {
        self.buffer_ref().buffered_consumed_slice()
    }

    #[inline]
    fn sniffed_prefix_remaining(&self) -> usize {
        self.buffer_ref().sniffed_prefix_remaining()
    }

    #[inline]
    fn legacy_prefix_complete(&self) -> bool {
        self.buffer_ref().legacy_prefix_complete()
    }

    #[inline]
    fn buffered_remaining(&self) -> usize {
        self.buffer_ref().buffered_remaining()
    }

    #[inline]
    fn buffered_remaining_slice(&self) -> &[u8] {
        self.buffer_ref().buffered_remaining_slice()
    }

    #[inline]
    fn buffered_remaining_vectored(&self) -> NegotiationBufferedSlices<'_> {
        self.buffer_ref().buffered_remaining_vectored()
    }

    #[inline]
    fn buffered_remaining_to_vec(&self) -> Result<Vec<u8>, TryReserveError> {
        self.buffer_ref().buffered_remaining_to_vec()
    }

    #[inline]
    fn copy_buffered_into(&self, target: &mut Vec<u8>) -> Result<usize, TryReserveError> {
        self.buffer_ref().copy_into_vec(target)
    }

    #[inline]
    fn copy_buffered_into_vectored(
        &self,
        bufs: &mut [IoSliceMut<'_>],
    ) -> Result<usize, BufferedCopyTooSmall> {
        self.buffer_ref().copy_all_into_vectored(bufs)
    }

    #[inline]
    fn copy_buffered_remaining_into_vectored(
        &self,
        bufs: &mut [IoSliceMut<'_>],
    ) -> Result<usize, BufferedCopyTooSmall> {
        self.buffer_ref().copy_remaining_into_vectored(bufs)
    }

    #[inline]
    fn copy_buffered_into_slice(&self, target: &mut [u8]) -> Result<usize, BufferedCopyTooSmall> {
        self.buffer_ref().copy_all_into_slice(target)
    }

    #[inline]
    fn copy_buffered_remaining_into_slice(
        &self,
        target: &mut [u8],
    ) -> Result<usize, BufferedCopyTooSmall> {
        self.buffer_ref().copy_remaining_into_slice(target)
    }

    #[inline]
    fn copy_buffered_into_array<const N: usize>(
        &self,
        target: &mut [u8; N],
    ) -> Result<usize, BufferedCopyTooSmall> {
        self.buffer_ref().copy_all_into_array(target)
    }

    #[inline]
    fn copy_buffered_remaining_into_array<const N: usize>(
        &self,
        target: &mut [u8; N],
    ) -> Result<usize, BufferedCopyTooSmall> {
        self.buffer_ref().copy_remaining_into_array(target)
    }

    #[inline]
    fn copy_buffered_into_writer<W: Write>(&self, target: &mut W) -> io::Result<usize> {
        self.buffer_ref().copy_all_into_writer(target)
    }

    #[inline]
    fn copy_buffered_remaining_into_writer<W: Write>(&self, target: &mut W) -> io::Result<usize> {
        self.buffer_ref().copy_remaining_into_writer(target)
    }
}
