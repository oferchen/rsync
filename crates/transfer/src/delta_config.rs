//! Delta generator configuration parameter object.
//!
//! This module provides the `DeltaGeneratorConfig` struct which encapsulates
//! all parameters needed for delta generation, following the Parameter Object
//! pattern to reduce function parameter counts and improve maintainability.

use protocol::{CompatibilityFlags, NegotiationResult, ProtocolVersion};

/// Configuration for delta generation from a received signature.
///
/// Groups all parameters needed for delta generation into a single struct,
/// following the Parameter Object pattern to reduce function argument count
/// and improve code maintainability.
///
/// This struct encapsulates the essential parameters required to generate
/// a delta script from a signature, including block size, checksum settings,
/// protocol version, and negotiated capabilities.
///
/// # Design Pattern
///
/// This implements the Parameter Object pattern to:
/// - Reduce function parameter count (from 7 parameters to 1)
/// - Improve code readability and maintainability
/// - Enable flexible configuration with builder methods
/// - Support backward-compatible API evolution
///
/// # Usage
///
/// ## Direct Construction
///
/// ```ignore
/// use transfer::DeltaGeneratorConfig;
///
/// let config = DeltaGeneratorConfig {
///     block_length: sum_head.block_length,
///     sig_blocks,
///     strong_sum_length: sum_head.s2length,
///     protocol: self.protocol,
///     negotiated_algorithms: self.negotiated_algorithms.as_ref(),
///     compat_flags: self.compat_flags.as_ref(),
///     checksum_seed: self.checksum_seed,
/// };
/// ```
///
/// ## Builder Pattern
///
/// ```ignore
/// use transfer::DeltaGeneratorConfig;
///
/// let config = DeltaGeneratorConfig::new(block_length, sig_blocks, strong_sum_length, protocol)
///     .with_negotiated_algorithms(&algorithms)
///     .with_compat_flags(&flags)
///     .with_checksum_seed(seed);
/// ```
///
/// # Examples
///
/// ```ignore
/// use transfer::DeltaGeneratorConfig;
/// use protocol::ProtocolVersion;
///
/// // Minimal configuration with defaults
/// let config = DeltaGeneratorConfig::new(
///     2048,           // block_length
///     sig_blocks,     // Vec<SignatureBlock>
///     16,             // strong_sum_length
///     ProtocolVersion::NEWEST,
/// );
///
/// // Full configuration with all options
/// let config = DeltaGeneratorConfig::new(2048, sig_blocks, 16, ProtocolVersion::NEWEST)
///     .with_negotiated_algorithms(&negotiated)
///     .with_compat_flags(&compat)
///     .with_checksum_seed(12345);
/// ```
///
/// # Performance Considerations
///
/// - Takes ownership of `sig_blocks` to avoid cloning strong checksum data
/// - Uses references for optional configuration to minimize copying
/// - Lifetime parameter `'a` ensures references remain valid during use
#[derive(Debug)]
pub struct DeltaGeneratorConfig<'a> {
    /// Block length used for signature computation.
    ///
    /// This determines the granularity of the delta algorithm.
    /// Smaller blocks allow finer-grained matching but increase overhead.
    ///
    /// Typically calculated using the square root heuristic from
    /// `calculate_signature_layout` in the signature crate.
    pub block_length: u32,

    /// Signature blocks received from the wire format.
    ///
    /// Each block contains rolling and strong checksums for matching.
    /// Takes ownership to avoid cloning strong_sum data, which can be
    /// expensive for large files with many blocks.
    ///
    /// The blocks are converted to engine format during delta generation.
    pub sig_blocks: Vec<protocol::wire::signature::SignatureBlock>,

    /// Length of the strong checksum in bytes.
    ///
    /// Determines how many bytes of the strong checksum are used for verification.
    /// Common values are 16 (MD5, MD4) or 20 (SHA-1).
    ///
    /// Must be non-zero and not exceed the digest size of the checksum algorithm.
    pub strong_sum_length: u8,

    /// Protocol version for algorithm selection.
    ///
    /// Different protocol versions may use different checksum algorithms:
    /// - Protocol < 30: MD4 or MD5 (based on `--checksum-choice`)
    /// - Protocol >= 30: Negotiated via capability exchange
    pub protocol: ProtocolVersion,

    /// Negotiated algorithms from capability exchange.
    ///
    /// When present, overrides the default algorithm selection based on protocol version.
    /// Only available for protocol >= 30 with capability negotiation enabled.
    ///
    /// See [`crate::ChecksumFactory::from_negotiation`] for algorithm selection logic.
    pub negotiated_algorithms: Option<&'a NegotiationResult>,

    /// Compatibility flags affecting checksum behavior.
    ///
    /// Controls checksum computation details for upstream compatibility.
    /// These flags affect details like MD5 seeding behavior.
    ///
    /// Available for protocol >= 30.
    pub compat_flags: Option<&'a CompatibilityFlags>,

    /// Checksum seed for rolling checksum computation.
    ///
    /// Used to initialize the rolling checksum state. This seed is exchanged
    /// during the protocol handshake and ensures both sides compute identical
    /// checksums for matching blocks.
    ///
    /// The seed is typically derived from the current time to prevent
    /// algorithmic complexity attacks on the rolling checksum hash table.
    pub checksum_seed: i32,
}

impl<'a> DeltaGeneratorConfig<'a> {
    /// Creates a new delta generator configuration with required parameters.
    ///
    /// This is the recommended way to construct a `DeltaGeneratorConfig`.
    /// Optional parameters can be set using builder methods.
    ///
    /// # Arguments
    ///
    /// * `block_length` - Size of each signature block in bytes
    /// * `sig_blocks` - Signature blocks from the wire format
    /// * `strong_sum_length` - Length of strong checksums in bytes
    /// * `protocol` - Protocol version for algorithm selection
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use transfer::DeltaGeneratorConfig;
    /// use protocol::ProtocolVersion;
    ///
    /// let config = DeltaGeneratorConfig::new(
    ///     2048,
    ///     sig_blocks,
    ///     16,
    ///     ProtocolVersion::NEWEST,
    /// );
    /// ```
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
        }
    }

    /// Sets the negotiated algorithms from capability exchange.
    ///
    /// # Arguments
    ///
    /// * `algorithms` - Negotiated checksum and compression algorithms
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let config = DeltaGeneratorConfig::new(2048, sig_blocks, 16, protocol)
    ///     .with_negotiated_algorithms(&negotiated);
    /// ```
    #[must_use]
    pub fn with_negotiated_algorithms(mut self, algorithms: &'a NegotiationResult) -> Self {
        self.negotiated_algorithms = Some(algorithms);
        self
    }

    /// Sets the compatibility flags for checksum behavior.
    ///
    /// # Arguments
    ///
    /// * `flags` - Compatibility flags from protocol setup
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let config = DeltaGeneratorConfig::new(2048, sig_blocks, 16, protocol)
    ///     .with_compat_flags(&compat);
    /// ```
    #[must_use]
    pub fn with_compat_flags(mut self, flags: &'a CompatibilityFlags) -> Self {
        self.compat_flags = Some(flags);
        self
    }

    /// Sets the checksum seed for rolling checksum computation.
    ///
    /// # Arguments
    ///
    /// * `seed` - Checksum seed value
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let config = DeltaGeneratorConfig::new(2048, sig_blocks, 16, protocol)
    ///     .with_checksum_seed(12345);
    /// ```
    #[must_use]
    pub fn with_checksum_seed(mut self, seed: i32) -> Self {
        self.checksum_seed = seed;
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
