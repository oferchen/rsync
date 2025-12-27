use std::io;

use thiserror::Error;

/// Error returned when `NegotiationBufferedSlices::copy_to_slice` receives an undersized buffer.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
#[error(
    "buffer length {provided} is insufficient for negotiation transcript requiring {required} bytes"
)]
pub struct CopyToSliceError {
    required: usize,
    provided: usize,
}

impl CopyToSliceError {
    pub(crate) const fn new(required: usize, provided: usize) -> Self {
        Self { required, provided }
    }

    /// Number of bytes required to store the buffered negotiation transcript.
    #[must_use]
    pub const fn required(self) -> usize {
        self.required
    }

    /// Number of bytes supplied by the caller.
    #[must_use]
    pub const fn provided(self) -> usize {
        self.provided
    }

    /// Returns how many additional bytes would have satisfied the copy request.
    ///
    /// The value saturates when the provided length exceeds the recorded requirement so the
    /// method remains robust even if the error is constructed with inconsistent inputs. Callers
    /// that surface diagnostics to users can therefore embed the `missing` count directly in their
    /// messages without worrying about underflow. When produced by
    /// `NegotiationBufferedSlices::copy_to_slice`, the return value matches `required - provided`,
    /// mirroring the conventions used by upstream rsync when reporting undersized scratch buffers.
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

/// Error returned when a caller-provided buffer is too small to hold the sniffed bytes.
///
/// The structure reports how many bytes were required to copy the replay data and how many were
/// provided by the caller. It mirrors upstream rsync's approach of signalling insufficient
/// capacity without mutating the destination, allowing higher layers to retry with a suitably
/// sized buffer while keeping the captured negotiation prefix intact.
///
/// The error implements [`From`] for [`io::Error`], making it straightforward to integrate with
/// APIs that expect transport errors. The conversion marks the error as
/// [`io::ErrorKind::InvalidInput`], matching upstream rsync's diagnostics when a caller supplies a
/// buffer that cannot hold the sniffed negotiation transcript.
///
/// # Examples
///
/// Convert the error into an [`io::Error`] when a scratch buffer is too small:
///
/// ```
/// use rsync_io::sniff_negotiation_stream;
/// use std::io::{self, Cursor};
///
/// let stream = sniff_negotiation_stream(Cursor::new(b"@RSYNCD: 31.0\nrest".to_vec()))
///     .expect("sniff succeeds");
/// let mut scratch = [0u8; 4];
/// let err = stream
///     .copy_buffered_into_slice(&mut scratch)
///     .expect_err("insufficient capacity must error");
/// let io_err: io::Error = err.into();
/// assert_eq!(io_err.kind(), io::ErrorKind::InvalidInput);
/// assert!(io_err.to_string().contains("requires"));
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
#[error("buffered negotiation data requires {required} bytes but destination provided {provided}")]
pub struct BufferedCopyTooSmall {
    required: usize,
    provided: usize,
}

impl BufferedCopyTooSmall {
    pub(crate) const fn new(required: usize, provided: usize) -> Self {
        Self { required, provided }
    }

    /// Returns the number of bytes necessary to copy the buffered negotiation data.
    #[must_use]
    pub const fn required(self) -> usize {
        self.required
    }

    /// Returns the number of bytes made available by the caller.
    #[must_use]
    pub const fn provided(self) -> usize {
        self.provided
    }

    /// Returns how many additional bytes would have been required for the copy to succeed.
    ///
    /// The difference is calculated with saturation to guard against inconsistent inputs. When the
    /// error originates from helpers such as
    /// [`crate::negotiation::NegotiatedStream::copy_buffered_into_slice`], the
    /// return value matches `required - provided`, mirroring the missing capacity reported by
    /// upstream rsync diagnostics.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_io::sniff_negotiation_stream;
    /// use std::io::Cursor;
    ///
    /// let stream = sniff_negotiation_stream(Cursor::new(b"@RSYNCD: 31.0\nrest".to_vec()))
    ///     .expect("sniff succeeds");
    /// let mut scratch = [0u8; 4];
    /// let err = stream
    ///     .copy_buffered_into_slice(&mut scratch)
    ///     .expect_err("insufficient capacity must error");
    /// assert_eq!(err.missing(), stream.buffered_len() - scratch.len());
    /// ```
    #[must_use]
    pub const fn missing(self) -> usize {
        self.required.saturating_sub(self.provided)
    }
}

impl From<BufferedCopyTooSmall> for io::Error {
    fn from(err: BufferedCopyTooSmall) -> Self {
        io::Error::new(io::ErrorKind::InvalidInput, err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ==== CopyToSliceError tests ====

    #[test]
    fn copy_to_slice_error_new_stores_values() {
        let err = CopyToSliceError::new(100, 50);
        assert_eq!(err.required(), 100);
        assert_eq!(err.provided(), 50);
    }

    #[test]
    fn copy_to_slice_error_missing_calculates_difference() {
        let err = CopyToSliceError::new(100, 60);
        assert_eq!(err.missing(), 40);
    }

    #[test]
    fn copy_to_slice_error_missing_saturates_on_inconsistent_input() {
        // provided > required (inconsistent, but should not panic)
        let err = CopyToSliceError::new(50, 100);
        assert_eq!(err.missing(), 0);
    }

    #[test]
    fn copy_to_slice_error_missing_zero_when_equal() {
        let err = CopyToSliceError::new(100, 100);
        assert_eq!(err.missing(), 0);
    }

    #[test]
    fn copy_to_slice_error_display_includes_values() {
        let err = CopyToSliceError::new(256, 128);
        let msg = err.to_string();
        assert!(msg.contains("256"));
        assert!(msg.contains("128"));
        assert!(msg.contains("insufficient"));
    }

    #[test]
    fn copy_to_slice_error_converts_to_io_error() {
        let err = CopyToSliceError::new(100, 50);
        let io_err: io::Error = err.into();
        assert_eq!(io_err.kind(), io::ErrorKind::InvalidInput);
        assert!(io_err.to_string().contains("100"));
    }

    #[test]
    fn copy_to_slice_error_clone() {
        let err = CopyToSliceError::new(100, 50);
        let cloned = err;
        assert_eq!(err.required(), cloned.required());
        assert_eq!(err.provided(), cloned.provided());
    }

    #[test]
    fn copy_to_slice_error_debug_format() {
        let err = CopyToSliceError::new(100, 50);
        let debug = format!("{err:?}");
        assert!(debug.contains("CopyToSliceError"));
    }

    #[test]
    fn copy_to_slice_error_equality() {
        let a = CopyToSliceError::new(100, 50);
        let b = CopyToSliceError::new(100, 50);
        let c = CopyToSliceError::new(100, 60);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    // ==== BufferedCopyTooSmall tests ====

    #[test]
    fn buffered_copy_too_small_new_stores_values() {
        let err = BufferedCopyTooSmall::new(200, 100);
        assert_eq!(err.required(), 200);
        assert_eq!(err.provided(), 100);
    }

    #[test]
    fn buffered_copy_too_small_missing_calculates_difference() {
        let err = BufferedCopyTooSmall::new(150, 50);
        assert_eq!(err.missing(), 100);
    }

    #[test]
    fn buffered_copy_too_small_missing_saturates() {
        // provided > required should not panic
        let err = BufferedCopyTooSmall::new(50, 100);
        assert_eq!(err.missing(), 0);
    }

    #[test]
    fn buffered_copy_too_small_missing_zero_when_equal() {
        let err = BufferedCopyTooSmall::new(100, 100);
        assert_eq!(err.missing(), 0);
    }

    #[test]
    fn buffered_copy_too_small_display_includes_values() {
        let err = BufferedCopyTooSmall::new(512, 256);
        let msg = err.to_string();
        assert!(msg.contains("512"));
        assert!(msg.contains("256"));
        assert!(msg.contains("requires"));
    }

    #[test]
    fn buffered_copy_too_small_converts_to_io_error() {
        let err = BufferedCopyTooSmall::new(200, 100);
        let io_err: io::Error = err.into();
        assert_eq!(io_err.kind(), io::ErrorKind::InvalidInput);
        assert!(io_err.to_string().contains("200"));
    }

    #[test]
    fn buffered_copy_too_small_clone() {
        let err = BufferedCopyTooSmall::new(200, 100);
        let cloned = err;
        assert_eq!(err.required(), cloned.required());
        assert_eq!(err.provided(), cloned.provided());
    }

    #[test]
    fn buffered_copy_too_small_debug_format() {
        let err = BufferedCopyTooSmall::new(200, 100);
        let debug = format!("{err:?}");
        assert!(debug.contains("BufferedCopyTooSmall"));
    }

    #[test]
    fn buffered_copy_too_small_equality() {
        let a = BufferedCopyTooSmall::new(200, 100);
        let b = BufferedCopyTooSmall::new(200, 100);
        let c = BufferedCopyTooSmall::new(300, 100);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    // ==== Edge cases ====

    #[test]
    fn errors_handle_zero_values() {
        let copy_err = CopyToSliceError::new(0, 0);
        assert_eq!(copy_err.required(), 0);
        assert_eq!(copy_err.provided(), 0);
        assert_eq!(copy_err.missing(), 0);

        let buffered_err = BufferedCopyTooSmall::new(0, 0);
        assert_eq!(buffered_err.required(), 0);
        assert_eq!(buffered_err.provided(), 0);
        assert_eq!(buffered_err.missing(), 0);
    }

    #[test]
    fn errors_handle_large_values() {
        let copy_err = CopyToSliceError::new(usize::MAX, 0);
        assert_eq!(copy_err.required(), usize::MAX);
        assert_eq!(copy_err.missing(), usize::MAX);

        let buffered_err = BufferedCopyTooSmall::new(usize::MAX, usize::MAX / 2);
        assert_eq!(buffered_err.required(), usize::MAX);
        assert_eq!(buffered_err.missing(), usize::MAX - usize::MAX / 2);
    }
}
