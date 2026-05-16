use std::collections::TryReserveError;
use std::io::{self, IoSliceMut, Write};

use super::super::{BufferedCopyTooSmall, NegotiationBufferAccess, NegotiationBufferedSlices};
use super::base::NegotiatedStream;

impl<R> NegotiatedStream<R> {
    /// Returns the bytes that were required to classify the negotiation prologue.
    #[must_use]
    pub fn sniffed_prefix(&self) -> &[u8] {
        NegotiationBufferAccess::sniffed_prefix(self)
    }

    /// Returns the unread bytes buffered beyond the sniffed negotiation prefix.
    #[must_use]
    pub fn buffered_remainder(&self) -> &[u8] {
        NegotiationBufferAccess::buffered_remainder(self)
    }

    /// Returns the bytes captured during negotiation sniffing, including the prefix and remainder.
    #[must_use]
    pub fn buffered(&self) -> &[u8] {
        NegotiationBufferAccess::buffered(self)
    }

    /// Collects the buffered negotiation transcript into an owned [`Vec<u8>`].
    ///
    /// Allocates a new vector sized exactly for the captured transcript via
    /// [`Vec::try_reserve_exact`], propagating allocation failures as
    /// [`TryReserveError`].
    pub fn buffered_to_vec(&self) -> Result<Vec<u8>, TryReserveError> {
        NegotiationBufferAccess::buffered_to_vec(self)
    }

    /// Returns the buffered negotiation data split into vectored slices.
    ///
    /// The first slice contains the canonical legacy prefix (if present) while the second slice
    /// holds any additional payload captured alongside the prologue. Callers can forward the
    /// slices directly to [`Write::write_vectored`] without copying the buffered bytes.
    #[must_use]
    pub fn buffered_vectored(&self) -> NegotiationBufferedSlices<'_> {
        NegotiationBufferAccess::buffered_vectored(self)
    }

    /// Copies the buffered negotiation data into a caller-provided vector without consuming it.
    ///
    /// The vector is cleared before the bytes are appended; capacity is reserved
    /// via [`Vec::try_reserve`], so allocation failures surface as [`TryReserveError`].
    pub fn copy_buffered_into_vec(&self, target: &mut Vec<u8>) -> Result<usize, TryReserveError> {
        NegotiationBufferAccess::copy_buffered_into_vec(self, target)
    }

    /// Copies the unread portion of the buffered negotiation data into `target` without consuming it.
    ///
    /// Restricts the copy to bytes that have not yet been replayed. The vector
    /// is cleared before the bytes are appended and resized via [`Vec::try_reserve`].
    pub fn copy_buffered_remaining_into_vec(
        &self,
        target: &mut Vec<u8>,
    ) -> Result<usize, TryReserveError> {
        NegotiationBufferAccess::copy_buffered_remaining_into_vec(self, target)
    }

    /// Collects the unread portion of the buffered negotiation transcript into a new [`Vec<u8>`].
    ///
    /// Returns an owned buffer containing only bytes that still need to be replayed.
    /// Allocation failures surface as [`TryReserveError`].
    pub fn buffered_remaining_to_vec(&self) -> Result<Vec<u8>, TryReserveError> {
        NegotiationBufferAccess::buffered_remaining_to_vec(self)
    }

    /// Appends the buffered negotiation data to a caller-provided vector without consuming it.
    ///
    /// Like [`Self::copy_buffered_into_vec`] but preserves any existing contents
    /// in `target`. The replay cursor is unchanged.
    pub fn extend_buffered_into_vec(&self, target: &mut Vec<u8>) -> Result<usize, TryReserveError> {
        NegotiationBufferAccess::extend_buffered_into_vec(self, target)
    }

    /// Appends the unread portion of the buffered negotiation transcript to `target` without consuming it.
    ///
    /// Remaining-byte counterpart to [`Self::extend_buffered_into_vec`].
    pub fn extend_buffered_remaining_into_vec(
        &self,
        target: &mut Vec<u8>,
    ) -> Result<usize, TryReserveError> {
        NegotiationBufferAccess::extend_buffered_remaining_into_vec(self, target)
    }

    /// Returns the sniffed negotiation prefix together with any buffered remainder.
    ///
    /// The first element is the unread portion of the canonical prefix; the
    /// second is any additional payload captured alongside the detection bytes.
    #[must_use]
    pub fn buffered_split(&self) -> (&[u8], &[u8]) {
        NegotiationBufferAccess::buffered_split(self)
    }

    /// Returns the total number of buffered bytes staged for replay.
    #[must_use]
    pub fn buffered_len(&self) -> usize {
        NegotiationBufferAccess::buffered_len(self)
    }

    /// Returns how many buffered bytes have already been replayed.
    ///
    /// Increases as callers consume data via [`std::io::Read::read`],
    /// [`std::io::Read::read_vectored`], or [`std::io::BufRead::consume`].
    /// Once it matches [`Self::buffered_len`] the replay buffer is exhausted.
    #[must_use]
    pub fn buffered_consumed(&self) -> usize {
        NegotiationBufferAccess::buffered_consumed(self)
    }

    /// Returns the portion of the buffered negotiation transcript that has already been replayed.
    ///
    /// Empty until data is read from the [`NegotiatedStream`].
    #[must_use]
    pub fn buffered_consumed_slice(&self) -> &[u8] {
        NegotiationBufferAccess::buffered_consumed_slice(self)
    }

    /// Returns the length of the sniffed negotiation prefix.
    ///
    /// For legacy ASCII handshakes this matches the length of the canonical
    /// `@RSYNCD:` prefix. Pairs with [`Self::sniffed_prefix_remaining`].
    #[must_use]
    pub const fn sniffed_prefix_len(&self) -> usize {
        self.buffer_storage().sniffed_prefix_len()
    }

    /// Returns how many bytes from the sniffed negotiation prefix remain buffered.
    ///
    /// Decreases as callers consume the replay data. Zero means the detection
    /// prefix has been drained and subsequent reads operate on the inner transport.
    #[must_use]
    pub fn sniffed_prefix_remaining(&self) -> usize {
        NegotiationBufferAccess::sniffed_prefix_remaining(self)
    }

    /// Reports whether the canonical legacy negotiation prefix has been fully buffered.
    ///
    /// `true` once the entire `@RSYNCD:` marker has been captured. Binary
    /// negotiations always return `false`. Unaffected by consumption.
    #[must_use]
    pub fn legacy_prefix_complete(&self) -> bool {
        NegotiationBufferAccess::legacy_prefix_complete(self)
    }

    /// Returns the remaining number of buffered bytes that have not yet been read.
    #[must_use]
    pub fn buffered_remaining(&self) -> usize {
        NegotiationBufferAccess::buffered_remaining(self)
    }

    /// Returns the portion of the buffered negotiation data that has not been consumed yet.
    ///
    /// Mirrors [`std::io::BufRead::fill_buf`] but takes `&self` so callers can
    /// inspect the remaining transcript without borrowing mutably.
    #[must_use]
    pub fn buffered_remaining_slice(&self) -> &[u8] {
        NegotiationBufferAccess::buffered_remaining_slice(self)
    }

    /// Returns the unread portion of the buffered negotiation data as vectored slices.
    ///
    /// When the sniffed prefix has been partially consumed, the first slice
    /// covers remaining prefix bytes and the second slice covers buffered payload.
    #[must_use]
    pub fn buffered_remaining_vectored(&self) -> NegotiationBufferedSlices<'_> {
        NegotiationBufferAccess::buffered_remaining_vectored(self)
    }

    /// Copies the buffered negotiation prefix and any captured remainder into `target`.
    ///
    /// The destination vector is cleared before new data is written; capacity is
    /// grown as needed and the resulting length is returned.
    ///
    /// # Errors
    ///
    /// Propagates [`TryReserveError`] when the vector cannot be grown. On
    /// failure `target` retains its previous contents.
    pub fn copy_buffered_into(&self, target: &mut Vec<u8>) -> Result<usize, TryReserveError> {
        NegotiationBufferAccess::copy_buffered_into(self, target)
    }

    /// Copies the buffered negotiation data into the provided vectored buffers without consuming it.
    ///
    /// # Errors
    ///
    /// Returns [`BufferedCopyTooSmall`] when the combined capacity of `bufs` is
    /// smaller than the buffered negotiation payload.
    pub fn copy_buffered_into_vectored(
        &self,
        bufs: &mut [IoSliceMut<'_>],
    ) -> Result<usize, BufferedCopyTooSmall> {
        NegotiationBufferAccess::copy_buffered_into_vectored(self, bufs)
    }

    /// Copies the unread portion of the buffered negotiation data into the provided vectored buffers without consuming it.
    pub fn copy_buffered_remaining_into_vectored(
        &self,
        bufs: &mut [IoSliceMut<'_>],
    ) -> Result<usize, BufferedCopyTooSmall> {
        NegotiationBufferAccess::copy_buffered_remaining_into_vectored(self, bufs)
    }

    /// Copies the buffered negotiation data into the caller-provided slice without consuming it.
    ///
    /// Returns [`BufferedCopyTooSmall`] when the destination is too small; the
    /// slice is left unchanged on failure.
    pub fn copy_buffered_into_slice(
        &self,
        target: &mut [u8],
    ) -> Result<usize, BufferedCopyTooSmall> {
        NegotiationBufferAccess::copy_buffered_into_slice(self, target)
    }

    /// Copies the unread portion of the buffered negotiation data into `target` without consuming it.
    ///
    /// Restricts the operation to bytes that have not yet been replayed; the
    /// slice is left unchanged on failure.
    pub fn copy_buffered_remaining_into_slice(
        &self,
        target: &mut [u8],
    ) -> Result<usize, BufferedCopyTooSmall> {
        NegotiationBufferAccess::copy_buffered_remaining_into_slice(self, target)
    }

    /// Copies the buffered negotiation data into a caller-provided array without consuming it.
    ///
    /// Like [`Self::copy_buffered_into_slice`] but accepts a fixed-size array
    /// to avoid an explicit conversion at the call site.
    pub fn copy_buffered_into_array<const N: usize>(
        &self,
        target: &mut [u8; N],
    ) -> Result<usize, BufferedCopyTooSmall> {
        NegotiationBufferAccess::copy_buffered_into_array(self, target)
    }

    /// Copies the unread portion of the buffered negotiation data into a caller-provided array without consuming it.
    pub fn copy_buffered_remaining_into_array<const N: usize>(
        &self,
        target: &mut [u8; N],
    ) -> Result<usize, BufferedCopyTooSmall> {
        NegotiationBufferAccess::copy_buffered_remaining_into_array(self, target)
    }

    /// Streams the buffered negotiation data into the provided writer without consuming it.
    ///
    /// Any I/O error reported by the writer is propagated unchanged.
    pub fn copy_buffered_into_writer<W: Write>(&self, target: &mut W) -> io::Result<usize> {
        NegotiationBufferAccess::copy_buffered_into_writer(self, target)
    }

    /// Streams the unread portion of the buffered negotiation data into the provided writer.
    pub fn copy_buffered_remaining_into_writer<W: Write>(
        &self,
        target: &mut W,
    ) -> io::Result<usize> {
        NegotiationBufferAccess::copy_buffered_remaining_into_writer(self, target)
    }
}
