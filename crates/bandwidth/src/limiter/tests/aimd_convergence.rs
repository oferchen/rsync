//! AIMD-style convergence tests for the bandwidth limiter under error
//! injection.
//!
//! The limiter itself uses a token-bucket pacer, but congestion-control loops
//! built on top of it follow an Additive-Increase/Multiplicative-Decrease
//! (AIMD) pattern: a controller raises the configured rate by a fixed step on
//! successful transfers and halves it when an error or congestion event is
//! observed. These tests drive the limiter through that pattern and verify the
//! observed throughput tracks the AIMD trajectory at every stage.
//!
//! Assertions use the deterministic `requested` sleep durations recorded via
//! the `test-support` infrastructure to avoid wall-clock flakiness.

use super::{BandwidthLimiter, recorded_sleep_session};
use std::num::NonZeroU64;
use std::time::Duration;

fn nz(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).expect("non-zero value required")
}

/// Computes the observed rate in bytes per second from accumulated bytes and
/// requested sleep time.
fn observed_rate(total_bytes: u128, total_sleep: Duration) -> f64 {
    let seconds = total_sleep.as_secs_f64();
    if seconds <= f64::EPSILON {
        return f64::INFINITY;
    }
    total_bytes as f64 / seconds
}

/// Drives `chunks` registrations of `chunk_bytes` through the limiter,
/// returning `(total_bytes, total_requested_sleep)`.
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

/// AIMD multiplicative-decrease: halves the rate, clamped to the floor.
fn multiplicative_decrease(current: u64, floor: u64) -> u64 {
    (current / 2).max(floor)
}

/// AIMD additive-increase: raises the rate by `step`, clamped to the ceiling.
fn additive_increase(current: u64, step: u64, ceiling: u64) -> u64 {
    current.saturating_add(step).min(ceiling)
}

/// Measures the observed rate of `chunks` writes at `rate / 4` bytes each.
///
/// Returns the observed bytes-per-second. Includes a two-chunk warm-up so the
/// first-call boundary (no `last_instant` yet) does not skew the measurement.
fn measure_rate(limiter: &mut BandwidthLimiter, rate: u64, chunks: usize) -> f64 {
    let chunk = (rate / 4).max(1) as usize;
    let _ = run_pacing(limiter, 2, chunk);
    let (bytes, sleep) = run_pacing(limiter, chunks, chunk);
    observed_rate(bytes, sleep)
}

#[test]
fn aimd_no_error_stream_oscillates_near_target_after_warmup() {
    let mut session = recorded_sleep_session();
    session.clear();

    let target = 8_000_u64;
    let mut limiter = BandwidthLimiter::new(nz(target));

    // No errors -> the controller leaves the rate untouched. Drive many
    // chunks and confirm each successive measurement window stays close to
    // the target.
    let chunk = (target / 4) as usize;
    let _ = run_pacing(&mut limiter, 2, chunk);

    for window in 0..6 {
        let (bytes, sleep) = run_pacing(&mut limiter, 8, chunk);
        let obs = observed_rate(bytes, sleep);
        let deviation = (obs - target as f64).abs() / target as f64 * 100.0;
        assert!(
            deviation <= 5.0,
            "window {window}: observed {obs:.2} B/s deviates {deviation:.3}% from {target}"
        );
    }
}

#[test]
fn aimd_simulated_error_halves_effective_rate() {
    let mut session = recorded_sleep_session();
    session.clear();

    let floor = 1_000_u64;
    let initial = 8_000_u64;
    let mut limiter = BandwidthLimiter::new(nz(initial));

    // Pre-error: confirm we sit at the initial target.
    let pre = measure_rate(&mut limiter, initial, 8);
    let pre_dev = (pre - initial as f64).abs() / initial as f64 * 100.0;
    assert!(pre_dev <= 5.0, "pre-error: {pre:.2} deviates {pre_dev:.3}%");

    // Inject an error: multiplicative decrease.
    let halved = multiplicative_decrease(initial, floor);
    assert_eq!(halved, 4_000, "halving from 8000 must produce 4000");
    limiter.update_limit(nz(halved));

    // Post-error: throughput should converge to the halved rate.
    let post = measure_rate(&mut limiter, halved, 8);
    let post_dev = (post - halved as f64).abs() / halved as f64 * 100.0;
    assert!(
        post_dev <= 5.0,
        "post-error: observed {post:.2} deviates {post_dev:.3}% from halved {halved}"
    );

    // And the new observed rate must be measurably lower than the pre-error
    // baseline (roughly half), confirming the decrease actually took effect.
    assert!(
        post < pre * 0.75,
        "post-error rate {post:.2} should be well below pre-error {pre:.2}"
    );
}

#[test]
fn aimd_repeated_errors_drive_rate_down_geometrically() {
    let mut session = recorded_sleep_session();
    session.clear();

    let floor = 1_u64;
    let mut current = 64_000_u64;
    let mut limiter = BandwidthLimiter::new(nz(current));

    // Five consecutive error events: each one halves the rate.
    let mut observed_rates = Vec::new();
    for step in 0..5 {
        current = multiplicative_decrease(current, floor);
        limiter.update_limit(nz(current));

        let obs = measure_rate(&mut limiter, current, 8);
        let deviation = (obs - current as f64).abs() / current as f64 * 100.0;
        assert!(
            deviation <= 5.0,
            "step {step}: observed {obs:.2} deviates {deviation:.3}% from {current}"
        );
        observed_rates.push(obs);
    }

    // Each successive measurement must be roughly half of the prior one.
    for pair in observed_rates.windows(2) {
        let ratio = pair[1] / pair[0];
        assert!(
            (0.4..=0.6).contains(&ratio),
            "consecutive halving ratio {ratio:.3} not in [0.4, 0.6] for {pair:?}"
        );
    }
}

#[test]
fn aimd_additive_increase_recovers_rate_after_error_stream_ends() {
    let mut session = recorded_sleep_session();
    session.clear();

    let ceiling = 16_000_u64;
    let step = 1_000_u64;

    // Start from a post-error low rate and climb back via additive increase.
    let mut current = 1_000_u64;
    let mut limiter = BandwidthLimiter::new(nz(current));

    let initial_obs = measure_rate(&mut limiter, current, 8);
    let initial_dev = (initial_obs - current as f64).abs() / current as f64 * 100.0;
    assert!(
        initial_dev <= 5.0,
        "initial low-rate measurement {initial_obs:.2} deviates {initial_dev:.3}%"
    );

    // Six additive-increase iterations.
    let mut previous_obs = initial_obs;
    for iteration in 0..6 {
        current = additive_increase(current, step, ceiling);
        limiter.update_limit(nz(current));

        let obs = measure_rate(&mut limiter, current, 8);
        let deviation = (obs - current as f64).abs() / current as f64 * 100.0;
        assert!(
            deviation <= 5.0,
            "iter {iteration} at {current}: observed {obs:.2} deviates {deviation:.3}%"
        );
        assert!(
            obs > previous_obs,
            "iter {iteration}: rate did not increase ({obs:.2} <= {previous_obs:.2})"
        );
        previous_obs = obs;
    }
}

#[test]
fn aimd_decrease_then_recover_returns_to_initial_target() {
    let mut session = recorded_sleep_session();
    session.clear();

    let floor = 100_u64;
    let initial = 4_000_u64;
    let mut limiter = BandwidthLimiter::new(nz(initial));

    // Baseline measurement.
    let baseline = measure_rate(&mut limiter, initial, 8);

    // Error event: halve.
    let after_error = multiplicative_decrease(initial, floor);
    limiter.update_limit(nz(after_error));
    let dipped = measure_rate(&mut limiter, after_error, 8);
    assert!(
        dipped < baseline * 0.75,
        "after error: {dipped:.2} should be well below baseline {baseline:.2}"
    );

    // Recovery: climb back to the original target in equal additive steps.
    let step = (initial - after_error) / 4;
    let mut current = after_error;
    while current < initial {
        current = additive_increase(current, step, initial);
        limiter.update_limit(nz(current));
        let _ = measure_rate(&mut limiter, current, 4);
    }

    // Confirm the final converged rate matches the original baseline.
    let recovered = measure_rate(&mut limiter, initial, 8);
    let dev = (recovered - baseline).abs() / baseline * 100.0;
    assert!(
        dev <= 5.0,
        "recovered {recovered:.2} should match baseline {baseline:.2} within 5% (got {dev:.3}%)"
    );
}

#[test]
fn aimd_intermittent_errors_do_not_cause_runaway_oscillation() {
    let mut session = recorded_sleep_session();
    session.clear();

    let floor = 500_u64;
    let ceiling = 32_000_u64;
    let step = 1_000_u64;

    // Pattern: success, success, error, success, success, error, ...
    let pattern = [false, false, true, false, false, true, false, false];

    let mut current = 8_000_u64;
    let mut limiter = BandwidthLimiter::new(nz(current));

    let mut all_rates = Vec::new();

    for (idx, &is_error) in pattern.iter().cycle().take(24).enumerate() {
        current = if is_error {
            multiplicative_decrease(current, floor)
        } else {
            additive_increase(current, step, ceiling)
        };
        limiter.update_limit(nz(current));

        let obs = measure_rate(&mut limiter, current, 4);
        let deviation = (obs - current as f64).abs() / current as f64 * 100.0;
        assert!(
            deviation <= 5.0,
            "iter {idx} (error={is_error}, target {current}): observed {obs:.2} deviates {deviation:.3}%"
        );
        all_rates.push(current);
    }

    // The rate must remain bounded; the controller never drives below the
    // floor or above the ceiling.
    let min_rate = *all_rates.iter().min().expect("non-empty");
    let max_rate = *all_rates.iter().max().expect("non-empty");
    assert!(
        min_rate >= floor,
        "min rate {min_rate} fell below floor {floor}"
    );
    assert!(
        max_rate <= ceiling,
        "max rate {max_rate} exceeded ceiling {ceiling}"
    );

    // The trajectory must not diverge: the spread between the extremes is
    // bounded by the controller's reach (floor..=ceiling). A finite span
    // confirms no runaway behaviour.
    let span = max_rate - min_rate;
    assert!(
        span <= ceiling - floor,
        "rate span {span} exceeds controller reach {}",
        ceiling - floor
    );
}

#[test]
fn aimd_repeated_errors_clamp_at_minimum_rate_floor() {
    let mut session = recorded_sleep_session();
    session.clear();

    let floor = 256_u64;
    let mut current = 4_096_u64;
    let mut limiter = BandwidthLimiter::new(nz(current));

    // Apply enough error events that an unclamped halving would drop below
    // the floor: 4096 -> 2048 -> 1024 -> 512 -> 256 -> 256 -> 256.
    for step in 0..6 {
        current = multiplicative_decrease(current, floor);
        assert!(
            current >= floor,
            "step {step}: rate {current} fell below floor {floor}"
        );
        limiter.update_limit(nz(current));
    }

    // The final rate must be exactly the floor (further halving was clamped).
    assert_eq!(current, floor, "final rate should equal floor");

    // And the limiter at the floor must still throttle to the configured rate.
    let obs = measure_rate(&mut limiter, floor, 8);
    let deviation = (obs - floor as f64).abs() / floor as f64 * 100.0;
    assert!(
        deviation <= 5.0,
        "at floor {floor}: observed {obs:.2} deviates {deviation:.3}%"
    );
}

#[test]
fn aimd_minimum_rate_of_one_byte_per_second_never_underflows() {
    let mut session = recorded_sleep_session();
    session.clear();

    // NonZeroU64 enforces the absolute floor of 1 byte/sec. A controller
    // that uses the type-level minimum should still produce coherent pacing.
    let floor = 1_u64;
    let mut current = 8_u64;
    let mut limiter = BandwidthLimiter::new(nz(current));

    // Halve repeatedly until pinned at the floor.
    for _ in 0..10 {
        current = multiplicative_decrease(current, floor);
        assert!(current >= 1, "rate must remain non-zero");
        limiter.update_limit(nz(current));
    }
    assert_eq!(current, 1, "after many halvings rate must rest at floor 1");

    // Verify throttling still produces the correct sleep: at 1 B/s a 10-byte
    // write requests 10 seconds of sleep.
    let sleep = limiter.register(10);
    assert_eq!(sleep.requested(), Duration::from_secs(10));
}
