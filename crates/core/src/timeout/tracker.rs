//! Runtime timeout tracking for connection and I/O operations.
//!
//! Tracks elapsed time against configured limits, with reset capability
//! for the I/O timer on each successful read/write.

use super::config::TimeoutConfig;
use super::error::TimeoutError;
use std::time::{Duration, Instant};

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
        if let Some(timeout) = self.config.io_timeout() {
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
        if let Some(timeout) = self.config.connect_timeout() {
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
