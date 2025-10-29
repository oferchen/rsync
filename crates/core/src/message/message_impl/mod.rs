use std::borrow::Cow;
use std::io::IoSlice;

use super::{
    MAX_MESSAGE_SEGMENTS, MessageScratch, MessageSegments, Role, Severity, SourceLocation,
    VERSION_SUFFIX,
    numbers::{encode_signed_decimal, encode_unsigned_decimal},
};

mod classification;
mod constructors;
mod mutators;
mod render;
mod scratch_render;

/// Structured representation of an rsync user-visible message.
///
/// # Examples
///
/// ```
/// use rsync_core::{message::{Message, Role}, message_source};
///
/// let message = Message::error(23, "delta-transfer failure")
///     .with_role(Role::Sender)
///     .with_source(message_source!());
///
/// let rendered = message.to_string();
/// assert!(rendered.contains("delta-transfer failure"));
/// assert!(rendered.contains(&format!(
///     "[sender={}]",
///     rsync_core::version::RUST_VERSION
/// )));
/// ```
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
#[must_use = "messages must be formatted or emitted to reach users"]
pub struct Message {
    severity: Severity,
    code: Option<i32>,
    text: Cow<'static, str>,
    role: Option<Role>,
    source: Option<SourceLocation>,
}

impl Message {
    /// Returns the vectored representation of the rendered message.
    ///
    /// This helper exposes the same slices used internally when emitting the message into an
    /// [`std::io::Write`] implementor. Callers that integrate with custom buffered pipelines can
    /// reuse the returned segments with [`std::io::Write::write_vectored`], avoiding redundant
    /// allocations or per-segment formatting logic.
    #[must_use]
    pub fn as_segments<'a>(
        &'a self,
        scratch: &'a mut MessageScratch,
        include_newline: bool,
    ) -> MessageSegments<'a> {
        let mut segments: [IoSlice<'a>; MAX_MESSAGE_SEGMENTS] =
            [IoSlice::new(&[]); MAX_MESSAGE_SEGMENTS];
        let mut count = 0usize;
        let mut total_len = 0usize;

        let mut push = |slice: &'a [u8]| {
            if slice.is_empty() {
                return;
            }

            debug_assert!(
                count < segments.len(),
                "message segments exceeded allocation"
            );
            segments[count] = IoSlice::new(slice);
            count += 1;
            total_len += slice.len();
        };

        push(self.severity.prefix().as_bytes());
        push(self.text.as_bytes());

        if let Some(code) = self.code {
            push(b" (code ");
            let digits = encode_signed_decimal(i64::from(code), &mut scratch.code_digits);
            push(digits.as_bytes());
            push(b")");
        }

        if let Some(source) = &self.source {
            push(b" at ");
            push(source.path().as_bytes());
            push(b":");
            let digits =
                encode_unsigned_decimal(u64::from(source.line()), &mut scratch.line_digits);
            push(digits.as_bytes());
        }

        if let Some(role) = self.role {
            push(b" [");
            push(role.as_str().as_bytes());
            push(b"=");
            push(VERSION_SUFFIX.as_bytes());
            push(b"]");
        }

        if include_newline {
            push(b"\n");
        }

        MessageSegments {
            segments,
            count,
            total_len,
        }
    }

    /// Invokes the provided closure with the vectored representation of the message.
    #[inline]
    pub fn with_segments<R>(
        &self,
        include_newline: bool,
        f: impl FnOnce(&MessageSegments<'_>) -> R,
    ) -> R {
        MessageScratch::with_thread_local(|scratch| {
            let segments = self.as_segments(scratch, include_newline);
            f(&segments)
        })
    }

    /// Returns the number of bytes in the rendered message without a trailing newline.
    #[inline]
    #[must_use]
    pub fn byte_len(&self) -> usize {
        self.with_segments(false, |segments| segments.len())
    }

    /// Returns the number of bytes in the rendered message including the trailing newline.
    #[inline]
    #[must_use]
    pub fn line_byte_len(&self) -> usize {
        self.with_segments(true, |segments| segments.len())
    }
}
