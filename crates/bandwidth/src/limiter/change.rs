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
        changes
            .into_iter()
            .fold(Self::Unchanged, |acc, change| acc.combine(change))
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
}
