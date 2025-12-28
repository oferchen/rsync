//! crates/core/src/server/shared/checksum.rs
//!
//! Checksum factory for creating signature algorithms from negotiated parameters.
//!
//! This module provides [`ChecksumFactory`] which centralizes the logic for
//! converting protocol-layer checksum algorithms to engine-layer signature
//! algorithms. It eliminates code duplication between generator and receiver.
//!
//! # Upstream Reference
//!
//! - `checksum.c:sum_init()` - Checksum algorithm initialization
//! - `match.c:85-120` - Seed handling for different algorithms
//!
//! # Example
//!
//! ```ignore
//! use core::server::shared::ChecksumFactory;
//! use protocol::{ChecksumAlgorithm, CompatibilityFlags, NegotiationResult};
//!
//! let factory = ChecksumFactory::from_negotiation(
//!     Some(&negotiation_result),
//!     protocol_version,
//!     checksum_seed,
//!     Some(&compat_flags),
//! );
//!
//! let signature_algorithm = factory.signature_algorithm();
//! let digest_len = factory.digest_length();
//! ```

use checksums::strong::Md5Seed;
use engine::signature::SignatureAlgorithm;
use protocol::{ChecksumAlgorithm, CompatibilityFlags, NegotiationResult, ProtocolVersion};

/// Factory for creating checksum-related types from negotiated parameters.
///
/// This factory encapsulates the logic for:
/// - Converting protocol-layer [`ChecksumAlgorithm`] to engine-layer [`SignatureAlgorithm`]
/// - Handling seed configuration (legacy vs proper order for MD5)
/// - Providing consistent digest lengths across the codebase
///
/// # MD5 Seed Ordering
///
/// For MD5, the `CHECKSUM_SEED_FIX` compatibility flag determines hash ordering:
/// - Flag set (protocol 30+): seed hashed before data (proper order)
/// - Flag not set (legacy): seed hashed after data
///
/// # XXHash Seed Handling
///
/// XXHash variants (XXH64, XXH3, XXH128) use the checksum seed directly as
/// their internal seed parameter.
#[derive(Debug, Clone, Copy)]
pub struct ChecksumFactory {
    /// The negotiated checksum algorithm.
    algorithm: ChecksumAlgorithm,
    /// The checksum seed value.
    seed: i32,
    /// Whether to use proper (protocol 30+) seed ordering for MD5.
    use_proper_seed_order: bool,
}

impl ChecksumFactory {
    /// Creates a new ChecksumFactory from negotiation results.
    ///
    /// # Arguments
    ///
    /// * `negotiated` - Optional negotiation result from protocol 30+ capability exchange
    /// * `protocol` - The negotiated protocol version
    /// * `seed` - The checksum seed (random value exchanged during setup)
    /// * `compat_flags` - Optional compatibility flags from protocol setup
    ///
    /// # Algorithm Selection
    ///
    /// - If `negotiated` is `Some`, uses the negotiated checksum algorithm
    /// - If `negotiated` is `None` and protocol >= 30, defaults to MD5
    /// - If `negotiated` is `None` and protocol < 30, defaults to MD4
    #[must_use]
    pub fn from_negotiation(
        negotiated: Option<&NegotiationResult>,
        protocol: ProtocolVersion,
        seed: i32,
        compat_flags: Option<&CompatibilityFlags>,
    ) -> Self {
        let algorithm = if let Some(neg) = negotiated {
            neg.checksum
        } else if protocol.as_u8() >= 30 {
            ChecksumAlgorithm::MD5
        } else {
            ChecksumAlgorithm::MD4
        };

        let use_proper_seed_order =
            compat_flags.is_some_and(|flags| flags.contains(CompatibilityFlags::CHECKSUM_SEED_FIX));

        Self {
            algorithm,
            seed,
            use_proper_seed_order,
        }
    }

    /// Creates a ChecksumFactory with explicit algorithm and seed configuration.
    ///
    /// This is useful for testing or when the algorithm is known directly.
    #[must_use]
    pub const fn new(algorithm: ChecksumAlgorithm, seed: i32, use_proper_seed_order: bool) -> Self {
        Self {
            algorithm,
            seed,
            use_proper_seed_order,
        }
    }

    /// Returns the checksum algorithm.
    #[must_use]
    pub const fn algorithm(&self) -> ChecksumAlgorithm {
        self.algorithm
    }

    /// Returns the checksum seed.
    #[must_use]
    pub const fn seed(&self) -> i32 {
        self.seed
    }

    /// Returns whether proper seed ordering is enabled for MD5.
    #[must_use]
    pub const fn uses_proper_seed_order(&self) -> bool {
        self.use_proper_seed_order
    }

    /// Converts the checksum algorithm to a signature algorithm for the engine layer.
    ///
    /// This method handles the conversion from protocol-layer checksum algorithms
    /// to engine-layer signature algorithms, including proper seed configuration.
    ///
    /// # Returns
    ///
    /// The appropriate [`SignatureAlgorithm`] for use with the delta engine.
    #[must_use]
    pub const fn signature_algorithm(&self) -> SignatureAlgorithm {
        let seed_u64 = self.seed as u64;

        match self.algorithm {
            ChecksumAlgorithm::None => SignatureAlgorithm::Md4,
            ChecksumAlgorithm::MD4 => SignatureAlgorithm::Md4,
            ChecksumAlgorithm::MD5 => {
                let seed_config = if self.use_proper_seed_order {
                    Md5Seed::proper(self.seed)
                } else {
                    Md5Seed::legacy(self.seed)
                };
                SignatureAlgorithm::Md5 { seed_config }
            }
            ChecksumAlgorithm::SHA1 => SignatureAlgorithm::Sha1,
            ChecksumAlgorithm::XXH64 => SignatureAlgorithm::Xxh64 { seed: seed_u64 },
            ChecksumAlgorithm::XXH3 => SignatureAlgorithm::Xxh3 { seed: seed_u64 },
            ChecksumAlgorithm::XXH128 => SignatureAlgorithm::Xxh3_128 { seed: seed_u64 },
        }
    }

    /// Returns the digest length for the checksum algorithm.
    ///
    /// This is the number of bytes in the final hash output.
    ///
    /// # Digest Lengths
    ///
    /// | Algorithm | Digest Length |
    /// |-----------|---------------|
    /// | MD4       | 16 bytes      |
    /// | MD5       | 16 bytes      |
    /// | SHA1      | 20 bytes      |
    /// | XXH64     | 8 bytes       |
    /// | XXH3      | 8 bytes       |
    /// | XXH128    | 16 bytes      |
    #[must_use]
    pub const fn digest_length(&self) -> usize {
        match self.algorithm {
            ChecksumAlgorithm::None | ChecksumAlgorithm::MD4 | ChecksumAlgorithm::MD5 => 16,
            ChecksumAlgorithm::SHA1 => 20,
            ChecksumAlgorithm::XXH64 | ChecksumAlgorithm::XXH3 => 8,
            ChecksumAlgorithm::XXH128 => 16,
        }
    }

    /// Returns a legacy MD5 seed configuration.
    ///
    /// This is a convenience method for creating the seed configuration
    /// used with legacy protocols (< 30) or when CHECKSUM_SEED_FIX is not set.
    #[must_use]
    pub const fn legacy_md5_seed(&self) -> Md5Seed {
        Md5Seed::legacy(self.seed)
    }

    /// Returns a proper MD5 seed configuration.
    ///
    /// This is a convenience method for creating the seed configuration
    /// used with protocol 30+ when CHECKSUM_SEED_FIX is set.
    #[must_use]
    pub const fn proper_md5_seed(&self) -> Md5Seed {
        Md5Seed::proper(self.seed)
    }

    /// Returns the MD5 seed configuration based on the factory's settings.
    #[must_use]
    pub const fn md5_seed(&self) -> Md5Seed {
        if self.use_proper_seed_order {
            Md5Seed::proper(self.seed)
        } else {
            Md5Seed::legacy(self.seed)
        }
    }
}

impl Default for ChecksumFactory {
    /// Creates a default factory with MD4 algorithm and zero seed.
    ///
    /// This is primarily useful for testing.
    fn default() -> Self {
        Self {
            algorithm: ChecksumAlgorithm::MD4,
            seed: 0,
            use_proper_seed_order: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------------------
    // Construction tests
    // ------------------------------------------------------------------------

    fn protocol(version: u8) -> ProtocolVersion {
        ProtocolVersion::try_from(version).unwrap()
    }

    #[test]
    fn from_negotiation_with_negotiated_algorithm() {
        let negotiated = NegotiationResult {
            checksum: ChecksumAlgorithm::XXH3,
            compression: protocol::CompressionAlgorithm::Zlib,
        };
        let factory =
            ChecksumFactory::from_negotiation(Some(&negotiated), protocol(31), 12345, None);

        assert!(matches!(factory.algorithm(), ChecksumAlgorithm::XXH3));
        assert_eq!(factory.seed(), 12345);
        assert!(!factory.uses_proper_seed_order());
    }

    #[test]
    fn from_negotiation_defaults_to_md5_for_protocol_30() {
        let factory = ChecksumFactory::from_negotiation(None, protocol(30), 42, None);

        assert!(matches!(factory.algorithm(), ChecksumAlgorithm::MD5));
    }

    #[test]
    fn from_negotiation_defaults_to_md4_for_protocol_29() {
        let factory = ChecksumFactory::from_negotiation(None, protocol(29), 42, None);

        assert!(matches!(factory.algorithm(), ChecksumAlgorithm::MD4));
    }

    #[test]
    fn from_negotiation_respects_checksum_seed_fix_flag() {
        let compat_flags = CompatibilityFlags::CHECKSUM_SEED_FIX;
        let factory =
            ChecksumFactory::from_negotiation(None, protocol(31), 100, Some(&compat_flags));

        assert!(factory.uses_proper_seed_order());
    }

    #[test]
    fn from_negotiation_without_checksum_seed_fix_flag() {
        let compat_flags = CompatibilityFlags::INC_RECURSE;
        let factory =
            ChecksumFactory::from_negotiation(None, protocol(31), 100, Some(&compat_flags));

        assert!(!factory.uses_proper_seed_order());
    }

    #[test]
    fn new_creates_factory_with_explicit_values() {
        let factory = ChecksumFactory::new(ChecksumAlgorithm::SHA1, 999, true);

        assert!(matches!(factory.algorithm(), ChecksumAlgorithm::SHA1));
        assert_eq!(factory.seed(), 999);
        assert!(factory.uses_proper_seed_order());
    }

    #[test]
    fn default_creates_md4_with_zero_seed() {
        let factory = ChecksumFactory::default();

        assert!(matches!(factory.algorithm(), ChecksumAlgorithm::MD4));
        assert_eq!(factory.seed(), 0);
        assert!(!factory.uses_proper_seed_order());
    }

    // ------------------------------------------------------------------------
    // Signature algorithm conversion tests
    // ------------------------------------------------------------------------

    #[test]
    fn signature_algorithm_none_returns_md4() {
        let factory = ChecksumFactory::new(ChecksumAlgorithm::None, 0, false);
        assert!(matches!(
            factory.signature_algorithm(),
            SignatureAlgorithm::Md4
        ));
    }

    #[test]
    fn signature_algorithm_md4() {
        let factory = ChecksumFactory::new(ChecksumAlgorithm::MD4, 0, false);
        assert!(matches!(
            factory.signature_algorithm(),
            SignatureAlgorithm::Md4
        ));
    }

    #[test]
    fn signature_algorithm_md5_legacy_seed() {
        let factory = ChecksumFactory::new(ChecksumAlgorithm::MD5, 12345, false);
        let sig = factory.signature_algorithm();

        if let SignatureAlgorithm::Md5 { seed_config } = sig {
            // Legacy seed order
            assert!(!seed_config.proper_order);
        } else {
            panic!("Expected Md5 signature algorithm");
        }
    }

    #[test]
    fn signature_algorithm_md5_proper_seed() {
        let factory = ChecksumFactory::new(ChecksumAlgorithm::MD5, 12345, true);
        let sig = factory.signature_algorithm();

        if let SignatureAlgorithm::Md5 { seed_config } = sig {
            // Proper seed order
            assert!(seed_config.proper_order);
        } else {
            panic!("Expected Md5 signature algorithm");
        }
    }

    #[test]
    fn signature_algorithm_sha1() {
        let factory = ChecksumFactory::new(ChecksumAlgorithm::SHA1, 0, false);
        assert!(matches!(
            factory.signature_algorithm(),
            SignatureAlgorithm::Sha1
        ));
    }

    #[test]
    fn signature_algorithm_xxh64_with_seed() {
        let factory = ChecksumFactory::new(ChecksumAlgorithm::XXH64, 42, false);
        let sig = factory.signature_algorithm();

        if let SignatureAlgorithm::Xxh64 { seed } = sig {
            assert_eq!(seed, 42);
        } else {
            panic!("Expected Xxh64 signature algorithm");
        }
    }

    #[test]
    fn signature_algorithm_xxh3_with_seed() {
        let factory = ChecksumFactory::new(ChecksumAlgorithm::XXH3, 99, false);
        let sig = factory.signature_algorithm();

        if let SignatureAlgorithm::Xxh3 { seed } = sig {
            assert_eq!(seed, 99);
        } else {
            panic!("Expected Xxh3 signature algorithm");
        }
    }

    #[test]
    fn signature_algorithm_xxh128_with_seed() {
        let factory = ChecksumFactory::new(ChecksumAlgorithm::XXH128, 1000, false);
        let sig = factory.signature_algorithm();

        if let SignatureAlgorithm::Xxh3_128 { seed } = sig {
            assert_eq!(seed, 1000);
        } else {
            panic!("Expected Xxh3_128 signature algorithm");
        }
    }

    // ------------------------------------------------------------------------
    // Digest length tests
    // ------------------------------------------------------------------------

    #[test]
    fn digest_length_none() {
        let factory = ChecksumFactory::new(ChecksumAlgorithm::None, 0, false);
        assert_eq!(factory.digest_length(), 16);
    }

    #[test]
    fn digest_length_md4() {
        let factory = ChecksumFactory::new(ChecksumAlgorithm::MD4, 0, false);
        assert_eq!(factory.digest_length(), 16);
    }

    #[test]
    fn digest_length_md5() {
        let factory = ChecksumFactory::new(ChecksumAlgorithm::MD5, 0, false);
        assert_eq!(factory.digest_length(), 16);
    }

    #[test]
    fn digest_length_sha1() {
        let factory = ChecksumFactory::new(ChecksumAlgorithm::SHA1, 0, false);
        assert_eq!(factory.digest_length(), 20);
    }

    #[test]
    fn digest_length_xxh64() {
        let factory = ChecksumFactory::new(ChecksumAlgorithm::XXH64, 0, false);
        assert_eq!(factory.digest_length(), 8);
    }

    #[test]
    fn digest_length_xxh3() {
        let factory = ChecksumFactory::new(ChecksumAlgorithm::XXH3, 0, false);
        assert_eq!(factory.digest_length(), 8);
    }

    #[test]
    fn digest_length_xxh128() {
        let factory = ChecksumFactory::new(ChecksumAlgorithm::XXH128, 0, false);
        assert_eq!(factory.digest_length(), 16);
    }

    // ------------------------------------------------------------------------
    // MD5 seed configuration tests
    // ------------------------------------------------------------------------

    #[test]
    fn legacy_md5_seed_returns_legacy_config() {
        let factory = ChecksumFactory::new(ChecksumAlgorithm::MD5, 555, false);
        let seed = factory.legacy_md5_seed();
        assert!(!seed.proper_order);
    }

    #[test]
    fn proper_md5_seed_returns_proper_config() {
        let factory = ChecksumFactory::new(ChecksumAlgorithm::MD5, 555, false);
        let seed = factory.proper_md5_seed();
        assert!(seed.proper_order);
    }

    #[test]
    fn md5_seed_respects_factory_setting_legacy() {
        let factory = ChecksumFactory::new(ChecksumAlgorithm::MD5, 777, false);
        let seed = factory.md5_seed();
        assert!(!seed.proper_order);
    }

    #[test]
    fn md5_seed_respects_factory_setting_proper() {
        let factory = ChecksumFactory::new(ChecksumAlgorithm::MD5, 777, true);
        let seed = factory.md5_seed();
        assert!(seed.proper_order);
    }

    // ------------------------------------------------------------------------
    // Integration tests
    // ------------------------------------------------------------------------

    #[test]
    fn factory_clone_preserves_values() {
        let factory = ChecksumFactory::new(ChecksumAlgorithm::XXH3, 123, true);
        let cloned = factory;

        assert!(matches!(cloned.algorithm(), ChecksumAlgorithm::XXH3));
        assert_eq!(cloned.seed(), 123);
        assert!(cloned.uses_proper_seed_order());
    }

    #[test]
    fn all_algorithms_produce_valid_signature() {
        let algorithms = [
            ChecksumAlgorithm::None,
            ChecksumAlgorithm::MD4,
            ChecksumAlgorithm::MD5,
            ChecksumAlgorithm::SHA1,
            ChecksumAlgorithm::XXH64,
            ChecksumAlgorithm::XXH3,
            ChecksumAlgorithm::XXH128,
        ];

        for algo in algorithms {
            let factory = ChecksumFactory::new(algo, 42, false);
            let _sig = factory.signature_algorithm();
            let _len = factory.digest_length();
            // Just verify no panics occur
        }
    }

    #[test]
    fn negative_seed_handled_correctly() {
        let factory = ChecksumFactory::new(ChecksumAlgorithm::XXH64, -1, false);
        let sig = factory.signature_algorithm();

        if let SignatureAlgorithm::Xxh64 { seed } = sig {
            // -1 as i32 becomes a large positive u64
            assert_eq!(seed, u64::MAX);
        } else {
            panic!("Expected Xxh64");
        }
    }

    #[test]
    fn protocol_version_boundary_28_uses_md4() {
        let factory = ChecksumFactory::from_negotiation(None, protocol(28), 0, None);
        assert!(matches!(factory.algorithm(), ChecksumAlgorithm::MD4));
    }

    #[test]
    fn protocol_version_boundary_31_uses_md5() {
        let factory = ChecksumFactory::from_negotiation(None, protocol(31), 0, None);
        assert!(matches!(factory.algorithm(), ChecksumAlgorithm::MD5));
    }

    #[test]
    fn compat_flags_combined_with_checksum_seed_fix() {
        let compat_flags = CompatibilityFlags::INC_RECURSE | CompatibilityFlags::CHECKSUM_SEED_FIX;
        let factory = ChecksumFactory::from_negotiation(None, protocol(31), 0, Some(&compat_flags));
        assert!(factory.uses_proper_seed_order());
    }
}
