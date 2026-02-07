//! Timeout configuration and tracking for rsync connections and I/O operations.
//!
//! This module implements upstream rsync's `--timeout` and `--contimeout` behavior,
//! providing timeout handling for both connection establishment and I/O operations.
//!
//! # Upstream Reference
//!
//! - `--timeout=SECONDS` - Timeout for I/O operations (0 = no timeout)
//! - `--contimeout=SECONDS` - Timeout for connection establishment (0 = no timeout)
//! - Timeouts are reset on every successful read/write operation
//!
//! # Examples
//!
//! ```
//! use core::timeout::{TimeoutConfig, TimeoutTracker};
//! use std::time::Duration;
//!
//! // Configure timeouts
//! let config = TimeoutConfig::new()
//!     .with_io_timeout(30)
//!     .with_connect_timeout(10);
//!
//! // Track timeouts during operations
//! let mut tracker = TimeoutTracker::new(config);
//!
//! // Start connection phase
//! tracker.start_connect();
//! // ... establish connection ...
//! tracker.check_connect_timeout().expect("connection timeout");
//!
//! // Reset I/O timer on each operation
//! tracker.reset_io_timer();
//! // ... perform I/O ...
//! tracker.check_io_timeout().expect("I/O timeout");
//! ```

use crate::exit_code::{ExitCode, HasExitCode};
use std::fmt;
use std::time::{Duration, Instant};

/// Configuration for connection and I/O timeouts.
///
/// This struct holds the timeout settings for both connection establishment
/// and I/O operations. A value of `None` means no timeout is set.
///
/// # Upstream Behavior
///
/// - `--timeout=0` means no timeout (represented as `None`)
/// - `--timeout=N` means N seconds of I/O inactivity triggers timeout
/// - `--contimeout=0` means no connection timeout
/// - `--contimeout=N` means N seconds to establish connection
///
/// # Examples
///
/// ```
/// use core::timeout::TimeoutConfig;
///
/// // Default: no timeouts
/// let config = TimeoutConfig::default();
/// assert!(!config.is_io_timeout_enabled());
/// assert!(!config.is_connect_timeout_enabled());
///
/// // Set timeouts via builder pattern
/// let config = TimeoutConfig::new()
///     .with_io_timeout(30)
///     .with_connect_timeout(10);
/// assert!(config.is_io_timeout_enabled());
/// assert!(config.is_connect_timeout_enabled());
///
/// // Parse from CLI options
/// let config = TimeoutConfig::from_options(30, 10);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeoutConfig {
    /// I/O timeout duration (--timeout). `None` means no timeout.
    io_timeout: Option<Duration>,
    /// Connection timeout duration (--contimeout). `None` means no timeout.
    connect_timeout: Option<Duration>,
}

impl TimeoutConfig {
    /// Creates a new timeout configuration with no timeouts enabled.
    ///
    /// # Examples
    ///
    /// ```
    /// use core::timeout::TimeoutConfig;
    ///
    /// let config = TimeoutConfig::new();
    /// assert!(!config.is_io_timeout_enabled());
    /// assert!(!config.is_connect_timeout_enabled());
    /// ```
    #[must_use]
    pub const fn new() -> Self {
        Self {
            io_timeout: None,
            connect_timeout: None,
        }
    }

    /// Sets the I/O timeout in seconds.
    ///
    /// A value of 0 means no timeout (disables timeout).
    /// The timer is reset on every successful read/write operation.
    ///
    /// # Examples
    ///
    /// ```
    /// use core::timeout::TimeoutConfig;
    /// use std::time::Duration;
    ///
    /// let config = TimeoutConfig::new().with_io_timeout(30);
    /// assert_eq!(config.io_timeout(), Some(Duration::from_secs(30)));
    ///
    /// let config = TimeoutConfig::new().with_io_timeout(0);
    /// assert_eq!(config.io_timeout(), None);
    /// ```
    #[must_use]
    pub const fn with_io_timeout(mut self, seconds: u32) -> Self {
        self.io_timeout = if seconds == 0 {
            None
        } else {
            Some(Duration::from_secs(seconds as u64))
        };
        self
    }

    /// Sets the connection timeout in seconds.
    ///
    /// A value of 0 means no timeout (disables timeout).
    ///
    /// # Examples
    ///
    /// ```
    /// use core::timeout::TimeoutConfig;
    /// use std::time::Duration;
    ///
    /// let config = TimeoutConfig::new().with_connect_timeout(10);
    /// assert_eq!(config.connect_timeout(), Some(Duration::from_secs(10)));
    ///
    /// let config = TimeoutConfig::new().with_connect_timeout(0);
    /// assert_eq!(config.connect_timeout(), None);
    /// ```
    #[must_use]
    pub const fn with_connect_timeout(mut self, seconds: u32) -> Self {
        self.connect_timeout = if seconds == 0 {
            None
        } else {
            Some(Duration::from_secs(seconds as u64))
        };
        self
    }

    /// Creates a timeout configuration from CLI option values.
    ///
    /// A value of 0 for either parameter means no timeout.
    ///
    /// # Examples
    ///
    /// ```
    /// use core::timeout::TimeoutConfig;
    /// use std::time::Duration;
    ///
    /// let config = TimeoutConfig::from_options(30, 10);
    /// assert_eq!(config.io_timeout(), Some(Duration::from_secs(30)));
    /// assert_eq!(config.connect_timeout(), Some(Duration::from_secs(10)));
    ///
    /// let config = TimeoutConfig::from_options(0, 0);
    /// assert_eq!(config.io_timeout(), None);
    /// assert_eq!(config.connect_timeout(), None);
    /// ```
    #[must_use]
    pub const fn from_options(timeout: u32, contimeout: u32) -> Self {
        Self::new()
            .with_io_timeout(timeout)
            .with_connect_timeout(contimeout)
    }

    /// Returns the configured I/O timeout.
    ///
    /// # Examples
    ///
    /// ```
    /// use core::timeout::TimeoutConfig;
    /// use std::time::Duration;
    ///
    /// let config = TimeoutConfig::new().with_io_timeout(30);
    /// assert_eq!(config.io_timeout(), Some(Duration::from_secs(30)));
    /// ```
    #[must_use]
    pub const fn io_timeout(&self) -> Option<Duration> {
        self.io_timeout
    }

    /// Returns the configured connection timeout.
    ///
    /// # Examples
    ///
    /// ```
    /// use core::timeout::TimeoutConfig;
    /// use std::time::Duration;
    ///
    /// let config = TimeoutConfig::new().with_connect_timeout(10);
    /// assert_eq!(config.connect_timeout(), Some(Duration::from_secs(10)));
    /// ```
    #[must_use]
    pub const fn connect_timeout(&self) -> Option<Duration> {
        self.connect_timeout
    }

    /// Returns `true` if I/O timeout is enabled.
    ///
    /// # Examples
    ///
    /// ```
    /// use core::timeout::TimeoutConfig;
    ///
    /// let config = TimeoutConfig::new().with_io_timeout(30);
    /// assert!(config.is_io_timeout_enabled());
    ///
    /// let config = TimeoutConfig::new().with_io_timeout(0);
    /// assert!(!config.is_io_timeout_enabled());
    /// ```
    #[must_use]
    pub const fn is_io_timeout_enabled(&self) -> bool {
        self.io_timeout.is_some()
    }

    /// Returns `true` if connection timeout is enabled.
    ///
    /// # Examples
    ///
    /// ```
    /// use core::timeout::TimeoutConfig;
    ///
    /// let config = TimeoutConfig::new().with_connect_timeout(10);
    /// assert!(config.is_connect_timeout_enabled());
    ///
    /// let config = TimeoutConfig::new().with_connect_timeout(0);
    /// assert!(!config.is_connect_timeout_enabled());
    /// ```
    #[must_use]
    pub const fn is_connect_timeout_enabled(&self) -> bool {
        self.connect_timeout.is_some()
    }
}

impl Default for TimeoutConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// Runtime timeout tracking for connection and I/O operations.
///
/// This struct tracks elapsed time for both connection establishment and
/// I/O operations, allowing checks against configured timeout limits.
///
/// # Upstream Behavior
///
/// - Connection timer starts when `start_connect()` is called
/// - I/O timer is reset on every successful read/write via `reset_io_timer()`
/// - Timeouts are checked via `check_io_timeout()` and `check_connect_timeout()`
///
/// # Examples
///
/// ```
/// use core::timeout::{TimeoutConfig, TimeoutTracker};
/// use std::time::Duration;
///
/// let config = TimeoutConfig::new().with_io_timeout(30);
/// let mut tracker = TimeoutTracker::new(config);
///
/// // Reset timer before I/O
/// tracker.reset_io_timer();
///
/// // Check if timeout exceeded
/// assert!(tracker.check_io_timeout().is_ok());
///
/// // Time since last I/O
/// let elapsed = tracker.time_since_last_io();
/// assert!(elapsed < Duration::from_secs(1));
/// ```
#[derive(Debug)]
pub struct TimeoutTracker {
    /// Configuration for timeout limits
    config: TimeoutConfig,
    /// Last time I/O timer was reset
    last_io_activity: Instant,
    /// When connection phase started
    connect_start: Option<Instant>,
}

impl TimeoutTracker {
    /// Creates a new timeout tracker with the given configuration.
    ///
    /// The I/O timer is initialized to the current time.
    ///
    /// # Examples
    ///
    /// ```
    /// use core::timeout::{TimeoutConfig, TimeoutTracker};
    ///
    /// let config = TimeoutConfig::new().with_io_timeout(30);
    /// let tracker = TimeoutTracker::new(config);
    /// assert!(tracker.is_io_timeout_enabled());
    /// ```
    #[must_use]
    pub fn new(config: TimeoutConfig) -> Self {
        Self {
            config,
            last_io_activity: Instant::now(),
            connect_start: None,
        }
    }

    /// Resets the I/O timeout timer to the current time.
    ///
    /// This should be called after every successful read or write operation.
    ///
    /// # Examples
    ///
    /// ```
    /// use core::timeout::{TimeoutConfig, TimeoutTracker};
    /// use std::time::Duration;
    ///
    /// let config = TimeoutConfig::new().with_io_timeout(30);
    /// let mut tracker = TimeoutTracker::new(config);
    ///
    /// std::thread::sleep(Duration::from_millis(10));
    /// tracker.reset_io_timer();
    /// assert!(tracker.time_since_last_io() < Duration::from_millis(5));
    /// ```
    pub fn reset_io_timer(&mut self) {
        self.last_io_activity = Instant::now();
    }

    /// Checks if the I/O timeout has been exceeded.
    ///
    /// Returns `Ok(())` if within timeout limit or if timeout is disabled.
    /// Returns `Err(TimeoutError::IoTimeout)` if timeout exceeded.
    ///
    /// # Examples
    ///
    /// ```
    /// use core::timeout::{TimeoutConfig, TimeoutTracker};
    ///
    /// let config = TimeoutConfig::new().with_io_timeout(30);
    /// let tracker = TimeoutTracker::new(config);
    ///
    /// // Should be OK immediately after creation
    /// assert!(tracker.check_io_timeout().is_ok());
    /// ```
    pub fn check_io_timeout(&self) -> Result<(), TimeoutError> {
        if let Some(timeout) = self.config.io_timeout {
            let elapsed = self.last_io_activity.elapsed();
            if elapsed >= timeout {
                return Err(TimeoutError::IoTimeout {
                    elapsed,
                    limit: timeout,
                });
            }
        }
        Ok(())
    }

    /// Starts the connection timeout timer.
    ///
    /// This should be called at the beginning of connection establishment.
    ///
    /// # Examples
    ///
    /// ```
    /// use core::timeout::{TimeoutConfig, TimeoutTracker};
    ///
    /// let config = TimeoutConfig::new().with_connect_timeout(10);
    /// let mut tracker = TimeoutTracker::new(config);
    ///
    /// tracker.start_connect();
    /// assert!(tracker.check_connect_timeout().is_ok());
    /// ```
    pub fn start_connect(&mut self) {
        self.connect_start = Some(Instant::now());
    }

    /// Checks if the connection timeout has been exceeded.
    ///
    /// Returns `Ok(())` if within timeout limit or if timeout is disabled.
    /// Returns `Err(TimeoutError::ConnectTimeout)` if timeout exceeded.
    ///
    /// # Examples
    ///
    /// ```
    /// use core::timeout::{TimeoutConfig, TimeoutTracker};
    ///
    /// let config = TimeoutConfig::new().with_connect_timeout(10);
    /// let mut tracker = TimeoutTracker::new(config);
    ///
    /// tracker.start_connect();
    /// assert!(tracker.check_connect_timeout().is_ok());
    /// ```
    pub fn check_connect_timeout(&self) -> Result<(), TimeoutError> {
        if let Some(timeout) = self.config.connect_timeout {
            if let Some(start) = self.connect_start {
                let elapsed = start.elapsed();
                if elapsed >= timeout {
                    return Err(TimeoutError::ConnectTimeout {
                        elapsed,
                        limit: timeout,
                    });
                }
            }
        }
        Ok(())
    }

    /// Returns the time elapsed since the last I/O activity.
    ///
    /// # Examples
    ///
    /// ```
    /// use core::timeout::{TimeoutConfig, TimeoutTracker};
    /// use std::time::Duration;
    ///
    /// let config = TimeoutConfig::new();
    /// let tracker = TimeoutTracker::new(config);
    ///
    /// let elapsed = tracker.time_since_last_io();
    /// assert!(elapsed < Duration::from_secs(1));
    /// ```
    #[must_use]
    pub fn time_since_last_io(&self) -> Duration {
        self.last_io_activity.elapsed()
    }

    /// Returns `true` if I/O timeout is enabled.
    ///
    /// # Examples
    ///
    /// ```
    /// use core::timeout::{TimeoutConfig, TimeoutTracker};
    ///
    /// let config = TimeoutConfig::new().with_io_timeout(30);
    /// let tracker = TimeoutTracker::new(config);
    /// assert!(tracker.is_io_timeout_enabled());
    /// ```
    #[must_use]
    pub const fn is_io_timeout_enabled(&self) -> bool {
        self.config.is_io_timeout_enabled()
    }

    /// Returns `true` if connection timeout is enabled.
    ///
    /// # Examples
    ///
    /// ```
    /// use core::timeout::{TimeoutConfig, TimeoutTracker};
    ///
    /// let config = TimeoutConfig::new().with_connect_timeout(10);
    /// let tracker = TimeoutTracker::new(config);
    /// assert!(tracker.is_connect_timeout_enabled());
    /// ```
    #[must_use]
    pub const fn is_connect_timeout_enabled(&self) -> bool {
        self.config.is_connect_timeout_enabled()
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_no_timeouts() {
        let config = TimeoutConfig::default();
        assert_eq!(config.io_timeout(), None);
        assert_eq!(config.connect_timeout(), None);
        assert!(!config.is_io_timeout_enabled());
        assert!(!config.is_connect_timeout_enabled());
    }

    #[test]
    fn new_config_has_no_timeouts() {
        let config = TimeoutConfig::new();
        assert_eq!(config.io_timeout(), None);
        assert_eq!(config.connect_timeout(), None);
        assert!(!config.is_io_timeout_enabled());
        assert!(!config.is_connect_timeout_enabled());
    }

    #[test]
    fn builder_sets_io_timeout_correctly() {
        let config = TimeoutConfig::new().with_io_timeout(30);
        assert_eq!(config.io_timeout(), Some(Duration::from_secs(30)));
        assert!(config.is_io_timeout_enabled());
        assert!(!config.is_connect_timeout_enabled());
    }

    #[test]
    fn builder_sets_connect_timeout_correctly() {
        let config = TimeoutConfig::new().with_connect_timeout(10);
        assert_eq!(config.connect_timeout(), Some(Duration::from_secs(10)));
        assert!(!config.is_io_timeout_enabled());
        assert!(config.is_connect_timeout_enabled());
    }

    #[test]
    fn builder_sets_both_timeouts() {
        let config = TimeoutConfig::new()
            .with_io_timeout(30)
            .with_connect_timeout(10);
        assert_eq!(config.io_timeout(), Some(Duration::from_secs(30)));
        assert_eq!(config.connect_timeout(), Some(Duration::from_secs(10)));
        assert!(config.is_io_timeout_enabled());
        assert!(config.is_connect_timeout_enabled());
    }

    #[test]
    fn zero_io_timeout_means_disabled() {
        let config = TimeoutConfig::new().with_io_timeout(0);
        assert_eq!(config.io_timeout(), None);
        assert!(!config.is_io_timeout_enabled());
    }

    #[test]
    fn zero_connect_timeout_means_disabled() {
        let config = TimeoutConfig::new().with_connect_timeout(0);
        assert_eq!(config.connect_timeout(), None);
        assert!(!config.is_connect_timeout_enabled());
    }

    #[test]
    fn from_options_parsing() {
        let config = TimeoutConfig::from_options(30, 10);
        assert_eq!(config.io_timeout(), Some(Duration::from_secs(30)));
        assert_eq!(config.connect_timeout(), Some(Duration::from_secs(10)));

        let config = TimeoutConfig::from_options(0, 0);
        assert_eq!(config.io_timeout(), None);
        assert_eq!(config.connect_timeout(), None);

        let config = TimeoutConfig::from_options(60, 0);
        assert_eq!(config.io_timeout(), Some(Duration::from_secs(60)));
        assert_eq!(config.connect_timeout(), None);
    }

    #[test]
    fn timeout_tracker_creation() {
        let config = TimeoutConfig::new().with_io_timeout(30);
        let tracker = TimeoutTracker::new(config);
        assert!(tracker.is_io_timeout_enabled());
        assert!(!tracker.is_connect_timeout_enabled());
    }

    #[test]
    fn reset_io_timer_works() {
        let config = TimeoutConfig::new().with_io_timeout(30);
        let mut tracker = TimeoutTracker::new(config);

        std::thread::sleep(Duration::from_millis(10));
        assert!(tracker.time_since_last_io() >= Duration::from_millis(10));

        tracker.reset_io_timer();
        assert!(tracker.time_since_last_io() < Duration::from_millis(5));
    }

    #[test]
    fn check_io_timeout_with_no_timeout_enabled() {
        let config = TimeoutConfig::new();
        let tracker = TimeoutTracker::new(config);
        assert!(tracker.check_io_timeout().is_ok());

        std::thread::sleep(Duration::from_millis(10));
        assert!(tracker.check_io_timeout().is_ok());
    }

    #[test]
    fn check_io_timeout_within_limit_is_ok() {
        let config = TimeoutConfig::new().with_io_timeout(1);
        let tracker = TimeoutTracker::new(config);
        assert!(tracker.check_io_timeout().is_ok());
    }

    #[test]
    fn check_io_timeout_exceeded_returns_error() {
        let config = TimeoutConfig::new().with_io_timeout(0);
        let config = TimeoutConfig {
            io_timeout: Some(Duration::from_millis(1)),
            connect_timeout: None,
        };
        let tracker = TimeoutTracker::new(config);

        std::thread::sleep(Duration::from_millis(10));
        let result = tracker.check_io_timeout();
        assert!(result.is_err());

        if let Err(TimeoutError::IoTimeout { elapsed, limit }) = result {
            assert!(elapsed >= limit);
        } else {
            panic!("Expected IoTimeout error");
        }
    }

    #[test]
    fn check_connect_timeout_with_no_timeout_enabled() {
        let config = TimeoutConfig::new();
        let mut tracker = TimeoutTracker::new(config);
        tracker.start_connect();
        assert!(tracker.check_connect_timeout().is_ok());

        std::thread::sleep(Duration::from_millis(10));
        assert!(tracker.check_connect_timeout().is_ok());
    }

    #[test]
    fn check_connect_timeout_within_limit_is_ok() {
        let config = TimeoutConfig::new().with_connect_timeout(1);
        let mut tracker = TimeoutTracker::new(config);
        tracker.start_connect();
        assert!(tracker.check_connect_timeout().is_ok());
    }

    #[test]
    fn check_connect_timeout_exceeded_returns_error() {
        let config = TimeoutConfig {
            io_timeout: None,
            connect_timeout: Some(Duration::from_millis(1)),
        };
        let mut tracker = TimeoutTracker::new(config);
        tracker.start_connect();

        std::thread::sleep(Duration::from_millis(10));
        let result = tracker.check_connect_timeout();
        assert!(result.is_err());

        if let Err(TimeoutError::ConnectTimeout { elapsed, limit }) = result {
            assert!(elapsed >= limit);
        } else {
            panic!("Expected ConnectTimeout error");
        }
    }

    #[test]
    fn timeout_error_display_format() {
        let io_error = TimeoutError::IoTimeout {
            elapsed: Duration::from_secs(35),
            limit: Duration::from_secs(30),
        };
        let display = format!("{io_error}");
        assert!(display.contains("timeout in data send/receive"));
        assert!(display.contains("35.0s"));
        assert!(display.contains("30.0s"));

        let connect_error = TimeoutError::ConnectTimeout {
            elapsed: Duration::from_secs(15),
            limit: Duration::from_secs(10),
        };
        let display = format!("{connect_error}");
        assert!(display.contains("timeout waiting for daemon connection"));
        assert!(display.contains("15.0s"));
        assert!(display.contains("10.0s"));
    }

    #[test]
    fn io_timeout_exit_code_is_30() {
        let error = TimeoutError::IoTimeout {
            elapsed: Duration::from_secs(35),
            limit: Duration::from_secs(30),
        };
        assert_eq!(error.exit_code(), ExitCode::Timeout);
        assert_eq!(error.exit_code().as_i32(), 30);
        assert_eq!(error.exit_code_value(), 30);
    }

    #[test]
    fn connect_timeout_exit_code_is_35() {
        let error = TimeoutError::ConnectTimeout {
            elapsed: Duration::from_secs(15),
            limit: Duration::from_secs(10),
        };
        assert_eq!(error.exit_code(), ExitCode::ConnectionTimeout);
        assert_eq!(error.exit_code().as_i32(), 35);
        assert_eq!(error.exit_code_value(), 35);
    }

    #[test]
    fn time_since_last_io_tracking() {
        let config = TimeoutConfig::new();
        let tracker = TimeoutTracker::new(config);
        let elapsed = tracker.time_since_last_io();
        assert!(elapsed < Duration::from_secs(1));
    }

    #[test]
    fn edge_case_very_short_timeout() {
        let config = TimeoutConfig::new().with_io_timeout(1);
        let tracker = TimeoutTracker::new(config);
        assert_eq!(config.io_timeout(), Some(Duration::from_secs(1)));
        assert!(tracker.check_io_timeout().is_ok());
    }

    #[test]
    fn edge_case_very_long_timeout() {
        let config = TimeoutConfig::new().with_io_timeout(86400); // 24 hours
        let tracker = TimeoutTracker::new(config);
        assert_eq!(config.io_timeout(), Some(Duration::from_secs(86400)));
        assert!(tracker.check_io_timeout().is_ok());
    }

    #[test]
    fn timeout_error_implements_std_error() {
        let error = TimeoutError::IoTimeout {
            elapsed: Duration::from_secs(35),
            limit: Duration::from_secs(30),
        };
        let _: &dyn std::error::Error = &error;
    }

    #[test]
    fn timeout_error_is_clone() {
        let error = TimeoutError::IoTimeout {
            elapsed: Duration::from_secs(35),
            limit: Duration::from_secs(30),
        };
        let cloned = error.clone();
        assert_eq!(error, cloned);
    }

    #[test]
    fn timeout_error_is_debug() {
        let error = TimeoutError::IoTimeout {
            elapsed: Duration::from_secs(35),
            limit: Duration::from_secs(30),
        };
        let debug = format!("{error:?}");
        assert!(debug.contains("IoTimeout"));
    }

    #[test]
    fn config_is_copy() {
        let config = TimeoutConfig::new().with_io_timeout(30);
        let copy = config;
        assert_eq!(config, copy);
    }

    #[test]
    fn connect_timeout_not_started_checks_ok() {
        let config = TimeoutConfig::new().with_connect_timeout(10);
        let tracker = TimeoutTracker::new(config);
        // Without calling start_connect(), check should always succeed
        assert!(tracker.check_connect_timeout().is_ok());
    }

    #[test]
    fn multiple_reset_calls_work() {
        let config = TimeoutConfig::new().with_io_timeout(30);
        let mut tracker = TimeoutTracker::new(config);

        for _ in 0..5 {
            tracker.reset_io_timer();
            assert!(tracker.time_since_last_io() < Duration::from_millis(5));
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}
