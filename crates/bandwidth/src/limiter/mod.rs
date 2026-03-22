//! Token-bucket bandwidth limiter mirroring upstream rsync's I/O throttling.
//!
//! The module implements the pacing algorithm described in upstream
//! `io.c:sleep_for_bwlimit()`. A caller registers completed writes via
//! [`BandwidthLimiter::register`]; the limiter accumulates debt and sleeps
//! when the transfer exceeds the configured byte-per-second rate. An optional
//! burst parameter caps the maximum outstanding debt, preventing long stalls
//! after idle periods.
//!
//! Configuration updates and daemon-module override logic live in
//! [`change`], while sleep recording for deterministic tests lives in
//! [`sleep`] and [`test_support`].

use std::time::Duration;

mod change;
mod core;
mod sleep;

pub use change::{LimiterChange, apply_effective_limit};
pub use core::BandwidthLimiter;
pub use sleep::LimiterSleep;

pub(super) use sleep::{duration_from_microseconds, sleep_for};

pub(super) const MICROS_PER_SECOND: u128 = 1_000_000;
pub(super) const MINIMUM_SLEEP_MICROS: u128 = MICROS_PER_SECOND / 10;
pub(super) const MAX_REPRESENTABLE_MICROSECONDS: u128 =
    (u64::MAX as u128) * MICROS_PER_SECOND + (MICROS_PER_SECOND - 1);
pub(super) const MAX_SLEEP_DURATION: Duration = Duration::new(i64::MAX as u64, 999_999_999);
pub(super) const MIN_WRITE_MAX: usize = 512;

#[cfg(any(test, feature = "test-support"))]
mod test_support;
#[cfg(any(test, feature = "test-support"))]
pub(super) use self::test_support::append_recorded_sleeps;
#[cfg(any(test, feature = "test-support"))]
pub use self::test_support::{RecordedSleepIter, RecordedSleepSession, recorded_sleep_session};

#[cfg(test)]
mod tests;
