//! Shared enumeration describing compression algorithms supported by the workspace.
//!
//! This module also defines default compression level constants for each
//! algorithm, matching upstream rsync defaults.

use ::core::str::FromStr;
use std::num::NonZeroU8;

use thiserror::Error;

use crate::zlib::CompressionLevel;

/// Default zlib compression level matching upstream rsync.
/// upstream: token.c - `Z_DEFAULT_COMPRESSION` resolves to 6.
pub const ZLIB_DEFAULT_LEVEL: i32 = 6;

/// Maximum zstd compression level accepted by `--compress-level`.
/// upstream: token.c:74 - `ZSTD_maxCLevel()` returns 22 (ultra levels enabled).
pub const ZSTD_MAX_LEVEL: i32 = 22;

/// Default zstd compression level.
/// upstream: token.c - `ZSTD_CLEVEL_DEFAULT` is 3.
pub const ZSTD_DEFAULT_LEVEL: i32 = 3;

/// Fastest zstd compression level used for the `Fast` variant.
pub const ZSTD_FAST_LEVEL: i32 = 1;

/// Best zstd compression level used for the `Best` variant.
/// upstream: token.c - maximum level capped at 19 (ZSTD_maxCLevel without ultra).
pub const ZSTD_BEST_LEVEL: i32 = 19;

/// Default lz4 acceleration factor. Higher values trade compression ratio for speed.
pub const LZ4_DEFAULT_ACCELERATION: i32 = 1;

/// Raw `do_compression_level` value upstream uses when the user did not pass
/// `--compress-level`. upstream: rsync.h:1151 `#define CLVL_NOT_SPECIFIED INT_MIN`.
///
/// [`CompressionAlgorithm::resolve_debug_level`] treats this sentinel as
/// "unspecified" and substitutes the codec's default level, mirroring
/// `token.c:init_compression_level()`.
pub const CLVL_NOT_SPECIFIED: i32 = i32::MIN;

/// Minimum (fastest) zstd compression level, i.e. `ZSTD_minCLevel()`.
///
/// This is a large negative value whose exact magnitude depends on the linked
/// libzstd version, so it is queried at runtime through the safe `zstd` crate
/// API rather than hard-coded. That keeps oc's lower bound identical to the
/// libzstd it is built against, matching upstream, which likewise calls
/// `ZSTD_minCLevel()` at run time.
///
/// upstream: token.c:73 - `min_level = skip_compression_level = ZSTD_minCLevel()`.
#[cfg(feature = "zstd")]
#[must_use]
pub fn zstd_min_level() -> i32 {
    *::zstd::compression_level_range().start()
}

/// Compression algorithms recognised by the workspace.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum CompressionAlgorithm {
    /// Classic zlib/deflate compression.
    Zlib,
    /// LZ4 frame compression (`--compress-choice=lz4`).
    #[cfg(feature = "lz4")]
    Lz4,
    /// Zstandard compression (`--compress-choice=zstd`).
    #[cfg(feature = "zstd")]
    Zstd,
}

impl CompressionAlgorithm {
    /// Returns the canonical display name used for version output and diagnostics.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            CompressionAlgorithm::Zlib => "zlib",
            #[cfg(feature = "lz4")]
            CompressionAlgorithm::Lz4 => "lz4",
            #[cfg(feature = "zstd")]
            CompressionAlgorithm::Zstd => "zstd",
        }
    }

    // upstream: compat.c:valid_compressions_items[] - zstd first, then lz4, zlibx, zlib
    /// Returns the default compression algorithm used when callers enable `--compress`.
    ///
    /// Matches upstream rsync 3.4.1 negotiation precedence: zstd > lz4 > zlib.
    #[must_use]
    pub const fn default_algorithm() -> Self {
        #[cfg(feature = "zstd")]
        {
            CompressionAlgorithm::Zstd
        }
        #[cfg(all(not(feature = "zstd"), feature = "lz4"))]
        {
            CompressionAlgorithm::Lz4
        }
        #[cfg(all(not(feature = "zstd"), not(feature = "lz4")))]
        {
            CompressionAlgorithm::Zlib
        }
    }

    /// Returns the set of algorithms available in the current build.
    #[must_use]
    pub const fn available() -> &'static [CompressionAlgorithm] {
        #[cfg(all(feature = "zstd", feature = "lz4"))]
        {
            const ALGORITHMS: &[CompressionAlgorithm] = &[
                CompressionAlgorithm::Zlib,
                CompressionAlgorithm::Lz4,
                CompressionAlgorithm::Zstd,
            ];
            ALGORITHMS
        }

        #[cfg(all(feature = "zstd", not(feature = "lz4")))]
        {
            const ALGORITHMS: &[CompressionAlgorithm] =
                &[CompressionAlgorithm::Zlib, CompressionAlgorithm::Zstd];
            ALGORITHMS
        }

        #[cfg(all(feature = "lz4", not(feature = "zstd")))]
        {
            const ALGORITHMS: &[CompressionAlgorithm] =
                &[CompressionAlgorithm::Zlib, CompressionAlgorithm::Lz4];
            ALGORITHMS
        }

        #[cfg(all(not(feature = "zstd"), not(feature = "lz4")))]
        {
            const ALGORITHMS: &[CompressionAlgorithm] = &[CompressionAlgorithm::Zlib];
            ALGORITHMS
        }
    }

    /// Clamps a requested `--compress-level` value into this codec's valid
    /// range, mirroring upstream `token.c:init_compression_level()`.
    ///
    /// Upstream never rejects an out-of-range level - it saturates the value to
    /// the codec's `min_level`/`max_level` ("We don't bother with any errors or
    /// warnings -- just make sure that the values are valid."). Returns `None`
    /// when the level disables compression (zlib `off_level` of `0`); `Some`
    /// with the clamped level otherwise. For zstd the returned level may be a
    /// negative [`CompressionLevel::PreciseSigned`], since zstd's range extends
    /// below zero down to `ZSTD_minCLevel()`.
    #[must_use]
    pub fn clamp_level(self, level: i32) -> Option<CompressionLevel> {
        match self {
            CompressionAlgorithm::Zlib => clamp_zlib_level(level).map(CompressionLevel::Precise),
            #[cfg(feature = "lz4")]
            // upstream: token.c:81-87 - lz4 forces min/max/def to 0 and never
            // disables; oc represents this as the fastest expressible level.
            CompressionAlgorithm::Lz4 => Some(CompressionLevel::Precise(clamped(1))),
            // upstream: token.c:72-79,101-104 - reuse the shared resolver so the
            // encoder and the debug-print paths clamp identically. A negative
            // result is preserved as PreciseSigned instead of being raised to 1.
            #[cfg(feature = "zstd")]
            CompressionAlgorithm::Zstd => Some(CompressionLevel::from_signed(
                self.resolve_debug_level(level),
            )),
        }
    }

    /// Resolves the effective compression level upstream renders in the
    /// `--debug=NSTR` compress summary, mirroring `token.c:55`
    /// `init_compression_level()`.
    ///
    /// `raw_level` is the wire `do_compression_level`: [`CLVL_NOT_SPECIFIED`]
    /// when `--compress-level` was not supplied, otherwise the user value.
    /// Upstream substitutes the codec `def_level` for the sentinel (and, for
    /// zstd, for a literal `0`) and saturates a user value into the codec's
    /// `[min_level, max_level]` range *before* the debug print, so it never
    /// emits the raw sentinel. This is the single source of truth for that
    /// resolution, shared by the wire-negotiation and local-copy print paths.
    #[must_use]
    pub fn resolve_debug_level(self, raw_level: i32) -> i32 {
        match self {
            // upstream: token.c:62-70 - zlib/zlibx min 1, max 9 (Z_BEST_COMPRESSION),
            // def 6. CLVL_NOT_SPECIFIED resolves to def; other values saturate.
            CompressionAlgorithm::Zlib => {
                if raw_level == CLVL_NOT_SPECIFIED {
                    ZLIB_DEFAULT_LEVEL
                } else {
                    raw_level.clamp(1, 9)
                }
            }
            // upstream: token.c:72-79,101-104 - def ZSTD_CLEVEL_DEFAULT (3); a
            // literal 0 also maps to def; other values saturate into
            // [ZSTD_minCLevel(), ZSTD_maxCLevel()]. The lower bound is negative,
            // so "fast" levels below 1 (e.g. --compress-level=-5) are preserved,
            // not raised to 1.
            #[cfg(feature = "zstd")]
            CompressionAlgorithm::Zstd => {
                if raw_level == CLVL_NOT_SPECIFIED || raw_level == 0 {
                    ZSTD_DEFAULT_LEVEL
                } else {
                    raw_level.clamp(zstd_min_level(), ZSTD_MAX_LEVEL)
                }
            }
            // upstream: token.c:81-87 - lz4 forces min/max/def to 0.
            #[cfg(feature = "lz4")]
            CompressionAlgorithm::Lz4 => 0,
        }
    }
}

/// Wraps an already-clamped, guaranteed non-zero level.
fn clamped(level: u8) -> NonZeroU8 {
    NonZeroU8::new(level).expect("clamped level is non-zero")
}

/// Clamps into the zlib range, mirroring `token.c:59-70`.
///
/// `Z_DEFAULT_COMPRESSION` (`-1`) remaps to the real default level (6); `0` is
/// upstream's `off_level` and disables compression; all other values saturate
/// to `1..=9`.
fn clamp_zlib_level(level: i32) -> Option<NonZeroU8> {
    if level == -1 {
        return Some(clamped(ZLIB_DEFAULT_LEVEL as u8));
    }
    if level == 0 {
        return None;
    }
    Some(clamped(level.clamp(1, 9) as u8))
}

impl Default for CompressionAlgorithm {
    fn default() -> Self {
        Self::default_algorithm()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn available_algorithms_always_include_zlib() {
        let available = CompressionAlgorithm::available();
        assert!(available.contains(&CompressionAlgorithm::Zlib));
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn available_algorithms_include_zstd_when_feature_enabled() {
        let available = CompressionAlgorithm::available();
        assert!(available.contains(&CompressionAlgorithm::Zstd));
    }

    #[cfg(feature = "lz4")]
    #[test]
    fn available_algorithms_include_lz4_when_feature_enabled() {
        let available = CompressionAlgorithm::available();
        assert!(available.contains(&CompressionAlgorithm::Lz4));
    }

    #[cfg(all(not(feature = "zstd"), not(feature = "lz4")))]
    #[test]
    fn available_algorithms_only_include_zlib_when_no_optional_features_enabled() {
        let available = CompressionAlgorithm::available();
        assert_eq!(available, &[CompressionAlgorithm::Zlib]);
    }

    fn level(algorithm: CompressionAlgorithm, raw: i32) -> Option<i32> {
        algorithm.clamp_level(raw).map(|resolved| match resolved {
            CompressionLevel::None => 0,
            CompressionLevel::Fast => ZSTD_FAST_LEVEL,
            CompressionLevel::Default => ZSTD_DEFAULT_LEVEL,
            CompressionLevel::Best => ZSTD_BEST_LEVEL,
            CompressionLevel::Precise(value) => i32::from(value.get()),
            CompressionLevel::PreciseSigned(value) => value,
        })
    }

    #[test]
    fn zlib_clamp_saturates_and_disables_like_upstream() {
        // upstream: token.c:init_compression_level() zlib branch.
        assert_eq!(level(CompressionAlgorithm::Zlib, 0), None, "0 disables");
        assert_eq!(
            level(CompressionAlgorithm::Zlib, -1),
            Some(6),
            "-1 = default"
        );
        assert_eq!(level(CompressionAlgorithm::Zlib, -5), Some(1), "below min");
        assert_eq!(level(CompressionAlgorithm::Zlib, 5), Some(5), "in range");
        assert_eq!(level(CompressionAlgorithm::Zlib, 9), Some(9), "max");
        assert_eq!(level(CompressionAlgorithm::Zlib, 10), Some(9), "above max");
        assert_eq!(
            level(CompressionAlgorithm::Zlib, 99),
            Some(9),
            "far above max"
        );
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn zstd_clamp_uses_wider_range_like_upstream() {
        // upstream: token.c:init_compression_level() zstd branch - 0 selects the
        // default (3), max_level is ZSTD_maxCLevel() (22), and 0 never disables.
        assert_eq!(level(CompressionAlgorithm::Zstd, 0), Some(3), "0 = default");
        assert_eq!(level(CompressionAlgorithm::Zstd, 15), Some(15), "in range");
        assert_eq!(level(CompressionAlgorithm::Zstd, 22), Some(22), "max");
        assert_eq!(level(CompressionAlgorithm::Zstd, 99), Some(22), "above max");
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn zstd_clamp_preserves_negative_levels_to_the_encoder() {
        // WHY: the encoder path (clamp_level) - not just the debug-print path -
        // must carry a negative zstd "fast" level all the way to
        // ZSTD_c_compressionLevel. Guards against the historical unsigned
        // NonZeroU8 clamp that rewrote every negative to 1. upstream:
        // token.c:73,101-102 - the lower bound is ZSTD_minCLevel(), not 1.
        assert_eq!(
            level(CompressionAlgorithm::Zstd, -5),
            Some(-5),
            "-5 survives clamp_level as a signed encoder level"
        );
        let min = zstd_min_level();
        assert_eq!(
            level(CompressionAlgorithm::Zstd, min),
            Some(min),
            "the exact ZSTD_minCLevel() boundary is preserved"
        );
        assert_eq!(
            level(CompressionAlgorithm::Zstd, min - 1),
            Some(min),
            "below ZSTD_minCLevel() saturates UP to the min, never rejected"
        );
    }

    #[test]
    fn parsing_accepts_known_algorithms() {
        assert_eq!(
            "zlib".parse::<CompressionAlgorithm>().unwrap(),
            CompressionAlgorithm::Zlib
        );
        assert_eq!(
            "zlibx".parse::<CompressionAlgorithm>().unwrap(),
            CompressionAlgorithm::Zlib
        );
    }

    #[cfg(feature = "lz4")]
    #[test]
    fn parsing_accepts_lz4() {
        assert_eq!(
            "lz4".parse::<CompressionAlgorithm>().unwrap(),
            CompressionAlgorithm::Lz4
        );
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn parsing_accepts_zstd() {
        assert_eq!(
            "zstd".parse::<CompressionAlgorithm>().unwrap(),
            CompressionAlgorithm::Zstd
        );
    }

    #[test]
    fn parsing_rejects_unknown_algorithms() {
        let err = "brotli"
            .parse::<CompressionAlgorithm>()
            .expect_err("brotli unsupported");
        assert_eq!(err.input(), "brotli");
    }

    #[test]
    fn compression_algorithm_name_zlib() {
        assert_eq!(CompressionAlgorithm::Zlib.name(), "zlib");
    }

    #[cfg(feature = "lz4")]
    #[test]
    fn compression_algorithm_name_lz4() {
        assert_eq!(CompressionAlgorithm::Lz4.name(), "lz4");
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn compression_algorithm_name_zstd() {
        assert_eq!(CompressionAlgorithm::Zstd.name(), "zstd");
    }

    #[test]
    fn default_algorithm_matches_upstream_precedence() {
        let default = CompressionAlgorithm::default_algorithm();
        #[cfg(feature = "zstd")]
        assert_eq!(default, CompressionAlgorithm::Zstd);
        #[cfg(all(not(feature = "zstd"), feature = "lz4"))]
        assert_eq!(default, CompressionAlgorithm::Lz4);
        #[cfg(all(not(feature = "zstd"), not(feature = "lz4")))]
        assert_eq!(default, CompressionAlgorithm::Zlib);
        assert_eq!(CompressionAlgorithm::default(), default);
    }

    #[test]
    fn compression_algorithm_clone() {
        let algo = CompressionAlgorithm::Zlib;
        let cloned = algo;
        assert_eq!(algo, cloned);
    }

    #[test]
    fn compression_algorithm_copy() {
        let algo = CompressionAlgorithm::Zlib;
        let copied = algo;
        assert_eq!(algo, copied);
    }

    #[test]
    fn compression_algorithm_debug() {
        let debug = format!("{:?}", CompressionAlgorithm::Zlib);
        assert!(debug.contains("Zlib"));
    }

    #[test]
    fn compression_algorithm_eq() {
        assert_eq!(CompressionAlgorithm::Zlib, CompressionAlgorithm::Zlib);
    }

    #[test]
    fn compression_algorithm_hash() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(CompressionAlgorithm::Zlib);
        assert!(set.contains(&CompressionAlgorithm::Zlib));
    }

    #[test]
    fn parsing_trims_whitespace() {
        assert_eq!(
            "  zlib  ".parse::<CompressionAlgorithm>().unwrap(),
            CompressionAlgorithm::Zlib
        );
    }

    #[test]
    fn parsing_case_insensitive() {
        assert_eq!(
            "ZLIB".parse::<CompressionAlgorithm>().unwrap(),
            CompressionAlgorithm::Zlib
        );
        assert_eq!(
            "ZlIb".parse::<CompressionAlgorithm>().unwrap(),
            CompressionAlgorithm::Zlib
        );
    }

    #[test]
    fn parse_error_new() {
        let error = CompressionAlgorithmParseError::new("test");
        assert_eq!(error.input(), "test");
    }

    #[test]
    fn parse_error_display() {
        let error = CompressionAlgorithmParseError::new("invalid");
        let display = error.to_string();
        assert!(display.contains("invalid"));
        assert!(display.contains("unsupported"));
    }

    #[test]
    fn parse_error_debug() {
        let error = CompressionAlgorithmParseError::new("test");
        let debug = format!("{error:?}");
        assert!(debug.contains("CompressionAlgorithmParseError"));
    }

    #[test]
    fn parse_error_eq() {
        let a = CompressionAlgorithmParseError::new("test");
        let b = CompressionAlgorithmParseError::new("test");
        let c = CompressionAlgorithmParseError::new("other");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn parse_error_clone() {
        let error = CompressionAlgorithmParseError::new("test");
        let cloned = error.clone();
        assert_eq!(error, cloned);
    }

    #[test]
    fn available_is_not_empty() {
        assert!(!CompressionAlgorithm::available().is_empty());
    }

    #[test]
    fn zlib_default_level_matches_upstream() {
        assert_eq!(ZLIB_DEFAULT_LEVEL, 6);
    }

    #[test]
    fn zstd_default_level_matches_upstream() {
        assert_eq!(ZSTD_DEFAULT_LEVEL, 3);
    }

    #[test]
    fn zstd_fast_level_is_minimum_positive() {
        assert_eq!(ZSTD_FAST_LEVEL, 1);
    }

    #[test]
    fn zstd_best_level_matches_upstream_max() {
        assert_eq!(ZSTD_BEST_LEVEL, 19);
    }

    #[test]
    fn lz4_default_acceleration_is_standard() {
        assert_eq!(LZ4_DEFAULT_ACCELERATION, 1);
    }

    #[test]
    fn resolve_debug_level_substitutes_zlib_default_for_sentinel() {
        // upstream: token.c:66-69,93-94 - CLVL_NOT_SPECIFIED resolves to the
        // zlib def_level (6), never the raw INT_MIN sentinel.
        assert_eq!(
            CompressionAlgorithm::Zlib.resolve_debug_level(CLVL_NOT_SPECIFIED),
            6
        );
    }

    #[test]
    fn resolve_debug_level_saturates_zlib_into_range() {
        // upstream: token.c:101-104 - values saturate into [1, 9].
        assert_eq!(CompressionAlgorithm::Zlib.resolve_debug_level(9), 9);
        assert_eq!(CompressionAlgorithm::Zlib.resolve_debug_level(42), 9);
        assert_eq!(CompressionAlgorithm::Zlib.resolve_debug_level(-5), 1);
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn resolve_debug_level_substitutes_zstd_default() {
        // upstream: token.c:75-78,93-94 - both CLVL_NOT_SPECIFIED and a literal
        // 0 resolve to ZSTD_CLEVEL_DEFAULT (3).
        assert_eq!(
            CompressionAlgorithm::Zstd.resolve_debug_level(CLVL_NOT_SPECIFIED),
            3
        );
        assert_eq!(CompressionAlgorithm::Zstd.resolve_debug_level(0), 3);
        assert_eq!(CompressionAlgorithm::Zstd.resolve_debug_level(19), 19);
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn zstd_min_level_is_negative() {
        // upstream: token.c:73 - zstd's min_level is ZSTD_minCLevel(), which
        // libzstd documents as "a very large negative number". If this were
        // >= 1 the negative-level fix below would be silently meaningless.
        assert!(
            zstd_min_level() < 0,
            "ZSTD_minCLevel() must be negative, got {}",
            zstd_min_level()
        );
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn resolve_debug_level_preserves_negative_zstd_levels() {
        // upstream: token.c:73,101-104 - zstd's lower clamp bound is
        // ZSTD_minCLevel() (negative), NOT 1. A "fast" level such as
        // --compress-level=-5 is a valid zstd level that upstream passes
        // straight to ZSTD_c_compressionLevel; it must be preserved, never
        // raised to 1. Regression guard for the old `.clamp(1, ..)` bound that
        // silently rewrote every negative level to 1.
        assert_eq!(CompressionAlgorithm::Zstd.resolve_debug_level(-5), -5);
        let min = zstd_min_level();
        assert_eq!(
            CompressionAlgorithm::Zstd.resolve_debug_level(min),
            min,
            "the exact ZSTD_minCLevel() boundary is preserved"
        );
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn resolve_debug_level_clamps_below_min_to_zstd_min() {
        // upstream: token.c:101-102 - a level below min_level saturates UP to
        // min_level (ZSTD_minCLevel()); upstream never rejects it. Mirrors the
        // zlib below-min behaviour but at zstd's negative floor.
        let min = zstd_min_level();
        assert_eq!(
            CompressionAlgorithm::Zstd.resolve_debug_level(min - 1),
            min,
            "below-min saturates to ZSTD_minCLevel(), not rejected"
        );
        // The i32::MIN sentinel is CLVL_NOT_SPECIFIED, which resolves to the
        // codec default (3) - it is not treated as a below-min level.
        assert_eq!(
            CompressionAlgorithm::Zstd.resolve_debug_level(CLVL_NOT_SPECIFIED),
            ZSTD_DEFAULT_LEVEL
        );
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn resolve_debug_level_positive_and_zero_zstd_unchanged() {
        // No regression: positive levels, the default substitution for 0, and
        // the upper clamp to ZSTD_maxCLevel (22) are all unchanged by the
        // negative-level fix. upstream: token.c:74,77-78,103-104.
        assert_eq!(CompressionAlgorithm::Zstd.resolve_debug_level(0), 3);
        assert_eq!(CompressionAlgorithm::Zstd.resolve_debug_level(1), 1);
        assert_eq!(CompressionAlgorithm::Zstd.resolve_debug_level(15), 15);
        assert_eq!(CompressionAlgorithm::Zstd.resolve_debug_level(22), 22);
        assert_eq!(CompressionAlgorithm::Zstd.resolve_debug_level(99), 22);
    }

    #[cfg(feature = "lz4")]
    #[test]
    fn resolve_debug_level_lz4_is_zero() {
        // upstream: token.c:83-86 - lz4 min/max/def are all 0.
        assert_eq!(
            CompressionAlgorithm::Lz4.resolve_debug_level(CLVL_NOT_SPECIFIED),
            0
        );
        assert_eq!(CompressionAlgorithm::Lz4.resolve_debug_level(5), 0);
    }

    #[test]
    fn compression_level_ordering() {
        let (fast, default, best) = (ZSTD_FAST_LEVEL, ZSTD_DEFAULT_LEVEL, ZSTD_BEST_LEVEL);
        assert!(fast < default);
        assert!(default < best);
    }
}

/// Error returned when attempting to parse an unsupported compression algorithm.
#[derive(Clone, Debug, Eq, PartialEq, Error)]
#[error("unsupported compression algorithm: {input}")]
pub struct CompressionAlgorithmParseError {
    input: String,
}

impl CompressionAlgorithmParseError {
    /// Creates a parse error capturing the original input.
    #[must_use]
    pub fn new(input: impl Into<String>) -> Self {
        Self {
            input: input.into(),
        }
    }

    /// Input string that failed to parse as a compression algorithm.
    #[must_use]
    pub fn input(&self) -> &str {
        &self.input
    }
}

impl FromStr for CompressionAlgorithm {
    type Err = CompressionAlgorithmParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "zlib" | "zlibx" => Ok(CompressionAlgorithm::Zlib),
            #[cfg(feature = "lz4")]
            "lz4" => Ok(CompressionAlgorithm::Lz4),
            #[cfg(feature = "zstd")]
            "zstd" => Ok(CompressionAlgorithm::Zstd),
            other => Err(CompressionAlgorithmParseError::new(other.to_owned())),
        }
    }
}
