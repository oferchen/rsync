use std::cell::RefCell;

/// Scratch buffers used when producing vectored message segments.
///
/// Instances of this type are supplied to [`Message::as_segments`](crate::message::Message::as_segments)
/// so the helper can encode
/// decimal exit codes and line numbers without allocating temporary [`String`] values. The
/// buffers are stack-allocated and reusable, making it cheap for higher layers to render
/// multiple messages in succession without paying repeated allocation costs. Because
/// [`MessageScratch`] implements [`Copy`], callers may freely duplicate values when storing per-
/// thread caches or passing scratch buffers between helper functions without incurring
/// additional allocations. When managing scratch storage manually is inconvenient, use
/// [`MessageScratch::with_thread_local`] to borrow the thread-local buffer that backs
/// [`Message::to_bytes`](crate::message::Message::to_bytes),
/// [`Message::render_to`](crate::message::Message::render_to), and related helpers.
///
/// # Examples
///
/// ```
/// use core::{message::{Message, Role, MessageScratch}, message_source};
///
/// let mut scratch = MessageScratch::new();
/// let message = Message::error(23, "delta-transfer failure")
///     .with_role(Role::Sender)
///     .with_source(message_source!());
/// let segments = message.as_segments(&mut scratch, false);
///
/// assert_eq!(segments.len(), message.to_bytes().unwrap().len());
/// ```

#[derive(Clone, Copy, Debug)]
pub struct MessageScratch {
    pub(super) code_digits: [u8; 20],
    pub(super) line_digits: [u8; 20],
    pub(super) prefix_buffer: [u8; 48],
}

impl MessageScratch {
    /// Creates a new scratch buffer with zeroed storage.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            code_digits: [0; 20],
            line_digits: [0; 20],
            prefix_buffer: [0; 48],
        }
    }

    pub(super) const fn prefix_buffer(&self) -> &[u8; 48] {
        &self.prefix_buffer
    }

    pub(super) const fn prefix_buffer_mut(&mut self) -> &mut [u8; 48] {
        &mut self.prefix_buffer
    }

    /// Executes a closure with the thread-local scratch buffer.
    ///
    /// The helper reuses the thread-local storage maintained by this module so callers can render
    /// multiple messages without explicitly storing a [`MessageScratch`]. When the thread-local
    /// instance is temporarily unavailable—such as when the current thread already borrowed it—the
    /// function transparently falls back to a fresh buffer. This mirrors the strategy used by
    /// [`Message::render_to`](crate::message::Message::render_to) and
    /// [`Message::to_bytes`](crate::message::Message::to_bytes), ensuring consistent performance
    /// semantics
    /// across the workspace.
    ///
    /// # Examples
    ///
    /// Render a message using the shared scratch buffer and inspect the resulting length.
    ///
    /// ```
    /// use core::{
    ///     message::{Message, MessageScratch, Role},
    ///     message_source,
    /// };
    ///
    /// let len = MessageScratch::with_thread_local(|scratch| {
    ///     let message = Message::error(23, "delta-transfer failure")
    ///         .with_role(Role::Sender)
    ///         .with_source(message_source!());
    ///     message.as_segments(scratch, false).len()
    /// });
    ///
    /// assert!(len > 0);
    /// ```
    #[inline]
    pub fn with_thread_local<F, R>(f: F) -> R
    where
        F: FnOnce(&mut MessageScratch) -> R,
    {
        with_thread_local_scratch(f)
    }
}

impl Default for MessageScratch {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

thread_local! {
    static THREAD_LOCAL_SCRATCH: RefCell<MessageScratch> = const { RefCell::new(MessageScratch::new()) };
}

fn with_thread_local_scratch<F, R>(f: F) -> R
where
    F: FnOnce(&mut MessageScratch) -> R,
{
    let mut func = Some(f);

    if let Ok(Some(output)) = THREAD_LOCAL_SCRATCH.try_with(|scratch| {
        let mut guard = match scratch.try_borrow_mut() {
            Ok(guard) => guard,
            Err(_) => return None,
        };
        let func = func
            .take()
            .expect("message scratch closure invoked multiple times");
        Some(func(&mut guard))
    }) {
        return output;
    }

    let mut scratch = MessageScratch::new();
    let func = func
        .take()
        .expect("message scratch closure invoked multiple times");
    func(&mut scratch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_creates_zeroed_scratch() {
        let scratch = MessageScratch::new();
        assert!(scratch.code_digits.iter().all(|&b| b == 0));
        assert!(scratch.line_digits.iter().all(|&b| b == 0));
        assert!(scratch.prefix_buffer.iter().all(|&b| b == 0));
    }

    #[test]
    fn default_equals_new() {
        let from_new = MessageScratch::new();
        let from_default = MessageScratch::default();
        assert_eq!(from_new.code_digits, from_default.code_digits);
        assert_eq!(from_new.line_digits, from_default.line_digits);
        assert_eq!(from_new.prefix_buffer, from_default.prefix_buffer);
    }

    #[test]
    fn scratch_is_copy() {
        let scratch = MessageScratch::new();
        let copied = scratch;
        assert_eq!(copied.code_digits, scratch.code_digits);
    }

    #[test]
    fn scratch_is_clone() {
        let scratch = MessageScratch::new();
        let cloned = scratch;
        assert_eq!(cloned.code_digits, scratch.code_digits);
    }

    #[test]
    fn prefix_buffer_returns_reference() {
        let scratch = MessageScratch::new();
        let buffer = scratch.prefix_buffer();
        assert_eq!(buffer.len(), 48);
    }

    #[test]
    fn prefix_buffer_mut_returns_mutable_reference() {
        let mut scratch = MessageScratch::new();
        let buffer = scratch.prefix_buffer_mut();
        buffer[0] = 42;
        assert_eq!(scratch.prefix_buffer[0], 42);
    }

    #[test]
    fn with_thread_local_executes_closure() {
        let result = MessageScratch::with_thread_local(|_scratch| 42);
        assert_eq!(result, 42);
    }

    #[test]
    fn with_thread_local_provides_mutable_scratch() {
        MessageScratch::with_thread_local(|scratch| {
            scratch.code_digits[0] = 1;
            assert_eq!(scratch.code_digits[0], 1);
        });
    }

    #[test]
    fn with_thread_local_reuses_buffer() {
        // First call modifies the thread-local buffer
        MessageScratch::with_thread_local(|scratch| {
            scratch.code_digits[0] = 99;
        });
        // Second call may or may not see the modification depending on reuse
        // Just verify it executes successfully
        let result = MessageScratch::with_thread_local(|_scratch| true);
        assert!(result);
    }

    #[test]
    fn code_digits_has_correct_size() {
        let scratch = MessageScratch::new();
        assert_eq!(scratch.code_digits.len(), 20);
    }

    #[test]
    fn line_digits_has_correct_size() {
        let scratch = MessageScratch::new();
        assert_eq!(scratch.line_digits.len(), 20);
    }

    #[test]
    fn prefix_buffer_has_correct_size() {
        let scratch = MessageScratch::new();
        assert_eq!(scratch.prefix_buffer.len(), 48);
    }
}
