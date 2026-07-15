//! Delta generator configuration parameter object.
//!
//! This module provides the `DeltaGeneratorConfig` struct which encapsulates
//! all parameters needed for delta generation, following the Parameter Object
//! pattern to reduce function parameter counts and improve maintainability.

use protocol::{CompatibilityFlags, NegotiationResult, ProtocolVersion};

/// Configuration for delta generation from a received signature.
///
/// Parameter object that groups the inputs required by
/// [`crate::generate_delta_from_signature`]: block layout, signature blocks,
/// strong checksum length, protocol version, negotiated algorithms,
/// compatibility flags, checksum seed, and the in-place basis-update flag.
/// Takes ownership of `sig_blocks`
/// to avoid cloning strong-checksum data; references the optional
/// negotiation/compat state.
///
/// # Builder Pattern
///
/// ```ignore
/// let config = DeltaGeneratorConfig::new(block_length, sig_blocks, strong_sum_length, protocol)
///     .with_negotiated_algorithms(&algorithms)
///     .with_compat_flags(&flags)
///     .with_checksum_seed(seed);
/// ```
#[derive(Debug)]
pub struct DeltaGeneratorConfig<'a> {
    /// Block length used for signature computation. Smaller blocks allow
    /// finer-grained matching at the cost of higher per-block overhead;
    /// typically derived from the square-root heuristic in
    /// `signature::calculate_signature_layout`.
    pub block_length: u32,

    /// Signature blocks received from the wire format. Each block carries a
    /// rolling and a strong checksum.
    pub sig_blocks: Vec<protocol::wire::signature::SignatureBlock>,

    /// Length of the strong checksum in bytes. Common values are 16 (MD5,
    /// MD4) or 20 (SHA-1); must be non-zero and bounded by the digest size
    /// of the negotiated checksum algorithm.
    pub strong_sum_length: u8,

    /// Protocol version used to pick the strong-checksum algorithm when no
    /// explicit negotiation result is present: protocol < 30 falls back to
    /// MD4/MD5, protocol >= 30 expects [`Self::negotiated_algorithms`].
    pub protocol: ProtocolVersion,

    /// Negotiated algorithms from protocol >= 30 capability exchange. When
    /// `Some`, overrides the protocol-version-driven default. See
    /// [`crate::ChecksumFactory::from_negotiation`].
    pub negotiated_algorithms: Option<&'a NegotiationResult>,

    /// Compatibility flags affecting checksum computation (e.g. MD5 seeding)
    /// for protocol >= 30.
    pub compat_flags: Option<&'a CompatibilityFlags>,

    /// Rolling-checksum seed exchanged during the handshake. Both sides
    /// derive the seed from `time(NULL)` (or `--checksum-seed=NUM`) so block
    /// hashes match and a fixed seed cannot be exploited for hash-collision
    /// attacks.
    pub checksum_seed: i32,

    /// Whether the receiver is updating the basis file in place, activating
    /// the delta generator's backward-`Copy` suppression guard.
    ///
    /// Set per file by the sender loop from upstream's `updating_basis_file`
    /// condition (`sender.c:337`): the basis being matched is the destination
    /// itself, which the receiver overwrites as it applies the delta, so any
    /// `Copy` whose basis offset precedes the source cursor would read a region
    /// already clobbered. When `true` the generator demotes such a candidate to
    /// a literal; the reconstructed file stays byte-identical.
    ///
    /// upstream: match.c:211 (`updating_basis_file && s->sums[i].offset <
    /// offset`), sender.c:337 (the per-file assignment).
    pub updating_basis_file: bool,
}

impl<'a> DeltaGeneratorConfig<'a> {
    /// Creates a config with the four required fields and default optional
    /// state (no negotiated algorithms, no compat flags, zero seed).
    #[must_use]
    pub fn new(
        block_length: u32,
        sig_blocks: Vec<protocol::wire::signature::SignatureBlock>,
        strong_sum_length: u8,
        protocol: ProtocolVersion,
    ) -> Self {
        Self {
            block_length,
            sig_blocks,
            strong_sum_length,
            protocol,
            negotiated_algorithms: None,
            compat_flags: None,
            checksum_seed: 0,
            updating_basis_file: false,
        }
    }

    /// Attaches negotiated algorithms from protocol >= 30 capability exchange.
    #[must_use]
    pub fn with_negotiated_algorithms(mut self, algorithms: &'a NegotiationResult) -> Self {
        self.negotiated_algorithms = Some(algorithms);
        self
    }

    /// Attaches compatibility flags from protocol setup.
    #[must_use]
    pub fn with_compat_flags(mut self, flags: &'a CompatibilityFlags) -> Self {
        self.compat_flags = Some(flags);
        self
    }

    /// Sets the rolling-checksum seed.
    #[must_use]
    pub fn with_checksum_seed(mut self, seed: i32) -> Self {
        self.checksum_seed = seed;
        self
    }

    /// Marks the transfer as an in-place basis update so the delta generator
    /// activates upstream's backward-`Copy` suppression guard.
    ///
    /// upstream: sender.c:337 `updating_basis_file`.
    #[must_use]
    pub fn with_updating_basis_file(mut self, updating: bool) -> Self {
        self.updating_basis_file = updating;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::ProtocolVersion;

    #[test]
    fn delta_config_new_sets_required_fields() {
        let config = DeltaGeneratorConfig::new(2048, vec![], 16, ProtocolVersion::NEWEST);

        assert_eq!(config.block_length, 2048);
        assert_eq!(config.strong_sum_length, 16);
        assert_eq!(config.protocol, ProtocolVersion::NEWEST);
        assert_eq!(config.checksum_seed, 0);
        assert!(config.negotiated_algorithms.is_none());
        assert!(config.compat_flags.is_none());
    }

    #[test]
    fn delta_config_builder_pattern() {
        let config = DeltaGeneratorConfig::new(2048, vec![], 16, ProtocolVersion::NEWEST)
            .with_checksum_seed(12345);

        assert_eq!(config.checksum_seed, 12345);
    }

    #[test]
    fn delta_config_debug_format() {
        let config = DeltaGeneratorConfig::new(2048, vec![], 16, ProtocolVersion::NEWEST);

        let debug_output = format!("{config:?}");
        assert!(debug_output.contains("DeltaGeneratorConfig"));
        assert!(debug_output.contains("block_length"));
    }
}
