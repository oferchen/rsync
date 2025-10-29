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
        let effective_burst_specified = effective_burst.is_some() && burst_specified;

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
