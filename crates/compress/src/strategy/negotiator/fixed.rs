//! [`FixedCompressionNegotiator`] - bypass negotiation with a predetermined algorithm.

use super::trait_def::CompressionNegotiator;

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
    fn fixed_negotiator_is_debug() {
        let negotiator = FixedCompressionNegotiator::new("zlib");
        let debug = format!("{negotiator:?}");
        assert!(debug.contains("FixedCompressionNegotiator"));
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
