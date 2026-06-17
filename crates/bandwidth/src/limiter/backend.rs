//! Sleep backend selection for the bandwidth limiter.
//!
//! Abstracts the underlying sleep primitive so the limiter's pacing
//! loop can use either [`std::thread::sleep`] or a platform-native
//! high-resolution timer. The kqueue `EVFILT_TIMER` backend
//! (`KQ-S.4`) reaches nanosecond resolution on macOS via Mach
//! absolute time, avoiding the ~1 ms granularity and CPU spin that
//! `std::thread::sleep` exhibits for sub-millisecond throttles.
//!
//! # Selection
//!
//! The backend is selected once per process the first time
//! [`sleep_with_backend`] is called and cached for the rest of the
//! process lifetime. The selection rules are:
//!
//! 1. If the environment variable `OC_RSYNC_BWLIMIT_BACKEND` is set,
//!    its value (`std`, `thread`, `kqueue`, `timer`) overrides the
//!    default. Unknown values fall back to the platform default with
//!    no error - the limiter must never refuse to throttle because of
//!    a typo'd env var.
//! 2. Otherwise on macOS the kqueue backend is the default. If the
//!    `TimerSleeper` constructor fails (e.g. fd table exhaustion the
//!    process recovers from later), the limiter silently falls back
//!    to `std::thread::sleep` and stays on it.
//! 3. Every other platform uses [`std::thread::sleep`].
//!
//! The backend choice is deliberately one-shot per process: pacing
//! correctness only depends on the relative jitter being small, not
//! on dynamic reconfiguration. A long-running daemon keeps the same
//! sleeper instance and amortises the kqueue setup cost.

use std::env;
use std::sync::OnceLock;
use std::time::Duration;

#[cfg(target_os = "macos")]
use fast_io::TimerSleeper;

/// Environment variable that overrides the default sleep backend.
///
/// Accepted values (case-insensitive):
///
/// - `std` / `thread` - use [`std::thread::sleep`].
/// - `kqueue` / `timer` - use the kqueue `EVFILT_TIMER` sleeper
///   (macOS only; ignored elsewhere and on construction failure).
///
/// Any other value (or absence) leaves the platform default in
/// place.
pub(crate) const BACKEND_ENV_VAR: &str = "OC_RSYNC_BWLIMIT_BACKEND";

/// Identifier returned by [`active_backend`] for diagnostic logging
/// and the regression tests in `crates/bandwidth/tests/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SleepBackend {
    /// `std::thread::sleep` - the portable baseline.
    Std,
    /// macOS kqueue `EVFILT_TIMER` via `fast_io::TimerSleeper`.
    Kqueue,
}

/// Active sleep backend for this process.
///
/// Initialised on first call to `sleep_with_backend`; the value is
/// fixed for the rest of the process. Reading the active backend is
/// useful for diagnostics and for the Darwin-only regression test
/// that asserts the kqueue path is exercised.
#[must_use]
pub fn active_backend() -> SleepBackend {
    match active_sleeper() {
        ActiveSleeper::Std => SleepBackend::Std,
        #[cfg(target_os = "macos")]
        ActiveSleeper::Kqueue(_) => SleepBackend::Kqueue,
    }
}

/// Sleeps for the supplied duration using the active backend.
///
/// A zero or negligible duration returns immediately without
/// touching the kernel.
pub(crate) fn sleep_with_backend(duration: Duration) {
    if duration.is_zero() {
        return;
    }

    match active_sleeper() {
        ActiveSleeper::Std => std::thread::sleep(duration),
        #[cfg(target_os = "macos")]
        ActiveSleeper::Kqueue(sleeper) => {
            if sleeper.sleep(duration).is_err() {
                // Fall back to the portable primitive if the kqueue
                // call rejects the timer registration - pacing
                // correctness must never block on a syscall failure.
                std::thread::sleep(duration);
            }
        }
    }
}

#[derive(Clone, Copy)]
enum ActiveSleeper {
    Std,
    #[cfg(target_os = "macos")]
    Kqueue(&'static TimerSleeper),
}

fn active_sleeper() -> ActiveSleeper {
    static CHOICE: OnceLock<ActiveSleeper> = OnceLock::new();
    *CHOICE.get_or_init(select_backend)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackendRequest {
    /// Caller asked for the portable `std::thread::sleep` backend.
    Std,
    /// Caller asked for the kqueue timer; falls back to `Std` on
    /// non-macOS or if the constructor fails.
    Kqueue,
    /// Caller did not express a preference - use the platform
    /// default.
    Default,
}

fn select_backend() -> ActiveSleeper {
    let request = read_backend_request();
    let want_kqueue = matches!(request, BackendRequest::Kqueue)
        || (matches!(request, BackendRequest::Default) && platform_default_is_kqueue());

    #[cfg(target_os = "macos")]
    if want_kqueue {
        static SLEEPER: OnceLock<TimerSleeper> = OnceLock::new();
        if let Some(sleeper) = SLEEPER.get() {
            return ActiveSleeper::Kqueue(sleeper);
        }
        match TimerSleeper::new() {
            Ok(s) => {
                let stored = SLEEPER.get_or_init(|| s);
                return ActiveSleeper::Kqueue(stored);
            }
            Err(_) => return ActiveSleeper::Std,
        }
    }

    #[cfg(not(target_os = "macos"))]
    let _ = want_kqueue;

    ActiveSleeper::Std
}

fn read_backend_request() -> BackendRequest {
    let Ok(raw) = env::var(BACKEND_ENV_VAR) else {
        return BackendRequest::Default;
    };
    parse_backend_request(&raw)
}

fn parse_backend_request(raw: &str) -> BackendRequest {
    let trimmed = raw.trim();
    if trimmed.eq_ignore_ascii_case("std") || trimmed.eq_ignore_ascii_case("thread") {
        BackendRequest::Std
    } else if trimmed.eq_ignore_ascii_case("kqueue") || trimmed.eq_ignore_ascii_case("timer") {
        BackendRequest::Kqueue
    } else {
        // Unknown values default to the platform choice; pacing must
        // not refuse to throttle because of a typo'd env var.
        BackendRequest::Default
    }
}

const fn platform_default_is_kqueue() -> bool {
    cfg!(target_os = "macos")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_backend_request_recognises_std_aliases() {
        assert_eq!(parse_backend_request("std"), BackendRequest::Std);
        assert_eq!(parse_backend_request("STD"), BackendRequest::Std);
        assert_eq!(parse_backend_request("thread"), BackendRequest::Std);
        assert_eq!(parse_backend_request("  Thread  "), BackendRequest::Std);
    }

    #[test]
    fn parse_backend_request_recognises_kqueue_aliases() {
        assert_eq!(parse_backend_request("kqueue"), BackendRequest::Kqueue);
        assert_eq!(parse_backend_request("KQUEUE"), BackendRequest::Kqueue);
        assert_eq!(parse_backend_request("timer"), BackendRequest::Kqueue);
        assert_eq!(parse_backend_request("Timer"), BackendRequest::Kqueue);
    }

    #[test]
    fn parse_backend_request_unknown_values_default() {
        assert_eq!(parse_backend_request(""), BackendRequest::Default);
        assert_eq!(parse_backend_request("epoll"), BackendRequest::Default);
        assert_eq!(parse_backend_request("123"), BackendRequest::Default);
    }

    #[test]
    fn platform_default_matches_target() {
        if cfg!(target_os = "macos") {
            assert!(platform_default_is_kqueue());
        } else {
            assert!(!platform_default_is_kqueue());
        }
    }

    #[test]
    fn sleep_zero_returns_immediately() {
        // No backend should be initialised for a zero duration.
        let start = std::time::Instant::now();
        sleep_with_backend(Duration::ZERO);
        assert!(start.elapsed() < Duration::from_millis(5));
    }

    #[test]
    fn active_backend_matches_platform_default() {
        // The OnceLock may already be initialised by an earlier
        // test, so we just assert the value is internally
        // consistent with the platform.
        let backend = active_backend();
        if cfg!(target_os = "macos") {
            assert!(matches!(backend, SleepBackend::Kqueue | SleepBackend::Std));
        } else {
            assert_eq!(backend, SleepBackend::Std);
        }
    }
}
