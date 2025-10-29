use std::fmt;
use std::io;

use super::MessageSegments;

impl<'a> MessageSegments<'a> {
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
    /// use rsync_core::{
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
    /// let copied = segments.copy_to_slice(&mut buffer)?;
    ///
    /// assert_eq!(copied, segments.len());
    /// assert_eq!(buffer, message.to_bytes().unwrap());
    /// # Ok::<(), rsync_core::message::CopyToSliceError>(())
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
}

/// Error returned when [`MessageSegments::copy_to_slice`] receives an undersized buffer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CopyToSliceError {
    required: usize,
    provided: usize,
}

impl CopyToSliceError {
    pub(crate) const fn new(required: usize, provided: usize) -> Self {
        Self { required, provided }
    }

    /// Number of bytes required to hold the rendered message.
    #[must_use]
    pub const fn required(self) -> usize {
        self.required
    }

    /// Number of bytes supplied by the caller.
    #[must_use]
    pub const fn provided(self) -> usize {
        self.provided
    }

    /// Returns how many additional bytes were necessary to satisfy the copy operation.
    ///
    /// The calculation uses saturating subtraction so that callers can report the missing capacity
    /// directly in diagnostics even if the error was created with inconsistent inputs. When the
    /// error originates from [`MessageSegments::copy_to_slice`], the result matches
    /// `required - provided`, mirroring upstream rsync's messaging when a scratch buffer is too
    /// small to hold the fully rendered diagnostic.
    #[must_use]
    pub const fn missing(self) -> usize {
        self.required.saturating_sub(self.provided)
    }
}

impl fmt::Display for CopyToSliceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "buffer length {} is insufficient for message requiring {} bytes",
            self.provided, self.required
        )
    }
}

impl std::error::Error for CopyToSliceError {}

impl From<CopyToSliceError> for io::Error {
    fn from(err: CopyToSliceError) -> Self {
        io::Error::new(io::ErrorKind::InvalidInput, err)
    }
}
