use super::{BandwidthLimiter, LimiterChange, MINIMUM_SLEEP_MICROS, recorded_sleep_session};
use std::cmp::Ordering;
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
fn limiter_change_ordering_tracks_priority() {
    assert_eq!(
        LimiterChange::Unchanged.cmp(&LimiterChange::Updated),
        Ordering::Less
    );
    assert_eq!(
        LimiterChange::Enabled.cmp(&LimiterChange::Disabled),
        Ordering::Less
    );
    assert_eq!(
        LimiterChange::Disabled.cmp(&LimiterChange::Updated),
        Ordering::Greater
    );

    let mut variants = [
        LimiterChange::Disabled,
        LimiterChange::Updated,
        LimiterChange::Enabled,
        LimiterChange::Unchanged,
    ];
    variants.sort();

    assert_eq!(
        variants,
        [
            LimiterChange::Unchanged,
            LimiterChange::Updated,
            LimiterChange::Enabled,
            LimiterChange::Disabled,
        ]
    );
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
fn limiter_change_collect_collapses_iterator() {
    let aggregated: LimiterChange = [
        LimiterChange::Unchanged,
        LimiterChange::Updated,
        LimiterChange::Disabled,
    ]
    .into_iter()
    .collect();

    assert_eq!(aggregated, LimiterChange::Disabled);

    let empty: LimiterChange = std::iter::empty().collect();
    assert_eq!(empty, LimiterChange::Unchanged);
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

// ==================== Configuration update comprehensive tests ====================

#[test]
fn update_limit_preserves_burst_setting() {
    let mut session = recorded_sleep_session();
    session.clear();

    let burst = NonZeroU64::new(4096).unwrap();
    let mut limiter = BandwidthLimiter::with_burst(NonZeroU64::new(1024).unwrap(), Some(burst));

    // Update only the limit
    limiter.update_limit(NonZeroU64::new(2048).unwrap());

    // Burst should be preserved
    assert_eq!(limiter.burst_bytes(), Some(burst));
    assert_eq!(limiter.limit_bytes().get(), 2048);
}

#[test]
fn update_configuration_with_none_burst_clears_burst() {
    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::with_burst(
        NonZeroU64::new(1024).unwrap(),
        Some(NonZeroU64::new(4096).unwrap()),
    );

    // Update configuration with None burst
    limiter.update_configuration(NonZeroU64::new(2048).unwrap(), None);

    assert!(limiter.burst_bytes().is_none());
    assert_eq!(limiter.limit_bytes().get(), 2048);
}

#[test]
fn update_configuration_with_same_values_still_resets() {
    let mut session = recorded_sleep_session();
    session.clear();

    let limit = NonZeroU64::new(1024).unwrap();
    let burst = NonZeroU64::new(4096).unwrap();
    let mut limiter = BandwidthLimiter::with_burst(limit, Some(burst));

    // Accumulate debt
    let _ = limiter.register(10000);
    assert!(limiter.accumulated_debt_for_testing() > 0);

    // Update with same values
    limiter.update_configuration(limit, Some(burst));

    // Debt should still be cleared
    assert_eq!(limiter.accumulated_debt_for_testing(), 0);
}

#[test]
fn multiple_sequential_updates() {
    let mut limiter = BandwidthLimiter::new(NonZeroU64::new(1024).unwrap());

    for i in 1..=10 {
        let new_limit = NonZeroU64::new(1024 * i).unwrap();
        limiter.update_limit(new_limit);
        assert_eq!(limiter.limit_bytes(), new_limit);
    }
}

// ==================== LimiterChange state transition tests ====================

#[test]
fn limiter_change_combine_all_empty_is_unchanged() {
    // combine_all with empty iterator produces Unchanged
    let empty: [LimiterChange; 0] = [];
    assert_eq!(LimiterChange::combine_all(empty), LimiterChange::Unchanged);
}

#[test]
fn limiter_change_combine_is_commutative() {
    let changes = [
        LimiterChange::Unchanged,
        LimiterChange::Updated,
        LimiterChange::Enabled,
        LimiterChange::Disabled,
    ];

    for a in &changes {
        for b in &changes {
            assert_eq!(a.combine(*b), b.combine(*a));
        }
    }
}

#[test]
fn limiter_change_combine_is_associative() {
    let changes = [
        LimiterChange::Unchanged,
        LimiterChange::Updated,
        LimiterChange::Enabled,
        LimiterChange::Disabled,
    ];

    for a in &changes {
        for b in &changes {
            for c in &changes {
                let ab_c = a.combine(*b).combine(*c);
                let a_bc = a.combine(b.combine(*c));
                assert_eq!(ab_c, a_bc);
            }
        }
    }
}

#[test]
fn limiter_change_unchanged_is_identity() {
    for change in [
        LimiterChange::Unchanged,
        LimiterChange::Updated,
        LimiterChange::Enabled,
        LimiterChange::Disabled,
    ] {
        assert_eq!(change.combine(LimiterChange::Unchanged), change);
        assert_eq!(LimiterChange::Unchanged.combine(change), change);
    }
}

#[test]
fn limiter_change_disabled_dominates() {
    for change in [
        LimiterChange::Unchanged,
        LimiterChange::Updated,
        LimiterChange::Enabled,
        LimiterChange::Disabled,
    ] {
        assert_eq!(
            change.combine(LimiterChange::Disabled),
            LimiterChange::Disabled
        );
    }
}

#[test]
fn limiter_change_partial_ord_consistent_with_ord() {
    let changes = [
        LimiterChange::Unchanged,
        LimiterChange::Updated,
        LimiterChange::Enabled,
        LimiterChange::Disabled,
    ];

    for a in &changes {
        for b in &changes {
            assert_eq!(a.partial_cmp(b), Some(a.cmp(b)));
        }
    }
}

// ==================== Reset behavior comprehensive tests ====================

#[test]
fn reset_clears_all_mutable_state() {
    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(NonZeroU64::new(1024).unwrap());

    // Modify state through various operations
    let _ = limiter.register(10000);
    let _ = limiter.register(5000);

    // Reset
    limiter.reset();

    // All mutable state should be cleared
    assert_eq!(limiter.accumulated_debt_for_testing(), 0);

    // Configuration should be preserved
    assert_eq!(limiter.limit_bytes().get(), 1024);
}

#[test]
fn reset_preserves_burst_configuration() {
    let burst = NonZeroU64::new(4096).unwrap();
    let mut limiter = BandwidthLimiter::with_burst(NonZeroU64::new(1024).unwrap(), Some(burst));

    // Accumulate debt and reset
    let _ = limiter.register(10000);
    limiter.reset();

    // Burst should be preserved
    assert_eq!(limiter.burst_bytes(), Some(burst));
}

#[test]
fn reset_after_update_limit() {
    let mut limiter = BandwidthLimiter::new(NonZeroU64::new(1024).unwrap());
    limiter.update_limit(NonZeroU64::new(2048).unwrap());
    let _ = limiter.register(5000);

    limiter.reset();

    // Should preserve the updated limit
    assert_eq!(limiter.limit_bytes().get(), 2048);
    assert_eq!(limiter.accumulated_debt_for_testing(), 0);
}

// ==================== Clone and equality tests ====================

#[test]
fn cloned_limiter_has_same_configuration() {
    let original = BandwidthLimiter::with_burst(
        NonZeroU64::new(1024).unwrap(),
        Some(NonZeroU64::new(4096).unwrap()),
    );
    let cloned = original.clone();

    assert_eq!(original.limit_bytes(), cloned.limit_bytes());
    assert_eq!(original.burst_bytes(), cloned.burst_bytes());
    assert_eq!(original.write_max_bytes(), cloned.write_max_bytes());
}

#[test]
fn cloned_limiter_has_same_debt() {
    let mut session = recorded_sleep_session();
    session.clear();

    let mut original = BandwidthLimiter::new(NonZeroU64::new(1024).unwrap());
    let _ = original.register(5000);

    let original_debt = original.accumulated_debt_for_testing();
    let cloned = original.clone();

    assert_eq!(original_debt, cloned.accumulated_debt_for_testing());
}

// ==================== Write max calculation edge cases ====================

#[test]
fn write_max_scales_with_limit_at_boundaries() {
    // Very small limit
    let tiny = BandwidthLimiter::new(NonZeroU64::new(1).unwrap());
    assert_eq!(tiny.write_max_bytes(), 512); // MIN_WRITE_MAX

    // Medium limit
    let medium = BandwidthLimiter::new(NonZeroU64::new(1024 * 1024).unwrap());
    let medium_max = medium.write_max_bytes();
    assert!(medium_max >= 512);
    assert!(medium_max <= 1024 * 1024);

    // Large limit
    let large = BandwidthLimiter::new(NonZeroU64::new(1024 * 1024 * 1024).unwrap());
    let large_max = large.write_max_bytes();
    assert!(large_max >= medium_max);
}

#[test]
fn write_max_capped_at_burst() {
    // Large limit with small burst
    let limiter = BandwidthLimiter::with_burst(
        NonZeroU64::new(1024 * 1024 * 1024).unwrap(),
        Some(NonZeroU64::new(1024).unwrap()),
    );

    // write_max should be capped to max(burst, MIN_WRITE_MAX)
    assert!(limiter.write_max_bytes() <= 1024);
}

// ==================== Accessor consistency tests ====================

#[test]
fn limit_bytes_consistent_after_operations() {
    let mut limiter = BandwidthLimiter::new(NonZeroU64::new(1024).unwrap());
    let initial = limiter.limit_bytes();

    // Various operations shouldn't change limit_bytes
    let _ = limiter.register(1000);
    assert_eq!(limiter.limit_bytes(), initial);

    limiter.reset();
    assert_eq!(limiter.limit_bytes(), initial);
}

#[test]
fn burst_bytes_consistent_after_operations() {
    let burst = NonZeroU64::new(4096).unwrap();
    let mut limiter = BandwidthLimiter::with_burst(NonZeroU64::new(1024).unwrap(), Some(burst));

    // Various operations shouldn't change burst_bytes
    let _ = limiter.register(1000);
    assert_eq!(limiter.burst_bytes(), Some(burst));

    limiter.reset();
    assert_eq!(limiter.burst_bytes(), Some(burst));

    // Only explicit update should change it
    limiter.update_configuration(NonZeroU64::new(1024).unwrap(), None);
    assert!(limiter.burst_bytes().is_none());
}
