use super::{BandwidthLimiter, LimiterChange, apply_effective_limit};
use std::num::NonZeroU64;

#[test]
fn apply_effective_limit_disables_limiter_when_unrestricted() {
    let mut limiter = Some(BandwidthLimiter::new(NonZeroU64::new(1024).unwrap()));

    let change = apply_effective_limit(&mut limiter, None, true, None, false);

    assert_eq!(change, LimiterChange::Disabled);
    assert!(limiter.is_none());
}

#[test]
fn apply_effective_limit_reports_unchanged_when_already_disabled() {
    let mut limiter: Option<BandwidthLimiter> = None;

    let change = apply_effective_limit(&mut limiter, None, true, None, false);

    assert!(limiter.is_none());
    assert_eq!(change, LimiterChange::Unchanged);
}

#[test]
fn apply_effective_limit_ignores_unspecified_limit_argument() {
    let initial = NonZeroU64::new(2048).unwrap();
    let mut limiter = Some(BandwidthLimiter::new(initial));

    let new_limit = NonZeroU64::new(1024).unwrap();
    let change = apply_effective_limit(&mut limiter, Some(new_limit), false, None, false);

    let limiter = limiter.expect("limiter remains active when limit is ignored");
    assert_eq!(limiter.limit_bytes(), initial);
    assert_eq!(limiter.burst_bytes(), None);
    assert_eq!(change, LimiterChange::Unchanged);
}

#[test]
fn apply_effective_limit_caps_existing_limit() {
    let mut limiter = Some(BandwidthLimiter::new(
        NonZeroU64::new(8 * 1024 * 1024).unwrap(),
    ));
    let cap = NonZeroU64::new(1024 * 1024).unwrap();

    let change = apply_effective_limit(&mut limiter, Some(cap), true, None, false);

    let limiter = limiter.expect("limiter should remain active");
    assert_eq!(change, LimiterChange::Updated);
    assert_eq!(limiter.limit_bytes(), cap);
}

#[test]
fn apply_effective_limit_initialises_limiter_when_absent() {
    let mut limiter = None;
    let cap = NonZeroU64::new(4 * 1024 * 1024).unwrap();

    let change = apply_effective_limit(&mut limiter, Some(cap), true, None, false);

    let limiter = limiter.expect("limiter should be created");
    assert_eq!(change, LimiterChange::Enabled);
    assert_eq!(limiter.limit_bytes(), cap);
}

#[test]
fn apply_effective_limit_initialises_limiter_with_burst() {
    let mut limiter = None;
    let limit = NonZeroU64::new(6 * 1024 * 1024).unwrap();
    let burst = NonZeroU64::new(512 * 1024).unwrap();

    let change = apply_effective_limit(&mut limiter, Some(limit), true, Some(burst), true);

    let limiter = limiter.expect("limiter should be created with burst");
    assert_eq!(change, LimiterChange::Enabled);
    assert_eq!(limiter.limit_bytes(), limit);
    assert_eq!(limiter.burst_bytes(), Some(burst));
}

#[test]
fn apply_effective_limit_updates_burst_when_specified() {
    let limit = NonZeroU64::new(4 * 1024 * 1024).unwrap();
    let mut limiter = Some(BandwidthLimiter::new(limit));
    let burst = NonZeroU64::new(2048).unwrap();

    let change = apply_effective_limit(&mut limiter, Some(limit), true, Some(burst), true);

    let limiter = limiter.expect("limiter should remain active");
    assert_eq!(change, LimiterChange::Updated);
    assert_eq!(limiter.limit_bytes(), limit);
    assert_eq!(limiter.burst_bytes(), Some(burst));
}

#[test]
fn apply_effective_limit_does_not_raise_existing_limit() {
    let initial = NonZeroU64::new(1024).unwrap();
    let mut limiter = Some(BandwidthLimiter::new(initial));
    let higher = NonZeroU64::new(8 * 1024).unwrap();

    let change = apply_effective_limit(&mut limiter, Some(higher), true, None, false);

    let limiter_ref = limiter
        .as_ref()
        .expect("limiter should remain active when limit increases");
    assert_eq!(limiter_ref.limit_bytes(), initial);
    assert_eq!(change, LimiterChange::Unchanged);

    let burst = NonZeroU64::new(4096).unwrap();
    let change = apply_effective_limit(&mut limiter, Some(higher), true, Some(burst), true);

    let limiter_ref = limiter
        .as_ref()
        .expect("limiter should remain active after burst update");
    assert_eq!(limiter_ref.limit_bytes(), initial);
    assert_eq!(limiter_ref.burst_bytes(), Some(burst));
    assert_eq!(change, LimiterChange::Updated);
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
    let change = apply_effective_limit(&mut limiter, Some(current_limit), true, None, false);
    assert_eq!(
        limiter
            .as_ref()
            .expect("limiter should remain active")
            .burst_bytes(),
        Some(burst)
    );
    assert_eq!(change, LimiterChange::Unchanged);

    // Explicit overrides update the burst even when the rate remains unchanged.
    let new_burst = NonZeroU64::new(4096).unwrap();
    let change = apply_effective_limit(
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
    assert_eq!(change, LimiterChange::Updated);

    // Burst-only overrides honour the existing limiter but leave absent limiters untouched.
    let change = apply_effective_limit(&mut limiter, None, false, Some(burst), true);
    assert_eq!(
        limiter
            .as_ref()
            .expect("limiter should remain active")
            .burst_bytes(),
        Some(burst)
    );
    assert_eq!(change, LimiterChange::Updated);

    let mut absent: Option<BandwidthLimiter> = None;
    let change = apply_effective_limit(&mut absent, None, false, Some(new_burst), true);
    assert!(absent.is_none());
    assert_eq!(change, LimiterChange::Unchanged);
}

#[test]
fn apply_effective_limit_clears_burst_with_burst_only_override() {
    let limit = NonZeroU64::new(2 * 1024 * 1024).unwrap();
    let burst = NonZeroU64::new(4096).unwrap();
    let mut limiter = Some(BandwidthLimiter::with_burst(limit, Some(burst)));

    let change = apply_effective_limit(&mut limiter, None, false, None, true);

    let limiter = limiter.expect("limiter should remain active");
    assert_eq!(limiter.limit_bytes(), limit);
    assert!(limiter.burst_bytes().is_none());
    assert_eq!(change, LimiterChange::Updated);
}

#[test]
fn apply_effective_limit_ignores_redundant_burst_only_override() {
    let limit = NonZeroU64::new(3 * 1024 * 1024).unwrap();
    let burst = NonZeroU64::new(2048).unwrap();
    let mut limiter = Some(BandwidthLimiter::with_burst(limit, Some(burst)));

    let change = apply_effective_limit(&mut limiter, None, false, Some(burst), true);

    let limiter = limiter.expect("limiter should remain active");
    assert_eq!(limiter.limit_bytes(), limit);
    assert_eq!(limiter.burst_bytes(), Some(burst));
    assert_eq!(change, LimiterChange::Unchanged);
}

#[test]
fn apply_effective_limit_removes_existing_burst_when_disabled() {
    let limit = NonZeroU64::new(2 * 1024 * 1024).unwrap();
    let mut limiter = Some(BandwidthLimiter::with_burst(limit, NonZeroU64::new(8192)));

    let change = apply_effective_limit(&mut limiter, Some(limit), true, None, true);

    let limiter = limiter.expect("limiter should remain active");
    assert_eq!(change, LimiterChange::Updated);
    assert_eq!(limiter.limit_bytes(), limit);
    assert!(limiter.burst_bytes().is_none());
}

#[test]
fn apply_effective_limit_ignores_unspecified_burst_override() {
    let burst = NonZeroU64::new(4096).unwrap();
    let limit = NonZeroU64::new(4 * 1024 * 1024).unwrap();
    let mut limiter = Some(BandwidthLimiter::with_burst(limit, Some(burst)));

    let replacement_burst = NonZeroU64::new(1024).unwrap();
    let change = apply_effective_limit(
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
    assert_eq!(change, LimiterChange::Unchanged);
}

#[test]
fn apply_effective_limit_ignores_unspecified_burst_when_creating_limiter() {
    let limit = NonZeroU64::new(3 * 1024 * 1024).unwrap();
    let mut limiter = None;
    let replacement_burst = NonZeroU64::new(2048).unwrap();

    let change = apply_effective_limit(
        &mut limiter,
        Some(limit),
        true,
        Some(replacement_burst),
        false,
    );

    let limiter = limiter.expect("limiter should be created");
    assert_eq!(limiter.limit_bytes(), limit);
    assert!(limiter.burst_bytes().is_none());
    assert_eq!(change, LimiterChange::Enabled);
}
