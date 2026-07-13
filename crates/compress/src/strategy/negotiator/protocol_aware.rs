//! [`ProtocolAwareCompressionNegotiator`] - version-gated negotiation.

use super::trait_def::CompressionNegotiator;
use crate::strategy::CompressionAlgorithmKind;
use crate::strategy::profile::ProtocolCompressionProfile;

/// Protocol-version-aware compression negotiator.
///
/// Adjusts the supported algorithm list and selection behaviour based on the
/// negotiated protocol version:
///
/// - **Protocol < 30**: No vstring negotiation exists in upstream rsync.
///   Compression is always zlib - the remote list is ignored entirely.
///   upstream: compat.c:556-563 - when `!do_negotiated_strings`, compression
///   defaults to `"zlib"` without exchanging vstrings.
///
/// - **Protocol 30-31**: Full vstring negotiation. Supports zlibx, zlib, none,
///   plus zstd and lz4 when their respective features are enabled. Same
///   behaviour as [`super::DefaultCompressionNegotiator`].
///
/// - **Protocol 32+**: Same as 30-31. Zstd is first preference when the feature
///   is enabled, matching upstream `valid_compressions_items[]` order.
///   upstream: compat.c:100-111 - zstd appears first in the list when
///   `SUPPORT_ZSTD` is defined.
///
/// # Example
///
/// ```
/// use compress::strategy::negotiator::{CompressionNegotiator, ProtocolAwareCompressionNegotiator};
///
/// // Protocol 28: always zlib, ignores remote list
/// let neg = ProtocolAwareCompressionNegotiator::new(28);
/// assert_eq!(neg.select_algorithm(&["zstd", "none"], false), "zlib");
/// assert_eq!(neg.supported_algorithms(), vec!["zlib"]);
///
/// // Protocol 31: full negotiation
/// let neg = ProtocolAwareCompressionNegotiator::new(31);
/// assert!(neg.supported_algorithms().contains(&"zlib"));
/// assert!(neg.supported_algorithms().contains(&"zlibx"));
/// ```
#[derive(Debug, Clone, Copy)]
pub struct ProtocolAwareCompressionNegotiator {
    protocol_version: u8,
}

impl ProtocolAwareCompressionNegotiator {
    /// Creates a protocol-aware compression negotiator for the given version.
    ///
    /// The protocol version determines which algorithms are available and
    /// whether vstring negotiation is used.
    #[must_use]
    pub const fn new(protocol_version: u8) -> Self {
        Self { protocol_version }
    }

    /// Returns the protocol version this negotiator was created for.
    #[must_use]
    pub const fn protocol_version(&self) -> u8 {
        self.protocol_version
    }
}

impl CompressionNegotiator for ProtocolAwareCompressionNegotiator {
    fn supported_algorithms(&self) -> Vec<&'static str> {
        // Single source of truth for per-protocol advertisement lists.
        // upstream: compat.c:100-112 valid_compressions_items[] (modern),
        // compat.c:556-568 (legacy zlib-only fallback).
        ProtocolCompressionProfile::for_protocol(self.protocol_version).advertised_algorithms()
    }

    fn select_algorithm(&self, remote_list: &[&str], is_server: bool) -> &'static str {
        let profile = ProtocolCompressionProfile::for_protocol(self.protocol_version);
        if !profile.supports_vstring_negotiation {
            // upstream: compat.c:562 - no vstring exchange; zlib is mandatory.
            // The remote list is irrelevant for legacy protocols.
            return "zlib";
        }

        // Protocol >= 30: delegate to the standard asymmetric selection rule.
        // upstream: compat.c:332-363 parse_negotiate_str()
        let supported = self.supported_algorithms();

        if is_server {
            // Server: iterate client's (remote) list, first match wins.
            // upstream: compat.c:353 `if (best == 1 || am_server) break;`
            for remote_algo in remote_list {
                if let Some(kind) = CompressionAlgorithmKind::from_name(remote_algo) {
                    if kind.is_available() && supported.contains(remote_algo) {
                        return kind.name();
                    }
                }
            }
        } else {
            // Client: iterate local list, first item also in remote list wins.
            for &local_algo in &supported {
                if remote_list.contains(&local_algo) {
                    return local_algo;
                }
            }
        }

        "none"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_aware_proto_28_supported_only_zlib() {
        let neg = ProtocolAwareCompressionNegotiator::new(28);
        assert_eq!(neg.supported_algorithms(), vec!["zlib"]);
    }

    #[test]
    fn protocol_aware_proto_29_supported_only_zlib() {
        let neg = ProtocolAwareCompressionNegotiator::new(29);
        assert_eq!(neg.supported_algorithms(), vec!["zlib"]);
    }

    #[test]
    fn protocol_aware_proto_28_always_zlib_client() {
        let neg = ProtocolAwareCompressionNegotiator::new(28);
        assert_eq!(neg.select_algorithm(&["zstd", "none"], false), "zlib");
        assert_eq!(neg.select_algorithm(&["none"], false), "zlib");
        assert_eq!(neg.select_algorithm(&[], false), "zlib");
    }

    #[test]
    fn protocol_aware_proto_28_always_zlib_server() {
        let neg = ProtocolAwareCompressionNegotiator::new(28);
        assert_eq!(neg.select_algorithm(&["zstd", "none"], true), "zlib");
        assert_eq!(neg.select_algorithm(&["none"], true), "zlib");
        assert_eq!(neg.select_algorithm(&[], true), "zlib");
    }

    #[test]
    fn protocol_aware_proto_29_always_zlib_ignores_remote() {
        let neg = ProtocolAwareCompressionNegotiator::new(29);
        assert_eq!(
            neg.select_algorithm(&["zstd", "lz4", "brotli"], false),
            "zlib"
        );
        assert_eq!(
            neg.select_algorithm(&["zstd", "lz4", "brotli"], true),
            "zlib"
        );
    }

    #[test]
    fn protocol_aware_proto_0_always_zlib() {
        let neg = ProtocolAwareCompressionNegotiator::new(0);
        assert_eq!(neg.supported_algorithms(), vec!["zlib"]);
        assert_eq!(neg.select_algorithm(&["zstd", "none"], false), "zlib");
    }

    #[test]
    fn protocol_aware_proto_1_always_zlib() {
        let neg = ProtocolAwareCompressionNegotiator::new(1);
        assert_eq!(neg.select_algorithm(&["none"], false), "zlib");
    }

    #[test]
    fn protocol_aware_proto_30_has_zlibx_zlib_none() {
        let neg = ProtocolAwareCompressionNegotiator::new(30);
        let supported = neg.supported_algorithms();
        assert!(supported.contains(&"zlibx"));
        assert!(supported.contains(&"zlib"));
        assert!(supported.contains(&"none"));
    }

    #[test]
    fn protocol_aware_proto_31_has_zlibx_zlib_none() {
        let neg = ProtocolAwareCompressionNegotiator::new(31);
        let supported = neg.supported_algorithms();
        assert!(supported.contains(&"zlibx"));
        assert!(supported.contains(&"zlib"));
        assert!(supported.contains(&"none"));
    }

    #[test]
    fn protocol_aware_proto_30_client_selects_zlib() {
        let neg = ProtocolAwareCompressionNegotiator::new(30);
        let selected = neg.select_algorithm(&["zlib", "none"], false);
        assert_eq!(selected, "zlib");
    }

    #[test]
    fn protocol_aware_proto_30_client_prefers_zlibx_over_zlib() {
        let neg = ProtocolAwareCompressionNegotiator::new(30);
        let selected = neg.select_algorithm(&["zlib", "zlibx", "none"], false);
        assert_eq!(selected, "zlibx");
    }

    #[test]
    fn protocol_aware_proto_31_server_respects_remote_order() {
        let neg = ProtocolAwareCompressionNegotiator::new(31);
        let selected = neg.select_algorithm(&["none", "zlib"], true);
        assert_eq!(selected, "none");
    }

    #[test]
    fn protocol_aware_proto_30_no_match_returns_none() {
        let neg = ProtocolAwareCompressionNegotiator::new(30);
        let selected = neg.select_algorithm(&["brotli", "snappy"], false);
        assert_eq!(selected, "none");
    }

    #[test]
    fn protocol_aware_proto_30_empty_remote_returns_none() {
        let neg = ProtocolAwareCompressionNegotiator::new(30);
        let selected = neg.select_algorithm(&[], false);
        assert_eq!(selected, "none");
    }

    #[test]
    fn protocol_aware_proto_32_has_zlibx_zlib_none() {
        let neg = ProtocolAwareCompressionNegotiator::new(32);
        let supported = neg.supported_algorithms();
        assert!(supported.contains(&"zlibx"));
        assert!(supported.contains(&"zlib"));
        assert!(supported.contains(&"none"));
    }

    #[test]
    fn protocol_aware_proto_32_client_selects_zlib_when_remote_has_it() {
        let neg = ProtocolAwareCompressionNegotiator::new(32);
        let selected = neg.select_algorithm(&["zlib", "none"], false);
        assert_eq!(selected, "zlib");
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn protocol_aware_proto_30_has_zstd() {
        let neg = ProtocolAwareCompressionNegotiator::new(30);
        let supported = neg.supported_algorithms();
        assert!(supported.contains(&"zstd"));
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn protocol_aware_proto_30_zstd_is_first_preference() {
        // upstream: compat.c:100-102 valid_compressions_items[] - zstd first.
        let neg = ProtocolAwareCompressionNegotiator::new(30);
        let supported = neg.supported_algorithms();
        assert_eq!(supported[0], "zstd");
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn protocol_aware_proto_31_client_prefers_zstd() {
        let neg = ProtocolAwareCompressionNegotiator::new(31);
        let selected = neg.select_algorithm(&["zstd", "zlib", "none"], false);
        assert_eq!(selected, "zstd");
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn protocol_aware_proto_32_zstd_preferred() {
        // upstream: compat.c:100-102 valid_compressions_items[] with SUPPORT_ZSTD.
        let neg = ProtocolAwareCompressionNegotiator::new(32);
        let supported = neg.supported_algorithms();
        assert_eq!(
            supported[0], "zstd",
            "zstd must be first preference for protocol 32+"
        );
        let selected = neg.select_algorithm(&["zstd", "zlib", "none"], false);
        assert_eq!(selected, "zstd");
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn protocol_aware_proto_32_server_picks_remote_zstd() {
        let neg = ProtocolAwareCompressionNegotiator::new(32);
        let selected = neg.select_algorithm(&["zstd", "zlib", "none"], true);
        assert_eq!(selected, "zstd");
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn protocol_aware_proto_32_zstd_not_in_remote_falls_back() {
        let neg = ProtocolAwareCompressionNegotiator::new(32);
        let selected = neg.select_algorithm(&["zlibx", "zlib", "none"], false);
        assert_eq!(selected, "zlibx");
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn protocol_aware_proto_28_ignores_zstd_in_remote() {
        let neg = ProtocolAwareCompressionNegotiator::new(28);
        let selected = neg.select_algorithm(&["zstd"], false);
        assert_eq!(selected, "zlib");
    }

    #[cfg(not(feature = "zstd"))]
    #[test]
    fn protocol_aware_proto_32_no_zstd_feature_falls_back() {
        let neg = ProtocolAwareCompressionNegotiator::new(32);
        let supported = neg.supported_algorithms();
        assert!(!supported.contains(&"zstd"));
        let selected = neg.select_algorithm(&["zstd", "zlib", "none"], false);
        assert_eq!(selected, "zlib");
    }

    #[cfg(not(feature = "zstd"))]
    #[test]
    fn protocol_aware_proto_30_no_zstd_feature() {
        let neg = ProtocolAwareCompressionNegotiator::new(30);
        let supported = neg.supported_algorithms();
        assert!(!supported.contains(&"zstd"));
    }

    #[test]
    fn protocol_aware_proto_255_high_version() {
        let neg = ProtocolAwareCompressionNegotiator::new(255);
        let supported = neg.supported_algorithms();
        assert!(supported.contains(&"zlibx"));
        assert!(supported.contains(&"zlib"));
        assert!(supported.contains(&"none"));
    }

    #[test]
    fn protocol_aware_version_accessor() {
        let neg = ProtocolAwareCompressionNegotiator::new(31);
        assert_eq!(neg.protocol_version(), 31);
    }

    #[test]
    fn protocol_aware_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ProtocolAwareCompressionNegotiator>();
    }

    #[test]
    fn protocol_aware_is_debug() {
        let neg = ProtocolAwareCompressionNegotiator::new(31);
        let debug = format!("{neg:?}");
        assert!(debug.contains("ProtocolAwareCompressionNegotiator"));
    }

    #[test]
    fn protocol_aware_trait_object_works() {
        let neg: Box<dyn CompressionNegotiator> =
            Box::new(ProtocolAwareCompressionNegotiator::new(28));
        assert_eq!(neg.select_algorithm(&["zstd", "none"], false), "zlib");

        let neg: Box<dyn CompressionNegotiator> =
            Box::new(ProtocolAwareCompressionNegotiator::new(31));
        let selected = neg.select_algorithm(&["zlib", "none"], false);
        assert_eq!(selected, "zlib");
    }

    #[test]
    fn protocol_aware_boundary_29_to_30() {
        let neg29 = ProtocolAwareCompressionNegotiator::new(29);
        assert_eq!(neg29.supported_algorithms(), vec!["zlib"]);
        assert_eq!(neg29.select_algorithm(&["none"], false), "zlib");

        let neg30 = ProtocolAwareCompressionNegotiator::new(30);
        let supported = neg30.supported_algorithms();
        assert!(supported.len() >= 3);
        assert_eq!(neg30.select_algorithm(&["none"], false), "none");
    }

    #[cfg(feature = "lz4")]
    #[test]
    fn protocol_aware_proto_30_lz4_auto_negotiated() {
        // upstream: compat.c:100-112 valid_compressions_items[] is NOT gated by
        // protocol version - lz4 is advertised at every negotiated protocol
        // (30, 31, 32) whenever SUPPORT_LZ4 is compiled in. Wire safety against
        // genuine proto-30 peers (e.g. rsync 3.0.9) comes from the `v`
        // capability / CF_VARINT_FLIST_FLAGS handshake (do_negotiated_strings),
        // not from pruning the list by version: a peer that lacks `v` never
        // receives this list at all. lz4's wire framing is validated
        // byte-for-byte against upstream 3.4.4.
        let neg = ProtocolAwareCompressionNegotiator::new(30);
        let supported = neg.supported_algorithms();
        assert!(
            supported.contains(&"lz4"),
            "lz4 must appear in the version-independent advertisement list"
        );
    }

    #[cfg(not(feature = "lz4"))]
    #[test]
    fn protocol_aware_proto_30_lz4_absent_without_feature() {
        let neg = ProtocolAwareCompressionNegotiator::new(30);
        let supported = neg.supported_algorithms();
        assert!(
            !supported.contains(&"lz4"),
            "lz4 must not appear when the lz4 feature is disabled"
        );
    }

    #[test]
    fn protocol_aware_proto_30_list_ends_with_none() {
        let neg = ProtocolAwareCompressionNegotiator::new(30);
        let supported = neg.supported_algorithms();
        assert_eq!(supported.last(), Some(&"none"));
    }

    #[test]
    fn protocol_aware_clone() {
        let neg = ProtocolAwareCompressionNegotiator::new(31);
        let cloned = neg;
        assert_eq!(neg.supported_algorithms(), cloned.supported_algorithms());
    }
}
