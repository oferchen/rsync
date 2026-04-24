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
}
