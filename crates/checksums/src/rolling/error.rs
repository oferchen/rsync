use core::fmt;

/// Errors that can occur while updating the rolling checksum state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RollingError {
    /// The checksum window is empty, preventing the rolling update from making progress.
    EmptyWindow,
    /// The checksum window length exceeds what can be represented in 32 bits.
    WindowTooLarge {
        /// Number of bytes present in the rolling window when the error was raised.
        len: usize,
    },
    /// The number of outgoing bytes does not match the number of incoming bytes.
    MismatchedSliceLength {
        /// Number of bytes being removed from the rolling window.
        outgoing: usize,
        /// Number of bytes being appended to the rolling window.
        incoming: usize,
    },
}

impl fmt::Display for RollingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyWindow => write!(f, "rolling checksum requires a non-empty window"),
            Self::WindowTooLarge { len } => write!(
                f,
                "rolling checksum window of {len} bytes exceeds 32-bit limit"
            ),
            Self::MismatchedSliceLength { outgoing, incoming } => write!(
                f,
                "rolling checksum requires outgoing ({outgoing}) and incoming ({incoming}) slices to have the same length"
            ),
        }
    }
}

impl std::error::Error for RollingError {}

/// Error returned when reconstructing a rolling checksum digest from a byte slice of the wrong length.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RollingSliceError {
    len: usize,
}

impl RollingSliceError {
    /// Number of bytes the caller supplied when the error was raised.
    #[must_use]
    pub const fn len(self) -> usize {
        self.len
    }

    /// Reports whether the provided slice was empty when the error occurred.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_checksums::{RollingDigest, RollingSliceError};
    ///
    /// let err = RollingDigest::from_le_slice(&[], 0).unwrap_err();
    /// assert!(err.is_empty());
    /// ```
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.len == 0
    }

    /// Number of bytes required to decode a rolling checksum digest.
    pub const EXPECTED_LEN: usize = 4;

    #[cfg_attr(test, allow(dead_code))]
    pub(crate) const fn new(len: usize) -> Self {
        Self { len }
    }
}

impl fmt::Display for RollingSliceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "rolling checksum digest requires {} bytes, received {}",
            Self::EXPECTED_LEN,
            self.len
        )
    }
}

impl std::error::Error for RollingSliceError {}
