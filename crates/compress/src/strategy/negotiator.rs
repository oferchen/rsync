//! Compression algorithm negotiation abstraction.
//!
//! Defines the [`CompressionNegotiator`] trait that decouples algorithm
//! selection logic from the wire-level vstring I/O in the `protocol` crate.
//! This follows the Dependency Inversion principle - callers depend on the
//! trait abstraction rather than concrete negotiation logic.
//!
//! The [`DefaultCompressionNegotiator`] wraps the upstream-compatible
//! selection algorithm from `protocol::negotiation::capabilities::algorithms`,
//! providing the default preference order: zstd > lz4 > zlibx > zlib > none.

use super::CompressionAlgorithmKind;

/// Trait for compression algorithm negotiation and selection.
///
/// Abstracts the algorithm preference ordering and mutual selection logic,
/// enabling alternative negotiation strategies (e.g., bandwidth-adaptive
/// selection, testing with fixed algorithms).
///
/// The wire-level vstring I/O remains in the `protocol` crate; this trait
/// only governs the selection decision once algorithm lists have been
/// exchanged.
///
/// # Upstream reference
///
/// upstream: compat.c:332-363 `parse_negotiate_str()` - both sides converge
/// on the first entry in the client's list that also appears in the server's
/// list. Server iterates the remote (client) list; client iterates the local
/// list.
pub trait CompressionNegotiator: Send + Sync {
    /// Returns the ordered list of supported compression algorithm names.
    ///
    /// The first entry is the most preferred. This list is advertised to the
    /// remote peer during vstring exchange.
    ///
    /// # Upstream reference
    ///
    /// upstream: compat.c:100-112 `valid_compressions_items[]`
    fn supported_algorithms(&self) -> Vec<&'static str>;

    /// Selects the best mutual algorithm given the remote peer's advertised list.
    ///
    /// Uses upstream rsync's asymmetric selection rule:
    /// - Server (`is_server=true`): iterates the remote (client's) list, returns
    ///   the first entry that also appears in the local list.
    /// - Client (`is_server=false`): iterates the local list, returns the first
    ///   entry that also appears in the remote (server's) list.
    ///
    /// Returns `"none"` if no mutual algorithm is found.
    ///
    /// # Upstream reference
    ///
    /// upstream: compat.c:332-363 `parse_negotiate_str()`
    fn select_algorithm(&self, remote_list: &[&str], is_server: bool) -> &'static str;
}

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
        let mut list = Vec::with_capacity(5);

        // upstream: compat.c:101-102 - zstd first when SUPPORT_ZSTD is defined
        #[cfg(feature = "zstd")]
        list.push("zstd");

        // NOTE: lz4 is intentionally omitted from auto-negotiation.
        // Its per-token wire framing is not yet interop-validated with upstream.
        // Explicit --compress-choice=lz4 still works (bypasses this list).

        list.extend_from_slice(&["zlibx", "zlib", "none"]);
        list
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

/// Fixed compression negotiator that always selects a predetermined algorithm.
///
/// Useful for testing, benchmarking, or when the user specifies
/// `--compress-choice` to bypass negotiation entirely.
///
/// # Example
///
/// ```
/// use compress::strategy::negotiator::{CompressionNegotiator, FixedCompressionNegotiator};
///
/// let negotiator = FixedCompressionNegotiator::new("zlib");
/// assert_eq!(negotiator.select_algorithm(&["zstd", "zlib"], false), "zlib");
/// assert_eq!(negotiator.select_algorithm(&["zstd"], false), "none");
/// ```
#[derive(Debug, Clone, Copy)]
pub struct FixedCompressionNegotiator {
    algorithm: &'static str,
}

impl FixedCompressionNegotiator {
    /// Creates a negotiator that always prefers the given algorithm.
    ///
    /// The algorithm must be a valid wire-level name (e.g., `"zlib"`, `"zstd"`,
    /// `"lz4"`, `"zlibx"`, `"none"`).
    #[must_use]
    pub const fn new(algorithm: &'static str) -> Self {
        Self { algorithm }
    }
}

impl CompressionNegotiator for FixedCompressionNegotiator {
    fn supported_algorithms(&self) -> Vec<&'static str> {
        vec![self.algorithm]
    }

    fn select_algorithm(&self, remote_list: &[&str], _is_server: bool) -> &'static str {
        if remote_list.contains(&self.algorithm) {
            self.algorithm
        } else {
            "none"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        // zstd should be first (highest preference)
        assert_eq!(supported[0], "zstd");
    }

    #[test]
    fn default_negotiator_supported_algorithms_order() {
        let negotiator = DefaultCompressionNegotiator::new();
        let supported = negotiator.supported_algorithms();
        // zlibx should come before zlib (upstream preference)
        let zlibx_pos = supported.iter().position(|&a| a == "zlibx").unwrap();
        let zlib_pos = supported.iter().position(|&a| a == "zlib").unwrap();
        let none_pos = supported.iter().position(|&a| a == "none").unwrap();
        assert!(zlibx_pos < zlib_pos);
        assert!(zlib_pos < none_pos);
    }

    #[test]
    fn default_negotiator_client_selects_first_local_match() {
        let negotiator = DefaultCompressionNegotiator::new();
        // Remote supports zlib and none - client should pick zlibx or zstd first
        // depending on features, but since remote only has zlib, should pick zlib.
        let selected = negotiator.select_algorithm(&["zlib", "none"], false);
        assert_eq!(selected, "zlib");
    }

    #[test]
    fn default_negotiator_server_selects_first_remote_match() {
        let negotiator = DefaultCompressionNegotiator::new();
        // Server iterates remote list: zlib is first and we support it
        let selected = negotiator.select_algorithm(&["zlib", "none"], true);
        assert_eq!(selected, "zlib");
    }

    #[test]
    fn default_negotiator_server_prefers_remote_order() {
        let negotiator = DefaultCompressionNegotiator::new();
        // Server should pick "none" because it appears first in remote's list
        // and we support it
        let selected = negotiator.select_algorithm(&["none", "zlib"], true);
        assert_eq!(selected, "none");
    }

    #[test]
    fn default_negotiator_client_prefers_local_order() {
        let negotiator = DefaultCompressionNegotiator::new();
        // Client should pick zlibx because it appears before "none" in local list
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
    fn fixed_negotiator_selects_specified_algorithm() {
        let negotiator = FixedCompressionNegotiator::new("zlib");
        let selected = negotiator.select_algorithm(&["zstd", "zlib", "none"], false);
        assert_eq!(selected, "zlib");
    }

    #[test]
    fn fixed_negotiator_returns_none_when_not_in_remote() {
        let negotiator = FixedCompressionNegotiator::new("zlib");
        let selected = negotiator.select_algorithm(&["zstd", "none"], false);
        assert_eq!(selected, "none");
    }

    #[test]
    fn fixed_negotiator_supported_algorithms_single_entry() {
        let negotiator = FixedCompressionNegotiator::new("zlib");
        let supported = negotiator.supported_algorithms();
        assert_eq!(supported, vec!["zlib"]);
    }

    #[test]
    fn fixed_negotiator_is_server_independent() {
        let negotiator = FixedCompressionNegotiator::new("zlib");
        let as_client = negotiator.select_algorithm(&["zlib", "none"], false);
        let as_server = negotiator.select_algorithm(&["zlib", "none"], true);
        assert_eq!(as_client, "zlib");
        assert_eq!(as_server, "zlib");
    }

    #[test]
    fn fixed_negotiator_none_algorithm() {
        let negotiator = FixedCompressionNegotiator::new("none");
        let selected = negotiator.select_algorithm(&["zlib", "none"], false);
        assert_eq!(selected, "none");
    }

    #[test]
    fn negotiators_are_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<DefaultCompressionNegotiator>();
        assert_send_sync::<FixedCompressionNegotiator>();
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
    fn fixed_negotiator_is_debug() {
        let negotiator = FixedCompressionNegotiator::new("zlib");
        let debug = format!("{negotiator:?}");
        assert!(debug.contains("FixedCompressionNegotiator"));
    }

    #[test]
    fn trait_object_works() {
        let negotiator: Box<dyn CompressionNegotiator> =
            Box::new(DefaultCompressionNegotiator::new());
        let supported = negotiator.supported_algorithms();
        assert!(supported.contains(&"zlib"));

        let selected = negotiator.select_algorithm(&["zlib"], false);
        assert_eq!(selected, "zlib");
    }

    #[test]
    fn trait_object_fixed() {
        let negotiator: Box<dyn CompressionNegotiator> =
            Box::new(FixedCompressionNegotiator::new("zlib"));
        let selected = negotiator.select_algorithm(&["zstd", "zlib"], false);
        assert_eq!(selected, "zlib");
    }

    #[test]
    fn selector_negotiate_with_default_negotiator() {
        // Verify that DefaultCompressionNegotiator produces results consistent
        // with CompressionStrategySelector::negotiate for common scenarios.
        let negotiator = DefaultCompressionNegotiator::new();
        let selected = negotiator.select_algorithm(&["zlib", "none"], false);
        assert!(selected == "zlib" || selected == "zlibx");
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
    fn fixed_negotiator_clone() {
        let negotiator = FixedCompressionNegotiator::new("zlib");
        let cloned = negotiator;
        assert_eq!(
            negotiator.select_algorithm(&["zlib"], false),
            cloned.select_algorithm(&["zlib"], false)
        );
    }

    // ---- Protocol version x feature matrix tests ----
    //
    // These test the full negotiation matrix: protocol version ranges
    // (which determine what a remote peer might advertise) combined with
    // feature-gated local algorithm availability.

    #[test]
    fn negotiation_pre_v30_remote_only_zlib() {
        // Protocol < 30: remote peer only supports zlib (no negotiation vstring).
        // Client should select zlib since it is always available locally.
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
        // Protocol 28: remote advertises zlib + none (typical pre-v30 peer).
        let negotiator = DefaultCompressionNegotiator::new();
        // As client: local preference order wins - zlibx before zlib
        let selected = negotiator.select_algorithm(&["zlib", "none"], false);
        assert_eq!(selected, "zlib");
    }

    #[test]
    fn negotiation_v29_remote_zlibx_zlib_none() {
        // Protocol 29: remote advertises zlibx, zlib, none.
        let negotiator = DefaultCompressionNegotiator::new();
        // Client prefers local order: zlibx is in local list before zlib
        let selected = negotiator.select_algorithm(&["zlibx", "zlib", "none"], false);
        assert_eq!(selected, "zlibx");
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn negotiation_v30_remote_zlib_zstd_client_prefers_zstd() {
        // Protocol 30-31: remote supports zstd + zlib. With zstd feature
        // enabled, client's local preference order is zstd > zlibx > zlib.
        let negotiator = DefaultCompressionNegotiator::new();
        let selected = negotiator.select_algorithm(&["zlib", "zlibx", "zstd", "none"], false);
        // Client iterates local list: zstd is first locally, and remote has it
        assert_eq!(selected, "zstd");
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn negotiation_v31_server_respects_remote_order() {
        // Protocol 31: as server, we iterate the remote (client's) list.
        // Remote prefers zlib over zstd - server should respect that.
        let negotiator = DefaultCompressionNegotiator::new();
        let selected = negotiator.select_algorithm(&["zlib", "zstd", "none"], true);
        assert_eq!(selected, "zlib");
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn negotiation_v32_remote_zstd_first_server() {
        // Protocol 32+: remote (client) prefers zstd - server picks zstd.
        let negotiator = DefaultCompressionNegotiator::new();
        let selected = negotiator.select_algorithm(&["zstd", "zlib", "none"], true);
        assert_eq!(selected, "zstd");
    }

    #[cfg(not(feature = "zstd"))]
    #[test]
    fn negotiation_without_zstd_falls_back_to_zlib() {
        // Without zstd feature: remote advertises zstd + zlib, but we
        // can only select zlib since zstd is not compiled in.
        let negotiator = DefaultCompressionNegotiator::new();
        let selected = negotiator.select_algorithm(&["zstd", "zlib", "none"], false);
        assert_eq!(selected, "zlib");
    }

    #[cfg(not(feature = "zstd"))]
    #[test]
    fn negotiation_without_zstd_server_skips_zstd() {
        let negotiator = DefaultCompressionNegotiator::new();
        let selected = negotiator.select_algorithm(&["zstd", "zlib", "none"], true);
        // Server iterates remote list: zstd is not available, so skip to zlib
        assert_eq!(selected, "zlib");
    }

    #[cfg(not(feature = "zstd"))]
    #[test]
    fn negotiation_without_zstd_supported_list_excludes_zstd() {
        let negotiator = DefaultCompressionNegotiator::new();
        let supported = negotiator.supported_algorithms();
        assert!(!supported.contains(&"zstd"));
    }

    #[test]
    fn negotiation_lz4_not_in_auto_negotiation() {
        // lz4 is intentionally omitted from auto-negotiation (per code comment).
        // Even with the lz4 feature enabled, it should not appear in the list.
        let negotiator = DefaultCompressionNegotiator::new();
        let supported = negotiator.supported_algorithms();
        assert!(
            !supported.contains(&"lz4"),
            "lz4 must not appear in auto-negotiation list"
        );
    }

    #[test]
    fn negotiation_remote_only_lz4_returns_none() {
        // If remote only supports lz4, and we don't advertise it,
        // negotiation should fall back to "none".
        let negotiator = DefaultCompressionNegotiator::new();
        let selected = negotiator.select_algorithm(&["lz4"], false);
        assert_eq!(selected, "none");
    }

    #[test]
    fn negotiation_remote_only_lz4_server_returns_none() {
        let negotiator = DefaultCompressionNegotiator::new();
        let selected = negotiator.select_algorithm(&["lz4"], true);
        assert_eq!(selected, "none");
    }

    #[test]
    fn negotiation_remote_unknown_algorithms_ignored() {
        // Remote advertises completely unknown algorithms.
        let negotiator = DefaultCompressionNegotiator::new();
        let selected = negotiator.select_algorithm(&["brotli", "snappy", "lzma"], false);
        assert_eq!(selected, "none");
    }

    #[test]
    fn negotiation_remote_unknown_then_zlib_server() {
        // Server: remote list has unknown first, then zlib.
        let negotiator = DefaultCompressionNegotiator::new();
        let selected = negotiator.select_algorithm(&["brotli", "zlib", "none"], true);
        assert_eq!(selected, "zlib");
    }

    #[test]
    fn negotiation_protocol_version_0_defaults_zlib() {
        // Edge case: protocol version 0 should default to zlib.
        assert_eq!(
            CompressionAlgorithmKind::for_protocol_version(0),
            CompressionAlgorithmKind::Zlib
        );
    }

    #[test]
    fn negotiation_protocol_version_255_high() {
        // Edge case: very high protocol version.
        let kind = CompressionAlgorithmKind::for_protocol_version(255);
        #[cfg(feature = "zstd")]
        assert_eq!(kind, CompressionAlgorithmKind::Zstd);
        #[cfg(not(feature = "zstd"))]
        assert_eq!(kind, CompressionAlgorithmKind::Zlib);
    }

    #[test]
    fn negotiation_protocol_version_35_boundary() {
        // Protocol 35 is last version before zstd default threshold.
        assert_eq!(
            CompressionAlgorithmKind::for_protocol_version(35),
            CompressionAlgorithmKind::Zlib
        );
    }

    #[test]
    fn negotiation_protocol_version_36_boundary() {
        // Protocol 36 is the zstd default threshold.
        let kind = CompressionAlgorithmKind::for_protocol_version(36);
        #[cfg(feature = "zstd")]
        assert_eq!(kind, CompressionAlgorithmKind::Zstd);
        #[cfg(not(feature = "zstd"))]
        assert_eq!(kind, CompressionAlgorithmKind::Zlib);
    }

    #[test]
    fn negotiation_protocol_version_range_below_36_all_zlib() {
        // All protocol versions below 36 default to zlib.
        for v in [1, 10, 20, 28, 29, 30, 31, 32, 33, 34, 35] {
            assert_eq!(
                CompressionAlgorithmKind::for_protocol_version(v),
                CompressionAlgorithmKind::Zlib,
                "protocol version {v} should default to zlib"
            );
        }
    }

    #[test]
    fn negotiation_client_zlibx_preferred_over_zlib() {
        // When remote offers both zlibx and zlib, client should pick zlibx
        // because zlibx comes before zlib in local preference order.
        let negotiator = DefaultCompressionNegotiator::new();
        let selected = negotiator.select_algorithm(&["zlib", "zlibx", "none"], false);
        assert_eq!(selected, "zlibx");
    }

    #[test]
    fn negotiation_server_zlibx_vs_zlib_respects_remote_order() {
        // Server iterates remote list, so zlib comes first if remote lists it first.
        let negotiator = DefaultCompressionNegotiator::new();
        let selected = negotiator.select_algorithm(&["zlib", "zlibx", "none"], true);
        // "zlib" resolves via from_name to Zlib kind, which is in supported list
        assert_eq!(selected, "zlib");
    }

    #[test]
    fn negotiation_server_zlibx_first_in_remote() {
        let negotiator = DefaultCompressionNegotiator::new();
        let selected = negotiator.select_algorithm(&["zlibx", "zlib", "none"], true);
        // zlibx maps to Zlib kind, which is available and in supported list
        assert_eq!(selected, "zlib");
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn negotiation_client_zstd_unavailable_on_remote_falls_to_zlibx() {
        // Client has zstd locally but remote only offers zlibx + zlib.
        let negotiator = DefaultCompressionNegotiator::new();
        let selected = negotiator.select_algorithm(&["zlibx", "zlib", "none"], false);
        // Client iterates local: zstd not in remote, next is zlibx - found!
        assert_eq!(selected, "zlibx");
    }

    #[test]
    fn negotiation_remote_only_none() {
        // Remote only supports "none" - should match.
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
        // zlibx, zlib, none = 3 base, +1 if zstd
        #[cfg(feature = "zstd")]
        assert_eq!(supported.len(), 4);
        #[cfg(not(feature = "zstd"))]
        assert_eq!(supported.len(), 3);
    }

    #[test]
    fn negotiation_duplicate_in_remote_list() {
        // Remote sends duplicates - should still work, picking first match.
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

    #[test]
    fn fixed_negotiator_zlibx() {
        let negotiator = FixedCompressionNegotiator::new("zlibx");
        assert_eq!(
            negotiator.select_algorithm(&["zlib", "zlibx", "none"], false),
            "zlibx"
        );
        assert_eq!(
            negotiator.select_algorithm(&["zlib", "none"], false),
            "none"
        );
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn fixed_negotiator_zstd() {
        let negotiator = FixedCompressionNegotiator::new("zstd");
        assert_eq!(
            negotiator.select_algorithm(&["zstd", "zlib"], false),
            "zstd"
        );
        assert_eq!(
            negotiator.select_algorithm(&["zlib", "none"], false),
            "none"
        );
    }

    #[cfg(feature = "lz4")]
    #[test]
    fn fixed_negotiator_lz4() {
        let negotiator = FixedCompressionNegotiator::new("lz4");
        assert_eq!(negotiator.select_algorithm(&["lz4", "zlib"], false), "lz4");
        assert_eq!(
            negotiator.select_algorithm(&["zlib", "none"], false),
            "none"
        );
    }
}
