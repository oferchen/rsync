//! [`DefaultCompressionNegotiator`] - upstream-compatible default selection.

use super::trait_def::CompressionNegotiator;
use crate::strategy::CompressionAlgorithmKind;
use crate::strategy::profile::ProtocolCompressionProfile;

/// Default compression negotiator matching upstream rsync 3.4.1 behaviour.
///
/// Advertises algorithms in upstream preference order: zstd > lz4 > zlibx >
/// zlib > none (with zstd and lz4 conditional on feature flags). Selection
/// uses the asymmetric client/server rule from `parse_negotiate_str()`.
///
/// # Example
///
/// ```
/// use compress::strategy::negotiator::{CompressionNegotiator, DefaultCompressionNegotiator};
///
/// let negotiator = DefaultCompressionNegotiator::new();
/// let supported = negotiator.supported_algorithms();
/// assert!(supported.contains(&"zlib"));
///
/// // Client selects first local preference that server also supports
/// let selected = negotiator.select_algorithm(&["zlib", "none"], false);
/// assert!(selected == "zlib" || selected == "zlibx");
/// ```
#[derive(Debug, Clone, Copy, Default)]
pub struct DefaultCompressionNegotiator;

impl DefaultCompressionNegotiator {
    /// Creates a new default compression negotiator.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl CompressionNegotiator for DefaultCompressionNegotiator {
    fn supported_algorithms(&self) -> Vec<&'static str> {
        // Default negotiator targets modern (protocol >= 30) peers; share the
        // single advertisement list with ProtocolAwareCompressionNegotiator so
        // both stay in lockstep with upstream's valid_compressions_items[].
        ProtocolCompressionProfile::MODERN.advertised_algorithms()
    }

    fn select_algorithm(&self, remote_list: &[&str], is_server: bool) -> &'static str {
        let supported = self.supported_algorithms();

        if is_server {
            // Server: iterate client's (remote) list, first match in our list wins.
            // upstream: compat.c:353 `if (best == 1 || am_server) break;`
            for remote_algo in remote_list {
                if let Some(kind) = CompressionAlgorithmKind::from_name(remote_algo) {
                    if kind.is_available() && supported.contains(remote_algo) {
                        return kind.name();
                    }
                }
            }
        } else {
            // Client: iterate our local list, first item also in server's (remote)
            // list wins. This gives client preference order priority.
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
    use crate::strategy::CompressionAlgorithmKind;

    #[test]
    fn default_negotiator_supported_algorithms_contains_zlib() {
        let negotiator = DefaultCompressionNegotiator::new();
        let supported = negotiator.supported_algorithms();
        assert!(supported.contains(&"zlib"));
        assert!(supported.contains(&"zlibx"));
        assert!(supported.contains(&"none"));
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn default_negotiator_supported_algorithms_contains_zstd() {
        let negotiator = DefaultCompressionNegotiator::new();
        let supported = negotiator.supported_algorithms();
        assert!(supported.contains(&"zstd"));
        assert_eq!(supported[0], "zstd");
    }

    #[test]
    fn default_negotiator_supported_algorithms_order() {
        let negotiator = DefaultCompressionNegotiator::new();
        let supported = negotiator.supported_algorithms();
        let zlibx_pos = supported.iter().position(|&a| a == "zlibx").unwrap();
        let zlib_pos = supported.iter().position(|&a| a == "zlib").unwrap();
        let none_pos = supported.iter().position(|&a| a == "none").unwrap();
        assert!(zlibx_pos < zlib_pos);
        assert!(zlib_pos < none_pos);
    }

    #[test]
    fn default_negotiator_client_selects_first_local_match() {
        let negotiator = DefaultCompressionNegotiator::new();
        let selected = negotiator.select_algorithm(&["zlib", "none"], false);
        assert_eq!(selected, "zlib");
    }

    #[test]
    fn default_negotiator_server_selects_first_remote_match() {
        let negotiator = DefaultCompressionNegotiator::new();
        let selected = negotiator.select_algorithm(&["zlib", "none"], true);
        assert_eq!(selected, "zlib");
    }

    #[test]
    fn default_negotiator_server_prefers_remote_order() {
        let negotiator = DefaultCompressionNegotiator::new();
        let selected = negotiator.select_algorithm(&["none", "zlib"], true);
        assert_eq!(selected, "none");
    }

    #[test]
    fn default_negotiator_client_prefers_local_order() {
        let negotiator = DefaultCompressionNegotiator::new();
        let selected = negotiator.select_algorithm(&["none", "zlibx"], false);
        assert_eq!(selected, "zlibx");
    }

    #[test]
    fn default_negotiator_no_match_returns_none() {
        let negotiator = DefaultCompressionNegotiator::new();
        let selected = negotiator.select_algorithm(&["brotli", "snappy"], false);
        assert_eq!(selected, "none");
    }

    #[test]
    fn default_negotiator_empty_remote_list_returns_none() {
        let negotiator = DefaultCompressionNegotiator::new();
        let selected = negotiator.select_algorithm(&[], false);
        assert_eq!(selected, "none");
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn default_negotiator_client_prefers_zstd_over_zlib() {
        let negotiator = DefaultCompressionNegotiator::new();
        let selected = negotiator.select_algorithm(&["zstd", "zlib", "none"], false);
        assert_eq!(selected, "zstd");
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn default_negotiator_server_picks_remote_zstd_first() {
        let negotiator = DefaultCompressionNegotiator::new();
        let selected = negotiator.select_algorithm(&["zstd", "zlib"], true);
        assert_eq!(selected, "zstd");
    }

    #[test]
    fn default_negotiator_is_default() {
        let _negotiator: DefaultCompressionNegotiator = Default::default();
    }

    #[test]
    fn default_negotiator_is_debug() {
        let negotiator = DefaultCompressionNegotiator::new();
        let debug = format!("{negotiator:?}");
        assert!(debug.contains("DefaultCompressionNegotiator"));
    }

    #[test]
    fn default_negotiator_clone() {
        let negotiator = DefaultCompressionNegotiator::new();
        let cloned = negotiator;
        assert_eq!(
            negotiator.supported_algorithms(),
            cloned.supported_algorithms()
        );
    }

    #[test]
    fn selector_negotiate_with_default_negotiator() {
        let negotiator = DefaultCompressionNegotiator::new();
        let selected = negotiator.select_algorithm(&["zlib", "none"], false);
        assert!(selected == "zlib" || selected == "zlibx");
    }

    #[test]
    fn negotiation_pre_v30_remote_only_zlib() {
        let negotiator = DefaultCompressionNegotiator::new();
        let selected = negotiator.select_algorithm(&["zlib"], false);
        assert_eq!(selected, "zlib");
    }

    #[test]
    fn negotiation_pre_v30_remote_only_zlib_server_side() {
        let negotiator = DefaultCompressionNegotiator::new();
        let selected = negotiator.select_algorithm(&["zlib"], true);
        assert_eq!(selected, "zlib");
    }

    #[test]
    fn negotiation_v28_remote_zlib_and_none() {
        let negotiator = DefaultCompressionNegotiator::new();
        let selected = negotiator.select_algorithm(&["zlib", "none"], false);
        assert_eq!(selected, "zlib");
    }

    #[test]
    fn negotiation_v29_remote_zlibx_zlib_none() {
        let negotiator = DefaultCompressionNegotiator::new();
        let selected = negotiator.select_algorithm(&["zlibx", "zlib", "none"], false);
        assert_eq!(selected, "zlibx");
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn negotiation_v30_remote_zlib_zstd_client_prefers_zstd() {
        let negotiator = DefaultCompressionNegotiator::new();
        let selected = negotiator.select_algorithm(&["zlib", "zlibx", "zstd", "none"], false);
        assert_eq!(selected, "zstd");
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn negotiation_v31_server_respects_remote_order() {
        let negotiator = DefaultCompressionNegotiator::new();
        let selected = negotiator.select_algorithm(&["zlib", "zstd", "none"], true);
        assert_eq!(selected, "zlib");
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn negotiation_v32_remote_zstd_first_server() {
        let negotiator = DefaultCompressionNegotiator::new();
        let selected = negotiator.select_algorithm(&["zstd", "zlib", "none"], true);
        assert_eq!(selected, "zstd");
    }

    #[cfg(not(feature = "zstd"))]
    #[test]
    fn negotiation_without_zstd_falls_back_to_zlib() {
        let negotiator = DefaultCompressionNegotiator::new();
        let selected = negotiator.select_algorithm(&["zstd", "zlib", "none"], false);
        assert_eq!(selected, "zlib");
    }

    #[cfg(not(feature = "zstd"))]
    #[test]
    fn negotiation_without_zstd_server_skips_zstd() {
        let negotiator = DefaultCompressionNegotiator::new();
        let selected = negotiator.select_algorithm(&["zstd", "zlib", "none"], true);
        assert_eq!(selected, "zlib");
    }

    #[cfg(not(feature = "zstd"))]
    #[test]
    fn negotiation_without_zstd_supported_list_excludes_zstd() {
        let negotiator = DefaultCompressionNegotiator::new();
        let supported = negotiator.supported_algorithms();
        assert!(!supported.contains(&"zstd"));
    }

    #[cfg(feature = "lz4")]
    #[test]
    fn negotiation_lz4_in_auto_negotiation() {
        let negotiator = DefaultCompressionNegotiator::new();
        let supported = negotiator.supported_algorithms();
        assert!(
            supported.contains(&"lz4"),
            "lz4 must appear in auto-negotiation list once its wire format is validated"
        );
    }

    #[cfg(not(feature = "lz4"))]
    #[test]
    fn negotiation_lz4_not_in_auto_negotiation() {
        let negotiator = DefaultCompressionNegotiator::new();
        let supported = negotiator.supported_algorithms();
        assert!(
            !supported.contains(&"lz4"),
            "lz4 must not appear in auto-negotiation list when the lz4 feature is off"
        );
    }

    #[cfg(feature = "lz4")]
    #[test]
    fn negotiation_remote_only_lz4_returns_lz4() {
        let negotiator = DefaultCompressionNegotiator::new();
        let selected = negotiator.select_algorithm(&["lz4"], false);
        assert_eq!(selected, "lz4");
    }

    #[cfg(not(feature = "lz4"))]
    #[test]
    fn negotiation_remote_only_lz4_returns_none() {
        let negotiator = DefaultCompressionNegotiator::new();
        let selected = negotiator.select_algorithm(&["lz4"], false);
        assert_eq!(selected, "none");
    }

    #[cfg(feature = "lz4")]
    #[test]
    fn negotiation_remote_only_lz4_server_returns_lz4() {
        let negotiator = DefaultCompressionNegotiator::new();
        let selected = negotiator.select_algorithm(&["lz4"], true);
        assert_eq!(selected, "lz4");
    }

    #[cfg(not(feature = "lz4"))]
    #[test]
    fn negotiation_remote_only_lz4_server_returns_none() {
        let negotiator = DefaultCompressionNegotiator::new();
        let selected = negotiator.select_algorithm(&["lz4"], true);
        assert_eq!(selected, "none");
    }

    #[test]
    fn negotiation_remote_unknown_algorithms_ignored() {
        let negotiator = DefaultCompressionNegotiator::new();
        let selected = negotiator.select_algorithm(&["brotli", "snappy", "lzma"], false);
        assert_eq!(selected, "none");
    }

    #[test]
    fn negotiation_remote_unknown_then_zlib_server() {
        let negotiator = DefaultCompressionNegotiator::new();
        let selected = negotiator.select_algorithm(&["brotli", "zlib", "none"], true);
        assert_eq!(selected, "zlib");
    }

    #[test]
    fn negotiation_protocol_version_0_defaults_zlib() {
        assert_eq!(
            CompressionAlgorithmKind::for_protocol_version(0),
            CompressionAlgorithmKind::Zlib
        );
    }

    #[test]
    fn negotiation_protocol_version_255_high() {
        let kind = CompressionAlgorithmKind::for_protocol_version(255);
        #[cfg(feature = "zstd")]
        assert_eq!(kind, CompressionAlgorithmKind::Zstd);
        #[cfg(not(feature = "zstd"))]
        assert_eq!(kind, CompressionAlgorithmKind::Zlib);
    }

    #[test]
    fn negotiation_protocol_version_29_boundary() {
        // upstream: compat.c:556-563 - last pre-vstring version, fallback zlib.
        assert_eq!(
            CompressionAlgorithmKind::for_protocol_version(29),
            CompressionAlgorithmKind::Zlib
        );
    }

    #[test]
    fn negotiation_protocol_version_30_boundary() {
        // upstream: compat.c:101-102 - first vstring version; zstd preferred.
        let kind = CompressionAlgorithmKind::for_protocol_version(30);
        #[cfg(feature = "zstd")]
        assert_eq!(kind, CompressionAlgorithmKind::Zstd);
        #[cfg(not(feature = "zstd"))]
        assert_eq!(kind, CompressionAlgorithmKind::Zlib);
    }

    #[test]
    fn negotiation_protocol_version_range_below_30_all_zlib() {
        for v in [1, 10, 20, 28, 29] {
            assert_eq!(
                CompressionAlgorithmKind::for_protocol_version(v),
                CompressionAlgorithmKind::Zlib,
                "protocol version {v} should default to zlib"
            );
        }
    }

    #[test]
    fn negotiation_client_zlibx_preferred_over_zlib() {
        let negotiator = DefaultCompressionNegotiator::new();
        let selected = negotiator.select_algorithm(&["zlib", "zlibx", "none"], false);
        assert_eq!(selected, "zlibx");
    }

    #[test]
    fn negotiation_server_zlibx_vs_zlib_respects_remote_order() {
        let negotiator = DefaultCompressionNegotiator::new();
        let selected = negotiator.select_algorithm(&["zlib", "zlibx", "none"], true);
        assert_eq!(selected, "zlib");
    }

    #[test]
    fn negotiation_server_zlibx_first_in_remote() {
        // zlibx maps to Zlib kind via from_name, which is in the supported list.
        let negotiator = DefaultCompressionNegotiator::new();
        let selected = negotiator.select_algorithm(&["zlibx", "zlib", "none"], true);
        assert_eq!(selected, "zlib");
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn negotiation_client_zstd_unavailable_on_remote_falls_to_zlibx() {
        let negotiator = DefaultCompressionNegotiator::new();
        let selected = negotiator.select_algorithm(&["zlibx", "zlib", "none"], false);
        assert_eq!(selected, "zlibx");
    }

    #[test]
    fn negotiation_remote_only_none() {
        let negotiator = DefaultCompressionNegotiator::new();
        let selected = negotiator.select_algorithm(&["none"], false);
        assert_eq!(selected, "none");
    }

    #[test]
    fn negotiation_remote_only_none_server() {
        let negotiator = DefaultCompressionNegotiator::new();
        let selected = negotiator.select_algorithm(&["none"], true);
        assert_eq!(selected, "none");
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn negotiation_zstd_is_first_in_supported_list() {
        let negotiator = DefaultCompressionNegotiator::new();
        let supported = negotiator.supported_algorithms();
        assert_eq!(
            supported[0], "zstd",
            "zstd must be highest preference when feature is enabled"
        );
    }

    #[test]
    fn negotiation_supported_list_always_ends_with_none() {
        let negotiator = DefaultCompressionNegotiator::new();
        let supported = negotiator.supported_algorithms();
        assert_eq!(
            supported.last(),
            Some(&"none"),
            "none must be last in preference order"
        );
    }

    #[test]
    fn negotiation_supported_list_size() {
        let negotiator = DefaultCompressionNegotiator::new();
        let supported = negotiator.supported_algorithms();
        // Base list is zlibx, zlib, none; zstd and lz4 each add one entry when
        // their feature is enabled (lz4 was added once its wire format was
        // validated against upstream).
        let expected = 3 + cfg!(feature = "zstd") as usize + cfg!(feature = "lz4") as usize;
        assert_eq!(supported.len(), expected);
    }

    #[test]
    fn negotiation_duplicate_in_remote_list() {
        let negotiator = DefaultCompressionNegotiator::new();
        let selected = negotiator.select_algorithm(&["zlib", "zlib", "none", "none"], false);
        assert_eq!(selected, "zlib");
    }

    #[test]
    fn negotiation_server_duplicate_in_remote_list() {
        let negotiator = DefaultCompressionNegotiator::new();
        let selected = negotiator.select_algorithm(&["zlib", "zlib", "none"], true);
        assert_eq!(selected, "zlib");
    }
}
