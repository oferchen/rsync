use super::{BandwidthLimiter, LimiterChange, recorded_sleep_session};
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
fn multiple_sequential_updates() {
    let mut limiter = BandwidthLimiter::new(NonZeroU64::new(1024).unwrap());

    for i in 1..=10 {
        let new_limit = NonZeroU64::new(1024 * i).unwrap();
        limiter.update_limit(new_limit);
        assert_eq!(limiter.limit_bytes(), new_limit);
    }
}

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

#[test]
fn reset_clears_all_mutable_state() {
    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(NonZeroU64::new(1024).unwrap());

    // Modify state through various operations
    let _ = limiter.register(10000);
    let _ = limiter.register(5000);

    limiter.reset();

    // All mutable state should be cleared
    assert_eq!(limiter.accumulated_debt_for_testing(), 0);

    // Configuration should be preserved
    assert_eq!(limiter.limit_bytes().get(), 1024);
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

#[test]
fn cloned_limiter_has_same_configuration() {
    let original = BandwidthLimiter::new(NonZeroU64::new(1024).unwrap());
    let cloned = original.clone();

    assert_eq!(original.limit_bytes(), cloned.limit_bytes());
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
fn limit_bytes_consistent_after_operations() {
    let mut limiter = BandwidthLimiter::new(NonZeroU64::new(1024).unwrap());
    let initial = limiter.limit_bytes();

    // Various operations shouldn't change limit_bytes
    let _ = limiter.register(1000);
    assert_eq!(limiter.limit_bytes(), initial);

    limiter.reset();
    assert_eq!(limiter.limit_bytes(), initial);
}
