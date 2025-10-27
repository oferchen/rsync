use super::{
    BandwidthLimiter, MINIMUM_SLEEP_MICROS, apply_effective_limit, duration_from_microseconds,
    recorded_sleep_session, sleep_for,
};
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

#[test]
fn recorded_sleep_session_into_vec_consumes_guard() {
    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(NonZeroU64::new(1024).unwrap());
    limiter.register(2048);

    let recorded = session.into_vec();
    assert!(!recorded.is_empty());

    let mut follow_up = recorded_sleep_session();
    assert!(follow_up.is_empty());
    let _ = follow_up.take();
}

#[test]
fn limiter_update_limit_resets_internal_state() {
    let mut session = recorded_sleep_session();
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
    let mut session = recorded_sleep_session();
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

#[test]
fn limiter_reset_clears_state_and_preserves_configuration() {
    let mut session = recorded_sleep_session();
    session.clear();

    let limit = NonZeroU64::new(1024).unwrap();
    let mut baseline = BandwidthLimiter::new(limit);
    baseline.register(4096);
    let baseline_sleeps = session.take();

    session.clear();

    let mut limiter = BandwidthLimiter::new(limit);
    limiter.register(4096);
    assert!(limiter.accumulated_debt_for_testing() > 0);

    session.clear();

    limiter.reset();
    assert_eq!(limiter.limit_bytes(), limit);
    assert_eq!(limiter.burst_bytes(), None);
    assert_eq!(limiter.accumulated_debt_for_testing(), 0);

    limiter.register(4096);
    let reset_sleeps = session.take();
    assert_eq!(reset_sleeps, baseline_sleeps);
}

#[test]
fn apply_effective_limit_disables_limiter_when_unrestricted() {
    let mut limiter = Some(BandwidthLimiter::new(NonZeroU64::new(1024).unwrap()));

    apply_effective_limit(&mut limiter, None, true, None, false);

    assert!(limiter.is_none());
}

#[test]
fn apply_effective_limit_caps_existing_limit() {
    let mut limiter = Some(BandwidthLimiter::new(
        NonZeroU64::new(8 * 1024 * 1024).unwrap(),
    ));
    let cap = NonZeroU64::new(1024 * 1024).unwrap();

    apply_effective_limit(&mut limiter, Some(cap), true, None, false);

    let limiter = limiter.expect("limiter should remain active");
    assert_eq!(limiter.limit_bytes(), cap);
}

#[test]
fn apply_effective_limit_initialises_limiter_when_absent() {
    let mut limiter = None;
    let cap = NonZeroU64::new(4 * 1024 * 1024).unwrap();

    apply_effective_limit(&mut limiter, Some(cap), true, None, false);

    let limiter = limiter.expect("limiter should be created");
    assert_eq!(limiter.limit_bytes(), cap);
}

#[test]
fn apply_effective_limit_updates_burst_when_specified() {
    let limit = NonZeroU64::new(4 * 1024 * 1024).unwrap();
    let mut limiter = Some(BandwidthLimiter::new(limit));
    let burst = NonZeroU64::new(2048).unwrap();

    apply_effective_limit(&mut limiter, Some(limit), true, Some(burst), true);

    let limiter = limiter.expect("limiter should remain active");
    assert_eq!(limiter.limit_bytes(), limit);
    assert_eq!(limiter.burst_bytes(), Some(burst));
}

#[test]
fn apply_effective_limit_updates_burst_only_when_explicit() {
    let burst = NonZeroU64::new(1024).unwrap();
    let mut limiter = Some(BandwidthLimiter::with_burst(
        NonZeroU64::new(2 * 1024 * 1024).unwrap(),
        Some(burst),
    ));

    let current_limit = NonZeroU64::new(2 * 1024 * 1024).unwrap();

    // Reaffirming the existing limit without marking a burst override keeps the original burst.
    apply_effective_limit(&mut limiter, Some(current_limit), true, None, false);
    assert_eq!(
        limiter
            .as_ref()
            .expect("limiter should remain active")
            .burst_bytes(),
        Some(burst)
    );

    // Explicit overrides update the burst even when the rate remains unchanged.
    let new_burst = NonZeroU64::new(4096).unwrap();
    apply_effective_limit(
        &mut limiter,
        Some(current_limit),
        true,
        Some(new_burst),
        true,
    );
    assert_eq!(
        limiter
            .as_ref()
            .expect("limiter should remain active")
            .burst_bytes(),
        Some(new_burst)
    );

    // Burst-only overrides honour the existing limiter but leave absent limiters untouched.
    apply_effective_limit(&mut limiter, None, false, Some(burst), true);
    assert_eq!(
        limiter
            .as_ref()
            .expect("limiter should remain active")
            .burst_bytes(),
        Some(burst)
    );

    let mut absent: Option<BandwidthLimiter> = None;
    apply_effective_limit(&mut absent, None, false, Some(new_burst), true);
    assert!(absent.is_none());
}

#[test]
fn apply_effective_limit_ignores_unspecified_burst_override() {
    let burst = NonZeroU64::new(4096).unwrap();
    let limit = NonZeroU64::new(4 * 1024 * 1024).unwrap();
    let mut limiter = Some(BandwidthLimiter::with_burst(limit, Some(burst)));

    let replacement_burst = NonZeroU64::new(1024).unwrap();
    apply_effective_limit(
        &mut limiter,
        Some(limit),
        true,
        Some(replacement_burst),
        false,
    );

    assert_eq!(
        limiter
            .as_ref()
            .expect("limiter should remain active")
            .burst_bytes(),
        Some(burst)
    );
}

#[test]
fn apply_effective_limit_ignores_unspecified_burst_when_creating_limiter() {
    let limit = NonZeroU64::new(3 * 1024 * 1024).unwrap();
    let mut limiter = None;
    let replacement_burst = NonZeroU64::new(2048).unwrap();

    apply_effective_limit(
        &mut limiter,
        Some(limit),
        true,
        Some(replacement_burst),
        false,
    );

    let limiter = limiter.expect("limiter should be created");
    assert_eq!(limiter.limit_bytes(), limit);
    assert!(limiter.burst_bytes().is_none());
}

#[test]
fn duration_from_microseconds_returns_zero_for_zero_input() {
    assert_eq!(duration_from_microseconds(0), Duration::ZERO);
}

#[test]
fn duration_from_microseconds_converts_fractional_seconds() {
    let micros = super::MICROS_PER_SECOND + 123;
    let duration = duration_from_microseconds(micros);
    assert_eq!(duration.as_secs(), 1);
    assert_eq!(duration.subsec_nanos(), 123_000);
}

#[test]
fn duration_from_microseconds_handles_u64_max_seconds_with_fraction() {
    let micros = u128::from(u64::MAX)
        .saturating_mul(super::MICROS_PER_SECOND)
        .saturating_add(1);
    let duration = duration_from_microseconds(micros);
    assert_eq!(duration.as_secs(), u64::MAX);
    assert_eq!(duration.subsec_micros(), 1);
}

#[test]
fn duration_from_microseconds_saturates_when_exceeding_supported_range() {
    let micros = super::MAX_REPRESENTABLE_MICROSECONDS.saturating_add(1);
    assert_eq!(duration_from_microseconds(micros), Duration::MAX);
}

#[test]
fn sleep_for_zero_duration_skips_recording() {
    let mut session = recorded_sleep_session();
    session.clear();

    sleep_for(Duration::ZERO);

    assert!(session.is_empty());
    let _ = session.take();
}
