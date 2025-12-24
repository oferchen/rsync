use std::collections::TryReserveError;

use protocol::NegotiationPrologueSniffer;

use super::super::super::{NegotiationBufferAccess, NegotiationBufferedSlices};
use super::NegotiatedStreamParts;

impl<R> NegotiatedStreamParts<R> {
    /// Returns the captured negotiation prefix.
    #[must_use]
    pub fn sniffed_prefix(&self) -> &[u8] {
        NegotiationBufferAccess::sniffed_prefix(self)
    }

    /// Returns the buffered remainder.
    #[must_use]
    pub fn buffered_remainder(&self) -> &[u8] {
        NegotiationBufferAccess::buffered_remainder(self)
    }

    /// Collects the buffered remainder into an owned [`Vec<u8>`].
    ///
    /// The helper mirrors [`Self::buffered_remainder`] while returning an owned buffer. It is
    /// particularly useful for diagnostics that snapshot the unread portion of the transcript without
    /// mutating the decomposed parts.
    #[must_use = "the owned buffer contains the unread negotiation transcript"]
    pub fn buffered_remaining_to_vec(&self) -> Result<Vec<u8>, TryReserveError> {
        NegotiationBufferAccess::buffered_remaining_to_vec(self)
    }

    /// Returns the buffered bytes captured during sniffing.
    #[must_use]
    pub fn buffered(&self) -> &[u8] {
        NegotiationBufferAccess::buffered(self)
    }

    /// Collects the buffered negotiation bytes into an owned [`Vec<u8>`].
    ///
    /// The helper mirrors [`Self::buffered`] but allocates a new vector sized for the captured
    /// transcript. Allocation failures propagate as [`TryReserveError`], matching the semantics of
    /// [`crate::negotiation::NegotiatedStream::buffered_to_vec`].
    #[must_use = "the owned buffer contains the negotiation transcript"]
    pub fn buffered_to_vec(&self) -> Result<Vec<u8>, TryReserveError> {
        NegotiationBufferAccess::buffered_to_vec(self)
    }

    /// Returns the buffered negotiation data split into vectored slices.
    #[must_use]
    pub fn buffered_vectored(&self) -> NegotiationBufferedSlices<'_> {
        NegotiationBufferAccess::buffered_vectored(self)
    }

    /// Rehydrates a [`NegotiationPrologueSniffer`] using the captured negotiation snapshot.
    ///
    /// The method mirrors the state captured during the initial prologue sniff,
    /// allowing callers to rebuild a sniffer without replaying the underlying
    /// transport. This keeps the high-level session APIs aligned with the
    /// protocol crate helpers that operate on sniffers.
    #[must_use = "the result indicates whether the sniffer could be rehydrated without reallocating"]
    pub fn rehydrate_sniffer(
        &self,
        sniffer: &mut NegotiationPrologueSniffer,
    ) -> Result<(), TryReserveError> {
        sniffer.rehydrate_from_parts(
            self.decision,
            self.buffer.sniffed_prefix_len(),
            self.buffer.buffered(),
        )
    }

    /// Returns the sniffed negotiation prefix together with any buffered remainder.
    ///
    /// The tuple mirrors [`crate::negotiation::NegotiatedStream::buffered_split`], giving callers convenient access
    /// to both slices when rebuilding replay buffers without cloning the stored negotiation
    /// bytes.
    #[must_use]
    pub fn buffered_split(&self) -> (&[u8], &[u8]) {
        NegotiationBufferAccess::buffered_split(self)
    }

    /// Returns the total number of bytes captured during sniffing.
    #[must_use]
    pub fn buffered_len(&self) -> usize {
        NegotiationBufferAccess::buffered_len(self)
    }

    /// Returns how many buffered bytes had already been replayed when the stream was decomposed.
    ///
    /// The counter mirrors [`crate::negotiation::NegotiatedStream::buffered_consumed`], allowing callers to observe the
    /// replay position without reconstructing the wrapper. It is useful when higher layers need to
    /// resume consumption from the point where the stream was split into parts.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_io::sniff_negotiation_stream;
    /// use std::io::{Cursor, Read};
    ///
    /// let mut stream = sniff_negotiation_stream(Cursor::new(b"@RSYNCD: 31.0\nreply".to_vec()))
    ///     .expect("sniff succeeds");
    /// let mut buf = [0u8; 5];
    /// stream.read(&mut buf).expect("buffered bytes are available");
    /// let consumed = stream.buffered_consumed();
    ///
    /// let parts = stream.into_parts();
    /// assert_eq!(parts.buffered_consumed(), consumed);
    /// ```
    #[must_use]
    pub fn buffered_consumed(&self) -> usize {
        NegotiationBufferAccess::buffered_consumed(self)
    }

    /// Returns the portion of the buffered negotiation transcript that had already been replayed.
    ///
    /// This mirrors [`crate::negotiation::NegotiatedStream::buffered_consumed_slice`] but operates on the decomposed
    /// parts, allowing diagnostics to print the consumed prefix without rebuilding the stream.
    #[must_use]
    pub fn buffered_consumed_slice(&self) -> &[u8] {
        NegotiationBufferAccess::buffered_consumed_slice(self)
    }

    /// Returns how many buffered bytes remain unread.
    #[must_use]
    pub fn buffered_remaining(&self) -> usize {
        NegotiationBufferAccess::buffered_remaining(self)
    }

    /// Returns the portion of the buffered negotiation data that has not been consumed yet.
    ///
    /// The slice starts at the current replay cursor and mirrors what would be produced next if the
    /// parts were turned back into a [`crate::negotiation::NegotiatedStream`] and read from. This is useful when the
    /// parts are temporarily inspected for diagnostics without rebuilding the wrapper.
    #[must_use]
    pub fn buffered_remaining_slice(&self) -> &[u8] {
        NegotiationBufferAccess::buffered_remaining_slice(self)
    }

    /// Returns the unread portion of the buffered negotiation data as vectored slices.
    #[must_use]
    pub fn buffered_remaining_vectored(&self) -> NegotiationBufferedSlices<'_> {
        NegotiationBufferAccess::buffered_remaining_vectored(self)
    }

    /// Returns the length of the sniffed negotiation prefix.
    #[must_use]
    pub const fn sniffed_prefix_len(&self) -> usize {
        self.buffer.sniffed_prefix_len()
    }

    /// Returns how many bytes from the sniffed negotiation prefix remain buffered.
    ///
    /// This mirrors [`crate::negotiation::NegotiatedStream::sniffed_prefix_remaining`], giving callers
    /// access to the replay position after extracting the parts without
    /// reconstructing a [`crate::negotiation::NegotiatedStream`].
    #[must_use]
    pub fn sniffed_prefix_remaining(&self) -> usize {
        NegotiationBufferAccess::sniffed_prefix_remaining(self)
    }

    /// Reports whether the canonical legacy negotiation prefix has been fully buffered.
    ///
    /// The method mirrors [`crate::negotiation::NegotiatedStream::legacy_prefix_complete`], making it possible to query
    /// the sniffed prefix state even after the stream has been decomposed into parts. It is `true`
    /// for legacy ASCII negotiations once the entire `@RSYNCD:` marker has been captured and `false`
    /// otherwise (including for binary sessions).
    #[must_use]
    pub fn legacy_prefix_complete(&self) -> bool {
        NegotiationBufferAccess::legacy_prefix_complete(self)
    }
}
