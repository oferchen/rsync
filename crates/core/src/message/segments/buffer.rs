use std::collections::TryReserveError;
use std::io;

use super::super::errors::map_message_reserve_error;
use super::base::MessageSegments;
use super::error::CopyToSliceError;

impl<'a> MessageSegments<'a> {
    /// Attempts to extend the provided buffer with the rendered message bytes without mapping
    /// allocation failures.
    ///
    /// The method ensures enough capacity for the rendered message by using
    /// [`Vec::try_reserve_exact`], avoiding the exponential growth strategy of
    /// [`Vec::try_reserve`]. Once space is reserved it appends each segment via
    /// [`Vec::extend_from_slice`], eliminating the intermediate zero-fill that
    /// [`Vec::resize`] would otherwise perform. This keeps allocations tight for
    /// call sites that accumulate multiple diagnostics into a single [`Vec<u8>`]
    /// without going through the [`Write`](std::io::Write) trait while ensuring no
    /// redundant memory writes occur. When allocation fails the original
    /// [`TryReserveError`] is returned to the caller, allowing higher layers to
    /// surface precise diagnostics or retry with an alternative strategy.
    ///
    /// # Examples
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
    ///
    /// let segments = message.as_segments(&mut scratch, false);
    /// let mut buffer = b"prefix: ".to_vec();
    /// let prefix_len = buffer.len();
    /// let appended = segments
    ///     .try_extend_vec(&mut buffer)
    ///     .expect("buffer extension succeeds");
    ///
    /// let rendered = message.to_bytes().unwrap();
    /// assert_eq!(&buffer[..prefix_len], b"prefix: ");
    /// assert_eq!(&buffer[prefix_len..], rendered.as_slice());
    /// assert_eq!(appended, rendered.len());
    /// ```
    #[must_use = "buffer extension reserves memory and may fail; handle allocation errors and inspect the appended length"]
    pub fn try_extend_vec(&self, buffer: &mut Vec<u8>) -> Result<usize, TryReserveError> {
        if self.is_empty() {
            return Ok(0);
        }

        let required = self.len();
        let spare = buffer.capacity().saturating_sub(buffer.len());
        if spare < required {
            let additional = required - spare;
            buffer.try_reserve_exact(additional)?;
            debug_assert!(
                buffer.capacity().saturating_sub(buffer.len()) >= required,
                "MessageSegments::try_extend_vec must reserve enough capacity for the entire message",
            );
        }

        let start = buffer.len();
        for slice in self.iter() {
            let bytes: &[u8] = slice.as_ref();
            if bytes.is_empty() {
                continue;
            }

            buffer.extend_from_slice(bytes);
        }

        debug_assert_eq!(buffer.len() - start, required);
        Ok(required)
    }

    /// Extends the provided buffer with the rendered message bytes.
    ///
    /// This convenience wrapper maps the [`TryReserveError`] returned by
    /// [`Self::try_extend_vec`] into an [`io::Error`] so callers that already
    /// operate in I/O contexts do not need to handle allocation failures
    /// explicitly.
    #[must_use = "buffer extension reserves memory and may fail; handle allocation errors and inspect the appended length"]
    pub fn extend_vec(&self, buffer: &mut Vec<u8>) -> io::Result<usize> {
        self.try_extend_vec(buffer)
            .map_err(map_message_reserve_error)
    }

    /// Copies the rendered message into the provided slice.
    ///
    /// The destination slice must be at least [`Self::len`] bytes long. When the capacity is
    /// insufficient the method returns [`CopyToSliceError`] describing the required length so callers
    /// can retry with a suitably sized buffer. This mirrors upstream rsync's approach of reusing
    /// stack-allocated buffers for message rendering while preserving deterministic allocation
    /// patterns.
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
    ///
    /// let mut buffer = vec![0u8; segments.len()];
    /// let copied = segments
    ///     .copy_to_slice(&mut buffer)
    ///     .expect("slice has sufficient capacity");
    ///
    /// assert_eq!(copied, segments.len());
    /// assert_eq!(buffer, message.to_bytes().unwrap());
    /// ```
    #[must_use = "callers should handle the number of copied bytes or the returned error"]
    pub fn copy_to_slice(&self, dest: &mut [u8]) -> Result<usize, CopyToSliceError> {
        let required = self.len();
        if dest.len() < required {
            return Err(CopyToSliceError::new(required, dest.len()));
        }

        let mut offset = 0usize;
        for slice in self.iter() {
            let bytes: &[u8] = slice.as_ref();
            let end = offset + bytes.len();
            dest[offset..end].copy_from_slice(bytes);
            offset = end;
        }
        Ok(required)
    }

    /// Collects the message segments into a freshly allocated [`Vec<u8>`].
    ///
    /// The helper mirrors [`Self::extend_vec`] but manages the buffer lifecycle
    /// internally, returning the rendered bytes directly. This keeps call sites
    /// concise when they only need an owned copy of the message without
    /// providing scratch storage up front. Allocation failures propagate as
    /// [`io::ErrorKind::OutOfMemory`] so diagnostics remain actionable.
    ///
    /// # Examples
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
    ///
    /// let segments = message.as_segments(&mut scratch, false);
    /// let collected = segments.to_vec().expect("allocation succeeds");
    ///
    /// assert_eq!(collected, message.to_bytes().unwrap());
    /// ```
    #[must_use = "collecting message segments allocates and can fail if memory reservations are unsuccessful"]
    pub fn to_vec(&self) -> io::Result<Vec<u8>> {
        let mut buffer = Vec::new();
        let _ = self.extend_vec(&mut buffer)?;
        Ok(buffer)
    }
}

#[cfg(test)]
mod tests {
    use crate::message::{Message, MessageScratch};

    #[test]
    fn try_extend_vec_appends_to_buffer() {
        let mut scratch = MessageScratch::new();
        let msg = Message::info("test");
        let segments = msg.as_segments(&mut scratch, false);
        let mut buffer = Vec::new();
        let result = segments.try_extend_vec(&mut buffer);
        assert!(result.is_ok());
        assert!(!buffer.is_empty());
    }

    #[test]
    fn try_extend_vec_returns_appended_length() {
        let mut scratch = MessageScratch::new();
        let msg = Message::info("test");
        let segments = msg.as_segments(&mut scratch, false);
        let mut buffer = Vec::new();
        let appended = segments.try_extend_vec(&mut buffer).unwrap();
        assert_eq!(appended, buffer.len());
        assert_eq!(appended, segments.len());
    }

    #[test]
    fn try_extend_vec_preserves_prefix() {
        let mut scratch = MessageScratch::new();
        let msg = Message::info("test");
        let segments = msg.as_segments(&mut scratch, false);
        let mut buffer = b"prefix:".to_vec();
        let prefix_len = buffer.len();
        let _ = segments.try_extend_vec(&mut buffer).unwrap();
        assert_eq!(&buffer[..prefix_len], b"prefix:");
    }

    #[test]
    fn extend_vec_maps_error_to_io() {
        let mut scratch = MessageScratch::new();
        let msg = Message::info("test");
        let segments = msg.as_segments(&mut scratch, false);
        let mut buffer = Vec::new();
        let result = segments.extend_vec(&mut buffer);
        assert!(result.is_ok());
    }

    #[test]
    fn copy_to_slice_copies_bytes() {
        let mut scratch = MessageScratch::new();
        let msg = Message::info("test");
        let segments = msg.as_segments(&mut scratch, false);
        let mut buffer = vec![0u8; segments.len()];
        let result = segments.copy_to_slice(&mut buffer);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), segments.len());
    }

    #[test]
    fn copy_to_slice_error_on_undersized_buffer() {
        let mut scratch = MessageScratch::new();
        let msg = Message::info("test");
        let segments = msg.as_segments(&mut scratch, false);
        let mut buffer = vec![0u8; 1];
        let result = segments.copy_to_slice(&mut buffer);
        assert!(result.is_err());
    }

    #[test]
    fn copy_to_slice_error_contains_sizes() {
        let mut scratch = MessageScratch::new();
        let msg = Message::info("test");
        let segments = msg.as_segments(&mut scratch, false);
        let mut buffer = vec![0u8; 1];
        let err = segments.copy_to_slice(&mut buffer).unwrap_err();
        assert_eq!(err.required(), segments.len());
        assert_eq!(err.provided(), 1);
    }

    #[test]
    fn to_vec_returns_collected_bytes() {
        let mut scratch = MessageScratch::new();
        let msg = Message::info("test");
        let segments = msg.as_segments(&mut scratch, false);
        let collected = segments.to_vec().unwrap();
        assert_eq!(collected.len(), segments.len());
    }

    #[test]
    fn to_vec_matches_extend_vec() {
        let mut scratch = MessageScratch::new();
        let msg = Message::info("test");
        let segments = msg.as_segments(&mut scratch, false);
        let collected = segments.to_vec().unwrap();

        let mut scratch2 = MessageScratch::new();
        let segments2 = msg.as_segments(&mut scratch2, false);
        let mut extended = Vec::new();
        segments2.extend_vec(&mut extended).unwrap();

        assert_eq!(collected, extended);
    }
}
