//! Seed configuration types for strategy creation.

use crate::strong::Md5Seed;

/// Configuration for MD5 seed handling in the strategy pattern.
///
/// See `Md5Seed` for details on seed ordering semantics.
#[derive(Clone, Copy, Debug)]
pub enum Md5SeedConfig {
    /// No seed - equivalent to standard MD5 behavior.
    None,
    /// Seed hashed before data (protocol 30+ with `CHECKSUM_SEED_FIX`).
    Proper(i32),
    /// Seed hashed after data (legacy protocol behavior).
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

/// Unified seed type handling different seeding requirements across algorithms.
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

/// Extracts a `u64` seed from a `SeedConfig`, falling back to 0 for unseeded configs.
pub(super) fn extract_u64_seed(seed: &SeedConfig) -> u64 {
    match seed {
        SeedConfig::Seed64(s) => *s,
        SeedConfig::Md5(Md5SeedConfig::Proper(s) | Md5SeedConfig::Legacy(s)) => *s as u64,
        _ => 0,
    }
}
