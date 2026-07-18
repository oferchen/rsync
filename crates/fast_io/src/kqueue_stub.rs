//! Portable `kqueue` fallback for non-macOS platforms.
//!
//! Provides the same public surface as the real
//! [`crate::kqueue`](super::kqueue) module so cross-platform callers
//! compile without `#[cfg]` branching. Every constructor returns an
//! `Unsupported` error because `kqueue(2)` is a BSD/macOS primitive
//! with no equivalent on Linux or Windows.
//!
//! Real consumer migrations will check
//! [`is_kqueue_available`] at runtime and fall back to the existing
//! buffered / io_uring / IOCP path when the loop cannot be constructed.

#![cfg(not(target_os = "macos"))]
#![allow(dead_code)]

use std::io;
#[cfg(not(unix))]
use std::os::raw::c_int;
use std::time::Duration;

/// Mirror of `RawFd`. On non-unix targets we fall back to `c_int` so
/// the public types still compile.
#[cfg(unix)]
pub type RawFd = std::os::unix::io::RawFd;
/// Mirror of `RawFd` for non-unix targets, aliased to `c_int` so the
/// public stub surface compiles cross-platform.
#[cfg(not(unix))]
pub type RawFd = c_int;

/// Filter type for readiness events (stub variant).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KEventFilter {
    /// `EVFILT_READ` analogue (never produced on this platform).
    Read,
    /// `EVFILT_WRITE` analogue (never produced on this platform).
    Write,
}

/// A readiness event (never constructed on non-macOS platforms).
#[derive(Debug, Clone, Copy)]
pub struct KEvent {
    /// File descriptor reported by the kernel.
    pub fd: RawFd,
    /// Filter that fired.
    pub filter: KEventFilter,
    /// User-data tag.
    pub user_data: u64,
    /// Filter-specific payload (`data` field of `struct kevent`).
    pub data: i64,
    /// Raw flags returned by the kernel.
    pub flags: u16,
}

impl KEvent {
    /// Returns whether the event reports EOF. Always `false` on the stub.
    #[must_use]
    pub fn is_eof(&self) -> bool {
        false
    }

    /// Returns whether the event reports an error. Always `false` on the stub.
    #[must_use]
    pub fn is_error(&self) -> bool {
        false
    }
}

/// Stub kqueue loop. Constructing one always returns
/// `io::ErrorKind::Unsupported`.
#[derive(Debug)]
pub struct KqueueLoop {
    _private: (),
}

impl KqueueLoop {
    /// Always returns `Unsupported` on this platform.
    ///
    /// # Errors
    ///
    /// Returns an `io::Error` with kind [`io::ErrorKind::Unsupported`].
    pub fn new() -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "kqueue is only available on macOS",
        ))
    }

    /// Returns `-1` on the stub - no fd is ever allocated.
    #[must_use]
    pub fn as_raw_fd(&self) -> RawFd {
        -1
    }

    /// Stub - always returns `Unsupported`.
    ///
    /// # Errors
    ///
    /// Returns an `io::Error` with kind [`io::ErrorKind::Unsupported`].
    pub fn submit_read(&self, _fd: RawFd, _user_data: u64) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "kqueue is only available on macOS",
        ))
    }

    /// Stub - always returns `Unsupported`.
    ///
    /// # Errors
    ///
    /// Returns an `io::Error` with kind [`io::ErrorKind::Unsupported`].
    pub fn submit_read_level(&self, _fd: RawFd, _user_data: u64) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "kqueue is only available on macOS",
        ))
    }

    /// Stub - always returns `Unsupported`.
    ///
    /// # Errors
    ///
    /// Returns an `io::Error` with kind [`io::ErrorKind::Unsupported`].
    pub fn submit_write(&self, _fd: RawFd, _user_data: u64) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "kqueue is only available on macOS",
        ))
    }

    /// Stub - always returns `Unsupported`.
    ///
    /// # Errors
    ///
    /// Returns an `io::Error` with kind [`io::ErrorKind::Unsupported`].
    pub fn remove(&self, _fd: RawFd, _filter: KEventFilter) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "kqueue is only available on macOS",
        ))
    }

    /// Stub - always returns `Unsupported`.
    ///
    /// # Errors
    ///
    /// Returns an `io::Error` with kind [`io::ErrorKind::Unsupported`].
    pub fn wait(&self, _timeout: Option<Duration>) -> io::Result<Vec<KEvent>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "kqueue is only available on macOS",
        ))
    }

    /// Stub - always returns `Unsupported`.
    ///
    /// # Errors
    ///
    /// Returns an `io::Error` with kind [`io::ErrorKind::Unsupported`].
    pub fn wait_with_capacity(
        &self,
        _timeout: Option<Duration>,
        _max_events: usize,
    ) -> io::Result<Vec<KEvent>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "kqueue is only available on macOS",
        ))
    }
}

/// Returns `false` on every non-macOS platform.
#[must_use]
pub fn is_kqueue_available() -> bool {
    false
}

/// Stub `EVFILT_TIMER` sleeper. Constructing one always returns
/// `io::ErrorKind::Unsupported` so cross-platform callers can probe
/// availability at runtime without `#[cfg]` branching.
#[derive(Debug)]
pub struct TimerSleeper {
    _private: (),
}

impl TimerSleeper {
    /// Always returns `Unsupported` on this platform.
    ///
    /// # Errors
    ///
    /// Returns an `io::Error` with kind [`io::ErrorKind::Unsupported`].
    pub fn new() -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "kqueue EVFILT_TIMER is only available on macOS",
        ))
    }

    /// Stub - always returns `Unsupported`.
    ///
    /// # Errors
    ///
    /// Returns an `io::Error` with kind [`io::ErrorKind::Unsupported`].
    pub fn sleep(&self, _duration: Duration) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "kqueue EVFILT_TIMER is only available on macOS",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_returns_unsupported() {
        let err = KqueueLoop::new().expect_err("stub never constructs");
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    }

    #[test]
    fn availability_is_false() {
        assert!(!is_kqueue_available());
    }

    #[test]
    fn timer_sleeper_new_returns_unsupported() {
        let err = TimerSleeper::new().expect_err("stub never constructs");
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    }
}
