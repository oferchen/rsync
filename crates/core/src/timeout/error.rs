//! Timeout error types with upstream-compatible exit codes.
//!
//! - `IoTimeout` maps to exit code 30 (RERR_TIMEOUT)
//! - `ConnectTimeout` maps to exit code 35 (RERR_CONTIMEOUT)

use crate::exit_code::{ExitCode, HasExitCode};
use std::fmt;
use std::time::Duration;

/// Timeout error types.
///
/// Represents timeout errors that can occur during connection or I/O operations.
/// Each variant includes the elapsed time and the configured limit for diagnostics.
///
/// # Exit Codes
///
/// - `IoTimeout` returns exit code 30 (RERR_TIMEOUT)
/// - `ConnectTimeout` returns exit code 35 (RERR_CONTIMEOUT)
///
/// # Examples
///
/// ```
/// use core::timeout::TimeoutError;
/// use core::exit_code::{ExitCode, HasExitCode};
/// use std::time::Duration;
///
/// let error = TimeoutError::IoTimeout {
///     elapsed: Duration::from_secs(35),
///     limit: Duration::from_secs(30),
/// };
///
/// assert_eq!(error.exit_code(), ExitCode::Timeout);
/// assert_eq!(error.exit_code().as_i32(), 30);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimeoutError {
    /// I/O timeout exceeded.
    ///
    /// Returned when I/O inactivity exceeds the configured `--timeout` value.
    IoTimeout {
        /// Time elapsed since last I/O activity
        elapsed: Duration,
        /// Configured timeout limit
        limit: Duration,
    },

    /// Connection timeout exceeded.
    ///
    /// Returned when connection establishment exceeds the configured `--contimeout` value.
    ConnectTimeout {
        /// Time elapsed since connection started
        elapsed: Duration,
        /// Configured connection timeout limit
        limit: Duration,
    },
}

impl TimeoutError {
    /// Returns the exit code for this timeout error.
    ///
    /// - `IoTimeout` returns 30 (RERR_TIMEOUT)
    /// - `ConnectTimeout` returns 35 (RERR_CONTIMEOUT)
    ///
    /// # Examples
    ///
    /// ```
    /// use core::timeout::TimeoutError;
    /// use std::time::Duration;
    ///
    /// let io_error = TimeoutError::IoTimeout {
    ///     elapsed: Duration::from_secs(35),
    ///     limit: Duration::from_secs(30),
    /// };
    /// assert_eq!(io_error.exit_code_value(), 30);
    ///
    /// let connect_error = TimeoutError::ConnectTimeout {
    ///     elapsed: Duration::from_secs(15),
    ///     limit: Duration::from_secs(10),
    /// };
    /// assert_eq!(connect_error.exit_code_value(), 35);
    /// ```
    #[must_use]
    pub const fn exit_code_value(&self) -> i32 {
        match self {
            Self::IoTimeout { .. } => 30,
            Self::ConnectTimeout { .. } => 35,
        }
    }
}

impl fmt::Display for TimeoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IoTimeout { elapsed, limit } => {
                write!(
                    f,
                    "timeout in data send/receive (elapsed: {:.1}s, limit: {:.1}s)",
                    elapsed.as_secs_f64(),
                    limit.as_secs_f64()
                )
            }
            Self::ConnectTimeout { elapsed, limit } => {
                write!(
                    f,
                    "timeout waiting for daemon connection (elapsed: {:.1}s, limit: {:.1}s)",
                    elapsed.as_secs_f64(),
                    limit.as_secs_f64()
                )
            }
        }
    }
}

impl std::error::Error for TimeoutError {}

impl HasExitCode for TimeoutError {
    fn exit_code(&self) -> ExitCode {
        match self {
            Self::IoTimeout { .. } => ExitCode::Timeout,
            Self::ConnectTimeout { .. } => ExitCode::ConnectionTimeout,
        }
    }
}
