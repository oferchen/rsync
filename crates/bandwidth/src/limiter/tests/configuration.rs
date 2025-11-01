use super::{BandwidthLimiter, LimiterChange, MINIMUM_SLEEP_MICROS, recorded_sleep_session};
use std::num::NonZeroU64;
use std::time::Duration;

#[test]
fn limiter_update_limit_resets_internal_state() {
    let mut session = recorded_sleep_session();
    session.clear();

    let new_limit = NonZeroU64::new(8 * 1024 * 1024).unwrap();
    let mut baseline = BandwidthLimiter::new(new_limit);
    let _ = baseline.register(4096);
    let baseline_sleeps = session.take();

    session.clear();

    let mut limiter = BandwidthLimiter::new(NonZeroU64::new(1024).unwrap());
    let _ = limiter.register(4096);
    session.clear();

    limiter.update_limit(new_limit);
    let sleep = limiter.register(4096);
    assert_eq!(limiter.limit_bytes(), new_limit);
    assert_eq!(limiter.recommended_read_size(1 << 20), 1 << 20);

    let updated_sleeps = session.take();
    assert_eq!(updated_sleeps, baseline_sleeps);
    let expected_requested = baseline_sleeps
        .iter()
        .copied()
        .fold(Duration::ZERO, |acc, chunk| acc.saturating_add(chunk));
    assert_eq!(sleep.requested(), expected_requested);
}

#[test]
fn limiter_update_configuration_resets_state_and_updates_burst() {
    let mut session = recorded_sleep_session();
    session.clear();

    let initial_limit = NonZeroU64::new(1024).unwrap();
    let initial_burst = NonZeroU64::new(4096).unwrap();
    let mut limiter = BandwidthLimiter::with_burst(initial_limit, Some(initial_burst));
    let _ = limiter.register(8192);
    assert!(limiter.accumulated_debt_for_testing() > 0);

    let new_limit = NonZeroU64::new(8 * 1024 * 1024).unwrap();
    let new_burst = NonZeroU64::new(2048).unwrap();
    limiter.update_configuration(new_limit, Some(new_burst));

    assert_eq!(limiter.limit_bytes(), new_limit);
    assert_eq!(limiter.burst_bytes(), Some(new_burst));
    assert_eq!(limiter.accumulated_debt_for_testing(), 0);

    session.clear();
    let sleep = limiter.register(1024);
    let recorded = session.take();
    assert!(
        recorded.is_empty()
            || recorded
                .iter()
                .all(|duration| duration.as_micros() <= MINIMUM_SLEEP_MICROS)
    );
    assert!(sleep.requested() <= Duration::from_micros(MINIMUM_SLEEP_MICROS as u64));
}

#[test]
fn limiter_reset_clears_state_and_preserves_configuration() {
    let mut session = recorded_sleep_session();
    session.clear();

    let limit = NonZeroU64::new(1024).unwrap();
    let mut baseline = BandwidthLimiter::new(limit);
    let _ = baseline.register(4096);
    let baseline_sleeps = session.take();

    session.clear();

    let mut limiter = BandwidthLimiter::new(limit);
    let _ = limiter.register(4096);
    assert!(limiter.accumulated_debt_for_testing() > 0);

    session.clear();

    limiter.reset();
    assert_eq!(limiter.limit_bytes(), limit);
    assert_eq!(limiter.burst_bytes(), None);
    assert_eq!(limiter.accumulated_debt_for_testing(), 0);

    let sleep = limiter.register(4096);
    let reset_sleeps = session.take();
    assert_eq!(reset_sleeps, baseline_sleeps);
    let expected_requested = baseline_sleeps
        .iter()
        .copied()
        .fold(Duration::ZERO, |acc, chunk| acc.saturating_add(chunk));
    assert_eq!(sleep.requested(), expected_requested);
}

#[test]
fn limiter_change_helper_methods_reflect_state() {
    assert!(!LimiterChange::Unchanged.is_changed());
    assert!(!LimiterChange::Unchanged.leaves_limiter_active());
    assert!(!LimiterChange::Unchanged.disables_limiter());

    assert!(LimiterChange::Enabled.is_changed());
    assert!(LimiterChange::Enabled.leaves_limiter_active());
    assert!(!LimiterChange::Enabled.disables_limiter());

    assert!(LimiterChange::Updated.is_changed());
    assert!(LimiterChange::Updated.leaves_limiter_active());
    assert!(!LimiterChange::Updated.disables_limiter());

    assert!(LimiterChange::Disabled.is_changed());
    assert!(!LimiterChange::Disabled.leaves_limiter_active());
    assert!(LimiterChange::Disabled.disables_limiter());
}

#[test]
fn limiter_change_combine_prefers_highest_precedence() {
    let cases = [
        (
            LimiterChange::Unchanged,
            LimiterChange::Unchanged,
            LimiterChange::Unchanged,
        ),
        (
            LimiterChange::Unchanged,
            LimiterChange::Updated,
            LimiterChange::Updated,
        ),
        (
            LimiterChange::Updated,
            LimiterChange::Enabled,
            LimiterChange::Enabled,
        ),
        (
            LimiterChange::Enabled,
            LimiterChange::Disabled,
            LimiterChange::Disabled,
        ),
        (
            LimiterChange::Updated,
            LimiterChange::Unchanged,
            LimiterChange::Updated,
        ),
    ];

    for (left, right, expected) in cases {
        assert_eq!(left.combine(right), expected);
        assert_eq!(right.combine(left), expected);
    }
}

#[test]
fn limiter_change_combine_all_matches_folded_combination() {
    let changes = [
        LimiterChange::Unchanged,
        LimiterChange::Updated,
        LimiterChange::Enabled,
        LimiterChange::Disabled,
    ];

    let folded = changes
        .into_iter()
        .fold(LimiterChange::Unchanged, |acc, change| acc.combine(change));
    assert_eq!(LimiterChange::combine_all(changes), folded);

    assert_eq!(
        LimiterChange::combine_all([LimiterChange::Unchanged]),
        LimiterChange::Unchanged
    );
    assert_eq!(
        LimiterChange::combine_all([LimiterChange::Updated, LimiterChange::Enabled]),
        LimiterChange::Enabled
    );
}

#[test]
fn limiter_write_max_enforces_minimum_threshold() {
    let limiter = BandwidthLimiter::new(NonZeroU64::new(128).unwrap());

    assert_eq!(limiter.write_max_bytes(), 512);
    assert_eq!(limiter.recommended_read_size(4096), 512);
}

#[test]
fn limiter_write_max_scales_with_limit() {
    let limit = NonZeroU64::new(128 * 1024).unwrap();
    let limiter = BandwidthLimiter::new(limit);

    assert_eq!(limiter.write_max_bytes(), 16_384);
    assert_eq!(limiter.recommended_read_size(1 << 20), 16_384);
}

#[test]
fn limiter_write_max_uses_burst_override() {
    let limit = NonZeroU64::new(1024 * 1024).unwrap();
    let burst = NonZeroU64::new(2048).unwrap();
    let limiter = BandwidthLimiter::with_burst(limit, Some(burst));

    assert_eq!(limiter.write_max_bytes(), burst.get() as usize);
    assert_eq!(
        limiter.recommended_read_size(usize::MAX),
        burst.get() as usize
    );
}

#[test]
fn limiter_write_max_honours_minimum_when_burst_is_small() {
    let limit = NonZeroU64::new(8 * 1024).unwrap();
    let burst = NonZeroU64::new(128).unwrap();
    let limiter = BandwidthLimiter::with_burst(limit, Some(burst));

    assert_eq!(limiter.write_max_bytes(), 512);
    assert_eq!(limiter.recommended_read_size(1024), 512);
}
