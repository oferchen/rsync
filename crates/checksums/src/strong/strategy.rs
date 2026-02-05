//! Strategy pattern for runtime checksum algorithm selection.
//!
//! This module provides a Strategy pattern implementation that allows runtime
//! selection of checksum algorithms based on protocol version, negotiated
//! capabilities, or explicit configuration.
//!
//! # Overview
//!
//! The Strategy pattern separates the algorithm selection logic from the
//! checksum computation, enabling:
//!
//! - Runtime algorithm selection without code duplication
//! - Protocol version-aware defaults
//! - Clean interface for adding new algorithms
//! - Type-safe seed handling
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────┐
//! │                    ChecksumStrategy (trait)                     │
//! │  ┌──────────────────────────────────────────────────────────┐   │
//! │  │ + compute(&self, data: &[u8]) -> ChecksumDigest          │   │
//! │  │ + compute_into(&self, data: &[u8], out: &mut [u8])       │   │
//! │  │ + digest_len(&self) -> usize                             │   │
//! │  │ + algorithm_name(&self) -> &'static str                  │   │
//! │  └──────────────────────────────────────────────────────────┘   │
//! └─────────────────────────────────────────────────────────────────┘
//!                                 ▲
//!                                 │ implements
//!         ┌───────────────────────┼───────────────────────┐
//!         │                       │                       │
//! ┌───────┴───────┐       ┌───────┴───────┐       ┌───────┴───────┐
//! │  Md4Strategy  │       │  Md5Strategy  │       │ Xxh3Strategy  │
//! └───────────────┘       └───────────────┘       └───────────────┘
//!
//! ┌─────────────────────────────────────────────────────────────────┐
//! │              ChecksumStrategySelector (factory)                 │
//! │  ┌──────────────────────────────────────────────────────────┐   │
//! │  │ + for_protocol(version, seed) -> Box<dyn ChecksumStrategy>│  │
//! │  │ + for_algorithm(algo, seed) -> Box<dyn ChecksumStrategy>  │  │
//! │  └──────────────────────────────────────────────────────────┘   │
//! └─────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Example
//!
//! ```
//! use checksums::strong::strategy::{
//!     ChecksumStrategy, ChecksumStrategySelector, ChecksumAlgorithmKind,
//! };
//!
//! // Select algorithm based on protocol version
//! let strategy = ChecksumStrategySelector::for_protocol_version(30, 0x12345678);
//! let digest = strategy.compute(b"data");
//! println!("Digest length: {} bytes", digest.len());
//!
//! // Select algorithm explicitly
//! let xxh3_strategy = ChecksumStrategySelector::for_algorithm(
//!     ChecksumAlgorithmKind::Xxh3,
//!     0x12345678,
//! );
//! let xxh3_digest = xxh3_strategy.compute(b"fast hash");
//! ```
//!
//! # Protocol Version Defaults
//!
//! | Protocol | Default Algorithm |
//! |----------|-------------------|
//! | < 30     | MD4               |
//! | >= 30    | MD5               |
//! | >= 31*   | XXH3 (if negotiated) |
//!
//! *XXH3 requires explicit negotiation in protocol 31+

use super::{Md4, Md5, Md5Seed, Sha1, Sha256, Sha512, StrongDigest, Xxh3, Xxh3_128, Xxh64};
use std::fmt;

// ============================================================================
// ChecksumDigest - Unified digest representation
// ============================================================================

/// Maximum digest length for any supported algorithm (SHA-512 = 64 bytes).
pub const MAX_DIGEST_LEN: usize = 64;

/// A checksum digest with a fixed maximum capacity.
///
/// This type avoids heap allocation for digest storage by using a fixed-size
/// buffer that can hold any supported digest. The actual digest length varies
/// by algorithm.
#[derive(Clone, Copy)]
pub struct ChecksumDigest {
    /// Internal buffer holding the digest bytes.
    buffer: [u8; MAX_DIGEST_LEN],
    /// Actual length of the digest.
    len: usize,
}

impl ChecksumDigest {
    /// Creates a new digest from a byte slice.
    ///
    /// # Panics
    ///
    /// Panics if `bytes.len() > MAX_DIGEST_LEN`.
    #[must_use]
    pub fn new(bytes: &[u8]) -> Self {
        assert!(
            bytes.len() <= MAX_DIGEST_LEN,
            "digest length {} exceeds maximum {}",
            bytes.len(),
            MAX_DIGEST_LEN
        );
        let mut buffer = [0u8; MAX_DIGEST_LEN];
        buffer[..bytes.len()].copy_from_slice(bytes);
        Self {
            buffer,
            len: bytes.len(),
        }
    }

    /// Returns the digest length in bytes.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Returns `true` if the digest is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Returns the digest as a byte slice.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.buffer[..self.len]
    }

    /// Copies the digest into the provided buffer.
    ///
    /// # Panics
    ///
    /// Panics if `out.len() < self.len()`.
    pub fn copy_to(&self, out: &mut [u8]) {
        assert!(
            out.len() >= self.len,
            "output buffer too small: {} < {}",
            out.len(),
            self.len
        );
        out[..self.len].copy_from_slice(&self.buffer[..self.len]);
    }

    /// Returns the digest truncated to the specified length.
    ///
    /// If `len >= self.len()`, returns a copy of the full digest.
    #[must_use]
    pub fn truncated(&self, len: usize) -> Self {
        let actual_len = len.min(self.len);
        let mut buffer = [0u8; MAX_DIGEST_LEN];
        buffer[..actual_len].copy_from_slice(&self.buffer[..actual_len]);
        Self {
            buffer,
            len: actual_len,
        }
    }
}

impl AsRef<[u8]> for ChecksumDigest {
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}

impl PartialEq for ChecksumDigest {
    fn eq(&self, other: &Self) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}

impl Eq for ChecksumDigest {}

impl fmt::Debug for ChecksumDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ChecksumDigest({:02x?})", self.as_bytes())
    }
}

impl fmt::Display for ChecksumDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.as_bytes() {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

// ============================================================================
// ChecksumAlgorithmKind - Algorithm enumeration
// ============================================================================

/// Enumeration of supported checksum algorithms.
///
/// This enum identifies the checksum algorithm without carrying seed/config
/// data, making it suitable for algorithm selection and comparison.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum ChecksumAlgorithmKind {
    /// MD4 - Legacy algorithm for rsync protocol < 30.
    Md4,
    /// MD5 - Default for rsync protocol >= 30.
    Md5,
    /// SHA-1 - Optional stronger algorithm.
    Sha1,
    /// SHA-256 - Cryptographically secure option.
    Sha256,
    /// SHA-512 - Maximum security option.
    Sha512,
    /// XXH64 - Fast 64-bit non-cryptographic hash.
    Xxh64,
    /// XXH3 - Fastest 64-bit non-cryptographic hash.
    Xxh3,
    /// XXH3-128 - Fast 128-bit non-cryptographic hash.
    Xxh3_128,
}

impl ChecksumAlgorithmKind {
    /// Returns the canonical name for the algorithm.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Md4 => "md4",
            Self::Md5 => "md5",
            Self::Sha1 => "sha1",
            Self::Sha256 => "sha256",
            Self::Sha512 => "sha512",
            Self::Xxh64 => "xxh64",
            Self::Xxh3 => "xxh3",
            Self::Xxh3_128 => "xxh128",
        }
    }

    /// Returns the digest length for the algorithm in bytes.
    #[must_use]
    pub const fn digest_len(&self) -> usize {
        match self {
            Self::Md4 | Self::Md5 | Self::Xxh3_128 => 16,
            Self::Sha1 => 20,
            Self::Sha256 => 32,
            Self::Sha512 => 64,
            Self::Xxh64 | Self::Xxh3 => 8,
        }
    }

    /// Returns `true` if this is a cryptographic hash algorithm.
    #[must_use]
    pub const fn is_cryptographic(&self) -> bool {
        matches!(
            self,
            Self::Md4 | Self::Md5 | Self::Sha1 | Self::Sha256 | Self::Sha512
        )
    }

    /// Parses an algorithm from a string name.
    ///
    /// Accepts canonical names and common aliases (case-insensitive).
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "md4" => Some(Self::Md4),
            "md5" => Some(Self::Md5),
            "sha1" | "sha-1" => Some(Self::Sha1),
            "sha256" | "sha-256" => Some(Self::Sha256),
            "sha512" | "sha-512" => Some(Self::Sha512),
            "xxh64" | "xxhash64" => Some(Self::Xxh64),
            "xxh3" | "xxhash3" => Some(Self::Xxh3),
            "xxh128" | "xxh3-128" | "xxhash128" => Some(Self::Xxh3_128),
            _ => None,
        }
    }

    /// Returns all supported algorithm kinds.
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[
            Self::Md4,
            Self::Md5,
            Self::Sha1,
            Self::Sha256,
            Self::Sha512,
            Self::Xxh64,
            Self::Xxh3,
            Self::Xxh3_128,
        ]
    }
}

impl fmt::Display for ChecksumAlgorithmKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

// ============================================================================
// ChecksumStrategy - Core trait
// ============================================================================

/// Strategy trait for checksum computation.
///
/// Implementations provide algorithm-specific checksum computation while
/// exposing a uniform interface for callers.
///
/// # Thread Safety
///
/// Implementations must be `Send + Sync` to support concurrent usage.
///
/// # Example
///
/// ```
/// use checksums::strong::strategy::{ChecksumStrategy, Md5Strategy};
///
/// let strategy = Md5Strategy::new();
/// let digest = strategy.compute(b"hello world");
/// assert_eq!(digest.len(), 16);
/// ```
pub trait ChecksumStrategy: Send + Sync {
    /// Computes the checksum digest for the input data.
    fn compute(&self, data: &[u8]) -> ChecksumDigest;

    /// Computes the checksum and writes it to the output buffer.
    ///
    /// The buffer must be at least [`digest_len()`](Self::digest_len) bytes.
    ///
    /// # Panics
    ///
    /// Panics if `out.len() < self.digest_len()`.
    fn compute_into(&self, data: &[u8], out: &mut [u8]) {
        let digest = self.compute(data);
        digest.copy_to(out);
    }

    /// Returns the digest length for this algorithm in bytes.
    fn digest_len(&self) -> usize;

    /// Returns the algorithm kind for this strategy.
    fn algorithm_kind(&self) -> ChecksumAlgorithmKind;

    /// Returns the human-readable algorithm name.
    fn algorithm_name(&self) -> &'static str {
        self.algorithm_kind().name()
    }
}

// ============================================================================
// Concrete Strategy Implementations
// ============================================================================

/// MD4 checksum strategy.
///
/// Used by rsync protocol versions < 30 as the default strong checksum.
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

/// MD5 checksum strategy with optional seeding.
///
/// Used by rsync protocol versions >= 30. Supports seeded hashing with
/// configurable seed ordering for protocol compatibility.
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

/// SHA-1 checksum strategy.
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

/// SHA-256 checksum strategy.
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

/// SHA-512 checksum strategy.
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

/// XXH64 checksum strategy with seeding.
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

/// XXH3-128 checksum strategy with seeding.
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

// ============================================================================
// ChecksumStrategySelector - Factory for strategy selection
// ============================================================================

/// Configuration for MD5 seed handling.
#[derive(Clone, Copy, Debug)]
pub enum Md5SeedConfig {
    /// No seed (default MD5 behavior).
    None,
    /// Proper seed ordering (seed hashed before data) - protocol 30+.
    Proper(i32),
    /// Legacy seed ordering (seed hashed after data) - protocol < 30.
    Legacy(i32),
}

impl Md5SeedConfig {
    /// Converts to the internal `Md5Seed` type.
    #[must_use]
    pub const fn to_md5_seed(self) -> Md5Seed {
        match self {
            Self::None => Md5Seed::none(),
            Self::Proper(v) => Md5Seed::proper(v),
            Self::Legacy(v) => Md5Seed::legacy(v),
        }
    }
}

impl Default for Md5SeedConfig {
    fn default() -> Self {
        Self::None
    }
}

/// Seed configuration for strategy creation.
///
/// This unified seed type handles the different seeding requirements of
/// various algorithms.
#[derive(Clone, Copy, Debug)]
pub enum SeedConfig {
    /// No seed (for algorithms that don't support seeding).
    None,
    /// 64-bit seed (for XXHash variants).
    Seed64(u64),
    /// MD5-specific seed configuration.
    Md5(Md5SeedConfig),
}

impl Default for SeedConfig {
    fn default() -> Self {
        Self::None
    }
}

impl From<u64> for SeedConfig {
    fn from(seed: u64) -> Self {
        Self::Seed64(seed)
    }
}

impl From<i32> for SeedConfig {
    fn from(seed: i32) -> Self {
        Self::Seed64(seed as u64)
    }
}

impl From<Md5SeedConfig> for SeedConfig {
    fn from(config: Md5SeedConfig) -> Self {
        Self::Md5(config)
    }
}

/// Factory for creating checksum strategies based on algorithm selection.
///
/// This selector provides the Strategy pattern's context, allowing runtime
/// selection of checksum algorithms based on:
///
/// - Protocol version
/// - Explicit algorithm choice
/// - Negotiated capabilities
pub struct ChecksumStrategySelector;

impl ChecksumStrategySelector {
    /// Selects the default algorithm for a given protocol version.
    ///
    /// # Protocol Defaults
    ///
    /// - Protocol < 30: MD4
    /// - Protocol >= 30: MD5
    ///
    /// # Arguments
    ///
    /// * `protocol_version` - The rsync protocol version number
    /// * `seed` - Seed value for algorithms that support seeding
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
            Box::new(Md5Strategy::with_proper_seed(seed))
        } else {
            Box::new(Md4Strategy::new())
        }
    }

    /// Selects the default algorithm with configurable MD5 seed ordering.
    ///
    /// Use this when you have explicit control over the CHECKSUM_SEED_FIX
    /// compatibility flag.
    ///
    /// # Arguments
    ///
    /// * `protocol_version` - The rsync protocol version number
    /// * `seed` - Seed value for algorithms that support seeding
    /// * `proper_seed_order` - Whether to use proper (protocol 30+) seed ordering
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
    /// # Arguments
    ///
    /// * `kind` - The algorithm to use
    /// * `seed` - Seed value (interpreted based on algorithm)
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
            ChecksumAlgorithmKind::Md5 => Box::new(Md5Strategy::with_proper_seed(seed)),
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
    /// This method provides full control over algorithm-specific seed handling.
    ///
    /// # Example
    ///
    /// ```
    /// use checksums::strong::strategy::{
    ///     ChecksumStrategySelector, ChecksumAlgorithmKind, SeedConfig, Md5SeedConfig,
    /// };
    ///
    /// // MD5 with legacy seed ordering
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
            ChecksumAlgorithmKind::Xxh64 => {
                let s = match seed {
                    SeedConfig::Seed64(s) => s,
                    SeedConfig::Md5(Md5SeedConfig::Proper(s) | Md5SeedConfig::Legacy(s)) => {
                        s as u64
                    }
                    _ => 0,
                };
                Box::new(Xxh64Strategy::new(s))
            }
            ChecksumAlgorithmKind::Xxh3 => {
                let s = match seed {
                    SeedConfig::Seed64(s) => s,
                    SeedConfig::Md5(Md5SeedConfig::Proper(s) | Md5SeedConfig::Legacy(s)) => {
                        s as u64
                    }
                    _ => 0,
                };
                Box::new(Xxh3Strategy::new(s))
            }
            ChecksumAlgorithmKind::Xxh3_128 => {
                let s = match seed {
                    SeedConfig::Seed64(s) => s,
                    SeedConfig::Md5(Md5SeedConfig::Proper(s) | Md5SeedConfig::Legacy(s)) => {
                        s as u64
                    }
                    _ => 0,
                };
                Box::new(Xxh3_128Strategy::new(s))
            }
        }
    }

    /// Creates a concrete (non-boxed) strategy for the algorithm.
    ///
    /// Use this when you know the algorithm at compile time and want to avoid
    /// dynamic dispatch overhead.
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

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------------------
    // ChecksumDigest tests
    // ------------------------------------------------------------------------

    #[test]
    fn digest_from_bytes() {
        let bytes = [1, 2, 3, 4, 5];
        let digest = ChecksumDigest::new(&bytes);
        assert_eq!(digest.len(), 5);
        assert_eq!(digest.as_bytes(), &bytes);
    }

    #[test]
    fn digest_empty() {
        let digest = ChecksumDigest::new(&[]);
        assert!(digest.is_empty());
        assert_eq!(digest.len(), 0);
    }

    #[test]
    fn digest_copy_to() {
        let bytes = [1, 2, 3, 4];
        let digest = ChecksumDigest::new(&bytes);
        let mut out = [0u8; 8];
        digest.copy_to(&mut out);
        assert_eq!(&out[..4], &bytes);
    }

    #[test]
    fn digest_truncated() {
        let bytes = [1, 2, 3, 4, 5, 6, 7, 8];
        let digest = ChecksumDigest::new(&bytes);
        let truncated = digest.truncated(4);
        assert_eq!(truncated.len(), 4);
        assert_eq!(truncated.as_bytes(), &[1, 2, 3, 4]);
    }

    #[test]
    fn digest_truncated_longer_than_original() {
        let bytes = [1, 2, 3, 4];
        let digest = ChecksumDigest::new(&bytes);
        let truncated = digest.truncated(10);
        assert_eq!(truncated.len(), 4);
        assert_eq!(truncated.as_bytes(), &bytes);
    }

    #[test]
    fn digest_equality() {
        let d1 = ChecksumDigest::new(&[1, 2, 3]);
        let d2 = ChecksumDigest::new(&[1, 2, 3]);
        let d3 = ChecksumDigest::new(&[1, 2, 4]);
        assert_eq!(d1, d2);
        assert_ne!(d1, d3);
    }

    #[test]
    fn digest_display() {
        let digest = ChecksumDigest::new(&[0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(format!("{digest}"), "deadbeef");
    }

    // ------------------------------------------------------------------------
    // ChecksumAlgorithmKind tests
    // ------------------------------------------------------------------------

    #[test]
    fn algorithm_kind_name() {
        assert_eq!(ChecksumAlgorithmKind::Md4.name(), "md4");
        assert_eq!(ChecksumAlgorithmKind::Md5.name(), "md5");
        assert_eq!(ChecksumAlgorithmKind::Sha1.name(), "sha1");
        assert_eq!(ChecksumAlgorithmKind::Sha256.name(), "sha256");
        assert_eq!(ChecksumAlgorithmKind::Sha512.name(), "sha512");
        assert_eq!(ChecksumAlgorithmKind::Xxh64.name(), "xxh64");
        assert_eq!(ChecksumAlgorithmKind::Xxh3.name(), "xxh3");
        assert_eq!(ChecksumAlgorithmKind::Xxh3_128.name(), "xxh128");
    }

    #[test]
    fn algorithm_kind_digest_len() {
        assert_eq!(ChecksumAlgorithmKind::Md4.digest_len(), 16);
        assert_eq!(ChecksumAlgorithmKind::Md5.digest_len(), 16);
        assert_eq!(ChecksumAlgorithmKind::Sha1.digest_len(), 20);
        assert_eq!(ChecksumAlgorithmKind::Sha256.digest_len(), 32);
        assert_eq!(ChecksumAlgorithmKind::Sha512.digest_len(), 64);
        assert_eq!(ChecksumAlgorithmKind::Xxh64.digest_len(), 8);
        assert_eq!(ChecksumAlgorithmKind::Xxh3.digest_len(), 8);
        assert_eq!(ChecksumAlgorithmKind::Xxh3_128.digest_len(), 16);
    }

    #[test]
    fn algorithm_kind_is_cryptographic() {
        assert!(ChecksumAlgorithmKind::Md4.is_cryptographic());
        assert!(ChecksumAlgorithmKind::Md5.is_cryptographic());
        assert!(ChecksumAlgorithmKind::Sha1.is_cryptographic());
        assert!(ChecksumAlgorithmKind::Sha256.is_cryptographic());
        assert!(ChecksumAlgorithmKind::Sha512.is_cryptographic());
        assert!(!ChecksumAlgorithmKind::Xxh64.is_cryptographic());
        assert!(!ChecksumAlgorithmKind::Xxh3.is_cryptographic());
        assert!(!ChecksumAlgorithmKind::Xxh3_128.is_cryptographic());
    }

    #[test]
    fn algorithm_kind_from_name() {
        assert_eq!(
            ChecksumAlgorithmKind::from_name("md4"),
            Some(ChecksumAlgorithmKind::Md4)
        );
        assert_eq!(
            ChecksumAlgorithmKind::from_name("MD5"),
            Some(ChecksumAlgorithmKind::Md5)
        );
        assert_eq!(
            ChecksumAlgorithmKind::from_name("sha-256"),
            Some(ChecksumAlgorithmKind::Sha256)
        );
        assert_eq!(
            ChecksumAlgorithmKind::from_name("xxhash3"),
            Some(ChecksumAlgorithmKind::Xxh3)
        );
        assert_eq!(ChecksumAlgorithmKind::from_name("invalid"), None);
    }

    #[test]
    fn algorithm_kind_all() {
        let all = ChecksumAlgorithmKind::all();
        assert_eq!(all.len(), 8);
        assert!(all.contains(&ChecksumAlgorithmKind::Md4));
        assert!(all.contains(&ChecksumAlgorithmKind::Xxh3_128));
    }

    // ------------------------------------------------------------------------
    // Strategy implementation tests
    // ------------------------------------------------------------------------

    #[test]
    fn md4_strategy_compute() {
        let strategy = Md4Strategy::new();
        let digest = strategy.compute(b"test");
        assert_eq!(digest.len(), 16);
        assert_eq!(strategy.digest_len(), 16);
        assert_eq!(strategy.algorithm_name(), "md4");
    }

    #[test]
    fn md5_strategy_unseeded() {
        let strategy = Md5Strategy::new();
        let digest = strategy.compute(b"test");
        assert_eq!(digest.len(), 16);
        assert_eq!(strategy.algorithm_name(), "md5");
    }

    #[test]
    fn md5_strategy_proper_seed() {
        let strategy = Md5Strategy::with_proper_seed(12345);
        let digest = strategy.compute(b"test");
        assert_eq!(digest.len(), 16);

        // Verify it differs from unseeded
        let unseeded = Md5Strategy::new();
        assert_ne!(digest, unseeded.compute(b"test"));
    }

    #[test]
    fn md5_strategy_legacy_seed() {
        let strategy = Md5Strategy::with_legacy_seed(12345);
        let digest = strategy.compute(b"test");
        assert_eq!(digest.len(), 16);

        // Verify proper and legacy produce different results
        let proper = Md5Strategy::with_proper_seed(12345);
        assert_ne!(digest, proper.compute(b"test"));
    }

    #[test]
    fn sha1_strategy_compute() {
        let strategy = Sha1Strategy::new();
        let digest = strategy.compute(b"test");
        assert_eq!(digest.len(), 20);
        assert_eq!(strategy.algorithm_name(), "sha1");
    }

    #[test]
    fn sha256_strategy_compute() {
        let strategy = Sha256Strategy::new();
        let digest = strategy.compute(b"test");
        assert_eq!(digest.len(), 32);
        assert_eq!(strategy.algorithm_name(), "sha256");
    }

    #[test]
    fn sha512_strategy_compute() {
        let strategy = Sha512Strategy::new();
        let digest = strategy.compute(b"test");
        assert_eq!(digest.len(), 64);
        assert_eq!(strategy.algorithm_name(), "sha512");
    }

    #[test]
    fn xxh64_strategy_compute() {
        let strategy = Xxh64Strategy::new(42);
        let digest = strategy.compute(b"test");
        assert_eq!(digest.len(), 8);
        assert_eq!(strategy.algorithm_name(), "xxh64");
    }

    #[test]
    fn xxh64_strategy_different_seeds() {
        let s1 = Xxh64Strategy::new(0);
        let s2 = Xxh64Strategy::new(1);
        assert_ne!(s1.compute(b"test"), s2.compute(b"test"));
    }

    #[test]
    fn xxh3_strategy_compute() {
        let strategy = Xxh3Strategy::new(42);
        let digest = strategy.compute(b"test");
        assert_eq!(digest.len(), 8);
        assert_eq!(strategy.algorithm_name(), "xxh3");
    }

    #[test]
    fn xxh3_128_strategy_compute() {
        let strategy = Xxh3_128Strategy::new(42);
        let digest = strategy.compute(b"test");
        assert_eq!(digest.len(), 16);
        assert_eq!(strategy.algorithm_name(), "xxh128");
    }

    #[test]
    fn compute_into_works() {
        let strategy = Md5Strategy::new();
        let mut buffer = [0u8; 32];
        strategy.compute_into(b"test", &mut buffer);

        let digest = strategy.compute(b"test");
        assert_eq!(&buffer[..16], digest.as_bytes());
    }

    // ------------------------------------------------------------------------
    // ChecksumStrategySelector tests
    // ------------------------------------------------------------------------

    #[test]
    fn selector_for_protocol_version_28() {
        let strategy = ChecksumStrategySelector::for_protocol_version(28, 0);
        assert_eq!(strategy.algorithm_name(), "md4");
    }

    #[test]
    fn selector_for_protocol_version_29() {
        let strategy = ChecksumStrategySelector::for_protocol_version(29, 0);
        assert_eq!(strategy.algorithm_name(), "md4");
    }

    #[test]
    fn selector_for_protocol_version_30() {
        let strategy = ChecksumStrategySelector::for_protocol_version(30, 12345);
        assert_eq!(strategy.algorithm_name(), "md5");
    }

    #[test]
    fn selector_for_protocol_version_31() {
        let strategy = ChecksumStrategySelector::for_protocol_version(31, 0);
        assert_eq!(strategy.algorithm_name(), "md5");
    }

    #[test]
    fn selector_for_protocol_version_with_seed_order() {
        let proper = ChecksumStrategySelector::for_protocol_version_with_seed_order(30, 123, true);
        let legacy = ChecksumStrategySelector::for_protocol_version_with_seed_order(30, 123, false);

        // Both should be MD5 but produce different results due to seed ordering
        assert_eq!(proper.algorithm_name(), "md5");
        assert_eq!(legacy.algorithm_name(), "md5");
        assert_ne!(proper.compute(b"test"), legacy.compute(b"test"));
    }

    #[test]
    fn selector_for_algorithm() {
        let test_data = b"test data for algorithm selection";

        for kind in ChecksumAlgorithmKind::all() {
            let strategy = ChecksumStrategySelector::for_algorithm(*kind, 42);
            assert_eq!(strategy.algorithm_kind(), *kind);
            assert_eq!(strategy.digest_len(), kind.digest_len());

            let digest = strategy.compute(test_data);
            assert_eq!(digest.len(), kind.digest_len());
        }
    }

    #[test]
    fn selector_with_seed_config_md5_legacy() {
        let strategy = ChecksumStrategySelector::with_seed_config(
            ChecksumAlgorithmKind::Md5,
            SeedConfig::Md5(Md5SeedConfig::Legacy(12345)),
        );
        assert_eq!(strategy.algorithm_name(), "md5");

        // Compare with proper ordering
        let proper = ChecksumStrategySelector::with_seed_config(
            ChecksumAlgorithmKind::Md5,
            SeedConfig::Md5(Md5SeedConfig::Proper(12345)),
        );
        assert_ne!(strategy.compute(b"test"), proper.compute(b"test"));
    }

    #[test]
    fn selector_with_seed_config_xxh3() {
        let strategy = ChecksumStrategySelector::with_seed_config(
            ChecksumAlgorithmKind::Xxh3,
            SeedConfig::Seed64(0x12345678),
        );
        assert_eq!(strategy.algorithm_name(), "xxh3");
    }

    #[test]
    fn selector_concrete_factories() {
        let md4 = ChecksumStrategySelector::md4();
        assert_eq!(md4.algorithm_name(), "md4");

        let md5 = ChecksumStrategySelector::md5();
        assert_eq!(md5.algorithm_name(), "md5");

        let md5_proper = ChecksumStrategySelector::md5_proper(123);
        let md5_legacy = ChecksumStrategySelector::md5_legacy(123);
        assert_ne!(md5_proper.compute(b"test"), md5_legacy.compute(b"test"));

        let sha1 = ChecksumStrategySelector::sha1();
        assert_eq!(sha1.digest_len(), 20);

        let sha256 = ChecksumStrategySelector::sha256();
        assert_eq!(sha256.digest_len(), 32);

        let sha512 = ChecksumStrategySelector::sha512();
        assert_eq!(sha512.digest_len(), 64);

        let xxh64 = ChecksumStrategySelector::xxh64(42);
        assert_eq!(xxh64.digest_len(), 8);

        let xxh3 = ChecksumStrategySelector::xxh3(42);
        assert_eq!(xxh3.digest_len(), 8);

        let xxh3_128 = ChecksumStrategySelector::xxh3_128(42);
        assert_eq!(xxh3_128.digest_len(), 16);
    }

    // ------------------------------------------------------------------------
    // Strategy trait object tests
    // ------------------------------------------------------------------------

    #[test]
    fn strategies_are_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}

        assert_send_sync::<Md4Strategy>();
        assert_send_sync::<Md5Strategy>();
        assert_send_sync::<Sha1Strategy>();
        assert_send_sync::<Sha256Strategy>();
        assert_send_sync::<Sha512Strategy>();
        assert_send_sync::<Xxh64Strategy>();
        assert_send_sync::<Xxh3Strategy>();
        assert_send_sync::<Xxh3_128Strategy>();
    }

    #[test]
    fn boxed_strategy_works() {
        let strategies: Vec<Box<dyn ChecksumStrategy>> = vec![
            Box::new(Md4Strategy::new()),
            Box::new(Md5Strategy::new()),
            Box::new(Sha1Strategy::new()),
            Box::new(Xxh3Strategy::new(0)),
        ];

        for strategy in &strategies {
            let digest = strategy.compute(b"test");
            assert_eq!(digest.len(), strategy.digest_len());
        }
    }

    #[test]
    fn consistent_results_across_calls() {
        let strategy = Sha256Strategy::new();
        let d1 = strategy.compute(b"deterministic");
        let d2 = strategy.compute(b"deterministic");
        assert_eq!(d1, d2);
    }

    #[test]
    fn different_inputs_different_results() {
        let strategy = Sha256Strategy::new();
        let d1 = strategy.compute(b"input1");
        let d2 = strategy.compute(b"input2");
        assert_ne!(d1, d2);
    }

    #[test]
    fn empty_input_produces_valid_digest() {
        for kind in ChecksumAlgorithmKind::all() {
            let strategy = ChecksumStrategySelector::for_algorithm(*kind, 0);
            let digest = strategy.compute(b"");
            assert_eq!(digest.len(), kind.digest_len());
        }
    }
}
