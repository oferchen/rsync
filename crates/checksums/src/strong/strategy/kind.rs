//! Enumeration of supported checksum algorithms.
//!
//! # Upstream Reference
//!
//! - `checksum.c` - algorithm selection based on protocol version
//! - `compat.c` - checksum negotiation via capability strings

use std::fmt;

/// Identifies a checksum algorithm without carrying seed or configuration data.
///
/// Suitable for algorithm selection, comparison, and protocol negotiation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum ChecksumAlgorithmKind {
    /// MD4 - legacy algorithm for rsync protocol < 30.
    ///
    /// Upstream rsync uses MD4 as the default strong checksum for older protocol
    /// versions. See upstream `checksum.c:get_checksum2()`.
    Md4,
    /// MD5 - default for rsync protocol >= 30.
    ///
    /// Upstream rsync switched to MD5 in protocol 30. Supports seeded hashing
    /// with configurable ordering via `CHECKSUM_SEED_FIX`.
    Md5,
    /// SHA-1 - negotiated via checksum capability strings (protocol 31+).
    Sha1,
    /// SHA-256 - cryptographically secure option for daemon authentication.
    Sha256,
    /// SHA-512 - maximum security option for daemon authentication.
    Sha512,
    /// XXH64 - fast 64-bit non-cryptographic hash for block matching.
    Xxh64,
    /// XXH3 - fastest 64-bit non-cryptographic hash, negotiated via `-e.LsfxCIvu`.
    Xxh3,
    /// XXH3-128 - fast 128-bit non-cryptographic hash with lower collision rate than XXH3-64.
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
    ///
    /// MD4, MD5, SHA-1, SHA-256, and SHA-512 are cryptographic (though MD4/MD5/SHA-1
    /// are considered broken). XXH64, XXH3, and XXH3-128 are non-cryptographic.
    #[must_use]
    pub const fn is_cryptographic(&self) -> bool {
        matches!(
            self,
            Self::Md4 | Self::Md5 | Self::Sha1 | Self::Sha256 | Self::Sha512
        )
    }

    /// Parses an algorithm from a string name used in upstream negotiation.
    ///
    /// Accepts canonical names and common aliases (case-insensitive).
    /// See upstream `compat.c` for the negotiation protocol.
    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "md4" => Some(Self::Md4),
            "md5" => Some(Self::Md5),
            "sha1" | "sha-1" => Some(Self::Sha1),
            "sha256" | "sha-256" => Some(Self::Sha256),
            "sha512" | "sha-512" => Some(Self::Sha512),
            "xxh64" | "xxhash" | "xxhash64" => Some(Self::Xxh64),
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
