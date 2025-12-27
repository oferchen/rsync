use std::io::IoSlice;
use std::iter::FusedIterator;
use std::slice;

use super::super::MAX_MESSAGE_SEGMENTS;

/// Collection of slices that jointly render a [`Message`](crate::message::Message).
///
/// The segments reference the message payload together with optional exit codes, source
/// locations, and role trailers. Callers obtain the structure through
/// [`Message::as_segments`](crate::message::Message::as_segments)
/// and can then stream the slices into vectored writers, aggregate statistics, or reuse the
/// layout when constructing custom buffers. `MessageSegments` implements [`AsRef`] so the
/// collected [`IoSlice`] values can be passed directly to APIs such as
/// [`write_vectored`](std::io::Write::write_vectored) without requiring an intermediate allocation.
///
/// # Examples
///
/// Convert the segments into a slice suitable for [`write_vectored`](std::io::Write::write_vectored).
///
/// ```
/// use core::{
///     message::{Message, MessageScratch, Role},
///     message_source,
/// };
///
/// let mut scratch = MessageScratch::new();
/// let message = Message::error(11, "error in file IO")
///     .with_role(Role::Receiver)
///     .with_source(message_source!());
/// let segments = message.as_segments(&mut scratch, false);
///
/// let slices: &[std::io::IoSlice<'_>] = segments.as_ref();
/// assert_eq!(slices.len(), segments.segment_count());
/// ```
#[derive(Clone, Debug)]
pub struct MessageSegments<'a> {
    pub(in crate::message) segments: [IoSlice<'a>; MAX_MESSAGE_SEGMENTS],
    pub(in crate::message) count: usize,
    pub(in crate::message) total_len: usize,
}

impl<'a> MessageSegments<'a> {
    /// Returns the populated slice view over the underlying [`IoSlice`] array.
    #[inline]
    #[must_use]
    pub fn as_slices(&self) -> &[IoSlice<'a>] {
        &self.segments[..self.count]
    }

    #[inline]
    pub(super) fn as_slices_mut(&mut self) -> &mut [IoSlice<'a>] {
        &mut self.segments[..self.count]
    }

    /// Returns the total number of bytes covered by the message segments.
    #[inline]
    #[must_use]
    pub const fn len(&self) -> usize {
        self.total_len
    }

    /// Reports the number of populated segments.
    #[inline]
    #[must_use]
    pub const fn segment_count(&self) -> usize {
        self.count
    }

    /// Returns an iterator over the populated [`IoSlice`] values.
    ///
    /// The iterator traverses the same slices that [`Self::as_slices`] exposes, preserving their
    /// original ordering so call sites can stream the message into custom sinks without allocating
    /// intermediate buffers. This mirrors upstream rsync's behaviour where formatted messages are
    /// emitted sequentially. The iterator borrows the segments, meaning the caller must keep the
    /// [`MessageSegments`] instance alive for the duration of the iteration.
    ///
    /// # Examples
    ///
    /// Iterate over the segments to compute their cumulative length.
    ///
    /// ```
    /// use core::{
    ///     message::{Message, MessageScratch, Role},
    ///     message_source,
    /// };
    ///
    /// let message = Message::error(23, "delta-transfer failure")
    ///     .with_role(Role::Sender)
    ///     .with_source(message_source!());
    /// let mut scratch = MessageScratch::new();
    /// let segments = message.as_segments(&mut scratch, false);
    /// let total: usize = segments.iter().map(|slice| slice.len()).sum();
    ///
    /// assert_eq!(total, segments.len());
    /// ```
    #[inline]
    #[must_use = "iterate over the slices to observe the rendered message segments"]
    pub fn iter(&self) -> slice::Iter<'_, IoSlice<'a>> {
        self.as_slices().iter()
    }

    /// Returns an iterator over the byte slices referenced by each segment.
    ///
    /// This is a convenience wrapper around [`Self::iter`] that exposes the underlying
    /// `&[u8]` views directly. It is especially useful when callers need to analyse or copy
    /// the rendered bytes without interacting with [`IoSlice`] explicitly, keeping their
    /// code agnostic of vectored I/O details. The iterator preserves the original ordering
    /// and implements [`DoubleEndedIterator`], [`ExactSizeIterator`], and [`FusedIterator`]
    /// so integrations can efficiently consume the slices in either direction.
    ///
    /// # Examples
    ///
    /// ```
    /// use core::{
    ///     message::{Message, MessageScratch, Role},
    ///     message_source,
    /// };
    ///
    /// let message = Message::error(23, "delta-transfer failure")
    ///     .with_role(Role::Sender)
    ///     .with_source(message_source!());
    /// let mut scratch = MessageScratch::new();
    /// let segments = message.as_segments(&mut scratch, false);
    /// let collected: Vec<&[u8]> = segments.iter_bytes().collect();
    ///
    /// assert_eq!(collected.len(), segments.segment_count());
    /// assert_eq!(collected.concat(), message.to_bytes().unwrap());
    /// ```
    #[inline]
    #[must_use]
    pub fn iter_bytes(
        &self,
    ) -> impl ExactSizeIterator<Item = &'_ [u8]> + DoubleEndedIterator + FusedIterator + '_ {
        self.iter().map(move |slice| slice.as_ref())
    }

    /// Reports whether any slices were produced or contain bytes.
    #[inline]
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.count == 0 || self.total_len == 0
    }

    /// Returns a mutable iterator over the populated vectored slices.
    ///
    /// This mirrors [`Self::iter`] but yields mutable references so callers can
    /// adjust slice boundaries prior to issuing writes. The iterator maintains
    /// the original ordering so diagnostics remain byte-identical to upstream
    /// rsync.
    ///
    /// # Examples
    ///
    /// Iterate mutably over the slices and confirm they are all non-empty.
    ///
    /// ```
    /// use core::{
    ///     message::{Message, MessageScratch, Role},
    ///     message_source,
    /// };
    ///
    /// let mut scratch = MessageScratch::new();
    /// let message = Message::error(23, "delta-transfer failure")
    ///     .with_role(Role::Sender)
    ///     .with_source(message_source!());
    /// let mut segments = message.as_segments(&mut scratch, false);
    ///
    /// for slice in &mut segments {
    ///     let bytes: &[u8] = slice.as_ref();
    ///     assert!(!bytes.is_empty());
    /// }
    /// ```
    #[inline]
    #[must_use = "consume the iterator to mutate the in-place segment descriptors"]
    pub fn iter_mut(&mut self) -> slice::IterMut<'_, IoSlice<'a>> {
        self.as_slices_mut().iter_mut()
    }
}

impl<'a> AsRef<[IoSlice<'a>]> for MessageSegments<'a> {
    #[inline]
    fn as_ref(&self) -> &[IoSlice<'a>] {
        self.as_slices()
    }
}

impl<'a> IntoIterator for &'a MessageSegments<'a> {
    type Item = &'a IoSlice<'a>;
    type IntoIter = slice::Iter<'a, IoSlice<'a>>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'a> IntoIterator for &'a mut MessageSegments<'a> {
    type Item = &'a mut IoSlice<'a>;
    type IntoIter = slice::IterMut<'a, IoSlice<'a>>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.iter_mut()
    }
}

impl<'a> IntoIterator for MessageSegments<'a> {
    type Item = IoSlice<'a>;
    type IntoIter = std::iter::Take<std::array::IntoIter<IoSlice<'a>, MAX_MESSAGE_SEGMENTS>>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.segments.into_iter().take(self.count)
    }
}

#[cfg(test)]
mod tests {
    use crate::message::{Message, MessageScratch};

    #[test]
    fn as_slices_returns_populated_segments() {
        let mut scratch = MessageScratch::new();
        let msg = Message::info("test message");
        let segments = msg.as_segments(&mut scratch, false);
        let slices = segments.as_slices();
        assert!(!slices.is_empty());
    }

    #[test]
    fn len_returns_total_bytes() {
        let mut scratch = MessageScratch::new();
        let msg = Message::info("hello");
        let segments = msg.as_segments(&mut scratch, false);
        assert!(segments.len() > 0);
    }

    #[test]
    fn segment_count_matches_slices_len() {
        let mut scratch = MessageScratch::new();
        let msg = Message::info("test");
        let segments = msg.as_segments(&mut scratch, false);
        assert_eq!(segments.segment_count(), segments.as_slices().len());
    }

    #[test]
    fn iter_yields_same_count_as_segment_count() {
        let mut scratch = MessageScratch::new();
        let msg = Message::info("test");
        let segments = msg.as_segments(&mut scratch, false);
        assert_eq!(segments.iter().count(), segments.segment_count());
    }

    #[test]
    fn iter_bytes_yields_byte_slices() {
        let mut scratch = MessageScratch::new();
        let msg = Message::info("test");
        let segments = msg.as_segments(&mut scratch, false);
        let bytes: Vec<&[u8]> = segments.iter_bytes().collect();
        assert_eq!(bytes.len(), segments.segment_count());
    }

    #[test]
    fn iter_bytes_total_matches_len() {
        let mut scratch = MessageScratch::new();
        let msg = Message::info("test");
        let segments = msg.as_segments(&mut scratch, false);
        let total: usize = segments.iter_bytes().map(|b| b.len()).sum();
        assert_eq!(total, segments.len());
    }

    #[test]
    fn is_empty_false_for_message() {
        let mut scratch = MessageScratch::new();
        let msg = Message::info("test");
        let segments = msg.as_segments(&mut scratch, false);
        assert!(!segments.is_empty());
    }

    #[test]
    fn iter_mut_allows_iteration() {
        let mut scratch = MessageScratch::new();
        let msg = Message::info("test");
        let mut segments = msg.as_segments(&mut scratch, false);
        let count = segments.iter_mut().count();
        assert_eq!(count, segments.segment_count());
    }

    #[test]
    fn as_ref_returns_same_as_slices() {
        let mut scratch = MessageScratch::new();
        let msg = Message::info("test");
        let segments = msg.as_segments(&mut scratch, false);
        let via_as_ref: &[std::io::IoSlice<'_>] = segments.as_ref();
        assert_eq!(via_as_ref.len(), segments.as_slices().len());
    }

    #[test]
    fn into_iter_ref_works() {
        let mut scratch = MessageScratch::new();
        let msg = Message::info("test");
        let segments = msg.as_segments(&mut scratch, false);
        let count = (&segments).into_iter().count();
        assert_eq!(count, segments.segment_count());
    }

    #[test]
    fn into_iter_owned_works() {
        let mut scratch = MessageScratch::new();
        let msg = Message::info("test");
        let segments = msg.as_segments(&mut scratch, false);
        let expected_count = segments.segment_count();
        let count = segments.into_iter().count();
        assert_eq!(count, expected_count);
    }

    #[test]
    fn clone_produces_identical_segments() {
        let mut scratch = MessageScratch::new();
        let msg = Message::info("test");
        let segments = msg.as_segments(&mut scratch, false);
        let cloned = segments.clone();
        assert_eq!(cloned.len(), segments.len());
        assert_eq!(cloned.segment_count(), segments.segment_count());
    }

    #[test]
    fn debug_format_works() {
        let mut scratch = MessageScratch::new();
        let msg = Message::info("test");
        let segments = msg.as_segments(&mut scratch, false);
        let debug = format!("{:?}", segments);
        assert!(debug.contains("MessageSegments"));
    }
}
