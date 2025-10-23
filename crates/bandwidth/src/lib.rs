#![deny(unsafe_code)]
#![deny(rustdoc::broken_intra_doc_links)]
#![deny(missing_docs)]

//! # Overview
//!
//! `rsync_bandwidth` centralises parsing and pacing logic for rsync's
//! `--bwlimit` option. The crate exposes helpers for decoding user supplied
//! bandwidth limits together with a [`BandwidthLimiter`] state machine that
//! mirrors upstream rsync's token bucket. Higher level crates reuse these
//! utilities to share validation and throttling behaviour between the client,
//! daemon, and future transport layers.
//!
//! # Design
//!
//! - [`parse_bandwidth_argument`] accepts textual rate specifications using the
//!   same syntax as upstream rsync (binary/decimal suffixes, fractional values,
//!   and optional `+1`/`-1` adjustments) and returns either an optional limit in
//!   bytes per second or a [`BandwidthParseError`].
//! - [`BandwidthLimiter`] implements the pacing algorithm used by the local copy
//!   engine and daemon. It keeps track of the accumulated byte debt and sleeps
//!   long enough to honour the configured limit while coalescing short bursts to
//!   avoid excessive context switches.
//!
//! # Invariants
//!
//! - Parsed rates are always rounded to the nearest multiple of 1024 bytes per
//!   second, matching upstream rsync.
//! - The limiter never sleeps for intervals shorter than 100ms to align with the
//!   behaviour of the C implementation.
//! - When the optional `test-support` feature is enabled (used by unit tests),
//!   sleep requests are recorded instead of reaching `std::thread::sleep`,
//!   keeping the tests deterministic and fast.
//!
//! # Examples
//!
//! Parse textual input and construct a limiter that bounds writes to 8 MiB/s.
//!
//! ```
//! use rsync_bandwidth::{parse_bandwidth_argument, BandwidthLimiter};
//! use std::num::NonZeroU64;
//!
//! let limit = parse_bandwidth_argument("8M").expect("valid limit")
//!     .expect("non-zero limit");
//! let mut limiter = BandwidthLimiter::new(limit);
//! let chunk = limiter.recommended_read_size(1 << 20);
//! assert!(chunk <= 1 << 20);
//! limiter.register(chunk);
//! ```
//!
//! # See also
//!
//! - [`rsync_core::client`](https://docs.rs/rsync-core/) and
//!   [`rsync_daemon`](https://docs.rs/rsync-daemon/) which reuse these helpers
//!   for CLI and daemon orchestration.

use std::num::NonZeroU64;
use std::time::{Duration, Instant};

#[cfg(any(test, feature = "test-support"))]
use std::mem;

#[cfg(any(test, feature = "test-support"))]
use std::sync::{Mutex, MutexGuard, OnceLock};

const MICROS_PER_SECOND: u128 = 1_000_000;
const MICROS_PER_SECOND_DIV_1024: u128 = MICROS_PER_SECOND / 1024;
const MINIMUM_SLEEP_MICROS: u128 = MICROS_PER_SECOND / 10;

#[cfg(any(test, feature = "test-support"))]
fn recorded_sleeps() -> &'static Mutex<Vec<Duration>> {
    static RECORDED_SLEEPS: OnceLock<Mutex<Vec<Duration>>> = OnceLock::new();
    RECORDED_SLEEPS.get_or_init(|| Mutex::new(Vec::new()))
}

#[cfg(any(test, feature = "test-support"))]
fn recorded_sleep_session_lock() -> &'static Mutex<()> {
    static SESSION_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    SESSION_LOCK.get_or_init(|| Mutex::new(()))
}

#[cfg(any(test, feature = "test-support"))]
/// Guard that provides exclusive access to the recorded sleep durations.
///
/// Tests obtain a [`RecordedSleepSession`] at the start of a scenario, call
/// [`RecordedSleepSession::clear`] to discard previous measurements, execute the
/// code under test, and finally inspect the captured durations via
/// [`RecordedSleepSession::take`]. Holding the guard ensures concurrent tests do
/// not drain or append to the shared buffer while assertions run, eliminating
/// the data races observed when multiple tests exercised the limiter in
/// parallel.
pub struct RecordedSleepSession<'a> {
    _guard: MutexGuard<'a, ()>,
}

#[cfg(any(test, feature = "test-support"))]
impl<'a> RecordedSleepSession<'a> {
    /// Removes any previously recorded durations.
    #[inline]
    pub fn clear(&mut self) {
        recorded_sleeps()
            .lock()
            .expect("lock recorded sleeps")
            .clear();
    }

    /// Returns `true` when no sleep durations have been recorded.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        recorded_sleeps()
            .lock()
            .expect("lock recorded sleeps")
            .is_empty()
    }

    /// Returns the number of recorded sleep intervals.
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        recorded_sleeps()
            .lock()
            .expect("lock recorded sleeps")
            .len()
    }

    /// Drains the recorded sleep durations, returning ownership of the vector.
    #[inline]
    pub fn take(&mut self) -> Vec<Duration> {
        let mut guard = recorded_sleeps().lock().expect("lock recorded sleeps");
        mem::take(&mut *guard)
    }
}

#[cfg(any(test, feature = "test-support"))]
/// Obtains a guard that serialises access to recorded sleep durations.
#[must_use]
pub fn recorded_sleep_session() -> RecordedSleepSession<'static> {
    RecordedSleepSession {
        _guard: recorded_sleep_session_lock()
            .lock()
            .expect("lock recorded sleep session"),
    }
}

#[cfg(any(test, feature = "test-support"))]
#[deprecated(note = "use `recorded_sleep_session()` to guard access during tests")]
/// Retrieves and clears the recorded sleep durations.
pub fn take_recorded_sleeps() -> Vec<Duration> {
    let mut session = recorded_sleep_session();
    session.take()
}

fn duration_from_microseconds(us: u128) -> Duration {
    if us == 0 {
        return Duration::ZERO;
    }

    let seconds = us / MICROS_PER_SECOND;
    let micros = (us % MICROS_PER_SECOND) as u32;

    if seconds >= u128::from(u64::MAX) {
        Duration::MAX
    } else {
        Duration::new(seconds as u64, micros.saturating_mul(1_000))
    }
}

fn sleep_for(duration: Duration) {
    if duration.is_zero() {
        return;
    }

    #[cfg(any(test, feature = "test-support"))]
    {
        recorded_sleeps()
            .lock()
            .expect("lock recorded sleeps")
            .push(duration);
        return;
    }

    #[cfg(not(any(test, feature = "test-support")))]
    {
        std::thread::sleep(duration);
    }
}

/// Errors returned when parsing a bandwidth limit fails.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BandwidthParseError {
    /// The argument did not follow rsync's recognised syntax.
    Invalid,
    /// The requested rate was too small (less than 512 bytes per second).
    TooSmall,
    /// The requested rate overflowed the supported range.
    TooLarge,
}

fn parse_decimal_components(text: &str) -> Result<(u128, u128, u128), BandwidthParseError> {
    let mut integer = 0u128;
    let mut fraction = 0u128;
    let mut denominator = 1u128;
    let mut saw_decimal = false;

    for ch in text.chars() {
        match ch {
            '0'..='9' => {
                let digit = u128::from(ch as u8 - b'0');
                if saw_decimal {
                    denominator = denominator
                        .checked_mul(10)
                        .ok_or(BandwidthParseError::TooLarge)?;
                    fraction = fraction
                        .checked_mul(10)
                        .and_then(|value| value.checked_add(digit))
                        .ok_or(BandwidthParseError::TooLarge)?;
                } else {
                    integer = integer
                        .checked_mul(10)
                        .and_then(|value| value.checked_add(digit))
                        .ok_or(BandwidthParseError::TooLarge)?;
                }
            }
            '.' | ',' => {
                if saw_decimal {
                    return Err(BandwidthParseError::Invalid);
                }
                saw_decimal = true;
            }
            _ => return Err(BandwidthParseError::Invalid),
        }
    }

    Ok((integer, fraction, denominator))
}

fn pow_u128(base: u32, exponent: u32) -> Result<u128, BandwidthParseError> {
    let mut acc = 1u128;
    for _ in 0..exponent {
        acc = acc
            .checked_mul(u128::from(base))
            .ok_or(BandwidthParseError::TooLarge)?;
    }
    Ok(acc)
}

/// Parses a `--bwlimit` style argument into an optional byte-per-second limit.
///
/// The function mirrors upstream rsync's behaviour. `Ok(None)` denotes an
/// unlimited transfer rate (users may specify `0` for this effect). Successful
/// parses return the rounded byte-per-second limit as [`NonZeroU64`].
pub fn parse_bandwidth_argument(text: &str) -> Result<Option<NonZeroU64>, BandwidthParseError> {
    if text.is_empty() {
        return Err(BandwidthParseError::Invalid);
    }

    let mut digits_seen = false;
    let mut decimal_seen = false;
    let mut numeric_end = text.len();

    for (index, ch) in text.char_indices() {
        if ch.is_ascii_digit() {
            digits_seen = true;
            continue;
        }

        if (ch == '.' || ch == ',') && !decimal_seen {
            decimal_seen = true;
            continue;
        }

        numeric_end = index;
        break;
    }

    let numeric_part = &text[..numeric_end];
    let remainder = &text[numeric_end..];

    if !digits_seen || numeric_part == "." || numeric_part == "," {
        return Err(BandwidthParseError::Invalid);
    }

    let (integer_part, fractional_part, denominator) = parse_decimal_components(numeric_part)?;

    let (suffix, mut remainder_after_suffix) =
        if remainder.is_empty() || remainder.starts_with('+') || remainder.starts_with('-') {
            ('K', remainder)
        } else {
            let mut chars = remainder.chars();
            let ch = chars.next().unwrap();
            (ch, chars.as_str())
        };

    let repetitions = match suffix.to_ascii_lowercase() {
        'b' => 0,
        'k' => 1,
        'm' => 2,
        'g' => 3,
        't' => 4,
        'p' => 5,
        _ => return Err(BandwidthParseError::Invalid),
    };

    let mut base: u32 = 1024;

    if !remainder_after_suffix.is_empty() {
        let bytes = remainder_after_suffix.as_bytes();
        match bytes[0] {
            b'b' | b'B' => {
                base = 1000;
                remainder_after_suffix = &remainder_after_suffix[1..];
            }
            b'i' | b'I' => {
                if bytes.len() < 2 {
                    return Err(BandwidthParseError::Invalid);
                }
                if matches!(bytes[1], b'b' | b'B') {
                    base = 1024;
                    remainder_after_suffix = &remainder_after_suffix[2..];
                } else {
                    return Err(BandwidthParseError::Invalid);
                }
            }
            b'+' | b'-' => {}
            _ => return Err(BandwidthParseError::Invalid),
        }
    }

    let mut adjust = 0i8;
    if !remainder_after_suffix.is_empty() {
        if remainder_after_suffix == "+1" && numeric_end > 0 {
            adjust = 1;
            remainder_after_suffix = "";
        } else if remainder_after_suffix == "-1" && numeric_end > 0 {
            adjust = -1;
            remainder_after_suffix = "";
        }
    }

    if !remainder_after_suffix.is_empty() {
        return Err(BandwidthParseError::Invalid);
    }

    let scale = pow_u128(base, repetitions)?;

    let numerator = integer_part
        .checked_mul(denominator)
        .and_then(|value| value.checked_add(fractional_part))
        .ok_or(BandwidthParseError::TooLarge)?;
    let product = numerator
        .checked_mul(scale)
        .ok_or(BandwidthParseError::TooLarge)?;

    let mut bytes = product / denominator;

    if adjust == -1 {
        if product >= denominator {
            bytes = bytes.checked_sub(1).ok_or(BandwidthParseError::TooLarge)?;
        } else {
            bytes = 0;
        }
    } else if adjust == 1 {
        bytes = bytes.checked_add(1).ok_or(BandwidthParseError::TooLarge)?;
    }

    if bytes == 0 {
        return Ok(None);
    }

    if bytes < 512 {
        return Err(BandwidthParseError::TooSmall);
    }

    let rounded = bytes
        .checked_add(512)
        .ok_or(BandwidthParseError::TooLarge)?
        / 1024;
    let rounded_bytes = rounded
        .checked_mul(1024)
        .ok_or(BandwidthParseError::TooLarge)?;

    let bytes_u64 = u64::try_from(rounded_bytes).map_err(|_| BandwidthParseError::TooLarge)?;
    NonZeroU64::new(bytes_u64)
        .ok_or(BandwidthParseError::TooSmall)
        .map(Some)
}

/// Token-bucket style limiter that mirrors upstream rsync's pacing rules.
#[derive(Clone, Debug)]
pub struct BandwidthLimiter {
    limit_bytes: NonZeroU64,
    kib_per_second: NonZeroU64,
    write_max: usize,
    total_written: u128,
    last_instant: Option<Instant>,
    simulated_elapsed_us: u128,
}

impl BandwidthLimiter {
    /// Constructs a new limiter from the supplied byte-per-second rate.
    #[must_use]
    pub fn new(limit: NonZeroU64) -> Self {
        let kib = limit
            .get()
            .checked_div(1024)
            .and_then(NonZeroU64::new)
            .expect("bandwidth limit must be at least 1024 bytes per second");
        let mut write_max = u128::from(kib.get()).saturating_mul(128);
        if write_max < 512 {
            write_max = 512;
        }
        let write_max = write_max.min(usize::MAX as u128) as usize;

        Self {
            limit_bytes: limit,
            kib_per_second: kib,
            write_max,
            total_written: 0,
            last_instant: None,
            simulated_elapsed_us: 0,
        }
    }

    /// Returns the configured limit in bytes per second.
    #[must_use]
    pub const fn limit_bytes(&self) -> NonZeroU64 {
        self.limit_bytes
    }

    /// Returns the maximum chunk size that should be written before sleeping.
    #[must_use]
    pub fn recommended_read_size(&self, buffer_len: usize) -> usize {
        let limit = self.write_max.max(1);
        buffer_len.min(limit)
    }

    /// Records a completed write and sleeps if the limiter accumulated debt.
    pub fn register(&mut self, bytes: usize) {
        if bytes == 0 {
            return;
        }

        self.total_written = self.total_written.saturating_add(bytes as u128);

        let start = Instant::now();

        let mut elapsed_us = self.simulated_elapsed_us;
        if let Some(previous) = self.last_instant {
            let elapsed = start.duration_since(previous);
            let measured = elapsed.as_micros().min(u128::from(u64::MAX));
            elapsed_us = elapsed_us.saturating_add(measured);
        }
        self.simulated_elapsed_us = 0;
        if elapsed_us > 0 {
            let allowed = elapsed_us.saturating_mul(u128::from(self.kib_per_second.get()))
                / MICROS_PER_SECOND_DIV_1024;
            if allowed >= self.total_written {
                self.total_written = 0;
            } else {
                self.total_written -= allowed;
            }
        }

        let sleep_us = self
            .total_written
            .saturating_mul(MICROS_PER_SECOND_DIV_1024)
            / u128::from(self.kib_per_second.get());

        if sleep_us < MINIMUM_SLEEP_MICROS {
            self.last_instant = Some(start);
            return;
        }

        let requested = duration_from_microseconds(sleep_us);
        if !requested.is_zero() {
            sleep_for(requested);
        }

        let end = Instant::now();
        let elapsed_us = end
            .checked_duration_since(start)
            .map(|duration| duration.as_micros().min(u128::from(u64::MAX)))
            .unwrap_or(0);
        if sleep_us > elapsed_us {
            self.simulated_elapsed_us = sleep_us - elapsed_us;
        }
        let remaining_us = sleep_us.saturating_sub(elapsed_us);
        let leftover = remaining_us.saturating_mul(u128::from(self.kib_per_second.get()))
            / MICROS_PER_SECOND_DIV_1024;

        self.total_written = leftover;
        self.last_instant = Some(end);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BandwidthLimiter, BandwidthParseError, MINIMUM_SLEEP_MICROS, parse_bandwidth_argument,
    };
    use proptest::prelude::*;
    use std::num::NonZeroU64;
    use std::time::Duration;

    #[test]
    fn parse_bandwidth_accepts_binary_units() {
        let limit = parse_bandwidth_argument("12M").expect("parse succeeds");
        assert_eq!(limit, NonZeroU64::new(12 * 1024 * 1024));
    }

    #[test]
    fn parse_bandwidth_accepts_decimal_units() {
        let limit = parse_bandwidth_argument("12MB").expect("parse succeeds");
        assert_eq!(limit, NonZeroU64::new(12_000_256));
    }

    #[test]
    fn parse_bandwidth_accepts_iec_suffixes() {
        let limit = parse_bandwidth_argument("1MiB").expect("parse succeeds");
        assert_eq!(limit, NonZeroU64::new(1_048_576));
    }

    #[test]
    fn parse_bandwidth_accepts_trailing_decimal_point() {
        let limit = parse_bandwidth_argument("1.").expect("parse succeeds");
        assert_eq!(limit, NonZeroU64::new(1024));
    }

    #[test]
    fn parse_bandwidth_accepts_zero_for_unlimited() {
        assert_eq!(parse_bandwidth_argument("0").expect("parse"), None);
    }

    #[test]
    fn parse_bandwidth_rejects_small_values() {
        let error = parse_bandwidth_argument("0.25K").unwrap_err();
        assert_eq!(error, BandwidthParseError::TooSmall);
    }

    #[test]
    fn parse_bandwidth_rejects_invalid_suffix() {
        let error = parse_bandwidth_argument("10Q").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);
    }

    #[test]
    fn parse_bandwidth_handles_fractional_values() {
        let limit = parse_bandwidth_argument("0.5M").expect("parse succeeds");
        assert_eq!(limit, NonZeroU64::new(512 * 1024));
    }

    #[test]
    fn parse_bandwidth_accepts_comma_fraction_separator() {
        let limit = parse_bandwidth_argument("0,5M").expect("parse succeeds");
        assert_eq!(limit, NonZeroU64::new(512 * 1024));
    }

    #[test]
    fn parse_bandwidth_accepts_positive_adjustment() {
        let limit = parse_bandwidth_argument("1K+1").expect("parse succeeds");
        assert_eq!(limit, NonZeroU64::new(1024));
    }

    #[test]
    fn parse_bandwidth_honours_negative_adjustment_for_small_values() {
        let limit = parse_bandwidth_argument("0.001M-1").expect("parse succeeds");
        assert_eq!(limit, NonZeroU64::new(0x400));
    }

    #[test]
    fn parse_bandwidth_negative_adjustment_can_trigger_too_small() {
        let error = parse_bandwidth_argument("0.0001M-1").unwrap_err();
        assert_eq!(error, BandwidthParseError::TooSmall);
    }

    #[test]
    fn parse_bandwidth_rejects_overflow() {
        let error = parse_bandwidth_argument("999999999999999999999999999999P").unwrap_err();
        assert_eq!(error, BandwidthParseError::TooLarge);
    }

    #[test]
    fn limiter_limits_chunk_size_for_slow_rates() {
        let limiter = BandwidthLimiter::new(NonZeroU64::new(1024).unwrap());
        assert_eq!(limiter.recommended_read_size(8192), 512);
        assert_eq!(limiter.recommended_read_size(256), 256);
    }

    #[test]
    fn limiter_preserves_buffer_for_fast_rates() {
        let limiter = BandwidthLimiter::new(NonZeroU64::new(8 * 1024 * 1024).unwrap());
        assert_eq!(limiter.recommended_read_size(8192), 8192);
    }

    #[test]
    fn limiter_records_sleep_for_large_writes() {
        let mut session = crate::recorded_sleep_session();
        session.clear();
        let mut limiter = BandwidthLimiter::new(NonZeroU64::new(1024).unwrap());
        limiter.register(4096);
        let recorded = session.take();
        assert!(
            recorded
                .iter()
                .any(|duration| duration >= &Duration::from_micros(MINIMUM_SLEEP_MICROS as u64))
        );
    }

    proptest! {
        #[test]
        fn parse_round_trips_when_limit_is_multiple_of_1024(value in 1u64..1_000_000u64) {
            let text = format!("{}K", value);
            let parsed = parse_bandwidth_argument(&text).expect("parse succeeds");
            let expected = NonZeroU64::new(value * 1024).expect("non-zero");
            prop_assert_eq!(parsed, Some(expected));
        }
    }
}
