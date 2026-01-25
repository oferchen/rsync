use super::core::BandwidthLimiter;
use std::cmp::Ordering;
use std::num::NonZeroU64;

/// Result returned by limiter updates describing how throttling changed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use]
pub enum LimiterChange {
    /// No adjustments were performed.
    Unchanged,
    /// Throttling was enabled by creating a new [`BandwidthLimiter`].
    Enabled,
    /// The existing limiter was reconfigured.
    Updated,
    /// Throttling was disabled.
    Disabled,
}

impl LimiterChange {
    const fn priority(self) -> u8 {
        match self {
            Self::Unchanged => 0,
            Self::Updated => 1,
            Self::Enabled => 2,
            Self::Disabled => 3,
        }
    }

    /// Returns the variant with the higher precedence between `self` and `other`.
    ///
    /// The precedence mirrors the state transitions performed by
    /// [`apply_effective_limit`]: [`LimiterChange::Disabled`] outranks
    /// [`LimiterChange::Enabled`], which in turn outranks
    /// [`LimiterChange::Updated`], while [`LimiterChange::Unchanged`]
    /// is always superseded by the other operand.
    pub const fn combine(self, other: Self) -> Self {
        if self.priority() >= other.priority() {
            self
        } else {
            other
        }
    }

    /// Collapses an iterator of changes into a single representative variant.
    pub fn combine_all<I>(changes: I) -> Self
    where
        I: IntoIterator<Item = Self>,
    {
        changes.into_iter().max().unwrap_or(Self::Unchanged)
    }

    /// Returns `true` when the limiter configuration or activation state changed.
    #[must_use]
    pub const fn is_changed(self) -> bool {
        !matches!(self, Self::Unchanged)
    }

    /// Returns `true` when the limiter remains active after the update.
    #[must_use]
    pub const fn leaves_limiter_active(self) -> bool {
        matches!(self, Self::Enabled | Self::Updated)
    }

    /// Returns `true` when throttling was disabled as a result of the update.
    #[must_use]
    pub const fn disables_limiter(self) -> bool {
        matches!(self, Self::Disabled)
    }
}

impl Ord for LimiterChange {
    fn cmp(&self, other: &Self) -> Ordering {
        self.priority().cmp(&other.priority())
    }
}

impl PartialOrd for LimiterChange {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl FromIterator<LimiterChange> for LimiterChange {
    fn from_iter<I: IntoIterator<Item = LimiterChange>>(iter: I) -> Self {
        Self::combine_all(iter)
    }
}

/// Applies a module-specific bandwidth cap to an optional limiter, mirroring upstream precedence rules.
pub fn apply_effective_limit(
    limiter: &mut Option<BandwidthLimiter>,
    limit: Option<NonZeroU64>,
    limit_specified: bool,
    burst: Option<NonZeroU64>,
    burst_specified: bool,
) -> LimiterChange {
    if !limit_specified && !burst_specified {
        return LimiterChange::Unchanged;
    }

    let mut change = LimiterChange::Unchanged;

    if limit_specified {
        if let Some(limit) = limit {
            if let Some(existing) = limiter {
                let target_limit = existing.limit_bytes().min(limit);
                let current_burst = existing.burst_bytes();
                let target_burst = if burst_specified {
                    burst
                } else {
                    current_burst
                };

                let limit_changed = target_limit != existing.limit_bytes();
                let burst_changed = target_burst != current_burst;

                if limit_changed || burst_changed {
                    existing.update_configuration(target_limit, target_burst);
                    change = change.combine(LimiterChange::Updated);
                }
            } else {
                let effective_burst = if burst_specified { burst } else { None };
                *limiter = Some(BandwidthLimiter::with_burst(limit, effective_burst));
                change = change.combine(LimiterChange::Enabled);
            }
        } else {
            let previous = limiter.take();
            if previous.is_some() {
                return LimiterChange::Disabled;
            }
            return LimiterChange::Unchanged;
        }
    }

    if burst_specified
        && !limit_specified
        && let Some(existing) = limiter.as_mut()
        && existing.burst_bytes() != burst
    {
        existing.update_configuration(existing.limit_bytes(), burst);
        change = change.combine(LimiterChange::Updated);
    }

    change
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limiter_change_unchanged_has_lowest_priority() {
        assert_eq!(LimiterChange::Unchanged.priority(), 0);
    }

    #[test]
    fn limiter_change_disabled_has_highest_priority() {
        assert!(LimiterChange::Disabled.priority() > LimiterChange::Enabled.priority());
        assert!(LimiterChange::Disabled.priority() > LimiterChange::Updated.priority());
        assert!(LimiterChange::Disabled.priority() > LimiterChange::Unchanged.priority());
    }

    #[test]
    fn limiter_change_combine_returns_higher_priority() {
        assert_eq!(
            LimiterChange::Unchanged.combine(LimiterChange::Updated),
            LimiterChange::Updated
        );
        assert_eq!(
            LimiterChange::Updated.combine(LimiterChange::Enabled),
            LimiterChange::Enabled
        );
        assert_eq!(
            LimiterChange::Enabled.combine(LimiterChange::Disabled),
            LimiterChange::Disabled
        );
    }

    #[test]
    fn limiter_change_combine_unchanged_with_unchanged() {
        assert_eq!(
            LimiterChange::Unchanged.combine(LimiterChange::Unchanged),
            LimiterChange::Unchanged
        );
    }

    #[test]
    fn limiter_change_combine_same_priority_returns_self() {
        assert_eq!(
            LimiterChange::Updated.combine(LimiterChange::Updated),
            LimiterChange::Updated
        );
    }

    #[test]
    fn limiter_change_combine_all_empty_returns_unchanged() {
        let changes: Vec<LimiterChange> = vec![];
        assert_eq!(
            LimiterChange::combine_all(changes),
            LimiterChange::Unchanged
        );
    }

    #[test]
    fn limiter_change_combine_all_single_returns_that_change() {
        assert_eq!(
            LimiterChange::combine_all(vec![LimiterChange::Enabled]),
            LimiterChange::Enabled
        );
    }

    #[test]
    fn limiter_change_combine_all_returns_highest_priority() {
        let changes = vec![
            LimiterChange::Unchanged,
            LimiterChange::Updated,
            LimiterChange::Enabled,
        ];
        assert_eq!(LimiterChange::combine_all(changes), LimiterChange::Enabled);
    }

    #[test]
    fn limiter_change_combine_all_disabled_wins() {
        let changes = vec![
            LimiterChange::Enabled,
            LimiterChange::Disabled,
            LimiterChange::Updated,
        ];
        assert_eq!(LimiterChange::combine_all(changes), LimiterChange::Disabled);
    }

    #[test]
    fn limiter_change_is_changed_true_for_updated() {
        assert!(LimiterChange::Updated.is_changed());
    }

    #[test]
    fn limiter_change_is_changed_true_for_enabled() {
        assert!(LimiterChange::Enabled.is_changed());
    }

    #[test]
    fn limiter_change_is_changed_true_for_disabled() {
        assert!(LimiterChange::Disabled.is_changed());
    }

    #[test]
    fn limiter_change_is_changed_false_for_unchanged() {
        assert!(!LimiterChange::Unchanged.is_changed());
    }

    #[test]
    fn limiter_change_leaves_limiter_active_for_enabled() {
        assert!(LimiterChange::Enabled.leaves_limiter_active());
    }

    #[test]
    fn limiter_change_leaves_limiter_active_for_updated() {
        assert!(LimiterChange::Updated.leaves_limiter_active());
    }

    #[test]
    fn limiter_change_leaves_limiter_active_false_for_disabled() {
        assert!(!LimiterChange::Disabled.leaves_limiter_active());
    }

    #[test]
    fn limiter_change_leaves_limiter_active_false_for_unchanged() {
        assert!(!LimiterChange::Unchanged.leaves_limiter_active());
    }

    #[test]
    fn limiter_change_disables_limiter_true_for_disabled() {
        assert!(LimiterChange::Disabled.disables_limiter());
    }

    #[test]
    fn limiter_change_disables_limiter_false_for_others() {
        assert!(!LimiterChange::Unchanged.disables_limiter());
        assert!(!LimiterChange::Updated.disables_limiter());
        assert!(!LimiterChange::Enabled.disables_limiter());
    }

    #[test]
    fn limiter_change_ord_unchanged_less_than_updated() {
        assert!(LimiterChange::Unchanged < LimiterChange::Updated);
    }

    #[test]
    fn limiter_change_ord_updated_less_than_enabled() {
        assert!(LimiterChange::Updated < LimiterChange::Enabled);
    }

    #[test]
    fn limiter_change_ord_enabled_less_than_disabled() {
        assert!(LimiterChange::Enabled < LimiterChange::Disabled);
    }

    #[test]
    fn limiter_change_partial_ord_returns_some() {
        assert!(
            LimiterChange::Updated
                .partial_cmp(&LimiterChange::Enabled)
                .is_some()
        );
    }

    #[test]
    fn limiter_change_from_iterator() {
        let changes = vec![LimiterChange::Unchanged, LimiterChange::Updated];
        let result: LimiterChange = changes.into_iter().collect();
        assert_eq!(result, LimiterChange::Updated);
    }

    #[test]
    fn limiter_change_clone_equals() {
        let change = LimiterChange::Enabled;
        assert_eq!(change.clone(), change);
    }

    #[test]
    fn limiter_change_debug() {
        let debug = format!("{:?}", LimiterChange::Enabled);
        assert!(debug.contains("Enabled"));
    }

    // ========================================================================
    // Tests for apply_effective_limit function
    // ========================================================================

    fn nz(value: u64) -> NonZeroU64 {
        NonZeroU64::new(value).expect("non-zero value required")
    }

    #[test]
    fn apply_effective_limit_unchanged_when_nothing_specified() {
        let mut limiter = None;
        let result = apply_effective_limit(&mut limiter, None, false, None, false);
        assert_eq!(result, LimiterChange::Unchanged);
        assert!(limiter.is_none());
    }

    #[test]
    fn apply_effective_limit_unchanged_when_limiter_exists_but_nothing_specified() {
        let mut limiter = Some(BandwidthLimiter::new(nz(1000)));
        let result = apply_effective_limit(&mut limiter, None, false, None, false);
        assert_eq!(result, LimiterChange::Unchanged);
        assert!(limiter.is_some());
    }

    #[test]
    fn apply_effective_limit_enables_limiter_when_none_exists() {
        let mut limiter = None;
        let result = apply_effective_limit(&mut limiter, Some(nz(1000)), true, None, false);
        assert_eq!(result, LimiterChange::Enabled);
        assert!(limiter.is_some());
        assert_eq!(limiter.as_ref().unwrap().limit_bytes().get(), 1000);
    }

    #[test]
    fn apply_effective_limit_enables_with_burst() {
        let mut limiter = None;
        let result = apply_effective_limit(&mut limiter, Some(nz(1000)), true, Some(nz(500)), true);
        assert_eq!(result, LimiterChange::Enabled);
        assert!(limiter.is_some());
        let l = limiter.as_ref().unwrap();
        assert_eq!(l.limit_bytes().get(), 1000);
        assert_eq!(l.burst_bytes().unwrap().get(), 500);
    }

    #[test]
    fn apply_effective_limit_disables_limiter_when_limit_is_none() {
        let mut limiter = Some(BandwidthLimiter::new(nz(1000)));
        let result = apply_effective_limit(&mut limiter, None, true, None, false);
        assert_eq!(result, LimiterChange::Disabled);
        assert!(limiter.is_none());
    }

    #[test]
    fn apply_effective_limit_unchanged_when_disabling_nonexistent_limiter() {
        let mut limiter: Option<BandwidthLimiter> = None;
        let result = apply_effective_limit(&mut limiter, None, true, None, false);
        assert_eq!(result, LimiterChange::Unchanged);
        assert!(limiter.is_none());
    }

    #[test]
    fn apply_effective_limit_updates_existing_limiter_limit_lower() {
        // When new limit is lower than existing, use new limit (min)
        let mut limiter = Some(BandwidthLimiter::new(nz(2000)));
        let result = apply_effective_limit(&mut limiter, Some(nz(1000)), true, None, false);
        assert_eq!(result, LimiterChange::Updated);
        assert_eq!(limiter.as_ref().unwrap().limit_bytes().get(), 1000);
    }

    #[test]
    fn apply_effective_limit_unchanged_when_limit_higher() {
        // When new limit is higher than existing, use existing (min) - no change
        let mut limiter = Some(BandwidthLimiter::new(nz(1000)));
        let result = apply_effective_limit(&mut limiter, Some(nz(2000)), true, None, false);
        assert_eq!(result, LimiterChange::Unchanged);
        assert_eq!(limiter.as_ref().unwrap().limit_bytes().get(), 1000);
    }

    #[test]
    fn apply_effective_limit_updates_burst_on_existing_limiter() {
        let mut limiter = Some(BandwidthLimiter::new(nz(1000)));
        let result = apply_effective_limit(&mut limiter, Some(nz(1000)), true, Some(nz(500)), true);
        assert_eq!(result, LimiterChange::Updated);
        assert_eq!(limiter.as_ref().unwrap().burst_bytes().unwrap().get(), 500);
    }

    #[test]
    fn apply_effective_limit_removes_burst_on_existing_limiter() {
        let mut limiter = Some(BandwidthLimiter::with_burst(nz(1000), Some(nz(500))));
        let result = apply_effective_limit(&mut limiter, Some(nz(1000)), true, None, true);
        assert_eq!(result, LimiterChange::Updated);
        assert!(limiter.as_ref().unwrap().burst_bytes().is_none());
    }

    #[test]
    fn apply_effective_limit_preserves_burst_when_not_specified() {
        let mut limiter = Some(BandwidthLimiter::with_burst(nz(1000), Some(nz(500))));
        // Update limit without specifying burst
        let result = apply_effective_limit(&mut limiter, Some(nz(800)), true, None, false);
        assert_eq!(result, LimiterChange::Updated);
        // Burst should be preserved
        assert_eq!(limiter.as_ref().unwrap().burst_bytes().unwrap().get(), 500);
        assert_eq!(limiter.as_ref().unwrap().limit_bytes().get(), 800);
    }

    #[test]
    fn apply_effective_limit_updates_burst_only_on_existing_limiter() {
        let mut limiter = Some(BandwidthLimiter::new(nz(1000)));
        // Update only burst without specifying limit
        let result = apply_effective_limit(&mut limiter, None, false, Some(nz(500)), true);
        assert_eq!(result, LimiterChange::Updated);
        assert_eq!(limiter.as_ref().unwrap().burst_bytes().unwrap().get(), 500);
        assert_eq!(limiter.as_ref().unwrap().limit_bytes().get(), 1000); // Unchanged
    }

    #[test]
    fn apply_effective_limit_burst_only_change_on_nonexistent_limiter() {
        // Changing burst only when no limiter exists should do nothing
        let mut limiter: Option<BandwidthLimiter> = None;
        let result = apply_effective_limit(&mut limiter, None, false, Some(nz(500)), true);
        assert_eq!(result, LimiterChange::Unchanged);
        assert!(limiter.is_none());
    }

    #[test]
    fn apply_effective_limit_burst_unchanged_when_same() {
        let mut limiter = Some(BandwidthLimiter::with_burst(nz(1000), Some(nz(500))));
        // Set burst to same value
        let result = apply_effective_limit(&mut limiter, None, false, Some(nz(500)), true);
        assert_eq!(result, LimiterChange::Unchanged);
    }

    #[test]
    fn apply_effective_limit_multiple_changes_combine_correctly() {
        // Test that both limit and burst changes result in Updated
        let mut limiter = Some(BandwidthLimiter::with_burst(nz(2000), Some(nz(1000))));
        let result = apply_effective_limit(&mut limiter, Some(nz(1500)), true, Some(nz(700)), true);
        assert_eq!(result, LimiterChange::Updated);
        // Limit should be min(2000, 1500) = 1500
        assert_eq!(limiter.as_ref().unwrap().limit_bytes().get(), 1500);
        assert_eq!(limiter.as_ref().unwrap().burst_bytes().unwrap().get(), 700);
    }

    #[test]
    fn apply_effective_limit_disable_takes_precedence() {
        // When limit is None and specified, disabling should happen regardless of burst
        let mut limiter = Some(BandwidthLimiter::with_burst(nz(1000), Some(nz(500))));
        let result = apply_effective_limit(&mut limiter, None, true, Some(nz(700)), true);
        assert_eq!(result, LimiterChange::Disabled);
        assert!(limiter.is_none());
    }

    #[test]
    fn apply_effective_limit_enable_with_burst_not_specified() {
        // Enable limiter without burst
        let mut limiter = None;
        let result =
            apply_effective_limit(&mut limiter, Some(nz(1000)), true, Some(nz(500)), false);
        assert_eq!(result, LimiterChange::Enabled);
        // Burst should be None since burst_specified is false
        assert!(limiter.as_ref().unwrap().burst_bytes().is_none());
    }

    #[test]
    fn apply_effective_limit_limit_not_specified_burst_change_only() {
        // Test the branch where limit_specified is false but burst_specified is true
        let mut limiter = Some(BandwidthLimiter::with_burst(nz(1000), Some(nz(500))));
        let result =
            apply_effective_limit(&mut limiter, Some(nz(2000)), false, Some(nz(700)), true);
        assert_eq!(result, LimiterChange::Updated);
        // Limit unchanged (because limit_specified is false)
        assert_eq!(limiter.as_ref().unwrap().limit_bytes().get(), 1000);
        // Burst updated
        assert_eq!(limiter.as_ref().unwrap().burst_bytes().unwrap().get(), 700);
    }

    #[test]
    fn apply_effective_limit_removes_burst_via_burst_only_update() {
        let mut limiter = Some(BandwidthLimiter::with_burst(nz(1000), Some(nz(500))));
        // Only specify burst change (to None)
        let result = apply_effective_limit(&mut limiter, None, false, None, true);
        assert_eq!(result, LimiterChange::Updated);
        assert!(limiter.as_ref().unwrap().burst_bytes().is_none());
    }

    #[test]
    fn apply_effective_limit_limiter_with_lower_existing_limit() {
        // Existing limit is lower than new limit - should not change
        let mut limiter = Some(BandwidthLimiter::new(nz(500)));
        let result = apply_effective_limit(&mut limiter, Some(nz(1000)), true, None, false);
        assert_eq!(result, LimiterChange::Unchanged);
        assert_eq!(limiter.as_ref().unwrap().limit_bytes().get(), 500);
    }

    #[test]
    fn apply_effective_limit_limiter_with_equal_existing_limit() {
        // Existing limit equals new limit - should not change
        let mut limiter = Some(BandwidthLimiter::new(nz(1000)));
        let result = apply_effective_limit(&mut limiter, Some(nz(1000)), true, None, false);
        assert_eq!(result, LimiterChange::Unchanged);
        assert_eq!(limiter.as_ref().unwrap().limit_bytes().get(), 1000);
    }

    // ========================================================================
    // Edge cases for apply_effective_limit
    // ========================================================================

    #[test]
    fn apply_effective_limit_very_small_limit() {
        let mut limiter = None;
        let result = apply_effective_limit(&mut limiter, Some(nz(1)), true, None, false);
        assert_eq!(result, LimiterChange::Enabled);
        assert_eq!(limiter.as_ref().unwrap().limit_bytes().get(), 1);
    }

    #[test]
    fn apply_effective_limit_very_large_limit() {
        let mut limiter = None;
        let result = apply_effective_limit(&mut limiter, Some(nz(u64::MAX)), true, None, false);
        assert_eq!(result, LimiterChange::Enabled);
        assert_eq!(limiter.as_ref().unwrap().limit_bytes().get(), u64::MAX);
    }

    #[test]
    fn apply_effective_limit_burst_larger_than_limit() {
        let mut limiter = None;
        let result =
            apply_effective_limit(&mut limiter, Some(nz(1000)), true, Some(nz(5000)), true);
        assert_eq!(result, LimiterChange::Enabled);
        assert_eq!(limiter.as_ref().unwrap().limit_bytes().get(), 1000);
        assert_eq!(limiter.as_ref().unwrap().burst_bytes().unwrap().get(), 5000);
    }

    #[test]
    fn apply_effective_limit_burst_equal_to_limit() {
        let mut limiter = None;
        let result =
            apply_effective_limit(&mut limiter, Some(nz(1000)), true, Some(nz(1000)), true);
        assert_eq!(result, LimiterChange::Enabled);
        assert_eq!(limiter.as_ref().unwrap().limit_bytes().get(), 1000);
        assert_eq!(limiter.as_ref().unwrap().burst_bytes().unwrap().get(), 1000);
    }

    #[test]
    fn apply_effective_limit_repeated_updates() {
        let mut limiter = None;

        // First enable
        let r1 = apply_effective_limit(&mut limiter, Some(nz(2000)), true, None, false);
        assert_eq!(r1, LimiterChange::Enabled);

        // Update to lower
        let r2 = apply_effective_limit(&mut limiter, Some(nz(1500)), true, None, false);
        assert_eq!(r2, LimiterChange::Updated);

        // Try to update to higher (should be unchanged due to min)
        let r3 = apply_effective_limit(&mut limiter, Some(nz(2000)), true, None, false);
        assert_eq!(r3, LimiterChange::Unchanged);

        // Disable
        let r4 = apply_effective_limit(&mut limiter, None, true, None, false);
        assert_eq!(r4, LimiterChange::Disabled);

        // Re-enable
        let r5 = apply_effective_limit(&mut limiter, Some(nz(1000)), true, None, false);
        assert_eq!(r5, LimiterChange::Enabled);
    }

    #[test]
    fn apply_effective_limit_burst_only_specified_but_same_value() {
        let mut limiter = Some(BandwidthLimiter::with_burst(nz(1000), Some(nz(500))));
        // Specify burst_specified=true but same burst value
        let result = apply_effective_limit(&mut limiter, None, false, Some(nz(500)), true);
        // Should be unchanged since burst value is the same
        assert_eq!(result, LimiterChange::Unchanged);
    }

    #[test]
    fn apply_effective_limit_both_specified_but_neither_changed() {
        // Both limit and burst specified but no actual change needed
        let mut limiter = Some(BandwidthLimiter::with_burst(nz(1000), Some(nz(500))));
        // Specify higher limit (min keeps existing) and same burst
        let result = apply_effective_limit(&mut limiter, Some(nz(2000)), true, Some(nz(500)), true);
        // limit_changed = false (target_limit = min(1000, 2000) = 1000 = existing)
        // burst_changed = false (500 == 500)
        assert_eq!(result, LimiterChange::Unchanged);
    }
}
