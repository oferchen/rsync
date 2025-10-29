use super::{BandwidthLimiter, MINIMUM_SLEEP_MICROS, recorded_sleep_session};
use std::num::NonZeroU64;
use std::time::Duration;

#[test]
fn limiter_limits_chunk_size_for_slow_rates() {
    let limiter = BandwidthLimiter::new(NonZeroU64::new(1024).unwrap());
    assert_eq!(limiter.recommended_read_size(8192), 512);
    assert_eq!(limiter.recommended_read_size(256), 256);
}

#[test]
fn limiter_supports_sub_kib_per_second_limits() {
    let limiter = BandwidthLimiter::new(NonZeroU64::new(600).unwrap());
    assert_eq!(limiter.recommended_read_size(8192), 512);
    assert_eq!(limiter.recommended_read_size(256), 256);
}

#[test]
fn limiter_write_max_bytes_reflects_effective_limit() {
    let fast = BandwidthLimiter::new(NonZeroU64::new(8 * 1024).unwrap());
    assert_eq!(fast.write_max_bytes(), 1024);

    let slow = BandwidthLimiter::new(NonZeroU64::new(600).unwrap());
    assert_eq!(slow.write_max_bytes(), 512);
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
fn limiter_write_max_bytes_respects_burst_override() {
    let capped = BandwidthLimiter::with_burst(
        NonZeroU64::new(8 * 1024 * 1024).unwrap(),
        Some(NonZeroU64::new(2048).unwrap()),
    );
    assert_eq!(capped.write_max_bytes(), 2048);

    let clamped = BandwidthLimiter::with_burst(
        NonZeroU64::new(8 * 1024 * 1024).unwrap(),
        Some(NonZeroU64::new(128).unwrap()),
    );
    assert_eq!(clamped.write_max_bytes(), 512);
}

#[test]
fn limiter_clamps_small_burst_to_minimum_write_size() {
    let limit = NonZeroU64::new(8 * 1024 * 1024).unwrap();
    let limiter = BandwidthLimiter::with_burst(limit, NonZeroU64::new(128));

    assert_eq!(limiter.recommended_read_size(16), 16);
    assert_eq!(limiter.recommended_read_size(8192), 512);
}

#[test]
fn limiter_records_sleep_for_large_writes() {
    let mut session = recorded_sleep_session();
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
fn limiter_records_precise_sleep_for_single_second() {
    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(NonZeroU64::new(1024).unwrap());
    limiter.register(1024);

    let recorded = session.take();
    assert_eq!(recorded, [Duration::from_secs(1)]);
}

#[test]
fn limiter_accumulates_debt_across_small_writes() {
    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(NonZeroU64::new(1024).unwrap());

    for _ in 0..16 {
        limiter.register(64);
    }

    let recorded = session.take();
    assert!(
        !recorded.is_empty(),
        "expected aggregated debt to trigger a sleep"
    );

    let total = recorded
        .iter()
        .copied()
        .try_fold(Duration::ZERO, |acc, chunk| acc.checked_add(chunk))
        .expect("sum fits within Duration::MAX");
    assert!(
        total >= Duration::from_micros(MINIMUM_SLEEP_MICROS as u64),
        "total sleep {:?} shorter than threshold {:?}",
        total,
        Duration::from_micros(MINIMUM_SLEEP_MICROS as u64)
    );
}

#[test]
fn limiter_clamps_debt_to_configured_burst() {
    let mut session = recorded_sleep_session();
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
