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

mod config;
mod error;
mod tracker;

pub use config::TimeoutConfig;
pub use error::TimeoutError;
pub use tracker::TimeoutTracker;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exit_code::{ExitCode, HasExitCode};
    use std::time::Duration;

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
