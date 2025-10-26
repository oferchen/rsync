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

/// Parsed `--bwlimit` components consisting of an optional rate and burst size.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BandwidthLimitComponents {
    rate: Option<NonZeroU64>,
    burst: Option<NonZeroU64>,
}

impl BandwidthLimitComponents {
    /// Constructs a new component set from the provided parts.
    #[must_use]
    pub const fn new(rate: Option<NonZeroU64>, burst: Option<NonZeroU64>) -> Self {
        Self { rate, burst }
    }

    /// Returns the configured byte-per-second rate, if any.
    #[must_use]
    pub const fn rate(self) -> Option<NonZeroU64> {
        self.rate
    }

    /// Returns the configured burst size in bytes, if any.
    #[must_use]
    pub const fn burst(self) -> Option<NonZeroU64> {
        self.burst
    }
}

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

    /// Consumes the session and returns the recorded durations.
    ///
    /// This convenience helper mirrors [`take`](Self::take) while allowing
    /// callers to move the guard by value. It is particularly useful in tests
    /// that wish to collect the recorded sleeps without keeping the session
    /// borrowed mutably for the remainder of the scope.
    #[inline]
    pub fn into_vec(mut self) -> Vec<Duration> {
        self.take()
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
    }

    #[cfg(not(any(test, feature = "test-support")))]
    {
        std::thread::sleep(duration);
    }
}

fn limit_parameters(limit: NonZeroU64, burst: Option<NonZeroU64>) -> (NonZeroU64, usize) {
    let kib = limit
        .get()
        .checked_div(1024)
        .and_then(NonZeroU64::new)
        .expect("bandwidth limit must be at least 1024 bytes per second");

    let mut write_max = u128::from(kib.get()).saturating_mul(128);
    if write_max < 512 {
        write_max = 512;
    }
    let mut write_max = write_max.min(usize::MAX as u128) as usize;

    if let Some(burst) = burst {
        let burst = burst.get().min(usize::MAX as u64);
        write_max = usize::try_from(burst).unwrap_or(usize::MAX).max(1);
    }

    (kib, write_max)
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
#[doc(alias = "--bwlimit")]
///
/// The function mirrors upstream rsync's behaviour. Leading and trailing ASCII
/// whitespace is ignored to match `strtod`'s parsing rules. `Ok(None)` denotes
/// an unlimited transfer rate (users may specify `0` for this effect).
/// Successful parses return the rounded byte-per-second limit as
/// [`NonZeroU64`].
pub fn parse_bandwidth_argument(text: &str) -> Result<Option<NonZeroU64>, BandwidthParseError> {
    let trimmed = text.trim_matches(|ch: char| ch.is_ascii_whitespace());

    if trimmed.is_empty() {
        return Err(BandwidthParseError::Invalid);
    }

    let mut unsigned = trimmed;
    let mut negative = false;

    if let Some(first) = unsigned.chars().next() {
        match first {
            '+' => {
                unsigned = &unsigned[first.len_utf8()..];
            }
            '-' => {
                negative = true;
                unsigned = &unsigned[first.len_utf8()..];
            }
            _ => {}
        }
    }

    if unsigned.is_empty() {
        return Err(BandwidthParseError::Invalid);
    }

    let mut digits_seen = false;
    let mut decimal_seen = false;
    let mut numeric_end = unsigned.len();

    for (index, ch) in unsigned.char_indices() {
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

    let numeric_part = &unsigned[..numeric_end];
    let remainder = &unsigned[numeric_end..];

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

    if negative {
        return Err(BandwidthParseError::Invalid);
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

/// Parses a bandwidth limit containing an optional burst component.
#[doc(alias = "--bwlimit")]
pub fn parse_bandwidth_limit(text: &str) -> Result<BandwidthLimitComponents, BandwidthParseError> {
    let trimmed = text.trim_matches(|ch: char| ch.is_ascii_whitespace());

    if let Some((rate_text, burst_text)) = trimmed.split_once(':') {
        let rate = parse_bandwidth_argument(rate_text)?;
        if rate.is_none() {
            return Ok(BandwidthLimitComponents::new(None, None));
        }

        let burst = parse_bandwidth_argument(burst_text)?;
        Ok(BandwidthLimitComponents::new(rate, burst))
    } else {
        parse_bandwidth_argument(trimmed).map(|rate| BandwidthLimitComponents::new(rate, None))
    }
}

/// Token-bucket style limiter that mirrors upstream rsync's pacing rules.
#[doc(alias = "--bwlimit")]
#[derive(Clone, Debug)]
pub struct BandwidthLimiter {
    limit_bytes: NonZeroU64,
    kib_per_second: NonZeroU64,
    write_max: usize,
    burst_bytes: Option<NonZeroU64>,
    total_written: u128,
    last_instant: Option<Instant>,
    simulated_elapsed_us: u128,
}

impl BandwidthLimiter {
    /// Constructs a new limiter from the supplied byte-per-second rate.
    #[must_use]
    pub fn new(limit: NonZeroU64) -> Self {
        Self::with_burst(limit, None)
    }

    /// Constructs a new limiter from the supplied rate and optional burst size.
    #[must_use]
    pub fn with_burst(limit: NonZeroU64, burst: Option<NonZeroU64>) -> Self {
        let (kib, write_max) = limit_parameters(limit, burst);

        Self {
            limit_bytes: limit,
            kib_per_second: kib,
            write_max,
            burst_bytes: burst,
            total_written: 0,
            last_instant: None,
            simulated_elapsed_us: 0,
        }
    }

    /// Updates the limiter so a new byte-per-second limit takes effect.
    ///
    /// Upstream rsync applies daemon-imposed caps by resetting its pacing state
    /// before continuing the transfer with the negotiated limit. Mirroring that
    /// behaviour keeps previously accumulated debt from leaking into the new
    /// configuration and ensures subsequent calls behave as if the limiter had
    /// been freshly constructed with the supplied rate.
    pub fn update_limit(&mut self, limit: NonZeroU64) {
        self.update_configuration(limit, self.burst_bytes);
    }

    /// Updates the limiter so both the rate and burst configuration take effect.
    ///
    /// Upstream rsync resets its token bucket whenever the daemon imposes a new
    /// `--bwlimit=RATE[:BURST]` combination. Reusing that behaviour keeps
    /// previously accumulated debt from leaking into the new configuration and
    /// ensures subsequent calls behave as if the limiter had just been
    /// constructed via [`BandwidthLimiter::with_burst`].
    #[doc(alias = "--bwlimit")]
    pub fn update_configuration(&mut self, limit: NonZeroU64, burst: Option<NonZeroU64>) {
        let (kib, write_max) = limit_parameters(limit, burst);

        self.limit_bytes = limit;
        self.kib_per_second = kib;
        self.write_max = write_max;
        self.burst_bytes = burst;
        self.total_written = 0;
        self.last_instant = None;
        self.simulated_elapsed_us = 0;
    }

    #[inline]
    fn clamp_debt_to_burst(&mut self) {
        if let Some(burst) = self.burst_bytes {
            let limit = u128::from(burst.get());
            if self.total_written > limit {
                self.total_written = limit;
            }
        }
    }

    /// Returns the configured limit in bytes per second.
    #[must_use]
    pub const fn limit_bytes(&self) -> NonZeroU64 {
        self.limit_bytes
    }

    /// Returns the configured burst size in bytes, if any.
    #[must_use]
    pub const fn burst_bytes(&self) -> Option<NonZeroU64> {
        self.burst_bytes
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
        self.clamp_debt_to_burst();

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

        self.clamp_debt_to_burst();

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
        self.clamp_debt_to_burst();
        self.last_instant = Some(end);
    }

    /// Returns the outstanding byte debt accumulated by the limiter.
    ///
    /// The accessor is compiled for tests (and the `test-support` feature) so
    /// scenarios can assert on the internal pacing state without relying on
    /// private fields. Production builds omit the helper entirely.
    #[cfg(any(test, feature = "test-support"))]
    #[allow(dead_code)]
    pub(crate) fn accumulated_debt_for_testing(&self) -> u128 {
        self.total_written
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BandwidthLimitComponents, BandwidthLimiter, BandwidthParseError, MINIMUM_SLEEP_MICROS,
        parse_bandwidth_argument, parse_bandwidth_limit,
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
    fn parse_bandwidth_accepts_leading_plus_sign() {
        let limit = parse_bandwidth_argument("+1M").expect("parse succeeds");
        assert_eq!(limit, NonZeroU64::new(1_048_576));
    }

    #[test]
    fn parse_bandwidth_accepts_comma_fraction_separator() {
        let limit = parse_bandwidth_argument("0,5M").expect("parse succeeds");
        assert_eq!(limit, NonZeroU64::new(512 * 1024));
    }

    #[test]
    fn parse_bandwidth_limit_accepts_burst_component() {
        let components = parse_bandwidth_limit("1M:64K").expect("parse succeeds");
        assert_eq!(
            components,
            BandwidthLimitComponents::new(NonZeroU64::new(1_048_576), NonZeroU64::new(64 * 1024),)
        );
    }

    #[test]
    fn parse_bandwidth_limit_zero_rate_disables_burst() {
        let components = parse_bandwidth_limit("0:128K").expect("parse succeeds");
        assert_eq!(components, BandwidthLimitComponents::new(None, None));
    }

    #[test]
    fn parse_bandwidth_limit_accepts_zero_burst() {
        let components = parse_bandwidth_limit("1M:0").expect("parse succeeds");
        assert_eq!(
            components,
            BandwidthLimitComponents::new(NonZeroU64::new(1_048_576), None)
        );
    }

    #[test]
    fn parse_bandwidth_trims_surrounding_whitespace() {
        let limit = parse_bandwidth_argument("\t 2M \n").expect("parse succeeds");
        assert_eq!(limit, NonZeroU64::new(2_097_152));
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
    fn parse_bandwidth_rejects_negative_values() {
        let error = parse_bandwidth_argument("-1M").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);
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
    fn limiter_respects_custom_burst() {
        let limiter = BandwidthLimiter::with_burst(
            NonZeroU64::new(8 * 1024 * 1024).unwrap(),
            NonZeroU64::new(2048),
        );
        assert_eq!(limiter.recommended_read_size(8192), 2048);
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

    #[test]
    fn limiter_clamps_debt_to_configured_burst() {
        let mut session = crate::recorded_sleep_session();
        session.clear();

        let burst = NonZeroU64::new(4096).expect("non-zero burst");
        let mut limiter = BandwidthLimiter::with_burst(
            NonZeroU64::new(8 * 1024 * 1024).expect("non-zero limit"),
            Some(burst),
        );

        limiter.register(1 << 20);

        assert!(
            limiter.accumulated_debt_for_testing() <= u128::from(burst.get()),
            "debt exceeds configured burst"
        );
    }

    #[test]
    fn recorded_sleep_session_into_vec_consumes_guard() {
        let mut session = crate::recorded_sleep_session();
        session.clear();

        let mut limiter = BandwidthLimiter::new(NonZeroU64::new(1024).unwrap());
        limiter.register(2048);

        let recorded = session.into_vec();
        assert!(!recorded.is_empty());

        let mut follow_up = crate::recorded_sleep_session();
        assert!(follow_up.is_empty());
        let _ = follow_up.take();
    }

    #[test]
    fn limiter_update_limit_resets_internal_state() {
        let mut session = crate::recorded_sleep_session();
        session.clear();

        let new_limit = NonZeroU64::new(8 * 1024 * 1024).unwrap();
        let mut baseline = BandwidthLimiter::new(new_limit);
        baseline.register(4096);
        let baseline_sleeps = session.take();

        session.clear();

        let mut limiter = BandwidthLimiter::new(NonZeroU64::new(1024).unwrap());
        limiter.register(4096);
        session.clear();

        limiter.update_limit(new_limit);
        limiter.register(4096);
        assert_eq!(limiter.limit_bytes(), new_limit);
        assert_eq!(limiter.recommended_read_size(1 << 20), 1 << 20);

        let updated_sleeps = session.take();
        assert_eq!(updated_sleeps, baseline_sleeps);
    }

    #[test]
    fn limiter_update_configuration_resets_state_and_updates_burst() {
        let mut session = crate::recorded_sleep_session();
        session.clear();

        let initial_limit = NonZeroU64::new(1024).unwrap();
        let initial_burst = NonZeroU64::new(4096).unwrap();
        let mut limiter = BandwidthLimiter::with_burst(initial_limit, Some(initial_burst));
        limiter.register(8192);
        assert!(limiter.accumulated_debt_for_testing() > 0);

        let new_limit = NonZeroU64::new(8 * 1024 * 1024).unwrap();
        let new_burst = NonZeroU64::new(2048).unwrap();
        limiter.update_configuration(new_limit, Some(new_burst));

        assert_eq!(limiter.limit_bytes(), new_limit);
        assert_eq!(limiter.burst_bytes(), Some(new_burst));
        assert_eq!(limiter.accumulated_debt_for_testing(), 0);

        session.clear();
        limiter.register(1024);
        let recorded = session.take();
        assert!(
            recorded.is_empty()
                || recorded
                    .iter()
                    .all(|duration| duration.as_micros() <= MINIMUM_SLEEP_MICROS)
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
