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

    pub(super) fn prefix_buffer(&self) -> &[u8; 48] {
        &self.prefix_buffer
    }

    pub(super) fn prefix_buffer_mut(&mut self) -> &mut [u8; 48] {
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
