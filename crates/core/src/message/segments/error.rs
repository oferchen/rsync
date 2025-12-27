use std::io;

use thiserror::Error;

/// Error returned when [`MessageSegments::copy_to_slice`][super::MessageSegments::copy_to_slice]
/// receives an undersized buffer.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
#[error("buffer length {provided} is insufficient for message requiring {required} bytes")]
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

impl From<CopyToSliceError> for io::Error {
    fn from(err: CopyToSliceError) -> Self {
        io::Error::new(io::ErrorKind::InvalidInput, err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_returns_required_bytes() {
        let err = CopyToSliceError::new(100, 50);
        assert_eq!(err.required(), 100);
    }

    #[test]
    fn provided_returns_provided_bytes() {
        let err = CopyToSliceError::new(100, 50);
        assert_eq!(err.provided(), 50);
    }

    #[test]
    fn missing_calculates_difference() {
        let err = CopyToSliceError::new(100, 50);
        assert_eq!(err.missing(), 50);
    }

    #[test]
    fn missing_saturates_when_provided_exceeds_required() {
        let err = CopyToSliceError::new(50, 100);
        assert_eq!(err.missing(), 0);
    }

    #[test]
    fn missing_zero_when_equal() {
        let err = CopyToSliceError::new(100, 100);
        assert_eq!(err.missing(), 0);
    }

    #[test]
    fn error_display_contains_values() {
        let err = CopyToSliceError::new(100, 50);
        let display = format!("{err}");
        assert!(display.contains("100"));
        assert!(display.contains("50"));
    }

    #[test]
    fn error_is_clone() {
        let err = CopyToSliceError::new(100, 50);
        let cloned = err;
        assert_eq!(cloned.required(), 100);
    }

    #[test]
    fn error_is_eq() {
        let err1 = CopyToSliceError::new(100, 50);
        let err2 = CopyToSliceError::new(100, 50);
        assert_eq!(err1, err2);
    }

    #[test]
    fn error_ne_different_required() {
        let err1 = CopyToSliceError::new(100, 50);
        let err2 = CopyToSliceError::new(200, 50);
        assert_ne!(err1, err2);
    }

    #[test]
    fn error_ne_different_provided() {
        let err1 = CopyToSliceError::new(100, 50);
        let err2 = CopyToSliceError::new(100, 60);
        assert_ne!(err1, err2);
    }

    #[test]
    fn from_io_error_returns_invalid_input() {
        let err = CopyToSliceError::new(100, 50);
        let io_err: io::Error = err.into();
        assert_eq!(io_err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn io_error_message_contains_details() {
        let err = CopyToSliceError::new(100, 50);
        let io_err: io::Error = err.into();
        let msg = io_err.to_string();
        assert!(msg.contains("100"));
        assert!(msg.contains("50"));
    }
}
