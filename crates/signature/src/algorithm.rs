//! crates/signature/src/algorithm.rs
//!
//! Strong checksum algorithm definitions for signature generation.

use checksums::strong::{Md4, Md5, Md5Seed, Sha1, StrongDigest, Xxh3, Xxh3_128, Xxh64};

/// Strong checksum strategies supported by the signature generator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SignatureAlgorithm {
    /// MD4, used by upstream rsync for historical protocol versions.
    Md4,
    /// MD5, negotiated when both peers enable the checksum.
    ///
    /// Supports optional seeded hashing with configurable ordering for protocol
    /// compatibility. The `seed_config` determines whether the seed is hashed
    /// before or after the file data, controlled by the CHECKSUM_SEED_FIX
    /// compatibility flag in protocol 30+.
    Md5 {
        /// Seed configuration for MD5 checksum calculation.
        seed_config: Md5Seed,
    },
    /// SHA-1, available when both peers advertise it.
    Sha1,
    /// XXH64, available in newer protocol combinations with an explicit seed.
    Xxh64 {
        /// Seed applied to the XXH64 instance.
        seed: u64,
    },
    /// XXH3/64, negotiated alongside modern protocol extensions.
    Xxh3 {
        /// Seed applied to the XXH3/64 instance.
        seed: u64,
    },
    /// XXH3/128, used when both peers support the extended checksum.
    Xxh3_128 {
        /// Seed applied to the XXH3/128 instance.
        seed: u64,
    },
}

impl SignatureAlgorithm {
    /// Returns the full digest width produced by the algorithm in bytes.
    #[inline]
    #[must_use]
    pub const fn digest_len(self) -> usize {
        match self {
            SignatureAlgorithm::Md4 | SignatureAlgorithm::Md5 { .. } => 16,
            SignatureAlgorithm::Sha1 => Sha1::DIGEST_LEN,
            SignatureAlgorithm::Xxh64 { .. } | SignatureAlgorithm::Xxh3 { .. } => 8,
            SignatureAlgorithm::Xxh3_128 { .. } => 16,
        }
    }

    /// Computes a strong digest for `data`, returning the full-length output.
    pub(crate) fn compute_full(self, data: &[u8]) -> Vec<u8> {
        match self {
            SignatureAlgorithm::Md4 => Md4::digest(data).as_ref().to_vec(),
            SignatureAlgorithm::Md5 { seed_config } => {
                Md5::digest_with_seed(seed_config, data).as_ref().to_vec()
            }
            SignatureAlgorithm::Sha1 => Sha1::digest(data).as_ref().to_vec(),
            SignatureAlgorithm::Xxh64 { seed } => Xxh64::digest(seed, data).as_ref().to_vec(),
            SignatureAlgorithm::Xxh3 { seed } => Xxh3::digest(seed, data).as_ref().to_vec(),
            SignatureAlgorithm::Xxh3_128 { seed } => Xxh3_128::digest(seed, data).as_ref().to_vec(),
        }
    }

    /// Computes a strong digest truncated to `len` bytes.
    ///
    /// Pre-allocates exactly `len` bytes to avoid wasted capacity from truncation.
    pub fn compute_truncated(self, data: &[u8], len: usize) -> Vec<u8> {
        let full_len = self.digest_len();
        if len >= full_len {
            // No truncation needed - return full digest
            return self.compute_full(data);
        }

        // Compute digest directly into fixed-size buffer, then copy truncated portion
        // This avoids allocating full capacity then truncating (which wastes capacity)
        let mut result = Vec::with_capacity(len);
        match self {
            SignatureAlgorithm::Md4 => {
                let digest = Md4::digest(data);
                result.extend_from_slice(&digest.as_ref()[..len]);
            }
            SignatureAlgorithm::Md5 { seed_config } => {
                let digest = Md5::digest_with_seed(seed_config, data);
                result.extend_from_slice(&digest.as_ref()[..len]);
            }
            SignatureAlgorithm::Sha1 => {
                let digest = Sha1::digest(data);
                result.extend_from_slice(&digest.as_ref()[..len]);
            }
            SignatureAlgorithm::Xxh64 { seed } => {
                let digest = Xxh64::digest(seed, data);
                result.extend_from_slice(&digest.as_ref()[..len]);
            }
            SignatureAlgorithm::Xxh3 { seed } => {
                let digest = Xxh3::digest(seed, data);
                result.extend_from_slice(&digest.as_ref()[..len]);
            }
            SignatureAlgorithm::Xxh3_128 { seed } => {
                let digest = Xxh3_128::digest(seed, data);
                result.extend_from_slice(&digest.as_ref()[..len]);
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md4_digest_len() {
        assert_eq!(SignatureAlgorithm::Md4.digest_len(), 16);
    }

    #[test]
    fn md5_digest_len() {
        let algo = SignatureAlgorithm::Md5 {
            seed_config: Md5Seed::none(),
        };
        assert_eq!(algo.digest_len(), 16);
    }

    #[test]
    fn sha1_digest_len() {
        assert_eq!(SignatureAlgorithm::Sha1.digest_len(), 20);
    }

    #[test]
    fn xxh64_digest_len() {
        let algo = SignatureAlgorithm::Xxh64 { seed: 0 };
        assert_eq!(algo.digest_len(), 8);
    }

    #[test]
    fn xxh3_digest_len() {
        let algo = SignatureAlgorithm::Xxh3 { seed: 0 };
        assert_eq!(algo.digest_len(), 8);
    }

    #[test]
    fn xxh3_128_digest_len() {
        let algo = SignatureAlgorithm::Xxh3_128 { seed: 0 };
        assert_eq!(algo.digest_len(), 16);
    }

    #[test]
    fn compute_truncated_shorter_than_full() {
        let algo = SignatureAlgorithm::Md4;
        let data = b"test data";
        let truncated = algo.compute_truncated(data, 8);
        assert_eq!(truncated.len(), 8);
    }

    #[test]
    fn compute_truncated_at_full_length() {
        let algo = SignatureAlgorithm::Md4;
        let data = b"test data";
        let truncated = algo.compute_truncated(data, 16);
        assert_eq!(truncated.len(), 16);
    }

    #[test]
    fn compute_truncated_longer_than_full() {
        let algo = SignatureAlgorithm::Xxh64 { seed: 42 };
        let data = b"test data";
        // Requesting more than digest length returns full digest
        let truncated = algo.compute_truncated(data, 16);
        assert_eq!(truncated.len(), 8);
    }

    #[test]
    fn algorithm_clone() {
        let algo = SignatureAlgorithm::Md4;
        let cloned = algo;
        assert_eq!(algo, cloned);
    }

    #[test]
    fn algorithm_debug() {
        let algo = SignatureAlgorithm::Md4;
        let debug = format!("{algo:?}");
        assert!(debug.contains("Md4"));
    }

    #[test]
    fn algorithm_eq() {
        let algo1 = SignatureAlgorithm::Xxh64 { seed: 42 };
        let algo2 = SignatureAlgorithm::Xxh64 { seed: 42 };
        let algo3 = SignatureAlgorithm::Xxh64 { seed: 0 };
        assert_eq!(algo1, algo2);
        assert_ne!(algo1, algo3);
    }
}
