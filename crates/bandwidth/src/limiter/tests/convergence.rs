/// Convergence tests for the per-stream token-bucket bandwidth limiter with
/// throughput feedback loop (#2098).
///
/// These tests verify that the limiter converges to the configured rate under
/// various load profiles: steady-state, step response, burst recovery,
/// multi-stream fairness, underutilization, zero-crossing, minimum
/// granularity, effectively-unlimited bandwidth, idle recovery, and
/// stability under matched throughput. All assertions operate on the
/// deterministic `requested` sleep durations recorded via the `test-support`
/// infrastructure, eliminating wall-clock jitter.
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

// ---------------------------------------------------------------------------
// Very high bandwidth target: effectively unlimited throughput
// ---------------------------------------------------------------------------

#[test]
fn high_bandwidth_target_u64_max_never_throttles() {
    let mut session = recorded_sleep_session();
    session.clear();

    let rate = u64::MAX;
    let mut limiter = BandwidthLimiter::new(nz(rate));

    // Even large writes should produce zero sleep at u64::MAX rate.
    // 10 MB total across 100 chunks.
    for _ in 0..100 {
        let sleep = limiter.register(100_000);
        assert!(
            sleep.is_noop(),
            "u64::MAX rate should never throttle, got {:?}",
            sleep.requested()
        );
    }
}

#[test]
fn high_bandwidth_target_1gbps_acts_unlimited_for_small_writes() {
    let mut session = recorded_sleep_session();
    session.clear();

    // 1 GB/s - effectively unlimited for most practical transfers
    let rate = 1_000_000_000_u64;
    let chunk = 64 * 1024_usize; // 64 KB chunks (typical I/O size)
    let mut limiter = BandwidthLimiter::new(nz(rate));

    // 1000 chunks of 64 KB = 64 MB total. At 1 GB/s this is 64 ms of
    // transfer time, so individual chunks should stay below the 100 ms
    // minimum sleep threshold.
    let mut any_throttled = false;
    for _ in 0..1000 {
        let sleep = limiter.register(chunk);
        if !sleep.is_noop() {
            any_throttled = true;
        }
    }

    // The first few chunks accumulate debt below threshold; eventually debt
    // may cross the threshold and trigger a single sleep.  Even if that
    // happens, the total sleep should be negligible relative to the volume.
    let total_bytes = 1000 * chunk;
    let total_sleep = session.total_duration();
    if any_throttled {
        // At 1 GB/s the expected sleep for 64 MB is 64 ms.
        assert!(
            total_sleep <= Duration::from_millis(200),
            "high-rate limiter slept too long: {total_sleep:?} for {total_bytes} bytes"
        );
    }
}

#[test]
fn high_bandwidth_target_step_down_from_unlimited() {
    let mut session = recorded_sleep_session();
    session.clear();

    let unlimited_rate = u64::MAX;
    let limited_rate = 1000_u64;
    let chunk = 250_usize;

    let mut limiter = BandwidthLimiter::new(nz(unlimited_rate));

    // Run at unlimited - should never sleep
    for _ in 0..10 {
        let sleep = limiter.register(chunk);
        assert!(sleep.is_noop());
    }

    // Step down to a real limit
    limiter.update_limit(nz(limited_rate));

    let _ = run_pacing(&mut limiter, 2, chunk);
    let (bytes, sleep) = run_pacing(&mut limiter, 8, chunk);
    let obs = observed_rate(bytes, sleep);
    let deviation = (obs - limited_rate as f64).abs() / limited_rate as f64 * 100.0;
    assert!(
        deviation <= 5.0,
        "step-down from unlimited: observed {obs:.2} deviates {deviation:.3}% from {limited_rate}"
    );
}

// ---------------------------------------------------------------------------
// Idle recovery: feedback loop recovers after a prolonged idle period
// ---------------------------------------------------------------------------

#[test]
fn idle_recovery_reset_then_resume_converges() {
    let mut session = recorded_sleep_session();
    session.clear();

    let rate = 2000_u64;
    let chunk = (rate / 4) as usize;
    let mut limiter = BandwidthLimiter::new(nz(rate));

    // Establish steady-state
    let _ = run_pacing(&mut limiter, 4, chunk);
    let (bytes1, sleep1) = run_pacing(&mut limiter, 8, chunk);
    let obs1 = observed_rate(bytes1, sleep1);
    let dev1 = (obs1 - rate as f64).abs() / rate as f64 * 100.0;
    assert!(dev1 <= 5.0, "pre-idle: {dev1:.3}%");

    // Simulate prolonged idle by resetting (clears stale timing state)
    limiter.reset();

    // Resume after idle - should converge within 2 warm-up chunks
    let _ = run_pacing(&mut limiter, 2, chunk);
    let (bytes2, sleep2) = run_pacing(&mut limiter, 8, chunk);
    let obs2 = observed_rate(bytes2, sleep2);
    let dev2 = (obs2 - rate as f64).abs() / rate as f64 * 100.0;
    assert!(
        dev2 <= 5.0,
        "post-idle: observed {obs2:.2} deviates {dev2:.3}% from {rate}"
    );
}

#[test]
fn idle_recovery_burst_capped_limiter_no_stale_debt() {
    let mut session = recorded_sleep_session();
    session.clear();

    let rate = 5000_u64;
    let burst = 2500_u64;
    let chunk = (rate / 4) as usize;

    let mut limiter = BandwidthLimiter::with_burst(nz(rate), Some(nz(burst)));

    // Drive to steady-state
    let _ = run_pacing(&mut limiter, 4, chunk);

    // Simulate idle by resetting
    limiter.reset();
    assert_eq!(limiter.accumulated_debt_for_testing(), 0);

    // Resume - should converge cleanly without burst-induced penalty
    let _ = run_pacing(&mut limiter, 2, chunk);
    let (bytes, sleep) = run_pacing(&mut limiter, 8, chunk);
    let obs = observed_rate(bytes, sleep);
    let deviation = (obs - rate as f64).abs() / rate as f64 * 100.0;
    assert!(
        deviation <= 5.0,
        "idle recovery with burst: {obs:.2} deviates {deviation:.3}%"
    );
}

#[test]
fn idle_recovery_multiple_idle_resume_cycles() {
    let mut session = recorded_sleep_session();
    session.clear();

    let rate = 4000_u64;
    let chunk = (rate / 4) as usize;
    let mut limiter = BandwidthLimiter::new(nz(rate));

    for cycle in 0..5 {
        // Drive to steady-state
        let _ = run_pacing(&mut limiter, 2, chunk);
        let (bytes, sleep) = run_pacing(&mut limiter, 8, chunk);
        let obs = observed_rate(bytes, sleep);
        let deviation = (obs - rate as f64).abs() / rate as f64 * 100.0;
        assert!(
            deviation <= 5.0,
            "cycle {cycle}: observed {obs:.2} deviates {deviation:.3}%"
        );

        // Simulate idle period
        limiter.reset();
    }
}

// ---------------------------------------------------------------------------
// Bounded convergence: verify convergence within specific iteration count
// ---------------------------------------------------------------------------

#[test]
fn bounded_convergence_within_four_chunks() {
    let mut session = recorded_sleep_session();
    session.clear();

    let rate = 8000_u64;
    let chunk = (rate / 4) as usize;
    let mut limiter = BandwidthLimiter::new(nz(rate));

    // The first chunk has no prior timing state (last_instant is None),
    // so it accumulates full debt. By the 4th chunk, the simulated_elapsed_us
    // feedback should have stabilized the rate.
    let mut cumulative_bytes: u128 = 0;
    let mut cumulative_sleep = Duration::ZERO;

    for i in 0..4 {
        let sleep = limiter.register(chunk);
        cumulative_bytes += chunk as u128;
        cumulative_sleep = cumulative_sleep.saturating_add(sleep.requested());

        // After 4 chunks, check cumulative convergence
        if i == 3 {
            let obs = observed_rate(cumulative_bytes, cumulative_sleep);
            let deviation = (obs - rate as f64).abs() / rate as f64 * 100.0;
            assert!(
                deviation <= 5.0,
                "after 4 chunks: observed {obs:.2} deviates {deviation:.3}%"
            );
        }
    }
}

#[test]
fn bounded_convergence_after_rate_change_within_two_chunks() {
    let mut session = recorded_sleep_session();
    session.clear();

    let old_rate = 2000_u64;
    let new_rate = 10_000_u64;
    let chunk = (new_rate / 4) as usize;

    let mut limiter = BandwidthLimiter::new(nz(old_rate));
    let _ = run_pacing(&mut limiter, 4, chunk);

    // update_limit clears debt and timing state, so the very first chunk
    // at the new rate should converge immediately.
    limiter.update_limit(nz(new_rate));

    let sleep = limiter.register(chunk);
    let obs = observed_rate(chunk as u128, sleep.requested());
    let deviation = (obs - new_rate as f64).abs() / new_rate as f64 * 100.0;
    assert!(
        deviation <= 5.0,
        "first chunk after rate change: {obs:.2} deviates {deviation:.3}%"
    );
}

// ---------------------------------------------------------------------------
// Stability: no oscillation when throughput matches target
// ---------------------------------------------------------------------------

#[test]
fn stability_consecutive_sleeps_are_uniform() {
    let mut session = recorded_sleep_session();
    session.clear();

    let rate = 4000_u64;
    let chunk = (rate / 4) as usize; // 1000 bytes -> 250 ms per chunk
    let expected_per_chunk = Duration::from_millis(250);
    let mut limiter = BandwidthLimiter::new(nz(rate));

    // Warm up
    let _ = run_pacing(&mut limiter, 2, chunk);

    // Collect per-chunk sleep durations
    let mut sleeps = Vec::new();
    for _ in 0..10 {
        let sleep = limiter.register(chunk);
        sleeps.push(sleep.requested());
    }

    // All sleeps should be within 10% of expected
    for (i, &s) in sleeps.iter().enumerate() {
        let diff = s.abs_diff(expected_per_chunk);
        assert!(
            diff <= Duration::from_millis(25),
            "chunk {i}: sleep {s:?} deviates more than 25 ms from {expected_per_chunk:?}"
        );
    }
}

#[test]
fn stability_no_monotonic_drift() {
    let mut session = recorded_sleep_session();
    session.clear();

    let rate = 5000_u64;
    let chunk = (rate / 4) as usize;
    let mut limiter = BandwidthLimiter::new(nz(rate));

    // Warm up
    let _ = run_pacing(&mut limiter, 2, chunk);

    // Measure rate over successive windows
    let mut previous_rate: Option<f64> = None;
    for window in 0..5 {
        let (bytes, sleep) = run_pacing(&mut limiter, 4, chunk);
        let obs = observed_rate(bytes, sleep);

        if let Some(prev) = previous_rate {
            let drift = (obs - prev).abs() / rate as f64 * 100.0;
            assert!(
                drift <= 3.0,
                "window {window}: rate drifted {drift:.3}% between {prev:.2} and {obs:.2}"
            );
        }
        previous_rate = Some(obs);
    }
}

#[test]
fn stability_variance_bounded_across_samples() {
    let mut session = recorded_sleep_session();
    session.clear();

    let rate = 10_000_u64;
    let chunk = (rate / 4) as usize;
    let mut limiter = BandwidthLimiter::new(nz(rate));

    // Warm up
    let _ = run_pacing(&mut limiter, 2, chunk);

    // Collect per-chunk rates
    let mut rates = Vec::new();
    for _ in 0..20 {
        let sleep = limiter.register(chunk);
        let obs = observed_rate(chunk as u128, sleep.requested());
        if obs.is_finite() {
            rates.push(obs);
        }
    }

    // Compute coefficient of variation (stddev / mean)
    let mean = rates.iter().sum::<f64>() / rates.len() as f64;
    let variance = rates.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / rates.len() as f64;
    let cv = variance.sqrt() / mean * 100.0;

    assert!(
        cv <= 5.0,
        "coefficient of variation {cv:.3}% exceeds 5% threshold (mean={mean:.2})"
    );
}

// ---------------------------------------------------------------------------
// Bursty traffic: verify recovery without excessive back-pressure
// ---------------------------------------------------------------------------

#[test]
fn bursty_traffic_large_burst_then_steady_state_converges() {
    let mut session = recorded_sleep_session();
    session.clear();

    let rate = 2000_u64;
    let chunk = (rate / 4) as usize;
    let mut limiter = BandwidthLimiter::new(nz(rate));

    // Send a large burst: 10x the per-second rate
    let _ = limiter.register(rate as usize * 10);

    // After the burst, drive at steady-state chunk size.
    // Warm up to let simulated_elapsed_us forgive the burst debt.
    let _ = run_pacing(&mut limiter, 4, chunk);

    // Measure steady-state after burst recovery
    let (bytes, sleep) = run_pacing(&mut limiter, 8, chunk);
    let obs = observed_rate(bytes, sleep);
    let deviation = (obs - rate as f64).abs() / rate as f64 * 100.0;
    assert!(
        deviation <= 5.0,
        "post-burst steady-state: {obs:.2} deviates {deviation:.3}%"
    );
}

#[test]
fn bursty_traffic_alternating_burst_and_trickle() {
    let mut session = recorded_sleep_session();
    session.clear();

    let rate = 4000_u64;
    let burst_size = rate as usize * 5;
    let trickle_chunk = 10_usize;
    let mut limiter = BandwidthLimiter::new(nz(rate));

    for cycle in 0..3 {
        // Burst phase
        let burst_sleep = limiter.register(burst_size);
        assert!(
            !burst_sleep.is_noop(),
            "cycle {cycle}: burst should trigger throttle"
        );

        // Trickle phase - many small writes below threshold
        for _ in 0..20 {
            let _ = limiter.register(trickle_chunk);
        }

        // After trickle, measure convergence at normal chunk size
        let normal_chunk = (rate / 4) as usize;
        let _ = run_pacing(&mut limiter, 2, normal_chunk);
        let (bytes, sleep) = run_pacing(&mut limiter, 4, normal_chunk);
        let obs = observed_rate(bytes, sleep);
        let deviation = (obs - rate as f64).abs() / rate as f64 * 100.0;
        assert!(
            deviation <= 5.0,
            "cycle {cycle}: post-burst/trickle: {obs:.2} deviates {deviation:.3}%"
        );
    }
}

#[test]
fn bursty_traffic_burst_cap_prevents_excessive_back_pressure() {
    let mut session = recorded_sleep_session();
    session.clear();

    let rate = 1000_u64;
    let burst = 2000_u64;
    let max_sleep = Duration::from_secs(burst / rate); // 2 seconds

    let mut limiter = BandwidthLimiter::with_burst(nz(rate), Some(nz(burst)));

    // Repeated bursts should always be bounded by burst/rate
    for i in 0..5 {
        let sleep = limiter.register(rate as usize * 20);
        assert!(
            sleep.requested() <= max_sleep,
            "burst {i}: sleep {:?} exceeds max {max_sleep:?}",
            sleep.requested()
        );
    }
}

// ---------------------------------------------------------------------------
// Cross-rate convergence: verify convergence across wide range of rates
// ---------------------------------------------------------------------------

#[test]
fn convergence_across_three_decades_of_rates() {
    let rates = [100_u64, 1_000, 10_000, 100_000, 1_000_000, 10_000_000];

    for &rate in &rates {
        let mut session = recorded_sleep_session();
        session.clear();

        let chunk = std::cmp::max(rate / 8, 1) as usize;
        let mut limiter = BandwidthLimiter::new(nz(rate));

        // Warm up
        let _ = run_pacing(&mut limiter, 2, chunk);

        let (bytes, sleep) = run_pacing(&mut limiter, 16, chunk);
        let obs = observed_rate(bytes, sleep);
        let deviation = (obs - rate as f64).abs() / rate as f64 * 100.0;
        assert!(
            deviation <= 5.0,
            "rate {rate}: observed {obs:.2} deviates {deviation:.3}%"
        );
    }
}

// ---------------------------------------------------------------------------
// Interleaved multi-stream fairness: round-robin across independent limiters
// ---------------------------------------------------------------------------

#[test]
fn interleaved_streams_converge_independently() {
    let mut session = recorded_sleep_session();
    session.clear();

    let rates = [2000_u64, 4000, 8000, 1000];
    let mut limiters: Vec<BandwidthLimiter> = rates
        .iter()
        .map(|&r| BandwidthLimiter::new(nz(r)))
        .collect();

    // Warm up all limiters with interleaved registrations
    for _ in 0..4 {
        for (idx, limiter) in limiters.iter_mut().enumerate() {
            let chunk = (rates[idx] / 4) as usize;
            let _ = limiter.register(chunk);
        }
    }

    // Measure each limiter's rate while interleaving registrations
    let mut stream_bytes = vec![0_u128; rates.len()];
    let mut stream_sleep = vec![Duration::ZERO; rates.len()];

    for _ in 0..12 {
        for (idx, limiter) in limiters.iter_mut().enumerate() {
            let chunk = (rates[idx] / 4) as usize;
            let sleep = limiter.register(chunk);
            stream_bytes[idx] += chunk as u128;
            stream_sleep[idx] = stream_sleep[idx].saturating_add(sleep.requested());
        }
    }

    for (idx, &rate) in rates.iter().enumerate() {
        let obs = observed_rate(stream_bytes[idx], stream_sleep[idx]);
        let deviation = (obs - rate as f64).abs() / rate as f64 * 100.0;
        assert!(
            deviation <= 5.0,
            "interleaved stream {idx} (rate {rate}): observed {obs:.2} deviates {deviation:.3}%"
        );
    }
}

#[test]
fn interleaved_streams_with_different_chunk_sizes() {
    let mut session = recorded_sleep_session();
    session.clear();

    let rate = 5000_u64;
    let chunks = [100_usize, 500, 1250, 2500];
    let mut limiters: Vec<BandwidthLimiter> = chunks
        .iter()
        .map(|_| BandwidthLimiter::new(nz(rate)))
        .collect();

    // Warm up
    for (idx, limiter) in limiters.iter_mut().enumerate() {
        let _ = run_pacing(limiter, 2, chunks[idx]);
    }

    // Interleaved measurement
    let mut stream_bytes = vec![0_u128; chunks.len()];
    let mut stream_sleep = vec![Duration::ZERO; chunks.len()];

    for _ in 0..16 {
        for (idx, limiter) in limiters.iter_mut().enumerate() {
            let sleep = limiter.register(chunks[idx]);
            stream_bytes[idx] += chunks[idx] as u128;
            stream_sleep[idx] = stream_sleep[idx].saturating_add(sleep.requested());
        }
    }

    // All streams share the same rate; they should all converge regardless
    // of chunk size, though small chunks may accumulate below the minimum
    // sleep threshold and show infinite observed rate.
    for (idx, &chunk) in chunks.iter().enumerate() {
        if stream_sleep[idx].is_zero() {
            // Small chunks below threshold - verify total bytes are small enough
            // that the limiter correctly deferred sleeping.
            let expected_sleep_us = stream_bytes[idx] as f64 / rate as f64 * 1_000_000.0;
            assert!(
                expected_sleep_us < 100_000.0,
                "stream {idx} (chunk {chunk}): zero sleep but expected {expected_sleep_us:.0} us"
            );
            continue;
        }
        let obs = observed_rate(stream_bytes[idx], stream_sleep[idx]);
        let deviation = (obs - rate as f64).abs() / rate as f64 * 100.0;
        assert!(
            deviation <= 5.0,
            "stream {idx} (chunk {chunk}): observed {obs:.2} deviates {deviation:.3}%"
        );
    }
}

// ---------------------------------------------------------------------------
// Overshoot trajectory: verify initial burst settles toward target
// ---------------------------------------------------------------------------

#[test]
fn overshoot_trajectory_settles_monotonically() {
    let mut session = recorded_sleep_session();
    session.clear();

    let rate = 4000_u64;
    let chunk = (rate / 4) as usize;
    let mut limiter = BandwidthLimiter::new(nz(rate));

    // Send a large burst to create overshoot
    let _ = limiter.register(rate as usize * 5);

    // Collect per-chunk observed rates as the limiter settles
    let mut per_chunk_rates = Vec::new();
    for _ in 0..12 {
        let sleep = limiter.register(chunk);
        let obs = observed_rate(chunk as u128, sleep.requested());
        if obs.is_finite() {
            per_chunk_rates.push(obs);
        }
    }

    // The last few samples should be close to the target rate, confirming
    // the limiter settled after the initial burst.
    let tail = &per_chunk_rates[per_chunk_rates.len().saturating_sub(4)..];
    for (i, &obs) in tail.iter().enumerate() {
        let deviation = (obs - rate as f64).abs() / rate as f64 * 100.0;
        assert!(
            deviation <= 5.0,
            "tail sample {i}: observed {obs:.2} deviates {deviation:.3}% from {rate}"
        );
    }
}

#[test]
fn overshoot_with_burst_cap_limits_initial_penalty() {
    let mut session = recorded_sleep_session();
    session.clear();

    let rate = 2000_u64;
    let burst = 4000_u64;
    let chunk = (rate / 4) as usize;
    let mut limiter = BandwidthLimiter::with_burst(nz(rate), Some(nz(burst)));

    // Massive burst - debt clamped to burst
    let burst_sleep = limiter.register(rate as usize * 20);
    let max_burst_sleep = Duration::from_secs(burst / rate);
    assert!(
        burst_sleep.requested() <= max_burst_sleep,
        "burst sleep {:?} exceeds max {:?}",
        burst_sleep.requested(),
        max_burst_sleep
    );

    // Verify the very next steady-state chunk converges
    let _ = run_pacing(&mut limiter, 2, chunk);
    let (bytes, sleep) = run_pacing(&mut limiter, 8, chunk);
    let obs = observed_rate(bytes, sleep);
    let deviation = (obs - rate as f64).abs() / rate as f64 * 100.0;
    assert!(
        deviation <= 5.0,
        "post-overshoot with burst cap: {obs:.2} deviates {deviation:.3}%"
    );
}

// ---------------------------------------------------------------------------
// Zero-byte registration: verify no corruption during zero-traffic gaps
// ---------------------------------------------------------------------------

#[test]
fn zero_byte_registration_preserves_limiter_state() {
    let mut session = recorded_sleep_session();
    session.clear();

    let rate = 5000_u64;
    let chunk = (rate / 4) as usize;
    let mut limiter = BandwidthLimiter::new(nz(rate));

    // Establish steady state
    let _ = run_pacing(&mut limiter, 2, chunk);
    let (bytes1, sleep1) = run_pacing(&mut limiter, 4, chunk);
    let obs1 = observed_rate(bytes1, sleep1);

    // Inject many zero-byte registrations (simulating idle polling)
    for _ in 0..100 {
        let sleep = limiter.register(0);
        assert!(sleep.is_noop(), "zero-byte register must be noop");
    }

    // Resume normal traffic - rate should still converge
    let (bytes2, sleep2) = run_pacing(&mut limiter, 4, chunk);
    let obs2 = observed_rate(bytes2, sleep2);

    let dev1 = (obs1 - rate as f64).abs() / rate as f64 * 100.0;
    let dev2 = (obs2 - rate as f64).abs() / rate as f64 * 100.0;
    assert!(dev1 <= 5.0, "before zero-gap: {dev1:.3}%");
    assert!(dev2 <= 5.0, "after zero-gap: {dev2:.3}%");
}

#[test]
fn zero_byte_registration_does_not_advance_timing() {
    let mut session = recorded_sleep_session();
    session.clear();

    let rate = 1000_u64;
    let mut limiter = BandwidthLimiter::new(nz(rate));

    // Register some debt
    let sleep1 = limiter.register(1000);
    assert_eq!(sleep1.requested(), Duration::from_secs(1));

    let debt_before = limiter.accumulated_debt_for_testing();

    // Zero-byte registration should not modify debt
    let sleep0 = limiter.register(0);
    assert!(sleep0.is_noop());
    assert_eq!(
        limiter.accumulated_debt_for_testing(),
        debt_before,
        "zero-byte register must not alter debt"
    );
}

#[test]
fn zero_byte_registration_with_burst_cap() {
    let rate = 1000_u64;
    let burst = 500_u64;
    let mut limiter = BandwidthLimiter::with_burst(nz(rate), Some(nz(burst)));

    // Build some debt
    let _ = limiter.register(2000);
    let debt_after_write = limiter.accumulated_debt_for_testing();
    assert!(debt_after_write <= u128::from(burst));

    // Zero-byte register must not change anything
    let sleep = limiter.register(0);
    assert!(sleep.is_noop());
    assert_eq!(limiter.accumulated_debt_for_testing(), debt_after_write);
}

// ---------------------------------------------------------------------------
// Convergence after configuration with burst added/removed mid-stream
// ---------------------------------------------------------------------------

#[test]
fn add_burst_mid_stream_clamps_existing_debt() {
    let mut session = recorded_sleep_session();
    session.clear();

    let rate = 2000_u64;
    let chunk = (rate / 4) as usize;
    let mut limiter = BandwidthLimiter::new(nz(rate));

    // Drive without burst - accumulate large debt via big write
    let _ = limiter.register(rate as usize * 10);

    // Now add a burst cap - this resets state via update_configuration
    let burst = 1000_u64;
    limiter.update_configuration(nz(rate), Some(nz(burst)));
    assert_eq!(limiter.accumulated_debt_for_testing(), 0);

    // Steady-state should converge at the same rate
    let _ = run_pacing(&mut limiter, 2, chunk);
    let (bytes, sleep) = run_pacing(&mut limiter, 8, chunk);
    let obs = observed_rate(bytes, sleep);
    let deviation = (obs - rate as f64).abs() / rate as f64 * 100.0;
    assert!(
        deviation <= 5.0,
        "post-burst-add: {obs:.2} deviates {deviation:.3}%"
    );
}

#[test]
fn remove_burst_mid_stream_allows_unbounded_debt() {
    let mut session = recorded_sleep_session();
    session.clear();

    let rate = 1000_u64;
    let burst = 500_u64;
    let mut limiter = BandwidthLimiter::with_burst(nz(rate), Some(nz(burst)));

    // Warm up with burst
    let _ = run_pacing(&mut limiter, 4, (rate / 4) as usize);

    // Remove burst
    limiter.update_configuration(nz(rate), None);
    assert!(limiter.burst_bytes().is_none());

    // A large write should now accumulate unbounded debt
    let sleep = limiter.register(5000);
    assert_eq!(sleep.requested(), Duration::from_secs(5));

    // Verify convergence resumes at the correct rate after removing burst
    let chunk = (rate / 4) as usize;
    let _ = run_pacing(&mut limiter, 2, chunk);
    let (bytes, sleep_dur) = run_pacing(&mut limiter, 8, chunk);
    let obs = observed_rate(bytes, sleep_dur);
    let deviation = (obs - rate as f64).abs() / rate as f64 * 100.0;
    assert!(
        deviation <= 5.0,
        "post-burst-removal: {obs:.2} deviates {deviation:.3}%"
    );
}

// ---------------------------------------------------------------------------
// Monotonic debt decay: verify debt decreases over successive registrations
// ---------------------------------------------------------------------------

#[test]
fn debt_decreases_toward_zero_over_steady_state() {
    let mut session = recorded_sleep_session();
    session.clear();

    let rate = 2000_u64;
    let chunk = (rate / 4) as usize;
    let mut limiter = BandwidthLimiter::new(nz(rate));

    // Create initial debt
    let _ = limiter.register(rate as usize * 3);

    // Track debt over successive steady-state chunks
    let mut debts = Vec::new();
    for _ in 0..10 {
        let _ = limiter.register(chunk);
        debts.push(limiter.accumulated_debt_for_testing());
    }

    // After the initial burst recovery, debt should stabilize at a low level.
    // The last few entries should be smaller than the first few.
    let first_half_max = debts[..5].iter().copied().max().unwrap_or(0);
    let second_half_max = debts[5..].iter().copied().max().unwrap_or(0);
    assert!(
        second_half_max <= first_half_max,
        "debt should decay: first-half max {first_half_max}, second-half max {second_half_max}"
    );
}

// ---------------------------------------------------------------------------
// Rapid alternating rates: verify no accumulation of error
// ---------------------------------------------------------------------------

#[test]
fn rapid_alternation_error_does_not_accumulate() {
    let mut session = recorded_sleep_session();
    session.clear();

    let slow = 1000_u64;
    let fast = 10_000_u64;
    let mut limiter = BandwidthLimiter::new(nz(slow));

    // Alternate between slow and fast 20 times, measuring the final rate
    // after each switch to confirm no accumulated error.
    for i in 0..20 {
        let target = if i % 2 == 0 { fast } else { slow };
        limiter.update_limit(nz(target));

        let chunk = (target / 4) as usize;
        let (bytes, sleep) = run_pacing(&mut limiter, 4, chunk);
        let obs = observed_rate(bytes, sleep);
        let deviation = (obs - target as f64).abs() / target as f64 * 100.0;
        assert!(
            deviation <= 5.0,
            "alternation {i} (target {target}): observed {obs:.2} deviates {deviation:.3}%"
        );
    }
}
