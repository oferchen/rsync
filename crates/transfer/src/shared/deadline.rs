//! Deadline enforcement for `--stop-at` / `--stop-after` / `--time-limit`.
//!
//! Converts the wall-clock `SystemTime` deadline from [`ServerConfig::stop_at`]
//! into a monotonic `Instant` that can be cheaply compared without syscalls.
//! The check is performed at file boundaries (between files, never mid-file)
//! to match upstream rsync's graceful stop behavior.
//!
//! # Upstream Reference
//!
//! - `main.c`: `stop_at_utime` global checked in transfer loop
//! - `io.c`: deadline checked during I/O operations

use std::io;
use std::time::{Instant, SystemTime};

/// Monotonic deadline derived from a wall-clock `SystemTime`.
///
/// Created once at transfer start and checked cheaply at each file boundary.
/// Using `Instant` avoids repeated `SystemTime::now()` calls which may involve
/// syscalls on some platforms.
#[derive(Debug, Clone, Copy)]
pub struct TransferDeadline {
    /// Monotonic instant at which the transfer should stop.
    instant: Instant,
}

impl TransferDeadline {
    /// Creates a deadline from an optional wall-clock `SystemTime`.
    ///
    /// Returns `None` if no deadline is configured. If the deadline is already
    /// in the past, returns a deadline that triggers immediately.
    pub fn from_system_time(stop_at: Option<SystemTime>) -> Option<Self> {
        stop_at.map(|deadline| {
            let now = SystemTime::now();
            let instant = match deadline.duration_since(now) {
                Ok(remaining) => Instant::now() + remaining,
                Err(_) => Instant::now(), // Already past
            };
            Self { instant }
        })
    }

    /// Returns `true` if the deadline has been reached.
    pub fn is_reached(&self) -> bool {
        Instant::now() >= self.instant
    }

    /// Returns an I/O error indicating the deadline was reached.
    ///
    /// The error uses `TimedOut` kind with a descriptive message matching
    /// upstream rsync's `--stop-at` behavior. Exit code 30 (RERR_TIMEOUT)
    /// is mapped by the caller.
    pub fn as_io_error() -> io::Error {
        io::Error::new(io::ErrorKind::TimedOut, "stopping at requested limit")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn from_system_time_none_returns_none() {
        assert!(TransferDeadline::from_system_time(None).is_none());
    }

    #[test]
    fn from_system_time_future_returns_some() {
        let future = SystemTime::now() + Duration::from_secs(3600);
        let deadline = TransferDeadline::from_system_time(Some(future));
        assert!(deadline.is_some());
        assert!(!deadline.unwrap().is_reached());
    }

    #[test]
    fn from_system_time_past_returns_reached() {
        let past = SystemTime::now() - Duration::from_secs(1);
        let deadline = TransferDeadline::from_system_time(Some(past));
        assert!(deadline.is_some());
        assert!(deadline.unwrap().is_reached());
    }

    #[test]
    fn as_io_error_is_timed_out() {
        let err = TransferDeadline::as_io_error();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        assert!(err.to_string().contains("stopping at requested limit"));
    }

    #[test]
    fn deadline_debug_format() {
        let future = SystemTime::now() + Duration::from_secs(60);
        let deadline = TransferDeadline::from_system_time(Some(future)).unwrap();
        let debug = format!("{deadline:?}");
        assert!(debug.contains("TransferDeadline"));
    }

    #[test]
    fn deadline_clone() {
        let future = SystemTime::now() + Duration::from_secs(60);
        let deadline = TransferDeadline::from_system_time(Some(future)).unwrap();
        let cloned = deadline;
        assert!(!cloned.is_reached());
    }
}
