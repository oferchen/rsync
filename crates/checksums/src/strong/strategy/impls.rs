//! Concrete `ChecksumStrategy` implementations for each algorithm.

use super::digest::ChecksumDigest;
use super::kind::ChecksumAlgorithmKind;
use super::trait_def::ChecksumStrategy;
use crate::strong::{Md4, Md5, Md5Seed, Sha1, Sha256, Sha512, StrongDigest, Xxh3, Xxh3_128, Xxh64};

/// MD4 checksum strategy for rsync protocol versions < 30.
///
/// Upstream rsync selects MD4 as the default strong checksum when the
/// negotiated protocol version is below 30. See upstream `checksum.c`.
#[derive(Clone, Copy, Debug, Default)]
pub struct Md4Strategy;

impl Md4Strategy {
    /// Creates a new MD4 strategy.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl ChecksumStrategy for Md4Strategy {
    fn compute(&self, data: &[u8]) -> ChecksumDigest {
        ChecksumDigest::new(Md4::digest(data).as_ref())
    }

    fn digest_len(&self) -> usize {
        Md4::DIGEST_LEN
    }

    fn algorithm_kind(&self) -> ChecksumAlgorithmKind {
        ChecksumAlgorithmKind::Md4
    }
}

/// MD5 checksum strategy with optional seeding for rsync protocol versions >= 30.
///
/// Supports seeded hashing with configurable seed ordering. The
/// `CHECKSUM_SEED_FIX` flag controls whether the seed is hashed before
/// (proper) or after (legacy) the file data - see upstream `checksum.c`.
#[derive(Clone, Copy, Debug)]
pub struct Md5Strategy {
    seed: Md5Seed,
}

impl Md5Strategy {
    /// Creates a new unseeded MD5 strategy.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            seed: Md5Seed::none(),
        }
    }

    /// Creates an MD5 strategy with proper seed ordering (protocol 30+).
    #[must_use]
    pub const fn with_proper_seed(seed_value: i32) -> Self {
        Self {
            seed: Md5Seed::proper(seed_value),
        }
    }

    /// Creates an MD5 strategy with legacy seed ordering (protocol < 30).
    #[must_use]
    pub const fn with_legacy_seed(seed_value: i32) -> Self {
        Self {
            seed: Md5Seed::legacy(seed_value),
        }
    }

    /// Creates an MD5 strategy with the provided seed configuration.
    #[must_use]
    pub const fn with_seed(seed: Md5Seed) -> Self {
        Self { seed }
    }
}

impl Default for Md5Strategy {
    fn default() -> Self {
        Self::new()
    }
}

impl ChecksumStrategy for Md5Strategy {
    fn compute(&self, data: &[u8]) -> ChecksumDigest {
        ChecksumDigest::new(Md5::digest_with_seed(self.seed, data).as_ref())
    }

    fn digest_len(&self) -> usize {
        Md5::DIGEST_LEN
    }

    fn algorithm_kind(&self) -> ChecksumAlgorithmKind {
        ChecksumAlgorithmKind::Md5
    }
}

/// SHA-1 checksum strategy (160-bit output).
///
/// Negotiated via checksum capability strings in protocol 31+.
#[derive(Clone, Copy, Debug, Default)]
pub struct Sha1Strategy;

impl Sha1Strategy {
    /// Creates a new SHA-1 strategy.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl ChecksumStrategy for Sha1Strategy {
    fn compute(&self, data: &[u8]) -> ChecksumDigest {
        ChecksumDigest::new(Sha1::digest(data).as_ref())
    }

    fn digest_len(&self) -> usize {
        Sha1::DIGEST_LEN
    }

    fn algorithm_kind(&self) -> ChecksumAlgorithmKind {
        ChecksumAlgorithmKind::Sha1
    }
}

/// SHA-256 checksum strategy (256-bit output).
///
/// Used for daemon authentication and high-security transfers.
#[derive(Clone, Copy, Debug, Default)]
pub struct Sha256Strategy;

impl Sha256Strategy {
    /// Creates a new SHA-256 strategy.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl ChecksumStrategy for Sha256Strategy {
    fn compute(&self, data: &[u8]) -> ChecksumDigest {
        ChecksumDigest::new(Sha256::digest(data).as_ref())
    }

    fn digest_len(&self) -> usize {
        Sha256::DIGEST_LEN
    }

    fn algorithm_kind(&self) -> ChecksumAlgorithmKind {
        ChecksumAlgorithmKind::Sha256
    }
}

/// SHA-512 checksum strategy (512-bit output).
///
/// Provides maximum collision resistance among the supported algorithms.
#[derive(Clone, Copy, Debug, Default)]
pub struct Sha512Strategy;

impl Sha512Strategy {
    /// Creates a new SHA-512 strategy.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl ChecksumStrategy for Sha512Strategy {
    fn compute(&self, data: &[u8]) -> ChecksumDigest {
        ChecksumDigest::new(Sha512::digest(data).as_ref())
    }

    fn digest_len(&self) -> usize {
        Sha512::DIGEST_LEN
    }

    fn algorithm_kind(&self) -> ChecksumAlgorithmKind {
        ChecksumAlgorithmKind::Sha512
    }
}

/// XXH64 checksum strategy with seeding (64-bit output).
///
/// Used by rsync protocol >= 30 for block checksums when XXH3 is not negotiated.
#[derive(Clone, Copy, Debug)]
pub struct Xxh64Strategy {
    seed: u64,
}

impl Xxh64Strategy {
    /// Creates a new XXH64 strategy with the given seed.
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self { seed }
    }

    /// Creates a new XXH64 strategy with zero seed.
    #[must_use]
    pub const fn unseeded() -> Self {
        Self::new(0)
    }
}

impl Default for Xxh64Strategy {
    fn default() -> Self {
        Self::unseeded()
    }
}

impl ChecksumStrategy for Xxh64Strategy {
    fn compute(&self, data: &[u8]) -> ChecksumDigest {
        ChecksumDigest::new(Xxh64::digest(self.seed, data).as_ref())
    }

    fn digest_len(&self) -> usize {
        Xxh64::DIGEST_LEN
    }

    fn algorithm_kind(&self) -> ChecksumAlgorithmKind {
        ChecksumAlgorithmKind::Xxh64
    }
}

/// XXH3 (64-bit) checksum strategy with seeding.
///
/// Fastest available hash - negotiated via the `-e.LsfxCIvu` capability string.
/// One-shot calls use runtime SIMD detection (AVX2/NEON).
#[derive(Clone, Copy, Debug)]
pub struct Xxh3Strategy {
    seed: u64,
}

impl Xxh3Strategy {
    /// Creates a new XXH3 strategy with the given seed.
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self { seed }
    }

    /// Creates a new XXH3 strategy with zero seed.
    #[must_use]
    pub const fn unseeded() -> Self {
        Self::new(0)
    }
}

impl Default for Xxh3Strategy {
    fn default() -> Self {
        Self::unseeded()
    }
}

impl ChecksumStrategy for Xxh3Strategy {
    fn compute(&self, data: &[u8]) -> ChecksumDigest {
        ChecksumDigest::new(Xxh3::digest(self.seed, data).as_ref())
    }

    fn digest_len(&self) -> usize {
        Xxh3::DIGEST_LEN
    }

    fn algorithm_kind(&self) -> ChecksumAlgorithmKind {
        ChecksumAlgorithmKind::Xxh3
    }
}

/// XXH3-128 checksum strategy with seeding (128-bit output).
///
/// Provides lower collision probability than XXH3-64 while maintaining
/// comparable throughput. One-shot calls use runtime SIMD detection.
#[derive(Clone, Copy, Debug)]
pub struct Xxh3_128Strategy {
    seed: u64,
}

impl Xxh3_128Strategy {
    /// Creates a new XXH3-128 strategy with the given seed.
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self { seed }
    }

    /// Creates a new XXH3-128 strategy with zero seed.
    #[must_use]
    pub const fn unseeded() -> Self {
        Self::new(0)
    }
}

impl Default for Xxh3_128Strategy {
    fn default() -> Self {
        Self::unseeded()
    }
}

impl ChecksumStrategy for Xxh3_128Strategy {
    fn compute(&self, data: &[u8]) -> ChecksumDigest {
        ChecksumDigest::new(Xxh3_128::digest(self.seed, data).as_ref())
    }

    fn digest_len(&self) -> usize {
        Xxh3_128::DIGEST_LEN
    }

    fn algorithm_kind(&self) -> ChecksumAlgorithmKind {
        ChecksumAlgorithmKind::Xxh3_128
    }
}
