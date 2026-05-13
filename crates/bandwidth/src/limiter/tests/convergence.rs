/// Convergence tests for the per-stream token-bucket bandwidth limiter with
/// throughput feedback loop.
///
/// These tests verify that the limiter converges to the configured rate under
/// various load profiles: steady-state, step response, burst recovery,
/// multi-stream fairness, underutilization, zero-crossing, and minimum
/// granularity. All assertions operate on the deterministic `requested` sleep
/// durations recorded via the `test-support` infrastructure, eliminating
/// wall-clock jitter.
use super::{BandwidthLimiter, recorded_sleep_session};
use std::num::NonZeroU64;
use std::time::Duration;

fn nz(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).expect("non-zero value required")
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

/// Computes the observed rate in bytes per second from accumulated bytes
/// and requested sleep time.
fn observed_rate(total_bytes: u128, total_sleep: Duration) -> f64 {
    let seconds = total_sleep.as_secs_f64();
    if seconds <= f64::EPSILON {
        return f64::INFINITY;
    }
    total_bytes as f64 / seconds
}

// ---------------------------------------------------------------------------
// Steady-state convergence
// ---------------------------------------------------------------------------

#[test]
fn steady_state_converges_to_target_rate_1kbps() {
    let mut session = recorded_sleep_session();
    session.clear();

    let rate = 1024_u64;
    let chunk_bytes = (rate / 4) as usize; // 256 B -> 250 ms sleep per chunk
    let mut limiter = BandwidthLimiter::new(nz(rate));

    // Warm-up: amortise the first-call boundary (no last_instant yet).
    let _ = run_pacing(&mut limiter, 2, chunk_bytes);

    let (total_bytes, total_sleep) = run_pacing(&mut limiter, 20, chunk_bytes);

    let observed = observed_rate(total_bytes, total_sleep);
    let deviation = (observed - rate as f64).abs() / rate as f64 * 100.0;
    assert!(
        deviation <= 5.0,
        "rate {rate} B/s: observed {observed:.2} B/s deviates {deviation:.3}%"
    );
}

#[test]
fn steady_state_converges_to_target_rate_1mbps() {
    let mut session = recorded_sleep_session();
    session.clear();

    let rate = 1_000_000_u64;
    let chunk_bytes = (rate / 8) as usize; // 125 KB -> 125 ms per chunk
    let mut limiter = BandwidthLimiter::new(nz(rate));

    let _ = run_pacing(&mut limiter, 2, chunk_bytes);
    let (total_bytes, total_sleep) = run_pacing(&mut limiter, 16, chunk_bytes);

    let observed = observed_rate(total_bytes, total_sleep);
    let deviation = (observed - rate as f64).abs() / rate as f64 * 100.0;
    assert!(
        deviation <= 5.0,
        "rate {rate} B/s: observed {observed:.2} B/s deviates {deviation:.3}%"
    );
}

#[test]
fn steady_state_converges_to_target_rate_100mbps() {
    let mut session = recorded_sleep_session();
    session.clear();

    let rate = 100_000_000_u64;
    let chunk_bytes = (rate / 8) as usize;
    let mut limiter = BandwidthLimiter::new(nz(rate));

    let _ = run_pacing(&mut limiter, 2, chunk_bytes);
    let (total_bytes, total_sleep) = run_pacing(&mut limiter, 16, chunk_bytes);

    let observed = observed_rate(total_bytes, total_sleep);
    let deviation = (observed - rate as f64).abs() / rate as f64 * 100.0;
    assert!(
        deviation <= 5.0,
        "rate {rate} B/s: observed {observed:.2} B/s deviates {deviation:.3}%"
    );
}

#[test]
fn steady_state_total_sleep_matches_expected_transfer_time() {
    let mut session = recorded_sleep_session();
    session.clear();

    let rate = 10_000_u64;
    let total_data = 100_000_usize;
    let chunk_bytes = (rate / 4) as usize;
    let num_chunks = total_data / chunk_bytes;

    let mut limiter = BandwidthLimiter::new(nz(rate));

    let mut total_requested = Duration::ZERO;
    for _ in 0..num_chunks {
        let sleep = limiter.register(chunk_bytes);
        total_requested = total_requested.saturating_add(sleep.requested());
    }

    // 100 KB at 10 KB/s = 10 seconds
    let expected = Duration::from_secs(10);
    let diff = total_requested.abs_diff(expected);
    assert!(
        diff <= Duration::from_secs(1),
        "total {total_requested:?} should be within 1s of {expected:?}"
    );
}

// ---------------------------------------------------------------------------
// Step response: change the target bandwidth suddenly
// ---------------------------------------------------------------------------

#[test]
fn step_response_increase_converges_within_one_chunk() {
    let mut session = recorded_sleep_session();
    session.clear();

    let slow_rate = 1024_u64;
    let fast_rate = 10_240_u64;
    let chunk = (fast_rate / 4) as usize;

    let mut limiter = BandwidthLimiter::new(nz(slow_rate));

    // Drive at slow rate
    let _ = run_pacing(&mut limiter, 4, chunk);

    // Step up to fast rate (clears debt)
    limiter.update_limit(nz(fast_rate));

    // Very first chunk at the new rate should already match it
    let (bytes, sleep) = run_pacing(&mut limiter, 1, chunk);
    let obs = observed_rate(bytes, sleep);
    let deviation = (obs - fast_rate as f64).abs() / fast_rate as f64 * 100.0;
    assert!(
        deviation <= 5.0,
        "step-up: observed {obs:.2} B/s deviates {deviation:.3}% from {fast_rate}"
    );
}

#[test]
fn step_response_decrease_converges_within_one_chunk() {
    let mut session = recorded_sleep_session();
    session.clear();

    let fast_rate = 10_240_u64;
    let slow_rate = 1024_u64;
    let chunk = (slow_rate / 4) as usize;

    let mut limiter = BandwidthLimiter::new(nz(fast_rate));
    let _ = run_pacing(&mut limiter, 4, chunk);

    // Step down to slow rate (clears debt)
    limiter.update_limit(nz(slow_rate));

    let (bytes, sleep) = run_pacing(&mut limiter, 1, chunk);
    let obs = observed_rate(bytes, sleep);
    let deviation = (obs - slow_rate as f64).abs() / slow_rate as f64 * 100.0;
    assert!(
        deviation <= 5.0,
        "step-down: observed {obs:.2} B/s deviates {deviation:.3}% from {slow_rate}"
    );
}

#[test]
fn step_response_multiple_rate_changes_settle() {
    let mut session = recorded_sleep_session();
    session.clear();

    let rates = [1024_u64, 8192, 2048, 16384, 4096];
    let mut limiter = BandwidthLimiter::new(nz(rates[0]));

    for &rate in &rates {
        limiter.update_limit(nz(rate));

        let chunk = (rate / 4) as usize;
        let _ = run_pacing(&mut limiter, 2, chunk);

        let (bytes, sleep) = run_pacing(&mut limiter, 8, chunk);
        let obs = observed_rate(bytes, sleep);
        let deviation = (obs - rate as f64).abs() / rate as f64 * 100.0;
        assert!(
            deviation <= 5.0,
            "after switching to {rate}: observed {obs:.2} deviates {deviation:.3}%"
        );
    }
}

// ---------------------------------------------------------------------------
// Burst handling: send a burst, verify throttle and recovery
// ---------------------------------------------------------------------------

#[test]
fn burst_capped_limiter_recovers_to_target_rate() {
    let mut session = recorded_sleep_session();
    session.clear();

    let rate = 1000_u64;
    let burst = 500_u64;
    let mut limiter = BandwidthLimiter::with_burst(nz(rate), Some(nz(burst)));

    // Large burst well above the burst cap
    let _ = limiter.register(10_000);

    // Debt must be clamped to burst
    assert!(
        limiter.accumulated_debt_for_testing() <= u128::from(burst),
        "debt {} exceeds burst {burst}",
        limiter.accumulated_debt_for_testing()
    );

    // After burst, subsequent steady-state writes should converge
    let chunk = (rate / 4) as usize;
    let _ = run_pacing(&mut limiter, 2, chunk); // warm-up post-burst
    let (bytes, sleep) = run_pacing(&mut limiter, 8, chunk);
    let obs = observed_rate(bytes, sleep);
    let deviation = (obs - rate as f64).abs() / rate as f64 * 100.0;
    assert!(
        deviation <= 5.0,
        "post-burst: observed {obs:.2} deviates {deviation:.3}% from {rate}"
    );
}

#[test]
fn burst_limits_maximum_sleep_duration() {
    let mut session = recorded_sleep_session();
    session.clear();

    let rate = 100_u64;
    let burst = 500_u64;
    let mut limiter = BandwidthLimiter::with_burst(nz(rate), Some(nz(burst)));

    // Write 10x the burst - debt should be clamped
    let sleep = limiter.register(5000);

    // Maximum sleep: burst / rate = 500 / 100 = 5 seconds
    assert!(
        sleep.requested() <= Duration::from_secs(5),
        "sleep {:?} exceeds max burst/rate = 5s",
        sleep.requested()
    );
}

#[test]
fn burst_repeated_large_writes_stay_clamped() {
    let mut session = recorded_sleep_session();
    session.clear();

    let rate = 500_u64;
    let burst = 1000_u64;
    let max_sleep = Duration::from_secs(burst / rate);

    let mut limiter = BandwidthLimiter::with_burst(nz(rate), Some(nz(burst)));

    for i in 0..10 {
        let sleep = limiter.register(5000);
        assert!(
            sleep.requested() <= max_sleep,
            "iteration {i}: sleep {:?} exceeds max {max_sleep:?}",
            sleep.requested()
        );
        assert!(
            limiter.accumulated_debt_for_testing() <= u128::from(burst),
            "iteration {i}: debt {} exceeds burst {burst}",
            limiter.accumulated_debt_for_testing()
        );
    }
}

// ---------------------------------------------------------------------------
// Multi-stream fairness: multiple independent limiters sharing a cap
// ---------------------------------------------------------------------------

#[test]
fn multi_stream_independent_limiters_converge_individually() {
    let mut session = recorded_sleep_session();
    session.clear();

    let per_stream_rate = 1000_u64;
    let num_streams = 4;
    let chunk = (per_stream_rate / 4) as usize;

    let mut limiters: Vec<BandwidthLimiter> = (0..num_streams)
        .map(|_| BandwidthLimiter::new(nz(per_stream_rate)))
        .collect();

    // Warm-up
    for limiter in &mut limiters {
        let _ = run_pacing(limiter, 2, chunk);
    }

    // Measure each stream independently
    for (idx, limiter) in limiters.iter_mut().enumerate() {
        let (bytes, sleep) = run_pacing(limiter, 8, chunk);
        let obs = observed_rate(bytes, sleep);
        let deviation = (obs - per_stream_rate as f64).abs() / per_stream_rate as f64 * 100.0;
        assert!(
            deviation <= 5.0,
            "stream {idx}: observed {obs:.2} deviates {deviation:.3}% from {per_stream_rate}"
        );
    }
}

#[test]
fn multi_stream_equal_shares_of_aggregate_cap() {
    let mut session = recorded_sleep_session();
    session.clear();

    let total_cap = 4000_u64;
    let num_streams = 4_u64;
    let per_stream = total_cap / num_streams;
    let chunk = (per_stream / 4) as usize;

    let mut limiters: Vec<BandwidthLimiter> = (0..num_streams)
        .map(|_| BandwidthLimiter::new(nz(per_stream)))
        .collect();

    for limiter in &mut limiters {
        let _ = run_pacing(limiter, 2, chunk);
    }

    let mut total_sleep = Duration::ZERO;
    let mut total_bytes: u128 = 0;

    for limiter in &mut limiters {
        let (bytes, sleep) = run_pacing(limiter, 8, chunk);
        total_bytes = total_bytes.saturating_add(bytes);
        total_sleep = total_sleep.saturating_add(sleep);
    }

    // Each stream independently limits to per_stream rate.
    // Aggregate observed rate should be close to total_cap.
    // Since streams run sequentially in the test, we check each stream
    // gets its fair share of the bandwidth.
    for limiter in &mut limiters {
        let (bytes, sleep) = run_pacing(limiter, 4, chunk);
        let obs = observed_rate(bytes, sleep);
        let deviation = (obs - per_stream as f64).abs() / per_stream as f64 * 100.0;
        assert!(
            deviation <= 5.0,
            "fair share: observed {obs:.2} deviates {deviation:.3}% from {per_stream}"
        );
    }
}

// ---------------------------------------------------------------------------
// Feedback accuracy: measured throughput matches actual throughput
// ---------------------------------------------------------------------------

#[test]
fn feedback_accuracy_requested_sleep_matches_expected() {
    let mut session = recorded_sleep_session();
    session.clear();

    let rate = 5000_u64;
    let chunk = 1000_usize;
    let mut limiter = BandwidthLimiter::new(nz(rate));

    // Each 1000-byte write at 5000 B/s should request 200 ms
    let sleep = limiter.register(chunk);
    assert_eq!(sleep.requested(), Duration::from_millis(200));
}

#[test]
fn feedback_accuracy_cumulative_matches_transfer_time() {
    let mut session = recorded_sleep_session();
    session.clear();

    let rate = 2000_u64;
    let chunk = 500_usize;
    let num_chunks = 20;
    let mut limiter = BandwidthLimiter::new(nz(rate));

    let (total_bytes, total_sleep) = run_pacing(&mut limiter, num_chunks, chunk);

    // 10000 bytes at 2000 B/s = 5 seconds
    let expected_secs = total_bytes as f64 / rate as f64;
    let actual_secs = total_sleep.as_secs_f64();
    let error = (actual_secs - expected_secs).abs() / expected_secs * 100.0;
    assert!(
        error <= 5.0,
        "cumulative sleep {actual_secs:.3}s vs expected {expected_secs:.3}s: error {error:.3}%"
    );
}

#[test]
fn feedback_accuracy_fractional_byte_rate() {
    let mut session = recorded_sleep_session();
    session.clear();

    // 333 B/s - not a clean divisor of common chunk sizes
    let rate = 333_u64;
    let chunk = 333_usize;
    let mut limiter = BandwidthLimiter::new(nz(rate));

    let sleep = limiter.register(chunk);
    // 333 / 333 = 1.0 seconds
    assert_eq!(sleep.requested(), Duration::from_secs(1));
}

// ---------------------------------------------------------------------------
// Underutilization: actual throughput below cap - no unnecessary throttling
// ---------------------------------------------------------------------------

#[test]
fn underutilization_sub_threshold_writes_are_noop() {
    let mut session = recorded_sleep_session();
    session.clear();

    // 10 MB/s rate, writing 1 byte at a time
    let rate = 10_000_000_u64;
    let mut limiter = BandwidthLimiter::new(nz(rate));

    // 1 byte at 10 MB/s = 0.1 us, well below 100 ms minimum threshold
    for _ in 0..100 {
        let sleep = limiter.register(1);
        assert!(
            sleep.is_noop(),
            "single byte at high rate should be noop, got {:?}",
            sleep.requested()
        );
    }
}

#[test]
fn underutilization_slow_sender_never_throttled() {
    let mut session = recorded_sleep_session();
    session.clear();

    // 1 MB/s rate
    let rate = 1_000_000_u64;
    let mut limiter = BandwidthLimiter::new(nz(rate));

    // Simulate slow sender: only 1 KB per registration, well below capacity.
    // At 1 MB/s, 1 KB = 1 ms of debt, below 100 ms threshold.
    for _ in 0..50 {
        let sleep = limiter.register(1000);
        assert!(
            sleep.is_noop(),
            "slow sender should not be throttled, got {:?}",
            sleep.requested()
        );
    }
}

#[test]
fn underutilization_exactly_at_threshold_triggers_sleep() {
    let mut session = recorded_sleep_session();
    session.clear();

    let rate = 1_000_000_u64;
    let mut limiter = BandwidthLimiter::new(nz(rate));

    // 100_000 bytes at 1 MB/s = exactly 100 ms = MINIMUM_SLEEP_MICROS
    let sleep = limiter.register(100_000);
    assert!(
        !sleep.is_noop(),
        "exactly at threshold should trigger sleep"
    );
    assert_eq!(sleep.requested(), Duration::from_millis(100));
}

#[test]
fn underutilization_just_below_threshold_is_noop() {
    let mut session = recorded_sleep_session();
    session.clear();

    let rate = 1_000_000_u64;
    let mut limiter = BandwidthLimiter::new(nz(rate));

    // 99_999 bytes at 1 MB/s = 99.999 ms, below 100 ms threshold
    let sleep = limiter.register(99_999);
    assert!(
        sleep.is_noop(),
        "just below threshold should be noop, got {:?}",
        sleep.requested()
    );
}

// ---------------------------------------------------------------------------
// Zero-crossing: target bandwidth set to zero (pause) and back
// ---------------------------------------------------------------------------

#[test]
fn zero_crossing_reset_simulates_pause_and_resume() {
    let mut session = recorded_sleep_session();
    session.clear();

    let rate = 1000_u64;
    let chunk = 250_usize;
    let mut limiter = BandwidthLimiter::new(nz(rate));

    // Run at normal rate
    let (bytes1, sleep1) = run_pacing(&mut limiter, 4, chunk);
    let obs1 = observed_rate(bytes1, sleep1);

    // Simulate pause by resetting state (limiter always has a NonZero rate;
    // the caller pauses by not calling register)
    limiter.reset();

    // Resume - should start clean, no stale debt from before the pause
    assert_eq!(limiter.accumulated_debt_for_testing(), 0);

    let (bytes2, sleep2) = run_pacing(&mut limiter, 4, chunk);
    let obs2 = observed_rate(bytes2, sleep2);

    // Both windows should converge to the target rate
    let dev1 = (obs1 - rate as f64).abs() / rate as f64 * 100.0;
    let dev2 = (obs2 - rate as f64).abs() / rate as f64 * 100.0;
    assert!(dev1 <= 5.0, "pre-pause: {dev1:.3}%");
    assert!(dev2 <= 5.0, "post-pause: {dev2:.3}%");
}

#[test]
fn zero_crossing_update_from_minimum_to_original() {
    let mut session = recorded_sleep_session();
    session.clear();

    let original_rate = 10_000_u64;
    let minimum_rate = 1_u64; // Simulate near-zero bandwidth
    let chunk = (original_rate / 4) as usize;

    let mut limiter = BandwidthLimiter::new(nz(original_rate));
    let _ = run_pacing(&mut limiter, 4, chunk);

    // Switch to minimum rate
    limiter.update_limit(nz(minimum_rate));
    // The update clears debt, so next register starts fresh
    assert_eq!(limiter.accumulated_debt_for_testing(), 0);

    // Switch back to original rate
    limiter.update_limit(nz(original_rate));
    assert_eq!(limiter.accumulated_debt_for_testing(), 0);

    // Should converge back to the original rate immediately
    let (bytes, sleep) = run_pacing(&mut limiter, 8, chunk);
    let obs = observed_rate(bytes, sleep);
    let deviation = (obs - original_rate as f64).abs() / original_rate as f64 * 100.0;
    assert!(
        deviation <= 5.0,
        "post-resume: observed {obs:.2} deviates {deviation:.3}% from {original_rate}"
    );
}

#[test]
fn zero_crossing_reset_preserves_configuration() {
    let rate = 5000_u64;
    let burst = 2000_u64;
    let mut limiter = BandwidthLimiter::with_burst(nz(rate), Some(nz(burst)));

    let _ = limiter.register(10_000);
    limiter.reset();

    assert_eq!(limiter.limit_bytes().get(), rate);
    assert_eq!(limiter.burst_bytes().unwrap().get(), burst);
    assert_eq!(limiter.accumulated_debt_for_testing(), 0);
}

// ---------------------------------------------------------------------------
// Minimum granularity: very small bandwidth limits
// ---------------------------------------------------------------------------

#[test]
fn minimum_granularity_1_byte_per_second() {
    let mut session = recorded_sleep_session();
    session.clear();

    let rate = 1_u64;
    let mut limiter = BandwidthLimiter::new(nz(rate));

    // 1 byte at 1 B/s = exactly 1 second
    let sleep = limiter.register(1);
    assert_eq!(sleep.requested(), Duration::from_secs(1));

    // 10 bytes at 1 B/s = 10 seconds
    limiter.reset();
    let sleep = limiter.register(10);
    assert_eq!(sleep.requested(), Duration::from_secs(10));
}

#[test]
fn minimum_granularity_1kbps_no_overflow() {
    let mut session = recorded_sleep_session();
    session.clear();

    let rate = 1024_u64;
    let mut limiter = BandwidthLimiter::new(nz(rate));

    // Transfer 10 KB at 1 KB/s = 10 seconds total
    let chunk = 512_usize;
    let num_chunks = 20;
    let (total_bytes, total_sleep) = run_pacing(&mut limiter, num_chunks, chunk);

    let expected_secs = total_bytes as f64 / rate as f64;
    let actual_secs = total_sleep.as_secs_f64();
    let error = (actual_secs - expected_secs).abs() / expected_secs * 100.0;
    assert!(
        error <= 5.0,
        "1 KBps: cumulative {actual_secs:.3}s vs expected {expected_secs:.3}s: error {error:.3}%"
    );
}

#[test]
fn minimum_granularity_single_byte_accumulation() {
    let mut session = recorded_sleep_session();
    session.clear();

    let rate = 10_u64;
    let mut limiter = BandwidthLimiter::new(nz(rate));

    // Write 10 single bytes - should accumulate to 1 second of sleep
    let mut total_requested = Duration::ZERO;
    for _ in 0..10 {
        let sleep = limiter.register(1);
        total_requested = total_requested.saturating_add(sleep.requested());
    }

    // 10 bytes at 10 B/s = 1 second
    let expected = Duration::from_secs(1);
    let diff = total_requested.abs_diff(expected);
    assert!(
        diff <= Duration::from_millis(200),
        "10 bytes at 10 B/s: total {total_requested:?} should be near {expected:?}"
    );
}

#[test]
fn minimum_granularity_no_u128_overflow_at_extreme_values() {
    let mut session = recorded_sleep_session();
    session.clear();

    let rate = 1_u64;
    let mut limiter = BandwidthLimiter::new(nz(rate));

    // Large write at minimum rate - should not panic from overflow
    let sleep = limiter.register(1_000_000);
    assert_eq!(sleep.requested(), Duration::from_secs(1_000_000));
}

#[test]
fn minimum_granularity_with_burst_cap() {
    let mut session = recorded_sleep_session();
    session.clear();

    let rate = 1_u64;
    let burst = 10_u64;
    let mut limiter = BandwidthLimiter::with_burst(nz(rate), Some(nz(burst)));

    // Write 1000 bytes - debt clamped to 10
    let sleep = limiter.register(1000);
    assert_eq!(sleep.requested(), Duration::from_secs(10));
    assert!(limiter.accumulated_debt_for_testing() <= u128::from(burst));
}

// ---------------------------------------------------------------------------
// Combined scenarios
// ---------------------------------------------------------------------------

#[test]
fn realistic_file_transfer_with_rate_change_mid_transfer() {
    let mut session = recorded_sleep_session();
    session.clear();

    let initial_rate = 10_000_u64;
    let boosted_rate = 50_000_u64;
    let mut limiter = BandwidthLimiter::new(nz(initial_rate));

    // First half at slow rate
    let chunk = (initial_rate / 4) as usize;
    let _ = run_pacing(&mut limiter, 2, chunk);
    let (bytes1, sleep1) = run_pacing(&mut limiter, 8, chunk);
    let obs1 = observed_rate(bytes1, sleep1);
    let dev1 = (obs1 - initial_rate as f64).abs() / initial_rate as f64 * 100.0;
    assert!(dev1 <= 5.0, "phase 1: {dev1:.3}%");

    // Boost rate mid-transfer
    limiter.update_limit(nz(boosted_rate));

    // Second half at fast rate
    let fast_chunk = (boosted_rate / 4) as usize;
    let _ = run_pacing(&mut limiter, 2, fast_chunk);
    let (bytes2, sleep2) = run_pacing(&mut limiter, 8, fast_chunk);
    let obs2 = observed_rate(bytes2, sleep2);
    let dev2 = (obs2 - boosted_rate as f64).abs() / boosted_rate as f64 * 100.0;
    assert!(dev2 <= 5.0, "phase 2: {dev2:.3}%");
}

#[test]
fn configuration_change_with_burst_converges_correctly() {
    let mut session = recorded_sleep_session();
    session.clear();

    let rate = 2000_u64;
    let mut limiter = BandwidthLimiter::new(nz(rate));

    // Add burst mid-transfer
    let burst = 1000_u64;
    limiter.update_configuration(nz(rate), Some(nz(burst)));

    // Large write should be clamped
    let sleep = limiter.register(5000);
    assert!(
        sleep.requested() <= Duration::from_millis(500),
        "burst-clamped sleep {:?} exceeds 500 ms",
        sleep.requested()
    );

    // Steady-state after burst add
    let chunk = (rate / 4) as usize;
    let _ = run_pacing(&mut limiter, 2, chunk);
    let (bytes, sleep_dur) = run_pacing(&mut limiter, 8, chunk);
    let obs = observed_rate(bytes, sleep_dur);
    let deviation = (obs - rate as f64).abs() / rate as f64 * 100.0;
    assert!(
        deviation <= 5.0,
        "post-burst-config: observed {obs:.2} deviates {deviation:.3}%"
    );
}

#[test]
fn rapid_rate_oscillation_does_not_diverge() {
    let mut session = recorded_sleep_session();
    session.clear();

    let rates = [1024_u64, 8192, 1024, 8192, 1024];
    let mut limiter = BandwidthLimiter::new(nz(rates[0]));

    for &rate in &rates {
        limiter.update_limit(nz(rate));
        let chunk = (rate / 4) as usize;
        let (bytes, sleep) = run_pacing(&mut limiter, 4, chunk);
        let obs = observed_rate(bytes, sleep);
        let deviation = (obs - rate as f64).abs() / rate as f64 * 100.0;
        assert!(
            deviation <= 5.0,
            "oscillation at {rate}: observed {obs:.2} deviates {deviation:.3}%"
        );
    }
}

#[test]
fn write_max_adapts_after_rate_change() {
    let slow_rate = 1024_u64;
    let fast_rate = 1024 * 100;

    let mut limiter = BandwidthLimiter::new(nz(slow_rate));
    let slow_wm = limiter.write_max_bytes();

    limiter.update_limit(nz(fast_rate));
    let fast_wm = limiter.write_max_bytes();

    assert!(
        fast_wm > slow_wm,
        "write_max should increase: slow={slow_wm}, fast={fast_wm}"
    );
}

#[test]
fn debt_fully_cleared_after_simulated_sleep() {
    let mut session = recorded_sleep_session();
    session.clear();

    let rate = 1000_u64;
    let mut limiter = BandwidthLimiter::new(nz(rate));

    // First registration creates full debt
    let sleep = limiter.register(1000);
    assert_eq!(sleep.requested(), Duration::from_secs(1));

    // After the simulated sleep, debt should be fully (or nearly) cleared
    // via the simulated_elapsed_us mechanism
    let second_sleep = limiter.register(100);

    // The simulated elapsed time from the first sleep should mostly forgive
    // the second write's debt
    assert!(
        second_sleep.requested() <= Duration::from_millis(200),
        "post-sleep debt should be low, got {:?}",
        second_sleep.requested()
    );
}
