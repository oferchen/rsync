//! Property-based tests for the `--bwlimit` token-bucket pacing algorithm.
//!
//! These tests exercise [`bandwidth::BandwidthLimiter`] over randomised
//! `(rate, burst, chunk)` combinations and verify the long-run pacing
//! invariants that mirror upstream rsync's `io.c:sleep_for_bwlimit()`:
//!
//! 1. **Rate convergence** - after a warm-up period the cumulative bytes
//!    divided by the cumulative requested-sleep time stays within +/-5 %
//!    of the configured byte-per-second rate.
//! 2. **Burst capacity** - a single registration after an idle period
//!    produces at most `min(N, burst)` outstanding debt, so the requested
//!    sleep duration cannot exceed `burst / rate` seconds.
//! 3. **Zero-rate (`bwlimit=0`) is unlimited** - the parser returns
//!    `Ok(None)` so no limiter is constructed and pacing is bypassed.
//!
//! The pacing model is event-driven: the limiter samples
//! [`std::time::Instant::now`] internally, and under the `test-support`
//! feature each requested sleep is appended to a global recorder so
//! tests can inspect the pacing schedule (see
//! `limiter/test_support.rs`). The actual `std::thread::sleep` still
//! runs alongside the recorder when the crate is compiled as a
//! dependency, so test inputs are sized to keep individual `requested`
//! durations to a few hundred milliseconds. We assert on the
//! *requested* duration rather than the *actual* duration so the
//! invariants hold deterministically regardless of OS scheduling.
//
// upstream: io.c:2025 `sleep_for_bwlimit()` - the rate enforcement
// routine our `BandwidthLimiter::register` mirrors. The C symbol is
// `sleep_for_bwlimit`; some references in older rsync documentation use
// the historical name `bwlimit_pause` for the same routine.

use bandwidth::{BandwidthLimiter, parse_bandwidth_argument, recorded_sleep_session};
use proptest::prelude::*;
use std::num::NonZeroU64;
use std::time::Duration;

/// Lowest rate exercised by the property tests: 1 KiB/s.
///
/// Upstream rsync clamps `--bwlimit` to a 512 B/s minimum, but the
/// limiter accepts any non-zero `NonZeroU64`. We pick 1 KiB/s as the
/// floor so chunks sized at 25 % of the rate exceed the 100 ms
/// minimum-sleep threshold (`MINIMUM_SLEEP_MICROS`) every iteration.
const MIN_RATE_BYTES_PER_SECOND: u64 = 1024;

/// Highest rate exercised by the property tests: 1 GiB/s.
///
/// Bracketing the rate at 1 GiB/s keeps the simulated `requested` sleep
/// totals well below `Duration::MAX` while still covering the high end
/// of realistic LAN throughput.
const MAX_RATE_BYTES_PER_SECOND: u64 = 1024 * 1024 * 1024;

/// Allowed deviation between observed and configured rate after warm-up.
const RATE_TOLERANCE_PERCENT: f64 = 5.0;

/// Number of `register` calls performed during the warm-up phase.
///
/// The warm-up amortises the first-call boundary (no `last_instant`
/// recorded yet) so the measurement window starts in steady state.
const WARMUP_CHUNKS: usize = 2;

/// Number of measurement chunks after warm-up.
///
/// Each chunk is sized so a single registration crosses the 100 ms
/// minimum-sleep threshold, producing exactly one sleep per call. Eight
/// chunks therefore amortise the systematic offset (a few microseconds
/// of wall-clock dt per call) to well under the 5 % tolerance.
const MEASURED_CHUNKS: usize = 8;

fn nz(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).expect("non-zero value required")
}

/// Drives the limiter through `chunks` registrations of `chunk_bytes`
/// and returns `(total_bytes, total_requested_sleep)`.
fn run_pacing(
    limiter: &mut BandwidthLimiter,
    chunks: usize,
    chunk_bytes: usize,
) -> (u128, Duration) {
    let mut total_bytes: u128 = 0;
    let mut total_sleep = Duration::ZERO;
    for _ in 0..chunks {
        let sleep = limiter.register(chunk_bytes);
        total_bytes = total_bytes.saturating_add(chunk_bytes as u128);
        total_sleep = total_sleep.saturating_add(sleep.requested());
    }
    (total_bytes, total_sleep)
}

/// Returns the observed steady-state rate in bytes per second.
///
/// Computed from the `requested` sleep durations to avoid wall-clock
/// jitter while still exercising the same arithmetic that
/// `sleep_for_bwlimit()` uses to derive `sleep_usec` upstream.
fn observed_rate(total_bytes: u128, total_sleep: Duration) -> f64 {
    let seconds = total_sleep.as_secs_f64();
    if seconds <= f64::EPSILON {
        return f64::INFINITY;
    }
    total_bytes as f64 / seconds
}

proptest! {
    // Case count is intentionally low: under the `test-support`
    // feature the limiter performs real `std::thread::sleep` calls
    // for the recorded chunks (the recorder coexists with the sleep,
    // mirroring upstream rsync's `select(2)`-based pacing). Each
    // proptest case can therefore wall-clock-sleep for hundreds of
    // milliseconds. Sixteen cases give meaningful coverage of the
    // rate range while keeping the total runtime under a minute.
    #![proptest_config(ProptestConfig {
        cases: 16,
        ..ProptestConfig::default()
    })]

    /// For any rate in the supported range the long-run throughput
    /// reported by the limiter stays within +/-5 % of the configured
    /// byte-per-second rate after warm-up.
    ///
    /// Chunks are sized to `rate / 8` bytes so each registration
    /// produces exactly one sleep of ~125 ms (just above the 100 ms
    /// minimum-sleep threshold). This isolates the rate-accuracy
    /// property from threshold-induced lumpiness while keeping the
    /// per-test wall-clock duration bounded.
    #[test]
    fn rate_convergence_within_tolerance(
        rate in MIN_RATE_BYTES_PER_SECOND..=MAX_RATE_BYTES_PER_SECOND,
    ) {
        let mut session = recorded_sleep_session();
        session.clear();

        // chunk_bytes = rate / 8 -> sleep_us = 125_000 (just above the
        // 100 ms threshold, regardless of the configured rate). Clamp
        // at 1 byte so the smallest rates still register progress.
        let chunk_bytes = ((rate / 8).max(1)) as usize;

        let mut limiter = BandwidthLimiter::new(nz(rate));

        // Warm-up to amortise the first-call boundary effects.
        let _ = run_pacing(&mut limiter, WARMUP_CHUNKS, chunk_bytes);

        let (total_bytes, total_sleep) = run_pacing(
            &mut limiter,
            MEASURED_CHUNKS,
            chunk_bytes,
        );

        prop_assume!(total_sleep > Duration::ZERO);

        let observed = observed_rate(total_bytes, total_sleep);
        let configured = rate as f64;
        let deviation = (observed - configured).abs() / configured * 100.0;

        prop_assert!(
            deviation <= RATE_TOLERANCE_PERCENT,
            "rate {rate} B/s, chunk {chunk_bytes} B: observed {observed:.2} B/s \
             deviates {deviation:.3}% (tolerance {RATE_TOLERANCE_PERCENT}%)",
        );
    }

    /// A single `register(bytes)` call after construction can never
    /// request a sleep longer than `min(bytes, burst) / rate` seconds.
    ///
    /// Upstream's `sleep_for_bwlimit()` clamps `total_written` against
    /// any configured burst so the steady-state debt is bounded.
    /// Inputs are constrained so the worst-case `requested` sleep
    /// (`burst / rate`) stays below 1 s of wall-clock time per case;
    /// the limiter actually sleeps for the recorded duration under the
    /// `test-support` feature.
    #[test]
    fn burst_capacity_caps_single_registration(
        rate in MIN_RATE_BYTES_PER_SECOND..=MAX_RATE_BYTES_PER_SECOND,
        // Burst is capped at rate/4 so the worst-case sleep is 250 ms.
        burst_fraction in 4u64..=64u64,
        // registration_multiple in [1, 8] -> register {1x..8x} burst.
        registration_multiple in 1u64..=8u64,
    ) {
        let mut session = recorded_sleep_session();
        session.clear();

        let burst = (rate / burst_fraction).max(MIN_RATE_BYTES_PER_SECOND);
        let registration_bytes = burst.saturating_mul(registration_multiple);
        let mut limiter = BandwidthLimiter::with_burst(nz(rate), Some(nz(burst)));
        let sleep = limiter.register(registration_bytes as usize);

        let cap_bytes = registration_bytes.min(burst) as u128;
        // expected_us = cap_bytes * 1_000_000 / rate, with one-microsecond
        // ceiling tolerance for the integer division upstream performs.
        let expected_us = cap_bytes
            .saturating_mul(1_000_000)
            .saturating_div(u128::from(rate))
            .saturating_add(1);
        let cap = Duration::from_micros(expected_us.min(u128::from(u64::MAX)) as u64);

        prop_assert!(
            sleep.requested() <= cap,
            "rate {rate}, burst {burst}, register {registration_bytes}: \
             requested {:?} exceeds cap {cap:?}",
            sleep.requested(),
        );
    }

    /// With a small burst, repeated saturating writes must keep each
    /// individual sleep bounded by `burst / rate` because the debt is
    /// re-clamped after every `register` call.
    ///
    /// `iterations` is intentionally small to keep wall-clock test
    /// runtime bounded - the limiter actually sleeps on the recorded
    /// chunks under the `test-support` feature, so we trade case count
    /// for iteration count and rely on proptest to randomise rates.
    #[test]
    fn burst_clamps_steady_state_sleep(
        rate in 1024u64..=131_072u64,
        burst_fraction in 4u64..=8u64,
    ) {
        let mut session = recorded_sleep_session();
        session.clear();

        // Burst is between 1/8 and 1/4 of one second of traffic at the
        // configured rate, ensuring the requested sleep stays above the
        // 100 ms minimum-sleep threshold (so the clamp engages) while
        // keeping per-iteration wall-clock time bounded at 250 ms.
        let burst = (rate / burst_fraction).max(MIN_RATE_BYTES_PER_SECOND);
        let mut limiter = BandwidthLimiter::with_burst(nz(rate), Some(nz(burst)));

        let cap_us = u128::from(burst)
            .saturating_mul(1_000_000)
            .saturating_div(u128::from(rate))
            .saturating_add(1);
        let cap = Duration::from_micros(cap_us.min(u128::from(u64::MAX)) as u64);

        for i in 0..4 {
            // Always write at least the burst amount so the clamp engages.
            let sleep = limiter.register((burst * 4) as usize);
            prop_assert!(
                sleep.requested() <= cap,
                "iteration {i}: rate {rate}, burst {burst}: \
                 requested {:?} exceeds cap {cap:?}",
                sleep.requested(),
            );
        }
    }

    /// When the chunk-induced debt stays below the 100 ms minimum sleep
    /// threshold the limiter must report a noop pacing decision. This
    /// matches `sleep_for_bwlimit()` returning early when
    /// `sleep_usec < ONE_SEC / 10`.
    #[test]
    fn below_minimum_threshold_is_noop(
        rate in 10_000_000u64..=MAX_RATE_BYTES_PER_SECOND,
    ) {
        let mut session = recorded_sleep_session();
        session.clear();

        // A single byte at >= 10 MB/s is far below the 100 ms threshold,
        // so the first registration must be a noop.
        let mut limiter = BandwidthLimiter::new(nz(rate));
        let sleep = limiter.register(1);

        prop_assert!(
            sleep.is_noop(),
            "rate {rate}: 1-byte registration should be a noop, got {:?}",
            sleep,
        );
    }
}

/// `parse_bandwidth_argument("0")` must return `Ok(None)`, signalling
/// "unlimited" and preventing any limiter from being constructed.
///
/// Upstream rsync interprets `--bwlimit=0` as the unlimited sentinel
/// in `options.c:2378` (`if (bwlimit < 0) bwlimit = 0`); a zero value
/// disables `sleep_for_bwlimit` calls entirely.
#[test]
fn zero_rate_means_unlimited() {
    let parsed = parse_bandwidth_argument("0").expect("parse succeeds");
    assert!(
        parsed.is_none(),
        "bwlimit=0 must yield None (unlimited), got {parsed:?}",
    );
}

/// Equivalent assertion through the full parser API: any of the
/// supported "zero" spellings (with explicit unit suffix or without)
/// must collapse to `None`.
#[test]
fn zero_rate_with_units_means_unlimited() {
    for spelling in ["0", "0K", "0M", "0KB", "0MiB"] {
        let parsed =
            parse_bandwidth_argument(spelling).unwrap_or_else(|err| panic!("{spelling}: {err}"));
        assert!(
            parsed.is_none(),
            "bwlimit={spelling} must yield None, got {parsed:?}",
        );
    }
}

/// Sanity check: when no limiter is constructed (the `bwlimit=0` path),
/// callers bypass pacing entirely. We model that here by skipping the
/// limiter and confirming the loop completes without consulting the
/// sleep recorder.
#[test]
fn unlimited_path_records_no_sleeps() {
    let mut session = recorded_sleep_session();
    session.clear();

    let parsed = parse_bandwidth_argument("0").expect("parse succeeds");
    assert!(parsed.is_none());

    // Simulate the caller path: with `None`, no limiter exists and no
    // sleep is requested.
    let limiter: Option<BandwidthLimiter> = parsed.map(BandwidthLimiter::new);
    assert!(limiter.is_none());

    assert!(
        session.is_empty(),
        "unlimited path must not record any sleeps",
    );
}
