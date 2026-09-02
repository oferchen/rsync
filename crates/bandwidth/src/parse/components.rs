//! Parsed `--bwlimit` rate component with daemon override support.
//!
//! [`BandwidthLimitComponents`] stores the result of parsing a bandwidth
//! argument and records whether the limit was explicitly supplied. This
//! distinction lets daemon modules apply upstream rsync's precedence rule
//! in `options.c:2392` - the strictest byte-per-second rate wins.

use std::num::NonZeroU64;
use std::str::FromStr;

use crate::limiter::{BandwidthLimiter, LimiterChange, apply_effective_limit};

use super::{BandwidthParseError, parse_bandwidth_limit};

/// Parsed `--bwlimit` component consisting of an optional rate.
///
/// In addition to the negotiated byte-per-second rate, the structure records
/// whether the user explicitly supplied the limit. This allows callers to
/// distinguish between inherited defaults and requests such as `--bwlimit=0`
/// that disable throttling while remaining user-driven decisions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BandwidthLimitComponents {
    rate: Option<NonZeroU64>,
    limit_specified: bool,
}

impl BandwidthLimitComponents {
    /// Constructs a new component set from the provided rate.
    ///
    /// When the rate is `None` the combination represents an unlimited
    /// configuration.
    #[must_use]
    pub const fn new(rate: Option<NonZeroU64>) -> Self {
        Self::new_internal(rate, rate.is_some())
    }

    /// Constructs a component set while explicitly controlling the specification flag.
    ///
    /// The helper mirrors upstream precedence rules where callers may need to
    /// distinguish between inherited defaults and user-supplied overrides. When
    /// a rate is provided, the combination always records that a limit was
    /// specified to reflect the caller's intent.
    #[must_use]
    pub const fn new_with_flags(rate: Option<NonZeroU64>, limit_specified: bool) -> Self {
        Self::new_internal(rate, limit_specified)
    }

    /// Returns a component set that disables throttling.
    ///
    /// Upstream rsync treats `--bwlimit=0` as unlimited. Providing an explicit
    /// constructor avoids sprinkling `None` throughout the codebase while making
    /// the intent of "unlimited" limits clear at the call site. The helper is
    /// `const` so it can be used in static initialisers and default values.
    #[must_use]
    pub const fn unlimited() -> Self {
        Self::new_internal(None, false)
    }

    /// Constructs a component set with an explicit specification flag.
    pub(crate) const fn with_internal_flags(
        rate: Option<NonZeroU64>,
        limit_specified: bool,
    ) -> Self {
        Self::new_internal(rate, limit_specified)
    }

    const fn new_internal(rate: Option<NonZeroU64>, limit_specified: bool) -> Self {
        let effective_limit_specified = if rate.is_some() {
            true
        } else {
            limit_specified
        };

        Self {
            rate,
            limit_specified: effective_limit_specified,
        }
    }

    /// Negotiated bytes-per-second rate, or `None` for unlimited.
    pub const fn rate(&self) -> Option<NonZeroU64> {
        self.rate
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
    /// configuration), the method returns `None`.
    pub fn to_limiter(&self) -> Option<BandwidthLimiter> {
        self.rate().map(BandwidthLimiter::new)
    }

    /// Consumes the components and constructs a [`BandwidthLimiter`].
    ///
    /// The behaviour matches [`Self::to_limiter`]; the by-value variant avoids
    /// cloning when the caller wishes to move ownership of the parsed
    /// components.
    pub fn into_limiter(self) -> Option<BandwidthLimiter> {
        self.rate.map(BandwidthLimiter::new)
    }

    /// Applies the component set to an existing limiter, mirroring rsync's precedence rule.
    ///
    /// The helper forwards to [`apply_effective_limit`] so higher layers do not
    /// have to thread the specification flag through their call sites. It
    /// returns the resulting [`LimiterChange`], allowing callers to surface
    /// diagnostics or skip follow-up work when no adjustments were required.
    pub fn apply_to_limiter(&self, limiter: &mut Option<BandwidthLimiter>) -> LimiterChange {
        apply_effective_limit(limiter, self.rate, self.limit_specified)
    }

    /// Returns a new component set that applies an overriding cap to the current configuration.
    ///
    /// The method mirrors upstream rsync's precedence rule when a daemon module defines its own
    /// `bwlimit`: the strictest byte-per-second rate wins. When the override disables throttling
    /// altogether (for example `bwlimit = 0`) the resulting component becomes unlimited, even if
    /// the caller previously supplied a rate. This allows higher layers to reason about the
    /// effective limiter without materialising a [`BandwidthLimiter`] instance solely to combine
    /// configuration sources.
    /// upstream: options.c:2392 - min(client, daemon) wins
    #[must_use]
    pub fn constrained_by(&self, override_components: &Self) -> Self {
        let mut rate = self.rate;
        let limit_specified = self.limit_specified || override_components.limit_specified;

        if override_components.limit_specified {
            if let Some(override_rate) = override_components.rate {
                rate = match rate {
                    Some(existing) => Some(existing.min(override_rate)),
                    None => Some(override_rate),
                };
            } else {
                rate = None;
            }
        }

        Self::new_internal(rate, limit_specified)
    }
}

impl FromStr for BandwidthLimitComponents {
    type Err = BandwidthParseError;

    /// Parses a `--bwlimit` style string into a bandwidth limit component.
    ///
    /// Delegates to [`parse_bandwidth_limit`] to handle rate and suffix parsing.
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
    fn new_with_rate() {
        let c = BandwidthLimitComponents::new(Some(nz(1000)));
        assert_eq!(c.rate(), Some(nz(1000)));
        assert!(c.limit_specified());
    }

    #[test]
    fn new_unlimited() {
        let c = BandwidthLimitComponents::new(None);
        assert!(c.is_unlimited());
        assert!(!c.limit_specified());
    }

    #[test]
    fn unlimited_constructor() {
        let c = BandwidthLimitComponents::unlimited();
        assert_eq!(c.rate(), None);
        assert!(c.is_unlimited());
        assert!(!c.limit_specified());
    }

    #[test]
    fn new_with_flags_without_rate_preserves_limit_specified() {
        // Even without rate, limit_specified flag can be set (an explicit
        // `--bwlimit=0` / unlimited override).
        let c = BandwidthLimitComponents::new_with_flags(None, true);
        assert!(c.limit_specified());
        assert!(c.is_unlimited());
    }

    #[test]
    fn is_unlimited_false_when_rate_present() {
        let c = BandwidthLimitComponents::new(Some(nz(100)));
        assert!(!c.is_unlimited());
    }

    #[test]
    fn to_limiter_returns_none_when_unlimited() {
        let c = BandwidthLimitComponents::unlimited();
        assert!(c.to_limiter().is_none());
    }

    #[test]
    fn to_limiter_returns_limiter_with_rate() {
        let c = BandwidthLimitComponents::new(Some(nz(1000)));
        let limiter = c.to_limiter();
        assert!(limiter.is_some());
        assert_eq!(limiter.unwrap().limit_bytes(), nz(1000));
    }

    #[test]
    fn into_limiter_returns_none_when_unlimited() {
        let c = BandwidthLimitComponents::unlimited();
        assert!(c.into_limiter().is_none());
    }

    #[test]
    fn into_limiter_returns_limiter_with_rate() {
        let c = BandwidthLimitComponents::new(Some(nz(2000)));
        let limiter = c.into_limiter();
        assert!(limiter.is_some());
        assert_eq!(limiter.unwrap().limit_bytes(), nz(2000));
    }

    #[test]
    fn apply_to_limiter_creates_new_limiter() {
        let c = BandwidthLimitComponents::new(Some(nz(1000)));
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
        // upstream: options.c:2392 - min(client, daemon) wins.
        let c1 = BandwidthLimitComponents::new(Some(nz(1000)));
        let c2 = BandwidthLimitComponents::new(Some(nz(500)));
        let constrained = c1.constrained_by(&c2);
        assert_eq!(constrained.rate(), Some(nz(500)));
    }

    #[test]
    fn constrained_by_unlimited_override_disables_limit() {
        let c1 = BandwidthLimitComponents::new(Some(nz(1000)));
        // Create an unlimited override with limit_specified = true
        let c2 = BandwidthLimitComponents::new_with_flags(None, true);
        let constrained = c1.constrained_by(&c2);
        assert!(constrained.is_unlimited());
    }

    #[test]
    fn constrained_by_no_override_preserves_original() {
        let c1 = BandwidthLimitComponents::new(Some(nz(1000)));
        let c2 = BandwidthLimitComponents::unlimited();
        let constrained = c1.constrained_by(&c2);
        assert_eq!(constrained.rate(), Some(nz(1000)));
    }

    #[test]
    fn constrained_by_override_has_higher_rate() {
        // When override rate is higher, keep the lower original rate
        let c1 = BandwidthLimitComponents::new(Some(nz(1000)));
        let c2 = BandwidthLimitComponents::new(Some(nz(2000)));
        let combined = c1.constrained_by(&c2);
        assert_eq!(combined.rate(), Some(nz(1000)));
    }

    #[test]
    fn constrained_by_override_has_lower_rate() {
        // When override rate is lower, use the override rate
        let c1 = BandwidthLimitComponents::new(Some(nz(2000)));
        let c2 = BandwidthLimitComponents::new(Some(nz(1000)));
        let combined = c1.constrained_by(&c2);
        assert_eq!(combined.rate(), Some(nz(1000)));
    }

    #[test]
    fn constrained_by_client_has_no_limit_override_adds() {
        // Client unlimited, override adds limit
        let c1 = BandwidthLimitComponents::unlimited();
        let c2 = BandwidthLimitComponents::new(Some(nz(5000)));
        let combined = c1.constrained_by(&c2);
        assert_eq!(combined.rate(), Some(nz(5000)));
    }

    #[test]
    fn constrained_by_both_unlimited() {
        let c1 = BandwidthLimitComponents::unlimited();
        let c2 = BandwidthLimitComponents::unlimited();
        let combined = c1.constrained_by(&c2);
        assert!(combined.is_unlimited());
        assert!(!combined.limit_specified());
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
    fn from_str_rejects_burst_component() {
        // upstream: no bwlimit accepts a `:BURST` component - it is not a size.
        let result: Result<BandwidthLimitComponents, _> = "100:50".parse();
        assert!(result.is_err());
    }

    #[test]
    fn default_is_unlimited() {
        let c = BandwidthLimitComponents::default();
        assert!(c.is_unlimited());
        assert_eq!(c, BandwidthLimitComponents::unlimited());
    }

    #[test]
    fn copy_semantics() {
        let c1 = BandwidthLimitComponents::new(Some(nz(1000)));
        let c2 = c1;
        assert_eq!(c1, c2);
    }

    #[test]
    fn equality() {
        let c1 = BandwidthLimitComponents::new(Some(nz(1000)));
        let c2 = BandwidthLimitComponents::new(Some(nz(1000)));
        let c3 = BandwidthLimitComponents::new(Some(nz(2000)));
        assert_eq!(c1, c2);
        assert_ne!(c1, c3);
    }
}
