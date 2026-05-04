//! Protocol-version-aware compression profile lookup table.
//!
//! Consolidates the per-protocol compression decisions (preferred codec,
//! whether vstring negotiation is supported, and the algorithm advertisement
//! list) into a single data-driven table. Replaces scattered
//! `if protocol_version >= N` ladders previously duplicated across
//! [`super::kind`], [`super::negotiator`], and the `protocol` crate's
//! capability layer.
//!
//! # Upstream Reference
//!
//! upstream: compat.c:100-112 `valid_compressions_items[]` - preference order.
//! upstream: compat.c:556-563 - vstring negotiation gated by
//! `do_negotiated_strings`, which is only set for protocol >= 30 once the
//! `v` capability flag is exchanged. Protocol < 30 unconditionally uses zlib.
//! upstream: rsync.h:114 - `PROTOCOL_VERSION 32` (current); rsync.h:149 -
//! `MAX_PROTOCOL_VERSION 40` (accepted upper bound).

use super::CompressionAlgorithmKind;

/// Snapshot of the compression behaviour negotiated for a given protocol
/// version range.
///
/// Each profile pairs the upstream-defined inclusive lower bound of a protocol
/// range with the codec preferences and capability flags that apply within the
/// range. Single source of truth for protocol-version-driven compression
/// decisions across the workspace.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct ProtocolCompressionProfile {
    /// Inclusive lower bound of the protocol range this profile covers.
    pub min_protocol: u8,
    /// `true` when the protocol exchanges vstrings to negotiate codecs.
    /// upstream: compat.c:556-563 - depends on `do_negotiated_strings`.
    pub supports_vstring_negotiation: bool,
    /// Codec preferred when negotiation is unavailable or unsuccessful.
    /// upstream: compat.c:562 - `recv_negotiate_str` defaults to `"zlib"`.
    pub fallback_codec: CompressionAlgorithmKind,
    /// Wire name advertised as first preference when zstd is compiled in.
    /// upstream: compat.c:101-102 - first entry in
    /// `valid_compressions_items[]` when `SUPPORT_ZSTD` is defined.
    pub preferred_codec_with_zstd: &'static str,
    /// Wire name advertised as first preference without zstd support.
    pub preferred_codec_without_zstd: &'static str,
}

impl ProtocolCompressionProfile {
    /// Profile for protocol versions below 30.
    ///
    /// Pre-30 peers do not exchange vstrings; compression is unconditionally
    /// zlib. upstream: compat.c:556-568.
    pub const LEGACY: Self = Self {
        min_protocol: 0,
        supports_vstring_negotiation: false,
        fallback_codec: CompressionAlgorithmKind::Zlib,
        preferred_codec_with_zstd: "zlib",
        preferred_codec_without_zstd: "zlib",
    };

    /// Profile for protocol versions 30 and above.
    ///
    /// Modern peers negotiate codecs via vstrings. Zstd is the first
    /// preference whenever it is compiled in; otherwise the advertisement
    /// falls back to `zlibx > zlib > none`.
    /// upstream: compat.c:100-112 `valid_compressions_items[]`.
    pub const MODERN: Self = Self {
        min_protocol: 30,
        supports_vstring_negotiation: true,
        fallback_codec: CompressionAlgorithmKind::Zlib,
        preferred_codec_with_zstd: "zstd",
        preferred_codec_without_zstd: "zlibx",
    };

    /// Ordered profile table - consulted by [`Self::for_protocol`] for the
    /// matching range. Highest `min_protocol` that does not exceed the
    /// requested version wins.
    pub const TABLE: &'static [Self] = &[Self::LEGACY, Self::MODERN];

    /// Returns the profile that applies to the given protocol version.
    #[must_use]
    pub const fn for_protocol(protocol_version: u8) -> Self {
        let mut chosen = Self::TABLE[0];
        let mut i = 1;
        while i < Self::TABLE.len() {
            let entry = Self::TABLE[i];
            if protocol_version >= entry.min_protocol {
                chosen = entry;
            }
            i += 1;
        }
        chosen
    }

    /// Returns the wire name of the codec advertised as first preference for
    /// this profile in the current build.
    #[must_use]
    pub const fn preferred_codec_name(&self) -> &'static str {
        #[cfg(feature = "zstd")]
        {
            self.preferred_codec_with_zstd
        }
        #[cfg(not(feature = "zstd"))]
        {
            self.preferred_codec_without_zstd
        }
    }

    /// Returns the canonical kind chosen as the protocol-default when no
    /// negotiation context is available. Mirrors upstream's
    /// `recv_negotiate_str` fallback (always zlib for pre-30 peers; zstd is
    /// preferred for 30+ when compiled in).
    #[must_use]
    pub const fn default_kind(&self) -> CompressionAlgorithmKind {
        if !self.supports_vstring_negotiation {
            return self.fallback_codec;
        }

        #[cfg(feature = "zstd")]
        {
            CompressionAlgorithmKind::Zstd
        }
        #[cfg(not(feature = "zstd"))]
        {
            self.fallback_codec
        }
    }

    /// Returns the ordered list of algorithm wire names to advertise during
    /// vstring negotiation. Mirrors `valid_compressions_items[]`. Returns a
    /// single-element list (`["zlib"]`) for legacy profiles where no
    /// negotiation occurs.
    #[must_use]
    pub fn advertised_algorithms(&self) -> Vec<&'static str> {
        if !self.supports_vstring_negotiation {
            return vec!["zlib"];
        }

        let mut list = Vec::with_capacity(5);

        // upstream: compat.c:101-102 - zstd first when SUPPORT_ZSTD is defined
        #[cfg(feature = "zstd")]
        list.push("zstd");

        // NOTE: lz4 is intentionally omitted from auto-negotiation. Its
        // per-token wire framing is not yet interop-validated with upstream.
        // Explicit --compress-choice=lz4 still works (bypasses this list).

        list.extend_from_slice(&["zlibx", "zlib", "none"]);
        list
    }
}

// Compile-time invariants for the LEGACY/MODERN profile constants. Stronger
// than runtime tests: a regression here is a build failure, not a test
// failure. Stated as `const _: () = assert!(...)` so they fold at compile
// time, which also keeps clippy::assertions_on_constants from firing on the
// equivalent runtime form.
const _: () = assert!(!ProtocolCompressionProfile::LEGACY.supports_vstring_negotiation);
const _: () = assert!(ProtocolCompressionProfile::MODERN.supports_vstring_negotiation);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proto_28_resolves_to_legacy() {
        let p = ProtocolCompressionProfile::for_protocol(28);
        assert_eq!(p, ProtocolCompressionProfile::LEGACY);
    }

    #[test]
    fn proto_29_resolves_to_legacy() {
        let p = ProtocolCompressionProfile::for_protocol(29);
        assert_eq!(p, ProtocolCompressionProfile::LEGACY);
    }

    #[test]
    fn proto_30_resolves_to_modern() {
        let p = ProtocolCompressionProfile::for_protocol(30);
        assert_eq!(p, ProtocolCompressionProfile::MODERN);
    }

    #[test]
    fn proto_31_resolves_to_modern() {
        let p = ProtocolCompressionProfile::for_protocol(31);
        assert_eq!(p, ProtocolCompressionProfile::MODERN);
    }

    #[test]
    fn proto_32_resolves_to_modern() {
        let p = ProtocolCompressionProfile::for_protocol(32);
        assert_eq!(p, ProtocolCompressionProfile::MODERN);
    }

    #[test]
    fn proto_0_resolves_to_legacy() {
        let p = ProtocolCompressionProfile::for_protocol(0);
        assert_eq!(p, ProtocolCompressionProfile::LEGACY);
    }

    #[test]
    fn proto_255_resolves_to_modern() {
        let p = ProtocolCompressionProfile::for_protocol(255);
        assert_eq!(p, ProtocolCompressionProfile::MODERN);
    }

    #[test]
    fn legacy_advertises_only_zlib() {
        let advertised = ProtocolCompressionProfile::LEGACY.advertised_algorithms();
        assert_eq!(advertised, vec!["zlib"]);
    }

    #[test]
    fn modern_advertises_zlibx_zlib_none_at_minimum() {
        let advertised = ProtocolCompressionProfile::MODERN.advertised_algorithms();
        assert!(advertised.contains(&"zlibx"));
        assert!(advertised.contains(&"zlib"));
        assert!(advertised.contains(&"none"));
        assert_eq!(advertised.last(), Some(&"none"));
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn modern_advertises_zstd_first_when_feature_enabled() {
        let advertised = ProtocolCompressionProfile::MODERN.advertised_algorithms();
        assert_eq!(advertised[0], "zstd");
    }

    #[cfg(not(feature = "zstd"))]
    #[test]
    fn modern_omits_zstd_without_feature() {
        let advertised = ProtocolCompressionProfile::MODERN.advertised_algorithms();
        assert!(!advertised.contains(&"zstd"));
    }

    #[test]
    fn modern_never_advertises_lz4() {
        let advertised = ProtocolCompressionProfile::MODERN.advertised_algorithms();
        assert!(!advertised.contains(&"lz4"));
    }

    #[test]
    fn legacy_default_kind_is_zlib() {
        assert_eq!(
            ProtocolCompressionProfile::LEGACY.default_kind(),
            CompressionAlgorithmKind::Zlib
        );
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn modern_default_kind_is_zstd_when_feature_enabled() {
        assert_eq!(
            ProtocolCompressionProfile::MODERN.default_kind(),
            CompressionAlgorithmKind::Zstd
        );
    }

    #[cfg(not(feature = "zstd"))]
    #[test]
    fn modern_default_kind_is_zlib_without_feature() {
        assert_eq!(
            ProtocolCompressionProfile::MODERN.default_kind(),
            CompressionAlgorithmKind::Zlib
        );
    }

    #[test]
    fn preferred_codec_name_legacy_is_zlib() {
        assert_eq!(
            ProtocolCompressionProfile::LEGACY.preferred_codec_name(),
            "zlib"
        );
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn preferred_codec_name_modern_with_zstd_is_zstd() {
        assert_eq!(
            ProtocolCompressionProfile::MODERN.preferred_codec_name(),
            "zstd"
        );
    }

    #[cfg(not(feature = "zstd"))]
    #[test]
    fn preferred_codec_name_modern_without_zstd_is_zlibx() {
        assert_eq!(
            ProtocolCompressionProfile::MODERN.preferred_codec_name(),
            "zlibx"
        );
    }

    #[test]
    fn table_is_ordered_by_min_protocol() {
        let table = ProtocolCompressionProfile::TABLE;
        for window in table.windows(2) {
            assert!(window[0].min_protocol < window[1].min_protocol);
        }
    }

    #[test]
    fn for_protocol_matches_old_kind_threshold_below_30() {
        // Pre-30 always resolved to Zlib in the legacy
        // CompressionAlgorithmKind::for_protocol_version match arm.
        for v in [0u8, 1, 10, 20, 27, 28, 29] {
            let p = ProtocolCompressionProfile::for_protocol(v);
            assert_eq!(p.fallback_codec, CompressionAlgorithmKind::Zlib);
        }
    }

    #[test]
    fn boundary_29_to_30_switches_profile() {
        let p29 = ProtocolCompressionProfile::for_protocol(29);
        let p30 = ProtocolCompressionProfile::for_protocol(30);
        assert_ne!(p29, p30);
        assert!(!p29.supports_vstring_negotiation);
        assert!(p30.supports_vstring_negotiation);
    }
}
