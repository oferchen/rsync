use std::fmt;
use std::io;

/// Error returned when [`MessageSegments::copy_to_slice`][super::MessageSegments::copy_to_slice]
/// receives an undersized buffer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CopyToSliceError {
    pub(super) required: usize,
    pub(super) provided: usize,
}

impl CopyToSliceError {
    pub(super) const fn new(required: usize, provided: usize) -> Self {
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
    /// error originates from [`MessageSegments::copy_to_slice`][super::MessageSegments::copy_to_slice],
    /// the result matches `required - provided`, mirroring upstream rsync's messaging when a scratch
    /// buffer is too small to hold the fully rendered diagnostic.
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
