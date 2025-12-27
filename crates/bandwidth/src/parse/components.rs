use std::num::NonZeroU64;
use std::str::FromStr;

use crate::limiter::{BandwidthLimiter, LimiterChange, apply_effective_limit};

use super::{BandwidthParseError, parse_bandwidth_limit};

/// Parsed `--bwlimit` components consisting of an optional rate and burst size.
///
/// In addition to the negotiated byte-per-second rate, the structure records
/// whether the user explicitly supplied the limit. This allows callers to
/// distinguish between inherited defaults and requests such as `--bwlimit=0`
/// that disable throttling while remaining user-driven decisions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BandwidthLimitComponents {
    rate: Option<NonZeroU64>,
    burst: Option<NonZeroU64>,
    limit_specified: bool,
    burst_specified: bool,
}

impl BandwidthLimitComponents {
    /// Constructs a new component set from the provided parts.
    ///
    /// When the rate is `None` the combination represents an unlimited
    /// configuration. Upstream rsync ignores any burst component in that case,
    /// so the helper mirrors that behaviour by discarding the supplied burst.
    #[must_use]
    pub const fn new(rate: Option<NonZeroU64>, burst: Option<NonZeroU64>) -> Self {
        Self::new_internal(rate, burst, rate.is_some(), burst.is_some())
    }

    /// Constructs a component set while explicitly controlling the specification flags.
    ///
    /// The helper mirrors upstream precedence rules where callers may need to
    /// distinguish between inherited defaults and user-supplied overrides.  It
    /// preserves explicit burst components even when the limit is unlimited so
    /// daemon modules can override the negotiated burst while keeping the
    /// existing limiter active.  When a rate is provided, the combination always
    /// records that a limit was specified to reflect the caller's intent.
    #[must_use]
    pub const fn new_with_flags(
        rate: Option<NonZeroU64>,
        burst: Option<NonZeroU64>,
        limit_specified: bool,
        burst_specified: bool,
    ) -> Self {
        let has_rate = rate.is_some();
        let effective_limit_specified = if has_rate { true } else { limit_specified };
        let effective_burst = if burst_specified { burst } else { None };
        let effective_burst_specified = if has_rate {
            burst_specified
        } else {
            effective_burst.is_some() && burst_specified
        };

        Self {
            rate,
            burst: effective_burst,
            limit_specified: effective_limit_specified,
            burst_specified: effective_burst_specified,
        }
    }

    /// Returns a component set that disables throttling.
    ///
    /// Upstream rsync treats `--bwlimit=0` as unlimited, ignoring any optional
    /// burst parameter. Providing an explicit constructor avoids sprinkling
    /// `None` pairs throughout the codebase while making the intent of
    /// "unlimited" limits clear at the call site. The helper is `const` so it
    /// can be used in static initialisers and default values.
    #[must_use]
    pub const fn unlimited() -> Self {
        Self::new_internal(None, None, false, false)
    }

    /// Constructs a new component set and records whether the burst component
    /// was explicitly supplied.
    #[must_use]
    pub const fn new_with_specified(
        rate: Option<NonZeroU64>,
        burst: Option<NonZeroU64>,
        burst_specified: bool,
    ) -> Self {
        Self::new_internal(rate, burst, rate.is_some(), burst_specified)
    }

    /// Constructs a component set with explicit specification flags.
    ///
    /// The helper mirrors the historical `new_internal` constructor so modules
    /// such as the command-line parser can retain fine-grained control over the
    /// `limit_specified` and `burst_specified` markers while reusing the logic
    /// that normalises unlimited configurations.
    #[must_use]
    pub(crate) const fn with_internal_flags(
        rate: Option<NonZeroU64>,
        burst: Option<NonZeroU64>,
        limit_specified: bool,
        burst_specified: bool,
    ) -> Self {
        Self::new_internal(rate, burst, limit_specified, burst_specified)
    }

    const fn new_internal(
        rate: Option<NonZeroU64>,
        burst: Option<NonZeroU64>,
        limit_specified: bool,
        burst_specified: bool,
    ) -> Self {
        let has_rate = rate.is_some();
        let effective_limit_specified = if has_rate { true } else { limit_specified };
        let effective_burst = if has_rate { burst } else { None };
        let effective_burst_specified = if has_rate { burst_specified } else { false };

        Self {
            rate,
            burst: effective_burst,
            limit_specified: effective_limit_specified,
            burst_specified: effective_burst_specified,
        }
    }

    /// Returns the configured byte-per-second rate, if any.
    #[must_use]
    pub const fn rate(&self) -> Option<NonZeroU64> {
        self.rate
    }

    /// Returns the configured burst size in bytes, if any.
    #[must_use]
    pub const fn burst(&self) -> Option<NonZeroU64> {
        self.burst
    }

    /// Indicates whether a burst component was explicitly specified.
    #[must_use]
    pub const fn burst_specified(&self) -> bool {
        self.burst_specified
    }

    /// Indicates whether the rate component was explicitly specified.
    #[must_use]
    pub const fn limit_specified(&self) -> bool {
        self.limit_specified
    }

    /// Indicates whether the limit disables throttling.
    #[must_use]
    pub const fn is_unlimited(self) -> bool {
        self.rate.is_none()
    }

    /// Converts the parsed components into a [`BandwidthLimiter`].
    ///
    /// When the rate component is absent (representing an unlimited
    /// configuration), the method returns `None`. Otherwise the limiter mirrors
    /// upstream rsync's token bucket by honouring the optional burst size.
    #[must_use]
    pub fn to_limiter(&self) -> Option<BandwidthLimiter> {
        self.rate()
            .map(|rate| BandwidthLimiter::with_burst(rate, self.burst()))
    }

    /// Consumes the components and constructs a [`BandwidthLimiter`].
    ///
    /// The behaviour matches [`Self::to_limiter`]; the by-value variant avoids
    /// cloning when the caller wishes to move ownership of the parsed
    /// components.
    #[must_use]
    pub fn into_limiter(self) -> Option<BandwidthLimiter> {
        self.rate
            .map(|rate| BandwidthLimiter::with_burst(rate, self.burst))
    }

    /// Applies the component set to an existing limiter, mirroring rsync's precedence rules.
    ///
    /// The helper forwards to [`apply_effective_limit`] so higher layers do not
    /// have to thread individual specification flags through their call sites.
    /// It returns the resulting [`LimiterChange`], allowing callers to surface
    /// diagnostics or skip follow-up work when no adjustments were required.
    pub fn apply_to_limiter(&self, limiter: &mut Option<BandwidthLimiter>) -> LimiterChange {
        apply_effective_limit(
            limiter,
            self.rate,
            self.limit_specified,
            self.burst,
            self.burst_specified,
        )
    }

    /// Returns a new component set that applies an overriding cap to the current configuration.
    ///
    /// The method mirrors upstream rsync's precedence rules when a daemon module defines its own
    /// `bwlimit`. The strictest byte-per-second rate wins while explicitly configured burst sizes
    /// take effect. When the override disables throttling altogether (for example `bwlimit = 0`)
    /// the resulting component becomes unlimited, even if the caller previously supplied a rate.
    /// This allows higher layers to reason about the effective limiter without materialising a
    /// [`BandwidthLimiter`] instance solely to combine configuration sources.
    #[must_use]
    pub fn constrained_by(&self, override_components: &Self) -> Self {
        let mut rate = self.rate;
        let mut burst = self.burst;
        let limit_specified = self.limit_specified || override_components.limit_specified;
        let mut burst_specified = self.burst_specified;
        let had_limit = self.rate.is_some();

        if override_components.limit_specified {
            match override_components.rate {
                Some(override_rate) => {
                    rate = match rate {
                        Some(existing) => Some(existing.min(override_rate)),
                        None => Some(override_rate),
                    };

                    if override_components.burst_specified {
                        burst = override_components.burst;
                        burst_specified = true;
                    } else if !had_limit {
                        burst = None;
                        burst_specified = false;
                    }
                }
                None => {
                    rate = None;
                    burst = None;
                    burst_specified = false;
                }
            }
        }

        if override_components.burst_specified
            && !override_components.limit_specified
            && rate.is_some()
        {
            burst = override_components.burst;
            burst_specified = true;
        }

        Self::new_internal(rate, burst, limit_specified, burst_specified)
    }
}

impl FromStr for BandwidthLimitComponents {
    type Err = BandwidthParseError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        parse_bandwidth_limit(text)
    }
}

impl Default for BandwidthLimitComponents {
    fn default() -> Self {
        Self::unlimited()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nz(val: u64) -> NonZeroU64 {
        NonZeroU64::new(val).unwrap()
    }

    #[test]
    fn new_with_rate_and_burst() {
        let c = BandwidthLimitComponents::new(Some(nz(1000)), Some(nz(500)));
        assert_eq!(c.rate(), Some(nz(1000)));
        assert_eq!(c.burst(), Some(nz(500)));
        assert!(c.limit_specified());
        assert!(c.burst_specified());
    }

    #[test]
    fn new_with_rate_only() {
        let c = BandwidthLimitComponents::new(Some(nz(1000)), None);
        assert_eq!(c.rate(), Some(nz(1000)));
        assert_eq!(c.burst(), None);
        assert!(c.limit_specified());
        assert!(!c.burst_specified());
    }

    #[test]
    fn new_without_rate_discards_burst() {
        // When rate is None (unlimited), burst is ignored
        let c = BandwidthLimitComponents::new(None, Some(nz(500)));
        assert_eq!(c.rate(), None);
        assert_eq!(c.burst(), None);
        assert!(!c.limit_specified());
        assert!(!c.burst_specified());
    }

    #[test]
    fn new_unlimited() {
        let c = BandwidthLimitComponents::new(None, None);
        assert!(c.is_unlimited());
        assert!(!c.limit_specified());
        assert!(!c.burst_specified());
    }

    #[test]
    fn unlimited_constructor() {
        let c = BandwidthLimitComponents::unlimited();
        assert_eq!(c.rate(), None);
        assert_eq!(c.burst(), None);
        assert!(c.is_unlimited());
        assert!(!c.limit_specified());
        assert!(!c.burst_specified());
    }

    #[test]
    fn new_with_specified_records_burst_flag() {
        let c = BandwidthLimitComponents::new_with_specified(Some(nz(1000)), Some(nz(500)), true);
        assert!(c.burst_specified());

        let c2 = BandwidthLimitComponents::new_with_specified(Some(nz(1000)), Some(nz(500)), false);
        assert!(!c2.burst_specified());
    }

    #[test]
    fn new_with_flags_with_rate() {
        let c = BandwidthLimitComponents::new_with_flags(Some(nz(1000)), Some(nz(500)), true, true);
        assert_eq!(c.rate(), Some(nz(1000)));
        assert_eq!(c.burst(), Some(nz(500)));
        assert!(c.limit_specified());
        assert!(c.burst_specified());
    }

    #[test]
    fn new_with_flags_without_rate_preserves_limit_specified() {
        // Even without rate, limit_specified flag can be set
        let c = BandwidthLimitComponents::new_with_flags(None, None, true, false);
        assert!(c.limit_specified());
        assert!(!c.burst_specified());
    }

    #[test]
    fn is_unlimited_true_when_no_rate() {
        let c = BandwidthLimitComponents::unlimited();
        assert!(c.is_unlimited());

        let c2 = BandwidthLimitComponents::new(None, None);
        assert!(c2.is_unlimited());
    }

    #[test]
    fn is_unlimited_false_when_rate_present() {
        let c = BandwidthLimitComponents::new(Some(nz(100)), None);
        assert!(!c.is_unlimited());
    }

    #[test]
    fn to_limiter_returns_none_when_unlimited() {
        let c = BandwidthLimitComponents::unlimited();
        assert!(c.to_limiter().is_none());
    }

    #[test]
    fn to_limiter_returns_limiter_with_rate() {
        let c = BandwidthLimitComponents::new(Some(nz(1000)), Some(nz(500)));
        let limiter = c.to_limiter();
        assert!(limiter.is_some());
        let l = limiter.unwrap();
        assert_eq!(l.limit_bytes(), nz(1000));
        assert_eq!(l.burst_bytes(), Some(nz(500)));
    }

    #[test]
    fn into_limiter_returns_none_when_unlimited() {
        let c = BandwidthLimitComponents::unlimited();
        assert!(c.into_limiter().is_none());
    }

    #[test]
    fn into_limiter_returns_limiter_with_rate() {
        let c = BandwidthLimitComponents::new(Some(nz(2000)), Some(nz(1000)));
        let limiter = c.into_limiter();
        assert!(limiter.is_some());
        let l = limiter.unwrap();
        assert_eq!(l.limit_bytes(), nz(2000));
        assert_eq!(l.burst_bytes(), Some(nz(1000)));
    }

    #[test]
    fn apply_to_limiter_creates_new_limiter() {
        let c = BandwidthLimitComponents::new(Some(nz(1000)), None);
        let mut limiter: Option<BandwidthLimiter> = None;
        let change = c.apply_to_limiter(&mut limiter);
        assert_eq!(change, LimiterChange::Enabled);
        assert!(limiter.is_some());
    }

    #[test]
    fn apply_to_limiter_unchanged_when_nothing_specified() {
        let c = BandwidthLimitComponents::unlimited();
        let mut limiter: Option<BandwidthLimiter> = None;
        let change = c.apply_to_limiter(&mut limiter);
        assert_eq!(change, LimiterChange::Unchanged);
        assert!(limiter.is_none());
    }

    #[test]
    fn constrained_by_takes_minimum_rate() {
        let c1 = BandwidthLimitComponents::new(Some(nz(1000)), None);
        let c2 = BandwidthLimitComponents::new(Some(nz(500)), None);
        let constrained = c1.constrained_by(&c2);
        assert_eq!(constrained.rate(), Some(nz(500)));
    }

    #[test]
    fn constrained_by_unlimited_override_disables_limit() {
        let c1 = BandwidthLimitComponents::new(Some(nz(1000)), None);
        // Create an unlimited override with limit_specified = true
        let c2 = BandwidthLimitComponents::new_with_flags(None, None, true, false);
        let constrained = c1.constrained_by(&c2);
        assert!(constrained.is_unlimited());
    }

    #[test]
    fn constrained_by_applies_override_burst() {
        let c1 = BandwidthLimitComponents::new(Some(nz(1000)), Some(nz(500)));
        let c2 = BandwidthLimitComponents::new(Some(nz(800)), Some(nz(200)));
        let constrained = c1.constrained_by(&c2);
        // Rate should be min of 1000 and 800 = 800
        assert_eq!(constrained.rate(), Some(nz(800)));
        // Burst should be from override
        assert_eq!(constrained.burst(), Some(nz(200)));
    }

    #[test]
    fn constrained_by_no_override_preserves_original() {
        let c1 = BandwidthLimitComponents::new(Some(nz(1000)), Some(nz(500)));
        let c2 = BandwidthLimitComponents::unlimited();
        let constrained = c1.constrained_by(&c2);
        assert_eq!(constrained.rate(), Some(nz(1000)));
        assert_eq!(constrained.burst(), Some(nz(500)));
    }

    #[test]
    fn constrained_by_burst_only_override() {
        let c1 = BandwidthLimitComponents::new(Some(nz(1000)), Some(nz(500)));
        // Override with only burst specified
        let c2 = BandwidthLimitComponents::new_with_flags(None, Some(nz(200)), false, true);
        let constrained = c1.constrained_by(&c2);
        assert_eq!(constrained.rate(), Some(nz(1000)));
        assert_eq!(constrained.burst(), Some(nz(200)));
        assert!(constrained.burst_specified());
    }

    #[test]
    fn from_str_parses_simple_number_as_kilobytes() {
        // Default unit is kilobytes, so 1000 means 1000 KB = 1024000 bytes
        let c: BandwidthLimitComponents = "1000".parse().unwrap();
        assert_eq!(c.rate(), Some(nz(1024000)));
    }

    #[test]
    fn from_str_parses_zero_as_unlimited() {
        let c: BandwidthLimitComponents = "0".parse().unwrap();
        assert!(c.is_unlimited());
    }

    #[test]
    fn from_str_parses_with_explicit_suffix() {
        // Explicit K suffix (kilobytes)
        let c: BandwidthLimitComponents = "1k".parse().unwrap();
        assert_eq!(c.rate(), Some(nz(1024)));

        // M suffix (megabytes)
        let c2: BandwidthLimitComponents = "1m".parse().unwrap();
        assert_eq!(c2.rate(), Some(nz(1024 * 1024)));
    }

    #[test]
    fn from_str_parses_bytes_suffix() {
        // B suffix means bytes, so 1024b = 1024 bytes
        let c: BandwidthLimitComponents = "1024b".parse().unwrap();
        assert_eq!(c.rate(), Some(nz(1024)));
    }

    #[test]
    fn from_str_invalid_returns_error() {
        let result: Result<BandwidthLimitComponents, _> = "not-a-number".parse();
        assert!(result.is_err());
    }

    #[test]
    fn default_is_unlimited() {
        let c = BandwidthLimitComponents::default();
        assert!(c.is_unlimited());
        assert_eq!(c, BandwidthLimitComponents::unlimited());
    }

    #[test]
    fn clone_equals_original() {
        let c = BandwidthLimitComponents::new(Some(nz(1000)), Some(nz(500)));
        assert_eq!(c.clone(), c);
    }

    #[test]
    fn copy_semantics() {
        let c1 = BandwidthLimitComponents::new(Some(nz(1000)), None);
        let c2 = c1;
        assert_eq!(c1, c2);
    }

    #[test]
    fn debug_format() {
        let c = BandwidthLimitComponents::new(Some(nz(1000)), None);
        let debug = format!("{c:?}");
        assert!(debug.contains("BandwidthLimitComponents"));
    }

    #[test]
    fn equality() {
        let c1 = BandwidthLimitComponents::new(Some(nz(1000)), Some(nz(500)));
        let c2 = BandwidthLimitComponents::new(Some(nz(1000)), Some(nz(500)));
        let c3 = BandwidthLimitComponents::new(Some(nz(2000)), Some(nz(500)));
        assert_eq!(c1, c2);
        assert_ne!(c1, c3);
    }

    #[test]
    fn with_internal_flags_preserves_all_flags() {
        let c = BandwidthLimitComponents::with_internal_flags(
            Some(nz(1000)),
            Some(nz(500)),
            true,
            true,
        );
        assert_eq!(c.rate(), Some(nz(1000)));
        assert_eq!(c.burst(), Some(nz(500)));
        assert!(c.limit_specified());
        assert!(c.burst_specified());
    }
}
