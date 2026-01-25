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

// ==================== Additional coverage tests for apply_effective_limit ====================

#[test]
fn apply_effective_limit_caps_limit_and_updates_burst_together() {
    // Test the case where both limit_changed and burst_changed are true
    // This ensures lines 119-122 are fully covered
    let initial_limit = NonZeroU64::new(8 * 1024 * 1024).unwrap();
    let initial_burst = NonZeroU64::new(4096).unwrap();
    let mut limiter = Some(BandwidthLimiter::with_burst(
        initial_limit,
        Some(initial_burst),
    ));

    let cap = NonZeroU64::new(2 * 1024 * 1024).unwrap();
    let new_burst = NonZeroU64::new(8192).unwrap();

    let change = apply_effective_limit(&mut limiter, Some(cap), true, Some(new_burst), true);

    let limiter = limiter.expect("limiter should remain active");
    assert_eq!(change, LimiterChange::Updated);
    assert_eq!(limiter.limit_bytes(), cap);
    assert_eq!(limiter.burst_bytes(), Some(new_burst));
}

#[test]
fn apply_effective_limit_limit_changed_burst_unchanged() {
    // Test where only limit changes, burst stays the same
    let initial_limit = NonZeroU64::new(8 * 1024 * 1024).unwrap();
    let initial_burst = NonZeroU64::new(4096).unwrap();
    let mut limiter = Some(BandwidthLimiter::with_burst(
        initial_limit,
        Some(initial_burst),
    ));

    let cap = NonZeroU64::new(2 * 1024 * 1024).unwrap();

    // Don't specify burst, so it should keep the existing burst
    let change = apply_effective_limit(&mut limiter, Some(cap), true, None, false);

    let limiter = limiter.expect("limiter should remain active");
    assert_eq!(change, LimiterChange::Updated);
    assert_eq!(limiter.limit_bytes(), cap);
    assert_eq!(limiter.burst_bytes(), Some(initial_burst)); // Preserved
}

#[test]
fn apply_effective_limit_burst_changed_limit_unchanged() {
    // Test where only burst changes, limit stays the same
    let limit = NonZeroU64::new(4 * 1024 * 1024).unwrap();
    let initial_burst = NonZeroU64::new(2048).unwrap();
    let mut limiter = Some(BandwidthLimiter::with_burst(limit, Some(initial_burst)));

    let new_burst = NonZeroU64::new(8192).unwrap();

    // Specify same limit but different burst
    let change = apply_effective_limit(&mut limiter, Some(limit), true, Some(new_burst), true);

    let limiter = limiter.expect("limiter should remain active");
    assert_eq!(change, LimiterChange::Updated);
    assert_eq!(limiter.limit_bytes(), limit);
    assert_eq!(limiter.burst_bytes(), Some(new_burst));
}

#[test]
fn apply_effective_limit_target_limit_calculation() {
    // Test the min() calculation for target_limit (line 108)
    // When new limit > existing limit, target_limit = existing
    let existing_limit = NonZeroU64::new(1024).unwrap();
    let mut limiter = Some(BandwidthLimiter::new(existing_limit));

    let higher_limit = NonZeroU64::new(8192).unwrap();

    let change = apply_effective_limit(&mut limiter, Some(higher_limit), true, None, false);

    let limiter = limiter.expect("limiter should remain active");
    assert_eq!(change, LimiterChange::Unchanged);
    assert_eq!(limiter.limit_bytes(), existing_limit);
}

#[test]
fn apply_effective_limit_preserves_current_burst_when_not_specified() {
    // Test the target_burst = current_burst path (lines 112-113)
    let limit = NonZeroU64::new(4 * 1024 * 1024).unwrap();
    let existing_burst = NonZeroU64::new(4096).unwrap();
    let mut limiter = Some(BandwidthLimiter::with_burst(limit, Some(existing_burst)));

    let lower_limit = NonZeroU64::new(2 * 1024 * 1024).unwrap();

    // burst_specified = false, so target_burst should be current_burst
    let change = apply_effective_limit(&mut limiter, Some(lower_limit), true, None, false);

    let limiter = limiter.expect("limiter should remain active");
    assert_eq!(change, LimiterChange::Updated);
    assert_eq!(limiter.limit_bytes(), lower_limit);
    assert_eq!(limiter.burst_bytes(), Some(existing_burst)); // Preserved
}

#[test]
fn apply_effective_limit_target_burst_from_burst_param_when_specified() {
    // Test the target_burst = burst path (lines 110-111)
    let limit = NonZeroU64::new(4 * 1024 * 1024).unwrap();
    let existing_burst = NonZeroU64::new(4096).unwrap();
    let mut limiter = Some(BandwidthLimiter::with_burst(limit, Some(existing_burst)));

    let lower_limit = NonZeroU64::new(2 * 1024 * 1024).unwrap();
    let new_burst = NonZeroU64::new(2048).unwrap();

    // burst_specified = true, so target_burst should be the new burst
    let change =
        apply_effective_limit(&mut limiter, Some(lower_limit), true, Some(new_burst), true);

    let limiter = limiter.expect("limiter should remain active");
    assert_eq!(change, LimiterChange::Updated);
    assert_eq!(limiter.limit_bytes(), lower_limit);
    assert_eq!(limiter.burst_bytes(), Some(new_burst)); // New burst
}

#[test]
fn apply_effective_limit_unchanged_when_neither_limit_nor_burst_changes() {
    // Test the case where limit_changed = false AND burst_changed = false
    let limit = NonZeroU64::new(4 * 1024 * 1024).unwrap();
    let burst = NonZeroU64::new(4096).unwrap();
    let mut limiter = Some(BandwidthLimiter::with_burst(limit, Some(burst)));

    // Specify same limit and same burst
    let change = apply_effective_limit(&mut limiter, Some(limit), true, Some(burst), true);

    let limiter = limiter.expect("limiter should remain active");
    assert_eq!(change, LimiterChange::Unchanged);
    assert_eq!(limiter.limit_bytes(), limit);
    assert_eq!(limiter.burst_bytes(), Some(burst));
}

#[test]
fn apply_effective_limit_disabled_when_limit_is_none_and_limiter_exists() {
    // Test line 129-131: when limit is None and limiter exists, return Disabled
    let mut limiter = Some(BandwidthLimiter::new(NonZeroU64::new(1024).unwrap()));

    let change = apply_effective_limit(&mut limiter, None, true, None, false);

    assert_eq!(change, LimiterChange::Disabled);
    assert!(limiter.is_none());
}

#[test]
fn apply_effective_limit_unchanged_when_limit_is_none_and_no_limiter() {
    // Test line 132-133: when limit is None and no limiter exists, return Unchanged
    let mut limiter: Option<BandwidthLimiter> = None;

    let change = apply_effective_limit(&mut limiter, None, true, None, false);

    assert_eq!(change, LimiterChange::Unchanged);
    assert!(limiter.is_none());
}

#[test]
fn apply_effective_limit_burst_only_update_on_existing_limiter() {
    // Test lines 137-144: burst-only update path
    let limit = NonZeroU64::new(4 * 1024 * 1024).unwrap();
    let initial_burst = NonZeroU64::new(2048).unwrap();
    let mut limiter = Some(BandwidthLimiter::with_burst(limit, Some(initial_burst)));

    let new_burst = NonZeroU64::new(4096).unwrap();

    // Only specify burst, not limit
    let change = apply_effective_limit(&mut limiter, None, false, Some(new_burst), true);

    let limiter = limiter.expect("limiter should remain active");
    assert_eq!(change, LimiterChange::Updated);
    assert_eq!(limiter.limit_bytes(), limit);
    assert_eq!(limiter.burst_bytes(), Some(new_burst));
}

#[test]
fn apply_effective_limit_burst_only_no_change_when_same() {
    // Test burst-only path where burst is already the same
    let limit = NonZeroU64::new(4 * 1024 * 1024).unwrap();
    let burst = NonZeroU64::new(2048).unwrap();
    let mut limiter = Some(BandwidthLimiter::with_burst(limit, Some(burst)));

    // Specify the same burst
    let change = apply_effective_limit(&mut limiter, None, false, Some(burst), true);

    let limiter = limiter.expect("limiter should remain active");
    assert_eq!(change, LimiterChange::Unchanged);
    assert_eq!(limiter.burst_bytes(), Some(burst));
}

#[test]
fn apply_effective_limit_burst_only_skipped_when_limiter_absent() {
    // Test burst-only update is skipped when no limiter exists
    let mut limiter: Option<BandwidthLimiter> = None;
    let burst = NonZeroU64::new(2048).unwrap();

    let change = apply_effective_limit(&mut limiter, None, false, Some(burst), true);

    assert_eq!(change, LimiterChange::Unchanged);
    assert!(limiter.is_none());
}

#[test]
fn apply_effective_limit_preserves_burst_when_limit_changes_burst_not_specified() {
    // Test line 113: when burst_specified is false, target_burst = current_burst
    // And we change the limit (not just burst)
    let initial_limit = NonZeroU64::new(8 * 1024 * 1024).unwrap();
    let initial_burst = NonZeroU64::new(4096).unwrap();
    let mut limiter = Some(BandwidthLimiter::with_burst(
        initial_limit,
        Some(initial_burst),
    ));

    let new_limit = NonZeroU64::new(4 * 1024 * 1024).unwrap();

    // Specify limit but not burst - burst should be preserved
    let change = apply_effective_limit(&mut limiter, Some(new_limit), true, None, false);

    let limiter = limiter.expect("limiter should remain active");
    assert_eq!(change, LimiterChange::Updated);
    assert_eq!(limiter.limit_bytes(), new_limit);
    assert_eq!(limiter.burst_bytes(), Some(initial_burst)); // Preserved from current_burst
}

#[test]
fn apply_effective_limit_limit_none_no_existing_limiter_returns_unchanged() {
    // Test lines 132-133: when limit is None (unlimited), limit_specified is true,
    // and there's no existing limiter, return Unchanged
    let mut limiter: Option<BandwidthLimiter> = None;

    let change = apply_effective_limit(&mut limiter, None, true, None, false);

    assert_eq!(change, LimiterChange::Unchanged);
    assert!(limiter.is_none());
}

#[test]
fn apply_effective_limit_keeps_current_burst_when_limit_changes_and_burst_not_specified() {
    // Another test for line 113: target_burst = current_burst path
    // Need: existing limiter with burst, new limit specified, burst not specified
    // And the limit must actually change to trigger the update
    let existing_limit = NonZeroU64::new(10 * 1024 * 1024).unwrap();
    let existing_burst = NonZeroU64::new(8192).unwrap();
    let mut limiter = Some(BandwidthLimiter::with_burst(
        existing_limit,
        Some(existing_burst),
    ));

    let lower_limit = NonZeroU64::new(5 * 1024 * 1024).unwrap();

    // limit_specified=true, burst_specified=false
    let change = apply_effective_limit(&mut limiter, Some(lower_limit), true, None, false);

    // Should have updated limit but preserved burst
    let limiter = limiter.expect("limiter should still exist");
    assert_eq!(change, LimiterChange::Updated);
    assert_eq!(limiter.limit_bytes(), lower_limit);
    assert_eq!(limiter.burst_bytes(), Some(existing_burst));
}

#[test]
fn apply_effective_limit_explicitly_disables_burst_with_limit_change() {
    // Test that we can explicitly set burst to None while also changing limit
    let existing_limit = NonZeroU64::new(10 * 1024 * 1024).unwrap();
    let existing_burst = NonZeroU64::new(8192).unwrap();
    let mut limiter = Some(BandwidthLimiter::with_burst(
        existing_limit,
        Some(existing_burst),
    ));

    let lower_limit = NonZeroU64::new(5 * 1024 * 1024).unwrap();

    // Specify both limit and burst (burst=None with burst_specified=true means "disable burst")
    let change = apply_effective_limit(&mut limiter, Some(lower_limit), true, None, true);

    let limiter = limiter.expect("limiter should still exist");
    assert_eq!(change, LimiterChange::Updated);
    assert_eq!(limiter.limit_bytes(), lower_limit);
    assert_eq!(limiter.burst_bytes(), None); // Burst was explicitly cleared
}
