//! crates/checksums/src/strong/xxhash.rs
//!
//! XXHash implementations with optional runtime SIMD detection.
//!
//! When the `xxh3-simd` feature is enabled (default), one-shot digest operations
//! use the `xxh3` crate which provides runtime detection of AVX2 (x86_64) and
//! NEON (aarch64) instructions. This allows portable binaries to automatically
//! use SIMD acceleration when available.
//!
//! Streaming operations always use `xxhash-rust` as the `xxh3` crate does not
//! provide streaming hashers. For most rsync block checksum operations, the
//! one-shot path is used, so SIMD acceleration applies where it matters most.

use super::StrongDigest;

// ============================================================================
// XXH64 - Uses xxhash-rust (no runtime SIMD needed, already very fast)
// ============================================================================

/// Streaming XXH64 hasher used by rsync when negotiated by newer protocols.
#[derive(Clone)]
pub struct Xxh64 {
    inner: xxhash_rust::xxh64::Xxh64,
}

impl Xxh64 {
    /// Creates a hasher with the supplied seed.
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self {
            inner: xxhash_rust::xxh64::Xxh64::new(seed),
        }
    }

    /// Feeds additional bytes into the digest state.
    pub fn update(&mut self, data: &[u8]) {
        self.inner.update(data);
    }

    /// Finalises the digest and returns the little-endian XXH64 output.
    #[must_use]
    pub fn finalize(self) -> [u8; 8] {
        self.inner.digest().to_le_bytes()
    }

    /// Convenience helper that computes the XXH64 digest for `data` in one shot.
    #[must_use]
    pub fn digest(seed: u64, data: &[u8]) -> [u8; 8] {
        xxhash_rust::xxh64::xxh64(data, seed).to_le_bytes()
    }
}

impl StrongDigest for Xxh64 {
    type Seed = u64;
    type Digest = [u8; 8];
    const DIGEST_LEN: usize = 8;

    fn with_seed(seed: Self::Seed) -> Self {
        Xxh64::new(seed)
    }

    fn update(&mut self, data: &[u8]) {
        self.inner.update(data);
    }

    fn finalize(self) -> Self::Digest {
        self.inner.digest().to_le_bytes()
    }
}

// ============================================================================
// XXH3-64 with runtime SIMD for one-shot operations
// ============================================================================

/// Streaming XXH3 hasher that produces 64-bit digests.
///
/// When the `xxh3-simd` feature is enabled (default), the one-shot [`digest`](Self::digest)
/// method uses the `xxh3` crate with runtime SIMD detection (AVX2/NEON).
/// Streaming operations use `xxhash-rust` as the `xxh3` crate lacks streaming support.
pub struct Xxh3 {
    inner: xxhash_rust::xxh3::Xxh3,
}

impl Xxh3 {
    /// Creates a hasher with the supplied seed.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self {
            inner: xxhash_rust::xxh3::Xxh3::with_seed(seed),
        }
    }

    /// Feeds additional bytes into the digest state.
    pub fn update(&mut self, data: &[u8]) {
        self.inner.update(data);
    }

    /// Finalises the digest and returns the little-endian XXH3/64 output.
    #[must_use]
    pub fn finalize(self) -> [u8; 8] {
        self.inner.digest().to_le_bytes()
    }

    /// Convenience helper that computes the XXH3/64 digest for `data` in one shot.
    ///
    /// When the `xxh3-simd` feature is enabled, this uses runtime SIMD detection
    /// to automatically use AVX2 (x86_64) or NEON (aarch64) when available.
    #[must_use]
    #[cfg(feature = "xxh3-simd")]
    pub fn digest(seed: u64, data: &[u8]) -> [u8; 8] {
        xxh3::hash64_with_seed(data, seed).to_le_bytes()
    }

    /// Convenience helper that computes the XXH3/64 digest for `data` in one shot.
    ///
    /// Uses `xxhash-rust` with compile-time SIMD detection only.
    /// Enable the `xxh3-simd` feature for runtime SIMD detection.
    #[must_use]
    #[cfg(not(feature = "xxh3-simd"))]
    pub fn digest(seed: u64, data: &[u8]) -> [u8; 8] {
        xxhash_rust::xxh3::xxh3_64_with_seed(data, seed).to_le_bytes()
    }
}

impl StrongDigest for Xxh3 {
    type Seed = u64;
    type Digest = [u8; 8];
    const DIGEST_LEN: usize = 8;

    fn with_seed(seed: Self::Seed) -> Self {
        Xxh3::new(seed)
    }

    fn update(&mut self, data: &[u8]) {
        self.inner.update(data);
    }

    fn finalize(self) -> Self::Digest {
        self.inner.digest().to_le_bytes()
    }
}

// ============================================================================
// XXH3-128 with runtime SIMD for one-shot operations
// ============================================================================

/// Streaming XXH3 hasher that produces 128-bit digests.
///
/// When the `xxh3-simd` feature is enabled (default), the one-shot [`digest`](Self::digest)
/// method uses the `xxh3` crate with runtime SIMD detection (AVX2/NEON).
/// Streaming operations use `xxhash-rust` as the `xxh3` crate lacks streaming support.
pub struct Xxh3_128 {
    inner: xxhash_rust::xxh3::Xxh3,
}

impl Xxh3_128 {
    /// Creates a hasher with the supplied seed.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self {
            inner: xxhash_rust::xxh3::Xxh3::with_seed(seed),
        }
    }

    /// Feeds additional bytes into the digest state.
    pub fn update(&mut self, data: &[u8]) {
        self.inner.update(data);
    }

    /// Finalises the digest and returns the little-endian XXH3/128 output.
    #[must_use]
    pub fn finalize(self) -> [u8; 16] {
        self.inner.digest128().to_le_bytes()
    }

    /// Convenience helper that computes the XXH3/128 digest for `data` in one shot.
    ///
    /// When the `xxh3-simd` feature is enabled, this uses runtime SIMD detection
    /// to automatically use AVX2 (x86_64) or NEON (aarch64) when available.
    #[must_use]
    #[cfg(feature = "xxh3-simd")]
    pub fn digest(seed: u64, data: &[u8]) -> [u8; 16] {
        xxh3::hash128_with_seed(data, seed).to_le_bytes()
    }

    /// Convenience helper that computes the XXH3/128 digest for `data` in one shot.
    ///
    /// Uses `xxhash-rust` with compile-time SIMD detection only.
    /// Enable the `xxh3-simd` feature for runtime SIMD detection.
    #[must_use]
    #[cfg(not(feature = "xxh3-simd"))]
    pub fn digest(seed: u64, data: &[u8]) -> [u8; 16] {
        xxhash_rust::xxh3::xxh3_128_with_seed(data, seed).to_le_bytes()
    }
}

impl StrongDigest for Xxh3_128 {
    type Seed = u64;
    type Digest = [u8; 16];
    const DIGEST_LEN: usize = 16;

    fn with_seed(seed: Self::Seed) -> Self {
        Xxh3_128::new(seed)
    }

    fn update(&mut self, data: &[u8]) {
        self.inner.update(data);
    }

    fn finalize(self) -> Self::Digest {
        self.inner.digest128().to_le_bytes()
    }
}

// ============================================================================
// Runtime SIMD detection query
// ============================================================================

/// Returns whether the XXH3 one-shot operations use runtime SIMD detection.
///
/// When `true`, the `xxh3` crate automatically detects and uses AVX2 (x86_64)
/// or NEON (aarch64) instructions at runtime for [`Xxh3::digest`] and
/// [`Xxh3_128::digest`], providing optimal performance on any CPU without
/// requiring compile-time flags.
///
/// Streaming operations (using `update`/`finalize`) always use `xxhash-rust`
/// which relies on compile-time SIMD detection.
#[must_use]
#[cfg(feature = "xxh3-simd")]
pub const fn xxh3_simd_available() -> bool {
    true
}

/// Returns whether the XXH3 one-shot operations use runtime SIMD detection.
///
/// When `false`, all operations use `xxhash-rust` which relies on compile-time
/// SIMD detection. Enable the `xxh3-simd` feature for runtime detection.
#[must_use]
#[cfg(not(feature = "xxh3-simd"))]
pub const fn xxh3_simd_available() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xxh64_matches_reference_values() {
        let vectors = [
            (0, b"".as_slice()),
            (0, b"a".as_slice()),
            (0, b"The quick brown fox jumps over the lazy dog".as_slice()),
            (123, b"rsync".as_slice()),
        ];

        for (seed, input) in vectors {
            let mut hasher = Xxh64::new(seed);
            let mid = input.len() / 2;
            hasher.update(&input[..mid]);
            hasher.update(&input[mid..]);
            let digest = hasher.finalize();
            let expected = xxhash_rust::xxh64::xxh64(input, seed).to_le_bytes();
            assert_eq!(digest, expected);

            let one_shot = Xxh64::digest(seed, input);
            assert_eq!(one_shot, expected);
        }
    }

    #[test]
    fn xxh3_64_streaming_matches_reference() {
        let vectors = [
            (0, b"".as_slice()),
            (0, b"example".as_slice()),
            (1234, b"rsync-check".as_slice()),
            (0, b"The quick brown fox jumps over the lazy dog".as_slice()),
        ];

        for (seed, input) in vectors {
            let mut hasher = Xxh3::new(seed);
            let split = input.len() / 2;
            hasher.update(&input[..split]);
            hasher.update(&input[split..]);
            let streaming_digest = hasher.finalize();

            // Streaming should match xxhash-rust reference
            let expected = xxhash_rust::xxh3::xxh3_64_with_seed(input, seed).to_le_bytes();
            assert_eq!(
                streaming_digest, expected,
                "streaming should match reference for seed={seed}, input={input:?}"
            );
        }
    }

    #[test]
    fn xxh3_64_oneshot_matches_reference() {
        let vectors = [
            (0, b"".as_slice()),
            (0, b"example".as_slice()),
            (1234, b"rsync-check".as_slice()),
            (0, b"The quick brown fox jumps over the lazy dog".as_slice()),
        ];

        for (seed, input) in vectors {
            let one_shot = Xxh3::digest(seed, input);

            // One-shot should match xxhash-rust reference (both implementations
            // produce the same output, just potentially with different performance)
            let expected = xxhash_rust::xxh3::xxh3_64_with_seed(input, seed).to_le_bytes();
            assert_eq!(
                one_shot, expected,
                "one-shot should match reference for seed={seed}, input={input:?}"
            );
        }
    }

    #[test]
    fn xxh3_128_streaming_matches_reference() {
        let vectors = [
            (0, b"".as_slice()),
            (0, b"The quick brown fox".as_slice()),
            (42, b"delta-transfer".as_slice()),
        ];

        for (seed, input) in vectors {
            let mut hasher = Xxh3_128::new(seed);
            let split = input.len().saturating_sub(1);
            hasher.update(&input[..split]);
            hasher.update(&input[split..]);
            let streaming_digest = hasher.finalize();

            let expected = xxhash_rust::xxh3::xxh3_128_with_seed(input, seed).to_le_bytes();
            assert_eq!(
                streaming_digest, expected,
                "streaming should match reference for seed={seed}, input={input:?}"
            );
        }
    }

    #[test]
    fn xxh3_128_oneshot_matches_reference() {
        let vectors = [
            (0, b"".as_slice()),
            (0, b"The quick brown fox".as_slice()),
            (42, b"delta-transfer".as_slice()),
        ];

        for (seed, input) in vectors {
            let one_shot = Xxh3_128::digest(seed, input);

            let expected = xxhash_rust::xxh3::xxh3_128_with_seed(input, seed).to_le_bytes();
            assert_eq!(
                one_shot, expected,
                "one-shot should match reference for seed={seed}, input={input:?}"
            );
        }
    }

    #[test]
    fn xxh3_simd_availability_is_consistent() {
        let available = xxh3_simd_available();
        #[cfg(feature = "xxh3-simd")]
        assert!(available, "xxh3-simd feature enabled, should report true");
        #[cfg(not(feature = "xxh3-simd"))]
        assert!(
            !available,
            "xxh3-simd feature disabled, should report false"
        );
    }

    #[test]
    fn xxh3_different_seeds_different_digests() {
        let input = b"test input";
        let digest1 = Xxh3::digest(0, input);
        let digest2 = Xxh3::digest(1, input);
        assert_ne!(digest1, digest2);
    }

    #[test]
    fn xxh3_128_different_seeds_different_digests() {
        let input = b"test input";
        let digest1 = Xxh3_128::digest(0, input);
        let digest2 = Xxh3_128::digest(1, input);
        assert_ne!(digest1, digest2);
    }

    #[test]
    fn xxh3_digest_len_is_8() {
        assert_eq!(Xxh3::DIGEST_LEN, 8);
        let digest = Xxh3::digest(0, b"test");
        assert_eq!(digest.as_ref().len(), 8);
    }

    #[test]
    fn xxh3_128_digest_len_is_16() {
        assert_eq!(Xxh3_128::DIGEST_LEN, 16);
        let digest = Xxh3_128::digest(0, b"test");
        assert_eq!(digest.as_ref().len(), 16);
    }

    #[test]
    fn xxh64_digest_len_is_8() {
        assert_eq!(Xxh64::DIGEST_LEN, 8);
        let digest = Xxh64::digest(0, b"test");
        assert_eq!(digest.as_ref().len(), 8);
    }

    #[test]
    fn xxh64_different_seeds_different_digests() {
        let input = b"test input";
        let digest1 = Xxh64::digest(0, input);
        let digest2 = Xxh64::digest(1, input);
        assert_ne!(digest1.as_ref(), digest2.as_ref());
    }

    #[test]
    fn empty_input_produces_valid_digests() {
        let empty = b"";
        assert_eq!(Xxh64::digest(0, empty).as_ref().len(), Xxh64::DIGEST_LEN);
        assert_eq!(Xxh3::digest(0, empty).as_ref().len(), Xxh3::DIGEST_LEN);
        assert_eq!(
            Xxh3_128::digest(0, empty).as_ref().len(),
            Xxh3_128::DIGEST_LEN
        );
    }

    #[test]
    fn large_input_oneshot_matches_streaming() {
        // Test with a larger input to exercise SIMD paths
        let input: Vec<u8> = (0..10000).map(|i| (i % 256) as u8).collect();
        let seed = 12345u64;

        // XXH3-64
        let one_shot = Xxh3::digest(seed, &input);
        let mut streaming = Xxh3::new(seed);
        streaming.update(&input);
        let streamed = streaming.finalize();
        assert_eq!(
            one_shot, streamed,
            "large input: one-shot should match streaming for XXH3-64"
        );

        // XXH3-128
        let one_shot_128 = Xxh3_128::digest(seed, &input);
        let mut streaming_128 = Xxh3_128::new(seed);
        streaming_128.update(&input);
        let streamed_128 = streaming_128.finalize();
        assert_eq!(
            one_shot_128, streamed_128,
            "large input: one-shot should match streaming for XXH3-128"
        );
    }
}
