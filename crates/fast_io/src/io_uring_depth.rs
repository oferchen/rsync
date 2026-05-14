//! Validation helpers for the io_uring submission queue depth tunable.
//!
//! Mirrors the kernel's `io_uring_setup(2)` requirements: the depth must be
//! a power of two within the supported `[IO_URING_DEPTH_MIN, IO_URING_DEPTH_MAX]`
//! range.

/// Minimum value accepted for the io_uring submission queue depth tunable.
///
/// The kernel rejects rings with zero entries, so callers must request at
/// least one SQE slot.
pub const IO_URING_DEPTH_MIN: u32 = 1;

/// Maximum value accepted for the io_uring submission queue depth tunable.
///
/// The kernel caps SQ entries at 32768 (2^15) for non-privileged callers, so
/// we surface that as the upper bound for the CLI tunable. Larger values are
/// rejected at parse time rather than at ring construction time.
pub const IO_URING_DEPTH_MAX: u32 = 32768;

/// Errors returned by [`validate_io_uring_depth`] when the requested submission
/// queue depth is outside the supported range or not a power of two.
///
/// Mirrors the kernel's `io_uring_setup(2)` requirements: the depth must be
/// in `[IO_URING_DEPTH_MIN, IO_URING_DEPTH_MAX]` and a power of two.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum IoUringDepthError {
    /// The supplied depth was zero. The kernel requires at least one SQE.
    #[error("--io-uring-depth must be at least {IO_URING_DEPTH_MIN}")]
    Zero,
    /// The supplied depth was not a power of two. `io_uring_setup(2)` rounds
    /// up internally, but rejecting non-powers here keeps behaviour explicit.
    #[error("--io-uring-depth must be a power of two (got {0})")]
    NotPowerOfTwo(u32),
    /// The supplied depth exceeded [`IO_URING_DEPTH_MAX`].
    #[error("--io-uring-depth must be at most {IO_URING_DEPTH_MAX} (got {0})")]
    TooLarge(u32),
}

/// Validates a user-supplied io_uring submission queue depth.
///
/// Accepts powers of two in the inclusive range
/// `[IO_URING_DEPTH_MIN, IO_URING_DEPTH_MAX]`. Returns the validated value on
/// success or an [`IoUringDepthError`] describing why the input is invalid.
///
/// # Examples
///
/// ```
/// use fast_io::{IO_URING_DEPTH_MAX, IoUringDepthError, validate_io_uring_depth};
///
/// assert_eq!(validate_io_uring_depth(256), Ok(256));
/// assert_eq!(validate_io_uring_depth(0), Err(IoUringDepthError::Zero));
/// assert_eq!(
///     validate_io_uring_depth(100),
///     Err(IoUringDepthError::NotPowerOfTwo(100)),
/// );
/// assert_eq!(
///     validate_io_uring_depth(IO_URING_DEPTH_MAX * 2),
///     Err(IoUringDepthError::TooLarge(IO_URING_DEPTH_MAX * 2)),
/// );
/// ```
pub fn validate_io_uring_depth(depth: u32) -> Result<u32, IoUringDepthError> {
    if depth == 0 {
        return Err(IoUringDepthError::Zero);
    }
    if depth > IO_URING_DEPTH_MAX {
        return Err(IoUringDepthError::TooLarge(depth));
    }
    if !depth.is_power_of_two() {
        return Err(IoUringDepthError::NotPowerOfTwo(depth));
    }
    Ok(depth)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_io_uring_depth_accepts_default() {
        assert_eq!(validate_io_uring_depth(64), Ok(64));
    }

    #[test]
    fn validate_io_uring_depth_accepts_power_of_two() {
        for &depth in &[1u32, 2, 4, 8, 16, 32, 256, 1024, 4096, IO_URING_DEPTH_MAX] {
            assert_eq!(validate_io_uring_depth(depth), Ok(depth));
        }
    }

    #[test]
    fn validate_io_uring_depth_rejects_zero() {
        assert_eq!(validate_io_uring_depth(0), Err(IoUringDepthError::Zero));
    }

    #[test]
    fn validate_io_uring_depth_rejects_non_power_of_two() {
        assert_eq!(
            validate_io_uring_depth(100),
            Err(IoUringDepthError::NotPowerOfTwo(100)),
        );
        assert_eq!(
            validate_io_uring_depth(3),
            Err(IoUringDepthError::NotPowerOfTwo(3)),
        );
    }

    #[test]
    fn validate_io_uring_depth_rejects_too_large() {
        let too_large = IO_URING_DEPTH_MAX * 2;
        assert_eq!(
            validate_io_uring_depth(too_large),
            Err(IoUringDepthError::TooLarge(too_large)),
        );
    }

    #[test]
    fn io_uring_depth_error_messages_mention_flag() {
        assert!(
            IoUringDepthError::Zero
                .to_string()
                .contains("--io-uring-depth")
        );
        assert!(
            IoUringDepthError::NotPowerOfTwo(7)
                .to_string()
                .contains("--io-uring-depth")
        );
        assert!(
            IoUringDepthError::TooLarge(IO_URING_DEPTH_MAX + 1)
                .to_string()
                .contains("--io-uring-depth")
        );
    }
}
