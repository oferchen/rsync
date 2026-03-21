//! Timeout configuration for rsync connections and I/O operations.
//!
//! Implements upstream rsync's `--timeout` and `--contimeout` settings
//! as a builder-pattern configuration struct.

use std::time::Duration;

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
    pub(crate) io_timeout: Option<Duration>,
    /// Connection timeout duration (--contimeout). `None` means no timeout.
    pub(crate) connect_timeout: Option<Duration>,
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
