use std::io;

/// Error returned when `NegotiationBufferedSlices::copy_to_slice` receives an undersized buffer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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

impl std::fmt::Display for CopyToSliceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "buffer length {} is insufficient for negotiation transcript requiring {} bytes",
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
/// use transport::sniff_negotiation_stream;
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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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
    /// use transport::sniff_negotiation_stream;
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

impl std::fmt::Display for BufferedCopyTooSmall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "buffered negotiation data requires {} bytes but destination provided {}",
            self.required, self.provided
        )
    }
}

impl std::error::Error for BufferedCopyTooSmall {}

impl From<BufferedCopyTooSmall> for io::Error {
    fn from(err: BufferedCopyTooSmall) -> Self {
        io::Error::new(io::ErrorKind::InvalidInput, err)
    }
}
