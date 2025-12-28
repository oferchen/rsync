use std::ffi::OsString;
use std::num::NonZeroU64;

use crate::bandwidth::{self, BandwidthLimiter, BandwidthParseError};

/// Bandwidth limit expressed in bytes per second.
///
/// # Examples
/// ```
/// use core::client::BandwidthLimit;
/// use std::num::NonZeroU64;
///
/// let limit = BandwidthLimit::from_bytes_per_second(NonZeroU64::new(1024).unwrap());
/// assert_eq!(limit.bytes_per_second().get(), 1024);
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BandwidthLimit {
    bytes_per_second: NonZeroU64,
    burst_bytes: Option<NonZeroU64>,
    burst_specified: bool,
}

impl BandwidthLimit {
    const fn new_internal(
        bytes_per_second: NonZeroU64,
        burst: Option<NonZeroU64>,
        burst_specified: bool,
    ) -> Self {
        Self {
            bytes_per_second,
            burst_bytes: burst,
            burst_specified,
        }
    }

    /// Creates a new [`BandwidthLimit`] from the supplied byte-per-second value.
    #[must_use]
    pub const fn from_bytes_per_second(bytes_per_second: NonZeroU64) -> Self {
        Self::new_internal(bytes_per_second, None, false)
    }

    /// Creates a new [`BandwidthLimit`] from a rate and optional burst size.
    #[must_use]
    pub const fn from_rate_and_burst(
        bytes_per_second: NonZeroU64,
        burst: Option<NonZeroU64>,
    ) -> Self {
        Self::new_internal(bytes_per_second, burst, burst.is_some())
    }

    /// Converts parsed [`bandwidth::BandwidthLimitComponents`] into a
    /// [`BandwidthLimit`].
    ///
    /// Returning `None` mirrors upstream rsync's interpretation of `0` as an
    /// unlimited rate. Callers that parse `--bwlimit` arguments can therefore
    /// reuse the shared decoding logic and only materialise a [`BandwidthLimit`]
    /// when throttling is active.
    #[must_use]
    pub const fn from_components(components: bandwidth::BandwidthLimitComponents) -> Option<Self> {
        match components.rate() {
            Some(rate) => Some(Self::new_internal(
                rate,
                components.burst(),
                components.burst_specified(),
            )),
            None => None,
        }
    }

    /// Parses a textual `--bwlimit` value into an optional [`BandwidthLimit`].
    pub fn parse(text: &str) -> Result<Option<Self>, BandwidthParseError> {
        let components = bandwidth::parse_bandwidth_limit(text)?;
        Ok(Self::from_components(components))
    }

    /// Returns the configured rate in bytes per second.
    #[must_use]
    pub const fn bytes_per_second(self) -> NonZeroU64 {
        self.bytes_per_second
    }

    /// Returns the configured burst size in bytes, if any.
    #[must_use]
    pub const fn burst_bytes(self) -> Option<NonZeroU64> {
        self.burst_bytes
    }

    /// Indicates whether a burst component was explicitly specified.
    #[must_use]
    pub const fn burst_specified(self) -> bool {
        self.burst_specified
    }

    /// Produces the shared [`bandwidth::BandwidthLimitComponents`] representation
    /// for this limit.
    ///
    /// The conversion retains both the byte-per-second rate and the optional burst
    /// component so higher layers can forward the configuration to helpers that
    /// operate on the shared parsing type. Returning a dedicated value keeps the
    /// conversion explicit while avoiding the need for callers to reach into the
    /// `bandwidth` crate directly when they already hold a [`BandwidthLimit`].
    #[must_use]
    pub const fn components(&self) -> bandwidth::BandwidthLimitComponents {
        bandwidth::BandwidthLimitComponents::new_with_specified(
            Some(self.bytes_per_second),
            self.burst_bytes,
            self.burst_specified,
        )
    }

    /// Consumes the limit and returns the
    /// [`bandwidth::BandwidthLimitComponents`] representation.
    ///
    /// This by-value variant mirrors [`Self::components`] for callers that want
    /// to forward the components without keeping the original [`BandwidthLimit`]
    /// instance alive.
    #[must_use]
    pub const fn into_components(self) -> bandwidth::BandwidthLimitComponents {
        self.components()
    }

    /// Constructs a [`BandwidthLimiter`] that enforces this configuration.
    ///
    /// The limiter mirrors upstream rsync's token bucket by combining the
    /// configured rate with the optional burst component. Returning a concrete
    /// limiter keeps higher layers from re-encoding the rate/burst tuple when
    /// they need to apply throttling to local copies or daemon transfers.
    #[must_use]
    pub fn to_limiter(&self) -> BandwidthLimiter {
        BandwidthLimiter::with_burst(self.bytes_per_second, self.burst_bytes)
    }

    /// Consumes the limit and produces a [`BandwidthLimiter`].
    ///
    /// This by-value variant mirrors [`Self::to_limiter`] while avoiding the
    /// additional copy of the [`BandwidthLimit`] structure when the caller no
    /// longer needs it.
    #[must_use]
    pub fn into_limiter(self) -> BandwidthLimiter {
        self.to_limiter()
    }

    /// Returns the sanitised `--bwlimit` argument expected by legacy fallbacks.
    ///
    /// When delegating remote transfers to the system `rsync` binary we must
    /// forward the throttling setting using the byte-per-second form accepted by
    /// upstream releases. This helper mirrors the formatting performed by
    /// upstream `rsync` when normalising parsed limits, ensuring fallback
    /// invocations receive identical values.
    #[must_use]
    pub fn fallback_argument(&self) -> OsString {
        let mut value = self.bytes_per_second.get().to_string();
        if self.burst_specified {
            value.push(':');
            value.push_str(
                &self
                    .burst_bytes
                    .map_or_else(|| "0".to_owned(), |burst| burst.get().to_string()),
            );
        }

        OsString::from(value)
    }

    /// Returns the argument that disables bandwidth limiting for fallbacks.
    #[must_use]
    pub fn fallback_unlimited_argument() -> OsString {
        OsString::from("0")
    }
}

impl From<BandwidthLimit> for bandwidth::BandwidthLimitComponents {
    fn from(limit: BandwidthLimit) -> Self {
        limit.into_components()
    }
}

impl From<&BandwidthLimit> for bandwidth::BandwidthLimitComponents {
    fn from(limit: &BandwidthLimit) -> Self {
        limit.components()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod bandwidth_limit_tests {
        use super::*;

        #[test]
        fn from_bytes_per_second() {
            let limit = BandwidthLimit::from_bytes_per_second(NonZeroU64::new(1024).unwrap());
            assert_eq!(limit.bytes_per_second().get(), 1024);
            assert!(limit.burst_bytes().is_none());
            assert!(!limit.burst_specified());
        }

        #[test]
        fn from_rate_and_burst_without_burst() {
            let limit = BandwidthLimit::from_rate_and_burst(NonZeroU64::new(2048).unwrap(), None);
            assert_eq!(limit.bytes_per_second().get(), 2048);
            assert!(limit.burst_bytes().is_none());
            assert!(!limit.burst_specified());
        }

        #[test]
        fn from_rate_and_burst_with_burst() {
            let limit = BandwidthLimit::from_rate_and_burst(
                NonZeroU64::new(1024).unwrap(),
                Some(NonZeroU64::new(4096).unwrap()),
            );
            assert_eq!(limit.bytes_per_second().get(), 1024);
            assert_eq!(limit.burst_bytes().unwrap().get(), 4096);
            assert!(limit.burst_specified());
        }

        #[test]
        fn from_components_with_rate() {
            let components =
                bandwidth::BandwidthLimitComponents::new(Some(NonZeroU64::new(512).unwrap()), None);
            let limit = BandwidthLimit::from_components(components);
            assert!(limit.is_some());
            assert_eq!(limit.unwrap().bytes_per_second().get(), 512);
        }

        #[test]
        fn from_components_without_rate() {
            let components = bandwidth::BandwidthLimitComponents::new(None, None);
            let limit = BandwidthLimit::from_components(components);
            assert!(limit.is_none());
        }

        #[test]
        fn parse_simple_value() {
            // Default unit is kilobytes, so 1024 = 1024 * 1024 bytes
            let result = BandwidthLimit::parse("1024").unwrap();
            assert!(result.is_some());
            let limit = result.unwrap();
            assert_eq!(limit.bytes_per_second().get(), 1024 * 1024);
        }

        #[test]
        fn parse_with_suffix_k() {
            let result = BandwidthLimit::parse("100k").unwrap();
            assert!(result.is_some());
            let limit = result.unwrap();
            assert_eq!(limit.bytes_per_second().get(), 100 * 1024);
        }

        #[test]
        fn parse_with_suffix_m() {
            let result = BandwidthLimit::parse("1m").unwrap();
            assert!(result.is_some());
            let limit = result.unwrap();
            assert_eq!(limit.bytes_per_second().get(), 1024 * 1024);
        }

        #[test]
        fn parse_zero_returns_none() {
            let result = BandwidthLimit::parse("0").unwrap();
            assert!(result.is_none());
        }

        #[test]
        fn clone_and_copy() {
            let limit = BandwidthLimit::from_bytes_per_second(NonZeroU64::new(500).unwrap());
            let cloned = limit;
            let copied = limit;
            assert_eq!(limit, cloned);
            assert_eq!(limit, copied);
        }

        #[test]
        fn debug_format() {
            let limit = BandwidthLimit::from_bytes_per_second(NonZeroU64::new(1000).unwrap());
            let debug = format!("{limit:?}");
            assert!(debug.contains("BandwidthLimit"));
            assert!(debug.contains("1000"));
        }

        #[test]
        fn components() {
            let limit = BandwidthLimit::from_rate_and_burst(
                NonZeroU64::new(1024).unwrap(),
                Some(NonZeroU64::new(2048).unwrap()),
            );
            let components = limit.components();
            assert_eq!(components.rate().unwrap().get(), 1024);
            assert_eq!(components.burst().unwrap().get(), 2048);
        }

        #[test]
        fn into_components() {
            let limit = BandwidthLimit::from_bytes_per_second(NonZeroU64::new(256).unwrap());
            let components = limit.into_components();
            assert_eq!(components.rate().unwrap().get(), 256);
        }

        #[test]
        fn to_limiter() {
            let limit = BandwidthLimit::from_bytes_per_second(NonZeroU64::new(1024).unwrap());
            let _limiter = limit.to_limiter(); // Just ensure no panic
        }

        #[test]
        fn into_limiter() {
            let limit = BandwidthLimit::from_bytes_per_second(NonZeroU64::new(1024).unwrap());
            let _limiter = limit.into_limiter(); // Just ensure no panic
        }

        #[test]
        fn fallback_argument_simple() {
            let limit = BandwidthLimit::from_bytes_per_second(NonZeroU64::new(1024).unwrap());
            assert_eq!(limit.fallback_argument(), OsString::from("1024"));
        }

        #[test]
        fn fallback_argument_with_burst() {
            let limit = BandwidthLimit::from_rate_and_burst(
                NonZeroU64::new(1024).unwrap(),
                Some(NonZeroU64::new(2048).unwrap()),
            );
            assert_eq!(limit.fallback_argument(), OsString::from("1024:2048"));
        }

        #[test]
        fn fallback_unlimited_argument() {
            assert_eq!(
                BandwidthLimit::fallback_unlimited_argument(),
                OsString::from("0")
            );
        }

        #[test]
        fn from_bandwidth_limit_impl() {
            let limit = BandwidthLimit::from_bytes_per_second(NonZeroU64::new(100).unwrap());
            let components: bandwidth::BandwidthLimitComponents = limit.into();
            assert_eq!(components.rate().unwrap().get(), 100);
        }

        #[test]
        fn from_bandwidth_limit_ref_impl() {
            let limit = BandwidthLimit::from_bytes_per_second(NonZeroU64::new(200).unwrap());
            let components: bandwidth::BandwidthLimitComponents = (&limit).into();
            assert_eq!(components.rate().unwrap().get(), 200);
        }
    }
}
