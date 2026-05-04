use std::borrow::Cow;
use std::io::IoSlice;

use super::{
    MAX_MESSAGE_SEGMENTS, MessageScratch, MessageSegments, Role, Severity, SourceLocation,
    VERSION_SUFFIX,
    numbers::{encode_signed_decimal, encode_unsigned_decimal},
};
use crate::branding::Brand;

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
/// use core::{message::{Message, Role}, message_source};
///
/// let message = Message::error(23, "delta-transfer failure")
///     .with_role(Role::Sender)
///     .with_source(message_source!());
///
/// let rendered = message.to_string();
/// assert!(rendered.contains("delta-transfer failure"));
/// assert!(rendered.contains(&format!(
///     "[sender={}]",
///     core::version::RUST_VERSION
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
    brand: Brand,
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

        let prefix_len = self.render_prefix(self.brand, scratch);

        let code_digits_range = self.code.map(|code| {
            let digits = encode_signed_decimal(i64::from(code), &mut scratch.code_digits);
            let len = digits.len();
            (scratch.code_digits.len() - len, len)
        });

        let source_info = self.source.as_ref().map(|source| {
            let digits =
                encode_unsigned_decimal(u64::from(source.line()), &mut scratch.line_digits);
            let len = digits.len();
            let start = scratch.line_digits.len() - len;
            (source.path().as_bytes(), start, len)
        });

        {
            let buffer = scratch.prefix_buffer();
            push(&buffer[..prefix_len]);
        }
        push(self.text.as_bytes());

        if let Some((start, len)) = code_digits_range {
            push(b" (code ");
            let digits = &scratch.code_digits[start..start + len];
            push(digits);
            push(b")");
        }

        if let Some((path_bytes, start, len)) = source_info {
            push(b" at ");
            push(path_bytes);
            push(b":");
            let digits = &scratch.line_digits[start..start + len];
            push(digits);
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

    fn render_prefix(&self, brand: Brand, scratch: &mut MessageScratch) -> usize {
        let program_name = match brand {
            Brand::Oc => match self.role {
                Some(Role::Daemon | Role::Server) => Brand::Oc.daemon_program_name(),
                _ => Brand::Oc.client_program_name(),
            },
            Brand::Upstream => Brand::Upstream.client_program_name(),
        };
        let severity = self.severity.as_str();
        let buffer = scratch.prefix_buffer_mut();
        let mut len = 0usize;

        buffer[..program_name.len()].copy_from_slice(program_name.as_bytes());
        len += program_name.len();
        buffer[len] = b' ';
        len += 1;
        buffer[len..len + severity.len()].copy_from_slice(severity.as_bytes());
        len += severity.len();
        buffer[len] = b':';
        len += 1;
        buffer[len] = b' ';
        len += 1;

        len
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
