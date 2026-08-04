use super::{BandwidthLimiter, LimiterChange, apply_effective_limit};
use std::num::NonZeroU64;

#[test]
fn apply_effective_limit_disables_limiter_when_unrestricted() {
    let mut limiter = Some(BandwidthLimiter::new(NonZeroU64::new(1024).unwrap()));

    let change = apply_effective_limit(&mut limiter, None, true);

    assert_eq!(change, LimiterChange::Disabled);
    assert!(limiter.is_none());
}

#[test]
fn apply_effective_limit_reports_unchanged_when_already_disabled() {
    let mut limiter: Option<BandwidthLimiter> = None;

    let change = apply_effective_limit(&mut limiter, None, true);

    assert!(limiter.is_none());
    assert_eq!(change, LimiterChange::Unchanged);
}

#[test]
fn apply_effective_limit_ignores_unspecified_limit_argument() {
    let initial = NonZeroU64::new(2048).unwrap();
    let mut limiter = Some(BandwidthLimiter::new(initial));

    let new_limit = NonZeroU64::new(1024).unwrap();
    let change = apply_effective_limit(&mut limiter, Some(new_limit), false);

    let limiter = limiter.expect("limiter remains active when limit is ignored");
    assert_eq!(limiter.limit_bytes(), initial);
    assert_eq!(change, LimiterChange::Unchanged);
}

#[test]
fn apply_effective_limit_caps_existing_limit() {
    let mut limiter = Some(BandwidthLimiter::new(
        NonZeroU64::new(8 * 1024 * 1024).unwrap(),
    ));
    let cap = NonZeroU64::new(1024 * 1024).unwrap();

    let change = apply_effective_limit(&mut limiter, Some(cap), true);

    let limiter = limiter.expect("limiter should remain active");
    assert_eq!(change, LimiterChange::Updated);
    assert_eq!(limiter.limit_bytes(), cap);
}

#[test]
fn apply_effective_limit_initialises_limiter_when_absent() {
    let mut limiter = None;
    let cap = NonZeroU64::new(4 * 1024 * 1024).unwrap();

    let change = apply_effective_limit(&mut limiter, Some(cap), true);

    let limiter = limiter.expect("limiter should be created");
    assert_eq!(change, LimiterChange::Enabled);
    assert_eq!(limiter.limit_bytes(), cap);
}

#[test]
fn apply_effective_limit_does_not_raise_existing_limit() {
    let initial = NonZeroU64::new(1024).unwrap();
    let mut limiter = Some(BandwidthLimiter::new(initial));
    let higher = NonZeroU64::new(8 * 1024).unwrap();

    let change = apply_effective_limit(&mut limiter, Some(higher), true);

    let limiter_ref = limiter
        .as_ref()
        .expect("limiter should remain active when limit increases");
    assert_eq!(limiter_ref.limit_bytes(), initial);
    assert_eq!(change, LimiterChange::Unchanged);
}

#[test]
fn apply_effective_limit_target_limit_calculation() {
    // When the new limit exceeds the existing one, the min() guard keeps
    // target_limit at the existing value.
    let existing_limit = NonZeroU64::new(1024).unwrap();
    let mut limiter = Some(BandwidthLimiter::new(existing_limit));

    let higher_limit = NonZeroU64::new(8192).unwrap();

    let change = apply_effective_limit(&mut limiter, Some(higher_limit), true);

    let limiter = limiter.expect("limiter should remain active");
    assert_eq!(change, LimiterChange::Unchanged);
    assert_eq!(limiter.limit_bytes(), existing_limit);
}

#[test]
fn apply_effective_limit_disabled_when_limit_is_none_and_limiter_exists() {
    let mut limiter = Some(BandwidthLimiter::new(NonZeroU64::new(1024).unwrap()));

    let change = apply_effective_limit(&mut limiter, None, true);

    assert_eq!(change, LimiterChange::Disabled);
    assert!(limiter.is_none());
}

#[test]
fn apply_effective_limit_unchanged_when_limit_is_none_and_no_limiter() {
    let mut limiter: Option<BandwidthLimiter> = None;

    let change = apply_effective_limit(&mut limiter, None, true);

    assert_eq!(change, LimiterChange::Unchanged);
    assert!(limiter.is_none());
}

#[test]
fn apply_effective_limit_limit_none_no_existing_limiter_returns_unchanged() {
    // When limit is None (unlimited), limit_specified is true, and no
    // limiter exists, the result is Unchanged.
    let mut limiter: Option<BandwidthLimiter> = None;

    let change = apply_effective_limit(&mut limiter, None, true);

    assert_eq!(change, LimiterChange::Unchanged);
    assert!(limiter.is_none());
}

#[test]
fn apply_effective_limit_state_transition_none_to_limited() {
    let mut limiter: Option<BandwidthLimiter> = None;
    let limit = NonZeroU64::new(1024).unwrap();

    let change = apply_effective_limit(&mut limiter, Some(limit), true);

    assert_eq!(change, LimiterChange::Enabled);
    assert!(limiter.is_some());
    assert_eq!(limiter.unwrap().limit_bytes(), limit);
}

#[test]
fn apply_effective_limit_state_transition_limited_to_none() {
    let mut limiter = Some(BandwidthLimiter::new(NonZeroU64::new(1024).unwrap()));

    let change = apply_effective_limit(&mut limiter, None, true);

    assert_eq!(change, LimiterChange::Disabled);
    assert!(limiter.is_none());
}

#[test]
fn apply_effective_limit_state_transition_limited_to_limited_same() {
    let limit = NonZeroU64::new(1024).unwrap();
    let mut limiter = Some(BandwidthLimiter::new(limit));

    let change = apply_effective_limit(&mut limiter, Some(limit), true);

    assert_eq!(change, LimiterChange::Unchanged);
    assert!(limiter.is_some());
}

#[test]
fn apply_effective_limit_state_transition_limited_to_limited_lower() {
    let high_limit = NonZeroU64::new(8192).unwrap();
    let low_limit = NonZeroU64::new(1024).unwrap();
    let mut limiter = Some(BandwidthLimiter::new(high_limit));

    let change = apply_effective_limit(&mut limiter, Some(low_limit), true);

    assert_eq!(change, LimiterChange::Updated);
    assert!(limiter.is_some());
    assert_eq!(limiter.unwrap().limit_bytes(), low_limit);
}

#[test]
fn apply_effective_limit_state_transition_limited_to_limited_higher_unchanged() {
    let low_limit = NonZeroU64::new(1024).unwrap();
    let high_limit = NonZeroU64::new(8192).unwrap();
    let mut limiter = Some(BandwidthLimiter::new(low_limit));

    // Trying to set higher limit doesn't increase it
    let change = apply_effective_limit(&mut limiter, Some(high_limit), true);

    assert_eq!(change, LimiterChange::Unchanged);
    assert!(limiter.is_some());
    assert_eq!(limiter.unwrap().limit_bytes(), low_limit);
}

#[test]
fn apply_effective_limit_all_params_none_with_no_limiter() {
    let mut limiter: Option<BandwidthLimiter> = None;

    // Nothing specified
    let change = apply_effective_limit(&mut limiter, None, false);

    assert_eq!(change, LimiterChange::Unchanged);
    assert!(limiter.is_none());
}

#[test]
fn apply_effective_limit_all_params_none_with_existing_limiter() {
    let mut limiter = Some(BandwidthLimiter::new(NonZeroU64::new(1024).unwrap()));

    // Nothing specified - should not affect limiter
    let change = apply_effective_limit(&mut limiter, None, false);

    assert_eq!(change, LimiterChange::Unchanged);
    assert!(limiter.is_some());
}
