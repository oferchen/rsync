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
        match limit {
            Some(limit) => match limiter {
                Some(existing) => {
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
                }
                None => {
                    let effective_burst = if burst_specified { burst } else { None };
                    *limiter = Some(BandwidthLimiter::with_burst(limit, effective_burst));
                    change = change.combine(LimiterChange::Enabled);
                }
            },
            None => {
                let previous = limiter.take();
                if previous.is_some() {
                    return LimiterChange::Disabled;
                }
                return LimiterChange::Unchanged;
            }
        }
    }

    if burst_specified && !limit_specified {
        if let Some(existing) = limiter.as_mut() {
            if existing.burst_bytes() != burst {
                existing.update_configuration(existing.limit_bytes(), burst);
                change = change.combine(LimiterChange::Updated);
            }
        }
    }

    change
}
