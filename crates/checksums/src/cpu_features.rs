//! Runtime SIMD level override for checksum dispatch.
//!
//! This module exposes a process-global override that lets callers pin SIMD
//! dispatch to a specific instruction-set level (or force the scalar fallback)
//! regardless of what the host CPU advertises. The CLI uses this to honour
//! the `--simd=<level>` flag, and benchmarks/tests use it to exercise specific
//! code paths.
//!
//! # Semantics
//!
//! - [`SimdLevel::Auto`] keeps the existing CPUID-based selection.
//! - Any other variant caps the dispatcher at the requested level. The
//!   dispatcher still verifies CPU support before activating a backend, so an
//!   override that is wider than the host's capabilities silently degrades to
//!   the next available backend.
//! - [`SimdLevel::None`] forces the scalar reference path on every dispatcher.
//!
//! The override is set once via [`set_simd_override`] and read by every
//! checksum dispatcher (rolling `x86`/`neon`, MD5 batch dispatcher) before
//! consulting CPUID. Subsequent calls to [`set_simd_override`] succeed only
//! when the value matches the previously set one - the override is a
//! one-shot, process-global handshake established at startup. Tests can
//! reset the override through the gated [`reset_simd_override_for_tests`]
//! helper.
//!
//! The override is stored in a process-global [`AtomicU8`] so reads are
//! lock-free on every dispatch.

use std::sync::atomic::{AtomicU8, Ordering};

/// SIMD instruction-set level the dispatcher should target.
///
/// `Auto` lets the dispatcher choose the widest backend the host CPU supports.
/// Every other variant caps dispatch at that level (or forces scalar). The
/// variants intentionally mirror the CLI surface: AVX-512, AVX2, SSE4 (x86
/// only), NEON (aarch64 only), and `None` for scalar.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SimdLevel {
    /// Use CPUID-based detection (default behaviour).
    #[default]
    Auto,
    /// Cap dispatch at AVX-512 (x86_64 only). Falls back to lower SIMD or
    /// scalar on architectures without AVX-512.
    Avx512,
    /// Cap dispatch at AVX2 (x86_64 only). Falls back to scalar on other
    /// architectures.
    Avx2,
    /// Cap dispatch at SSE4-class instructions (x86_64 only). Maps to the
    /// SSE4.1/SSSE3/SSE2 family for the MD5 dispatcher and to SSE2 for the
    /// rolling checksum (which has no SSE4 path).
    Sse4,
    /// Cap dispatch at NEON (aarch64 only). Falls back to scalar on other
    /// architectures.
    Neon,
    /// Force the scalar reference path for every dispatcher.
    None,
}

impl SimdLevel {
    /// Parses a SIMD level from its CLI spelling.
    ///
    /// Accepts the canonical lower-case forms (`auto`, `avx512`, `avx2`,
    /// `sse4`, `neon`, `none`) plus a small set of aliases (e.g. `avx-512`,
    /// `sse4.1`) for human convenience. Returns `None` for unrecognised
    /// spellings so the caller can render a CLI-appropriate error.
    pub fn parse_cli(value: &str) -> Option<Self> {
        let normalized: String = value
            .chars()
            .filter(|c| !matches!(c, '-' | '_' | '.'))
            .flat_map(char::to_lowercase)
            .collect();

        match normalized.as_str() {
            "auto" => Some(Self::Auto),
            "avx512" | "avx512f" | "avx512bw" => Some(Self::Avx512),
            "avx2" => Some(Self::Avx2),
            "sse4" | "sse41" | "sse4a" | "ssse3" | "sse2" => Some(Self::Sse4),
            "neon" | "asimd" => Some(Self::Neon),
            "none" | "off" | "scalar" | "disabled" => Some(Self::None),
            _ => None,
        }
    }

    /// Returns the canonical CLI spelling for this level.
    #[must_use]
    pub const fn as_cli_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Avx512 => "avx512",
            Self::Avx2 => "avx2",
            Self::Sse4 => "sse4",
            Self::Neon => "neon",
            Self::None => "none",
        }
    }
}

/// Subset of SIMD capabilities the dispatcher consults.
///
/// Each variant maps to a CPU feature gate the dispatcher verifies before
/// activating a backend. The override layer answers
/// [`feature_allowed`] by intersecting the active
/// override with the host's CPUID-detected capabilities.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SimdFeature {
    /// AVX-512F + AVX-512BW (16-lane MD5 backend).
    Avx512,
    /// AVX2 (rolling checksum 32-byte loop, 8-lane MD5 backend).
    Avx2,
    /// SSE4.1 (4-lane MD5 backend with `blendv`).
    Sse41,
    /// SSSE3 (4-lane MD5 backend with `pshufb`).
    Ssse3,
    /// SSE2 baseline (rolling checksum 16-byte loop, 4-lane MD5 backend).
    Sse2,
    /// ARM NEON (aarch64 4-lane MD5 backend, 16-byte rolling-checksum loop).
    Neon,
}

/// Sentinel byte indicating the override has not been installed.
///
/// Distinct from `SimdLevel::Auto as u8` so callers can distinguish "never
/// set" (treated as `Auto` for dispatch) from "explicitly set to `Auto`".
const UNINIT: u8 = u8::MAX;

static OVERRIDE: AtomicU8 = AtomicU8::new(UNINIT);

#[inline]
const fn level_to_byte(level: SimdLevel) -> u8 {
    match level {
        SimdLevel::Auto => 0,
        SimdLevel::Avx512 => 1,
        SimdLevel::Avx2 => 2,
        SimdLevel::Sse4 => 3,
        SimdLevel::Neon => 4,
        SimdLevel::None => 5,
    }
}

#[inline]
fn byte_to_level(byte: u8) -> Option<SimdLevel> {
    Some(match byte {
        0 => SimdLevel::Auto,
        1 => SimdLevel::Avx512,
        2 => SimdLevel::Avx2,
        3 => SimdLevel::Sse4,
        4 => SimdLevel::Neon,
        5 => SimdLevel::None,
        _ => return None,
    })
}

/// Installs the process-wide SIMD level override.
///
/// Call this exactly once during startup, before any checksum dispatcher is
/// consulted. Subsequent calls are accepted only when they request the same
/// level, otherwise [`Err`] carries the previously-set value so the caller
/// can render a diagnostic.
///
/// # Errors
///
/// Returns the previously-installed level when called more than once with a
/// different value.
pub fn set_simd_override(level: SimdLevel) -> Result<(), SimdLevel> {
    let new_byte = level_to_byte(level);
    match OVERRIDE.compare_exchange(UNINIT, new_byte, Ordering::SeqCst, Ordering::SeqCst) {
        Ok(_) => Ok(()),
        Err(existing_byte) => {
            let existing = byte_to_level(existing_byte).unwrap_or(SimdLevel::Auto);
            if existing == level {
                Ok(())
            } else {
                Err(existing)
            }
        }
    }
}

/// Returns the active SIMD override, defaulting to [`SimdLevel::Auto`].
#[must_use]
pub fn simd_override() -> SimdLevel {
    let byte = OVERRIDE.load(Ordering::SeqCst);
    if byte == UNINIT {
        SimdLevel::Auto
    } else {
        byte_to_level(byte).unwrap_or(SimdLevel::Auto)
    }
}

/// Clears any installed override and reinstalls the supplied value.
///
/// Reserved for tests; production code must use [`set_simd_override`] which
/// preserves the one-shot contract. The helper is `#[doc(hidden)]` and lives
/// behind a name that telegraphs its intended caller.
#[doc(hidden)]
pub fn reset_simd_override_for_tests(level: SimdLevel) {
    OVERRIDE.store(level_to_byte(level), Ordering::SeqCst);
}

/// Clears the override entirely so subsequent reads see [`SimdLevel::Auto`].
#[doc(hidden)]
pub fn clear_simd_override_for_tests() {
    OVERRIDE.store(UNINIT, Ordering::SeqCst);
}

/// Reports whether the requested SIMD feature is permitted by the active
/// override.
///
/// A `true` result means the dispatcher may activate this backend if the host
/// CPU also exposes the feature; the dispatcher must still consult CPUID. A
/// `false` result means the override forbids this feature regardless of CPU
/// support.
///
/// The mapping mirrors the CLI semantics:
///
/// | Override     | Avx512 | Avx2 | Sse41 | Ssse3 | Sse2 | Neon |
/// |--------------|:------:|:----:|:-----:|:-----:|:----:|:----:|
/// | `Auto`       | yes    | yes  | yes   | yes   | yes  | yes  |
/// | `Avx512`     | yes    | yes  | yes   | yes   | yes  | yes  |
/// | `Avx2`       | no     | yes  | yes   | yes   | yes  | no   |
/// | `Sse4`       | no     | no   | yes   | yes   | yes  | no   |
/// | `Neon`       | no     | no   | no    | no    | no   | yes  |
/// | `None`       | no     | no   | no    | no    | no   | no   |
#[must_use]
pub fn feature_allowed(feature: SimdFeature) -> bool {
    match (simd_override(), feature) {
        (SimdLevel::Auto, _) => true,
        (SimdLevel::None, _) => false,

        (SimdLevel::Avx512, _) => true,

        (SimdLevel::Avx2, SimdFeature::Avx512 | SimdFeature::Neon) => false,
        (SimdLevel::Avx2, _) => true,

        (SimdLevel::Sse4, SimdFeature::Sse41 | SimdFeature::Ssse3 | SimdFeature::Sse2) => true,
        (SimdLevel::Sse4, _) => false,

        (SimdLevel::Neon, SimdFeature::Neon) => true,
        (SimdLevel::Neon, _) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_canonical_cli_spellings() {
        assert_eq!(SimdLevel::parse_cli("auto"), Some(SimdLevel::Auto));
        assert_eq!(SimdLevel::parse_cli("avx512"), Some(SimdLevel::Avx512));
        assert_eq!(SimdLevel::parse_cli("avx2"), Some(SimdLevel::Avx2));
        assert_eq!(SimdLevel::parse_cli("sse4"), Some(SimdLevel::Sse4));
        assert_eq!(SimdLevel::parse_cli("neon"), Some(SimdLevel::Neon));
        assert_eq!(SimdLevel::parse_cli("none"), Some(SimdLevel::None));
    }

    #[test]
    fn parses_aliases_and_case_variants() {
        assert_eq!(SimdLevel::parse_cli("AVX-512"), Some(SimdLevel::Avx512));
        assert_eq!(SimdLevel::parse_cli("AVX_2"), Some(SimdLevel::Avx2));
        assert_eq!(SimdLevel::parse_cli("sse4.1"), Some(SimdLevel::Sse4));
        assert_eq!(SimdLevel::parse_cli("ssse3"), Some(SimdLevel::Sse4));
        assert_eq!(SimdLevel::parse_cli("ASIMD"), Some(SimdLevel::Neon));
        assert_eq!(SimdLevel::parse_cli("Off"), Some(SimdLevel::None));
        assert_eq!(SimdLevel::parse_cli("scalar"), Some(SimdLevel::None));
    }

    #[test]
    fn rejects_unknown_levels() {
        assert!(SimdLevel::parse_cli("avx1024").is_none());
        assert!(SimdLevel::parse_cli("").is_none());
        assert!(SimdLevel::parse_cli("yes").is_none());
    }

    #[test]
    fn cli_strings_round_trip() {
        for level in [
            SimdLevel::Auto,
            SimdLevel::Avx512,
            SimdLevel::Avx2,
            SimdLevel::Sse4,
            SimdLevel::Neon,
            SimdLevel::None,
        ] {
            assert_eq!(
                SimdLevel::parse_cli(level.as_cli_str()),
                Some(level),
                "round trip for {level:?}"
            );
        }
    }

    /// Pure-function table check that does not touch the global override.
    fn allowed(level: SimdLevel, feature: SimdFeature) -> bool {
        match (level, feature) {
            (SimdLevel::Auto, _) => true,
            (SimdLevel::None, _) => false,
            (SimdLevel::Avx512, _) => true,
            (SimdLevel::Avx2, SimdFeature::Avx512 | SimdFeature::Neon) => false,
            (SimdLevel::Avx2, _) => true,
            (SimdLevel::Sse4, SimdFeature::Sse41 | SimdFeature::Ssse3 | SimdFeature::Sse2) => true,
            (SimdLevel::Sse4, _) => false,
            (SimdLevel::Neon, SimdFeature::Neon) => true,
            (SimdLevel::Neon, _) => false,
        }
    }

    #[test]
    fn auto_allows_everything() {
        for feature in [
            SimdFeature::Avx512,
            SimdFeature::Avx2,
            SimdFeature::Sse41,
            SimdFeature::Ssse3,
            SimdFeature::Sse2,
            SimdFeature::Neon,
        ] {
            assert!(allowed(SimdLevel::Auto, feature), "auto: {feature:?}");
        }
    }

    #[test]
    fn none_blocks_everything() {
        for feature in [
            SimdFeature::Avx512,
            SimdFeature::Avx2,
            SimdFeature::Sse41,
            SimdFeature::Ssse3,
            SimdFeature::Sse2,
            SimdFeature::Neon,
        ] {
            assert!(!allowed(SimdLevel::None, feature), "none: {feature:?}");
        }
    }

    #[test]
    fn avx2_caps_below_avx512() {
        assert!(!allowed(SimdLevel::Avx2, SimdFeature::Avx512));
        assert!(allowed(SimdLevel::Avx2, SimdFeature::Avx2));
        assert!(allowed(SimdLevel::Avx2, SimdFeature::Sse41));
        assert!(allowed(SimdLevel::Avx2, SimdFeature::Sse2));
        assert!(!allowed(SimdLevel::Avx2, SimdFeature::Neon));
    }

    #[test]
    fn sse4_only_allows_sse_family() {
        assert!(!allowed(SimdLevel::Sse4, SimdFeature::Avx512));
        assert!(!allowed(SimdLevel::Sse4, SimdFeature::Avx2));
        assert!(allowed(SimdLevel::Sse4, SimdFeature::Sse41));
        assert!(allowed(SimdLevel::Sse4, SimdFeature::Ssse3));
        assert!(allowed(SimdLevel::Sse4, SimdFeature::Sse2));
        assert!(!allowed(SimdLevel::Sse4, SimdFeature::Neon));
    }

    #[test]
    fn neon_only_allows_neon() {
        for feature in [
            SimdFeature::Avx512,
            SimdFeature::Avx2,
            SimdFeature::Sse41,
            SimdFeature::Ssse3,
            SimdFeature::Sse2,
        ] {
            assert!(!allowed(SimdLevel::Neon, feature), "neon vs {feature:?}");
        }
        assert!(allowed(SimdLevel::Neon, SimdFeature::Neon));
    }

    #[test]
    fn level_byte_round_trip() {
        for level in [
            SimdLevel::Auto,
            SimdLevel::Avx512,
            SimdLevel::Avx2,
            SimdLevel::Sse4,
            SimdLevel::Neon,
            SimdLevel::None,
        ] {
            let byte = level_to_byte(level);
            assert_eq!(byte_to_level(byte), Some(level), "round trip for {level:?}");
        }
    }

    #[test]
    fn unknown_byte_decodes_to_none() {
        assert_eq!(byte_to_level(UNINIT), None);
        assert_eq!(byte_to_level(99), None);
    }
}
