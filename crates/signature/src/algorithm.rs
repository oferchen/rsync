//! crates/signature/src/algorithm.rs
//!
//! Strong checksum algorithm definitions for signature generation.

use std::fmt;

use checksums::strong::{
    Md4, Md5, Md5Seed, Sha1, StrongDigest, Xxh3, Xxh3_128, Xxh64, md4_digest_batch,
    md5_digest_batch,
};

/// Stack-allocated buffer for strong checksum digests, avoiding heap allocation
/// in the delta matching hot path.
///
/// Sized to hold the largest supported digest (SHA-1, 20 bytes). All other
/// algorithms produce 8 or 16 bytes, so this fits comfortably on the stack.
#[derive(Clone, Copy)]
pub struct DigestBuf {
    buf: [u8; Self::MAX_LEN],
    len: u8,
}

impl DigestBuf {
    /// Maximum digest length across all supported algorithms (SHA-1 = 20 bytes).
    pub const MAX_LEN: usize = 20;

    /// Returns the digest bytes as a slice.
    #[inline]
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.buf[..self.len as usize]
    }

    /// Returns the length of the digest in bytes.
    #[inline]
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len as usize
    }

    /// Returns `true` if the digest buffer is empty.
    #[inline]
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Copies digest bytes from `src`, truncated to `len`.
    ///
    /// The actual stored length is `min(len, src.len(), MAX_LEN)`.
    #[inline]
    pub fn from_slice(src: &[u8], len: usize) -> Self {
        debug_assert!(len <= Self::MAX_LEN);
        let mut buf = [0u8; Self::MAX_LEN];
        let copy_len = len.min(src.len()).min(Self::MAX_LEN);
        buf[..copy_len].copy_from_slice(&src[..copy_len]);
        Self {
            buf,
            len: copy_len as u8,
        }
    }
}

impl fmt::Debug for DigestBuf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DigestBuf({:02x?})", self.as_slice())
    }
}

impl PartialEq for DigestBuf {
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl Eq for DigestBuf {}

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

    /// Computes a strong digest truncated to `len` bytes, returning a
    /// stack-allocated [`DigestBuf`] to avoid heap allocation in the hot path.
    #[inline]
    pub fn compute_truncated(self, data: &[u8], len: usize) -> DigestBuf {
        let effective_len = len.min(self.digest_len()).min(DigestBuf::MAX_LEN);
        match self {
            SignatureAlgorithm::Md4 => {
                DigestBuf::from_slice(Md4::digest(data).as_ref(), effective_len)
            }
            SignatureAlgorithm::Md5 { seed_config } => DigestBuf::from_slice(
                Md5::digest_with_seed(seed_config, data).as_ref(),
                effective_len,
            ),
            SignatureAlgorithm::Sha1 => {
                DigestBuf::from_slice(Sha1::digest(data).as_ref(), effective_len)
            }
            SignatureAlgorithm::Xxh64 { seed } => {
                DigestBuf::from_slice(Xxh64::digest(seed, data).as_ref(), effective_len)
            }
            SignatureAlgorithm::Xxh3 { seed } => {
                DigestBuf::from_slice(Xxh3::digest(seed, data).as_ref(), effective_len)
            }
            SignatureAlgorithm::Xxh3_128 { seed } => {
                DigestBuf::from_slice(Xxh3_128::digest(seed, data).as_ref(), effective_len)
            }
        }
    }

    /// Computes strong digests for a batch of data slices, each truncated to `len` bytes.
    ///
    /// For MD4 and unseeded MD5, this uses SIMD-accelerated batch hashing (AVX2/AVX-512/NEON)
    /// to process multiple blocks in parallel. Other algorithms fall back to sequential
    /// per-element computation.
    ///
    /// Returns a `Vec` of [`DigestBuf`] in the same order as the input slices.
    pub fn compute_truncated_batch(self, blocks: &[&[u8]], len: usize) -> Vec<DigestBuf> {
        let effective_len = len.min(self.digest_len()).min(DigestBuf::MAX_LEN);

        match self {
            SignatureAlgorithm::Md4 => {
                let digests = md4_digest_batch(blocks);
                digests
                    .into_iter()
                    .map(|d| DigestBuf::from_slice(d.as_ref(), effective_len))
                    .collect()
            }
            SignatureAlgorithm::Md5 {
                seed_config:
                    Md5Seed {
                        value: None,
                        proper_order: _,
                    },
            } => {
                // Unseeded MD5: use SIMD batch path
                let digests = md5_digest_batch(blocks);
                digests
                    .into_iter()
                    .map(|d| DigestBuf::from_slice(d.as_ref(), effective_len))
                    .collect()
            }
            _ => {
                // Seeded MD5, SHA1, XXH64, XXH3, XXH3_128: per-element fallback
                blocks
                    .iter()
                    .map(|data| self.compute_truncated(data, len))
                    .collect()
            }
        }
    }

    /// Computes a strong digest over two non-contiguous slices, truncated to `len` bytes.
    ///
    /// Uses the streaming `update()` + `finalize()` API to hash both slices
    /// without copying them into a contiguous buffer first. This avoids O(n)
    /// rotation when computing strong checksums over a wrapped ring buffer.
    #[inline]
    pub fn compute_truncated_slices(self, a: &[u8], b: &[u8], len: usize) -> DigestBuf {
        if b.is_empty() {
            return self.compute_truncated(a, len);
        }
        let effective_len = len.min(self.digest_len()).min(DigestBuf::MAX_LEN);
        match self {
            SignatureAlgorithm::Md4 => {
                let mut h = Md4::new();
                h.update(a);
                h.update(b);
                DigestBuf::from_slice(h.finalize().as_ref(), effective_len)
            }
            SignatureAlgorithm::Md5 { seed_config } => {
                let mut h = Md5::with_seed(seed_config);
                h.update(a);
                h.update(b);
                DigestBuf::from_slice(h.finalize().as_ref(), effective_len)
            }
            SignatureAlgorithm::Sha1 => {
                let mut h = Sha1::new();
                h.update(a);
                h.update(b);
                DigestBuf::from_slice(h.finalize().as_ref(), effective_len)
            }
            SignatureAlgorithm::Xxh64 { seed } => {
                let mut h = Xxh64::with_seed(seed);
                h.update(a);
                h.update(b);
                DigestBuf::from_slice(h.finalize().as_ref(), effective_len)
            }
            SignatureAlgorithm::Xxh3 { seed } => {
                let mut h = Xxh3::with_seed(seed);
                h.update(a);
                h.update(b);
                DigestBuf::from_slice(h.finalize().as_ref(), effective_len)
            }
            SignatureAlgorithm::Xxh3_128 { seed } => {
                let mut h = Xxh3_128::with_seed(seed);
                h.update(a);
                h.update(b);
                DigestBuf::from_slice(h.finalize().as_ref(), effective_len)
            }
        }
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

    /// Verifies that `compute_truncated_slices` produces identical output to
    /// `compute_truncated` for all supported algorithms.
    #[test]
    fn compute_truncated_slices_matches_contiguous() {
        let data = b"hello world, this is test data for slices";
        let split_at = 13;
        let (a, b) = data.split_at(split_at);

        let algorithms: Vec<SignatureAlgorithm> = vec![
            SignatureAlgorithm::Md4,
            SignatureAlgorithm::Md5 {
                seed_config: Md5Seed::none(),
            },
            SignatureAlgorithm::Sha1,
            SignatureAlgorithm::Xxh64 { seed: 42 },
            SignatureAlgorithm::Xxh3 { seed: 99 },
            SignatureAlgorithm::Xxh3_128 { seed: 7 },
        ];

        for algo in algorithms {
            let contiguous = algo.compute_truncated(data, algo.digest_len());
            let split = algo.compute_truncated_slices(a, b, algo.digest_len());
            assert_eq!(
                contiguous.as_slice(),
                split.as_slice(),
                "mismatch for {algo:?}"
            );
        }
    }

    /// Verifies that `compute_truncated_slices` with an empty second slice
    /// delegates to `compute_truncated`.
    #[test]
    fn compute_truncated_slices_empty_second_slice() {
        let data = b"single slice data";
        let algo = SignatureAlgorithm::Md4;
        let contiguous = algo.compute_truncated(data, 16);
        let split = algo.compute_truncated_slices(data, &[], 16);
        assert_eq!(contiguous.as_slice(), split.as_slice());
    }

    /// Verifies truncation works correctly with the two-slice variant.
    #[test]
    fn compute_truncated_slices_respects_truncation() {
        let data = b"truncation test data here";
        let (a, b) = data.split_at(10);
        let algo = SignatureAlgorithm::Md4;

        let full = algo.compute_truncated_slices(a, b, 16);
        let truncated = algo.compute_truncated_slices(a, b, 8);
        assert_eq!(truncated.len(), 8);
        assert_eq!(truncated.as_slice(), &full.as_slice()[..8]);
    }

    /// Verifies batch computation matches per-element computation for all algorithms.
    #[test]
    fn compute_truncated_batch_matches_sequential() {
        let inputs: Vec<Vec<u8>> = (0..20)
            .map(|i| {
                (0..100 + i * 7)
                    .map(|j| ((j * 17 + i * 31) % 256) as u8)
                    .collect()
            })
            .collect();

        let algorithms: Vec<SignatureAlgorithm> = vec![
            SignatureAlgorithm::Md4,
            SignatureAlgorithm::Md5 {
                seed_config: Md5Seed::none(),
            },
            SignatureAlgorithm::Md5 {
                seed_config: Md5Seed::proper(42),
            },
            SignatureAlgorithm::Sha1,
            SignatureAlgorithm::Xxh64 { seed: 0 },
            SignatureAlgorithm::Xxh3 { seed: 99 },
            SignatureAlgorithm::Xxh3_128 { seed: 7 },
        ];

        for algo in algorithms {
            let len = algo.digest_len();
            let refs: Vec<&[u8]> = inputs.iter().map(|v| v.as_slice()).collect();
            let batch = algo.compute_truncated_batch(&refs, len);
            let sequential: Vec<DigestBuf> = inputs
                .iter()
                .map(|v| algo.compute_truncated(v, len))
                .collect();

            assert_eq!(
                batch.len(),
                sequential.len(),
                "length mismatch for {algo:?}"
            );
            for (i, (b, s)) in batch.iter().zip(sequential.iter()).enumerate() {
                assert_eq!(
                    b.as_slice(),
                    s.as_slice(),
                    "digest mismatch at index {i} for {algo:?}"
                );
            }
        }
    }

    /// Verifies batch computation with a single element matches per-element.
    #[test]
    fn compute_truncated_batch_single_element() {
        let data = b"single block data for batch test";
        let algo = SignatureAlgorithm::Md4;

        let batch = algo.compute_truncated_batch(&[data.as_slice()], 16);
        let single = algo.compute_truncated(data, 16);

        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].as_slice(), single.as_slice());
    }

    /// Verifies batch computation with empty input returns empty.
    #[test]
    fn compute_truncated_batch_empty_input() {
        let algo = SignatureAlgorithm::Md4;
        let batch = algo.compute_truncated_batch(&[], 16);
        assert!(batch.is_empty());
    }

    /// Verifies batch truncation works correctly.
    #[test]
    fn compute_truncated_batch_respects_truncation() {
        let data: Vec<&[u8]> = vec![b"block1", b"block2", b"block3"];
        let algo = SignatureAlgorithm::Md4;

        let full = algo.compute_truncated_batch(&data, 16);
        let truncated = algo.compute_truncated_batch(&data, 8);

        assert_eq!(full.len(), truncated.len());
        for (f, t) in full.iter().zip(truncated.iter()) {
            assert_eq!(t.len(), 8);
            assert_eq!(t.as_slice(), &f.as_slice()[..8]);
        }
    }
}
