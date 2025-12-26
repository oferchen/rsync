use thiserror::Error;

/// Errors that can occur while updating the rolling checksum state.
#[derive(Clone, Debug, Eq, PartialEq, Error)]
pub enum RollingError {
    /// The checksum window is empty, preventing the rolling update from making progress.
    #[error("rolling checksum requires a non-empty window")]
    EmptyWindow,
    /// The checksum window length exceeds what can be represented in 32 bits.
    #[error("rolling checksum window of {len} bytes exceeds 32-bit limit")]
    WindowTooLarge {
        /// Number of bytes present in the rolling window when the error was raised.
        len: usize,
    },
    /// The number of outgoing bytes does not match the number of incoming bytes.
    #[error(
        "rolling checksum requires outgoing ({outgoing}) and incoming ({incoming}) slices to have the same length"
    )]
    MismatchedSliceLength {
        /// Number of bytes being removed from the rolling window.
        outgoing: usize,
        /// Number of bytes being appended to the rolling window.
        incoming: usize,
    },
}

/// Error returned when reconstructing a rolling checksum digest from a byte slice of the wrong length.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
#[error("rolling checksum digest requires {expected} bytes, received {len}", expected = RollingSliceError::EXPECTED_LEN)]
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
    /// use checksums::{RollingDigest, RollingSliceError};
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

#[cfg(test)]
mod tests {
    use super::{RollingError, RollingSliceError};

    #[test]
    fn rolling_error_display_messages_are_descriptive() {
        let empty = RollingError::EmptyWindow.to_string();
        assert!(empty.contains("non-empty"));

        let too_large = RollingError::WindowTooLarge { len: 1 << 20 }.to_string();
        assert!(too_large.contains("exceeds"));
        assert!(too_large.contains("1048576"));

        let mismatched = RollingError::MismatchedSliceLength {
            outgoing: 2,
            incoming: 1,
        }
        .to_string();
        assert!(mismatched.contains("outgoing (2)"));
        assert!(mismatched.contains("incoming (1)"));
    }

    #[test]
    fn rolling_slice_error_reports_length_information() {
        let err = RollingSliceError::new(2);
        assert_eq!(err.len(), 2);
        assert!(!err.is_empty());
        assert_eq!(RollingSliceError::EXPECTED_LEN, 4);
        assert!(err.to_string().contains("received 2"));

        let empty_err = RollingSliceError::new(0);
        assert!(empty_err.is_empty());
    }
}
