use std::collections::TryReserveError;
use std::io;

use super::super::errors::map_message_reserve_error;
use super::MessageSegments;

impl<'a> MessageSegments<'a> {
    /// Attempts to extend the provided buffer with the rendered message bytes
    /// without mapping allocation failures.
    ///
    /// The method ensures enough capacity for the rendered message by using
    /// [`Vec::try_reserve_exact`], avoiding the exponential growth strategy of
    /// [`Vec::try_reserve`]. Once space is reserved it appends each segment via
    /// [`Vec::extend_from_slice`], eliminating the intermediate zero-fill that
    /// [`Vec::resize`] would otherwise perform. This keeps allocations tight for
    /// call sites that accumulate multiple diagnostics into a single [`Vec<u8>`]
    /// without going through the [`Write`](io::Write) trait while ensuring no
    /// redundant memory writes occur. When allocation fails the original
    /// [`TryReserveError`] is returned to the caller, allowing higher layers to
    /// surface precise diagnostics or retry with an alternative strategy.
    ///
    /// # Examples
    ///
    /// ```
    /// # use std::collections::TryReserveError;
    /// use rsync_core::{
    ///     message::{Message, MessageScratch, Role},
    ///     message_source,
    /// };
    ///
    /// # fn demo() -> Result<(), TryReserveError> {
    /// let mut scratch = MessageScratch::new();
    /// let message = Message::error(23, "delta-transfer failure")
    ///     .with_role(Role::Sender)
    ///     .with_source(message_source!());
    ///
    /// let segments = message.as_segments(&mut scratch, false);
    /// let mut buffer = b"prefix: ".to_vec();
    /// let prefix_len = buffer.len();
    /// let appended = segments.try_extend_vec(&mut buffer)?;
    ///
    /// assert_eq!(&buffer[..prefix_len], b"prefix: ");
    /// assert_eq!(&buffer[prefix_len..], message.to_bytes().unwrap().as_slice());
    /// assert_eq!(appended, message.to_bytes().unwrap().len());
    /// # Ok(())
    /// # }
    /// # demo().unwrap();
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
    /// use rsync_core::{
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
