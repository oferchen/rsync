//! Factory for creating checksum strategies based on algorithm selection.

use super::impls::{
    Md4Strategy, Md5Strategy, Sha1Strategy, Sha256Strategy, Sha512Strategy, Xxh3_128Strategy,
    Xxh3Strategy, Xxh64Strategy,
};
use super::kind::ChecksumAlgorithmKind;
use super::seed::{SeedConfig, extract_u64_seed};
use super::trait_def::ChecksumStrategy;
use crate::strong::Md5Seed;

/// Factory for creating checksum strategies based on algorithm selection.
///
/// Provides the Strategy pattern's context, allowing runtime selection of
/// checksum algorithms based on protocol version, explicit algorithm choice,
/// or negotiated capabilities.
pub struct ChecksumStrategySelector;

impl ChecksumStrategySelector {
    /// Selects the default algorithm for a given protocol version.
    ///
    /// # Protocol Defaults
    ///
    /// - Protocol < 30: MD4
    /// - Protocol >= 30: MD5
    ///
    /// # Example
    ///
    /// ```
    /// use checksums::strong::strategy::ChecksumStrategySelector;
    ///
    /// let strategy = ChecksumStrategySelector::for_protocol_version(30, 12345);
    /// assert_eq!(strategy.algorithm_name(), "md5");
    /// ```
    #[must_use]
    pub fn for_protocol_version(protocol_version: u8, seed: i32) -> Box<dyn ChecksumStrategy> {
        if protocol_version >= 30 {
            Box::new(Md5Strategy::with_legacy_seed(seed))
        } else {
            Box::new(Md4Strategy::new())
        }
    }

    /// Selects the default algorithm with configurable MD5 seed ordering.
    ///
    /// Use this when you have explicit control over the `CHECKSUM_SEED_FIX`
    /// compatibility flag.
    #[must_use]
    pub fn for_protocol_version_with_seed_order(
        protocol_version: u8,
        seed: i32,
        proper_seed_order: bool,
    ) -> Box<dyn ChecksumStrategy> {
        if protocol_version >= 30 {
            let md5_seed = if proper_seed_order {
                Md5Seed::proper(seed)
            } else {
                Md5Seed::legacy(seed)
            };
            Box::new(Md5Strategy::with_seed(md5_seed))
        } else {
            Box::new(Md4Strategy::new())
        }
    }

    /// Creates a strategy for the specified algorithm kind.
    ///
    /// # Example
    ///
    /// ```
    /// use checksums::strong::strategy::{
    ///     ChecksumStrategySelector, ChecksumAlgorithmKind,
    /// };
    ///
    /// let strategy = ChecksumStrategySelector::for_algorithm(
    ///     ChecksumAlgorithmKind::Xxh3,
    ///     0x12345678,
    /// );
    /// assert_eq!(strategy.algorithm_name(), "xxh3");
    /// ```
    #[must_use]
    pub fn for_algorithm(kind: ChecksumAlgorithmKind, seed: i32) -> Box<dyn ChecksumStrategy> {
        match kind {
            ChecksumAlgorithmKind::Md4 => Box::new(Md4Strategy::new()),
            ChecksumAlgorithmKind::Md5 => Box::new(Md5Strategy::with_legacy_seed(seed)),
            ChecksumAlgorithmKind::Sha1 => Box::new(Sha1Strategy::new()),
            ChecksumAlgorithmKind::Sha256 => Box::new(Sha256Strategy::new()),
            ChecksumAlgorithmKind::Sha512 => Box::new(Sha512Strategy::new()),
            ChecksumAlgorithmKind::Xxh64 => Box::new(Xxh64Strategy::new(seed as u64)),
            ChecksumAlgorithmKind::Xxh3 => Box::new(Xxh3Strategy::new(seed as u64)),
            ChecksumAlgorithmKind::Xxh3_128 => Box::new(Xxh3_128Strategy::new(seed as u64)),
        }
    }

    /// Creates a strategy with advanced seed configuration.
    ///
    /// Provides full control over algorithm-specific seed handling.
    ///
    /// # Example
    ///
    /// ```
    /// use checksums::strong::strategy::{
    ///     ChecksumStrategySelector, ChecksumAlgorithmKind, SeedConfig, Md5SeedConfig,
    /// };
    ///
    /// let strategy = ChecksumStrategySelector::with_seed_config(
    ///     ChecksumAlgorithmKind::Md5,
    ///     SeedConfig::Md5(Md5SeedConfig::Legacy(12345)),
    /// );
    /// ```
    #[must_use]
    pub fn with_seed_config(
        kind: ChecksumAlgorithmKind,
        seed: SeedConfig,
    ) -> Box<dyn ChecksumStrategy> {
        match kind {
            ChecksumAlgorithmKind::Md4 => Box::new(Md4Strategy::new()),
            ChecksumAlgorithmKind::Md5 => {
                let md5_seed = match seed {
                    SeedConfig::Md5(config) => config.to_md5_seed(),
                    SeedConfig::Seed64(s) => Md5Seed::proper(s as i32),
                    SeedConfig::None => Md5Seed::none(),
                };
                Box::new(Md5Strategy::with_seed(md5_seed))
            }
            ChecksumAlgorithmKind::Sha1 => Box::new(Sha1Strategy::new()),
            ChecksumAlgorithmKind::Sha256 => Box::new(Sha256Strategy::new()),
            ChecksumAlgorithmKind::Sha512 => Box::new(Sha512Strategy::new()),
            ChecksumAlgorithmKind::Xxh64 => Box::new(Xxh64Strategy::new(extract_u64_seed(&seed))),
            ChecksumAlgorithmKind::Xxh3 => Box::new(Xxh3Strategy::new(extract_u64_seed(&seed))),
            ChecksumAlgorithmKind::Xxh3_128 => {
                Box::new(Xxh3_128Strategy::new(extract_u64_seed(&seed)))
            }
        }
    }

    /// Creates a concrete (non-boxed) MD4 strategy.
    #[must_use]
    pub const fn md4() -> Md4Strategy {
        Md4Strategy::new()
    }

    /// Creates a concrete MD5 strategy.
    #[must_use]
    pub const fn md5() -> Md5Strategy {
        Md5Strategy::new()
    }

    /// Creates a concrete MD5 strategy with proper seed.
    #[must_use]
    pub const fn md5_proper(seed: i32) -> Md5Strategy {
        Md5Strategy::with_proper_seed(seed)
    }

    /// Creates a concrete MD5 strategy with legacy seed.
    #[must_use]
    pub const fn md5_legacy(seed: i32) -> Md5Strategy {
        Md5Strategy::with_legacy_seed(seed)
    }

    /// Creates a concrete SHA-1 strategy.
    #[must_use]
    pub const fn sha1() -> Sha1Strategy {
        Sha1Strategy::new()
    }

    /// Creates a concrete SHA-256 strategy.
    #[must_use]
    pub const fn sha256() -> Sha256Strategy {
        Sha256Strategy::new()
    }

    /// Creates a concrete SHA-512 strategy.
    #[must_use]
    pub const fn sha512() -> Sha512Strategy {
        Sha512Strategy::new()
    }

    /// Creates a concrete XXH64 strategy.
    #[must_use]
    pub const fn xxh64(seed: u64) -> Xxh64Strategy {
        Xxh64Strategy::new(seed)
    }

    /// Creates a concrete XXH3 strategy.
    #[must_use]
    pub const fn xxh3(seed: u64) -> Xxh3Strategy {
        Xxh3Strategy::new(seed)
    }

    /// Creates a concrete XXH3-128 strategy.
    #[must_use]
    pub const fn xxh3_128(seed: u64) -> Xxh3_128Strategy {
        Xxh3_128Strategy::new(seed)
    }
}
