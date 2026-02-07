//! crates/checksums/src/strong/xxhash.rs
//!
//! XXHash implementations with optional runtime SIMD detection.
//!
//! One-shot digest operations use the `xxh3` crate which provides runtime
//! detection of AVX2 (x86_64) and NEON (aarch64) instructions with an
//! automatic scalar fallback. This allows portable binaries to use SIMD
//! acceleration when available without any compile-time feature flags.
//!
//! Streaming operations use `xxhash-rust` as the `xxh3` crate does not
//! provide streaming hashers. For most rsync block checksum operations, the
//! one-shot path is used, so SIMD acceleration applies where it matters most.

use super::StrongDigest;

// ============================================================================
// XXH64 - Uses xxhash-rust (no runtime SIMD needed, already very fast)
// ============================================================================

/// Streaming XXH64 hasher used by rsync when negotiated by newer protocols.
///
/// XXH64 is an extremely fast non-cryptographic hash function that produces
/// 64-bit digests. It is used by rsync protocol version 30+ for block
/// checksums when XXH3 is not available.
///
/// # Examples
///
/// One-shot hashing with a seed:
///
/// ```
/// use checksums::strong::Xxh64;
///
/// // Seed is used to vary the hash output
/// let seed: u64 = 0x12345678;
/// let digest = Xxh64::digest(seed, b"data to hash");
/// assert_eq!(digest.len(), 8); // XXH64 produces 64-bit output
///
/// // Different seeds produce different outputs
/// let digest2 = Xxh64::digest(seed + 1, b"data to hash");
/// assert_ne!(digest, digest2);
/// ```
///
/// Incremental hashing:
///
/// ```
/// use checksums::strong::Xxh64;
///
/// let seed: u64 = 0;
///
/// let mut hasher = Xxh64::new(seed);
/// hasher.update(b"chunk 1");
/// hasher.update(b"chunk 2");
/// let digest = hasher.finalize();
///
/// // Equivalent to one-shot
/// assert_eq!(digest, Xxh64::digest(seed, b"chunk 1chunk 2"));
/// ```
///
/// Using the [`StrongDigest`](super::StrongDigest) trait:
///
/// ```
/// use checksums::strong::{Xxh64, StrongDigest};
///
/// // Create with explicit seed
/// let mut hasher: Xxh64 = StrongDigest::with_seed(42u64);
/// hasher.update(b"test");
/// let digest = hasher.finalize();
/// assert_eq!(digest.len(), Xxh64::DIGEST_LEN);
/// ```
#[derive(Clone)]
pub struct Xxh64 {
    inner: xxhash_rust::xxh64::Xxh64,
}

impl Xxh64 {
    /// Creates a hasher with the supplied seed.
    ///
    /// # Examples
    ///
    /// ```
    /// use checksums::strong::Xxh64;
    ///
    /// let mut hasher = Xxh64::new(0); // seed = 0
    /// hasher.update(b"data");
    /// let digest = hasher.finalize();
    /// assert_eq!(digest.len(), 8);
    /// ```
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self {
            inner: xxhash_rust::xxh64::Xxh64::new(seed),
        }
    }

    /// Feeds additional bytes into the digest state.
    ///
    /// # Examples
    ///
    /// ```
    /// use checksums::strong::Xxh64;
    ///
    /// let mut hasher = Xxh64::new(123);
    /// hasher.update(b"first part");
    /// hasher.update(b"second part");
    /// let digest = hasher.finalize();
    ///
    /// // Same as one-shot
    /// assert_eq!(digest, Xxh64::digest(123, b"first partsecond part"));
    /// ```
    pub fn update(&mut self, data: &[u8]) {
        self.inner.update(data);
    }

    /// Finalises the digest and returns the little-endian XXH64 output.
    ///
    /// # Examples
    ///
    /// ```
    /// use checksums::strong::Xxh64;
    ///
    /// let mut hasher = Xxh64::new(0);
    /// hasher.update(b"test");
    /// let digest = hasher.finalize();
    ///
    /// // The output is in little-endian format
    /// let _value = u64::from_le_bytes(digest);
    /// ```
    #[must_use]
    pub fn finalize(self) -> [u8; 8] {
        self.inner.digest().to_le_bytes()
    }

    /// Convenience helper that computes the XXH64 digest for `data` in one shot.
    ///
    /// # Examples
    ///
    /// ```
    /// use checksums::strong::Xxh64;
    ///
    /// // Hash with seed 0
    /// let digest = Xxh64::digest(0, b"hello");
    /// assert_eq!(digest.len(), 8);
    ///
    /// // Hash with custom seed for rsync block checksums
    /// let rsync_seed: u64 = 0xCAFEBABE;
    /// let block_hash = Xxh64::digest(rsync_seed, b"file block data");
    /// ```
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
/// The one-shot [`digest`](Self::digest) method uses the `xxh3` crate with
/// runtime SIMD detection (AVX2/NEON) and automatic scalar fallback.
/// Streaming operations use `xxhash-rust` as the `xxh3` crate lacks streaming support.
///
/// # Examples
///
/// One-shot hashing (uses SIMD when available):
///
/// ```
/// use checksums::strong::Xxh3;
///
/// let seed: u64 = 0;
/// let digest = Xxh3::digest(seed, b"fast hashing");
/// assert_eq!(digest.len(), 8); // XXH3-64 produces 64-bit output
/// ```
///
/// Streaming/incremental hashing:
///
/// ```
/// use checksums::strong::Xxh3;
///
/// let seed: u64 = 42;
/// let mut hasher = Xxh3::new(seed);
///
/// // Process data in chunks
/// hasher.update(b"chunk one");
/// hasher.update(b"chunk two");
///
/// let digest = hasher.finalize();
/// assert_eq!(digest, Xxh3::digest(seed, b"chunk onechunk two"));
/// ```
///
/// Checking for SIMD acceleration:
///
/// ```
/// use checksums::strong::xxh3_simd_available;
///
/// assert!(xxh3_simd_available()); // always true -- xxh3 is always compiled in
/// ```
pub struct Xxh3 {
    inner: xxhash_rust::xxh3::Xxh3,
}

impl Xxh3 {
    /// Creates a hasher with the supplied seed.
    ///
    /// # Examples
    ///
    /// ```
    /// use checksums::strong::Xxh3;
    ///
    /// let mut hasher = Xxh3::new(0);
    /// hasher.update(b"data");
    /// let digest = hasher.finalize();
    /// assert_eq!(digest.len(), 8);
    /// ```
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self {
            inner: xxhash_rust::xxh3::Xxh3::with_seed(seed),
        }
    }

    /// Feeds additional bytes into the digest state.
    ///
    /// # Examples
    ///
    /// ```
    /// use checksums::strong::Xxh3;
    ///
    /// let mut hasher = Xxh3::new(999);
    /// hasher.update(b"incremental ");
    /// hasher.update(b"hashing");
    /// let digest = hasher.finalize();
    /// ```
    pub fn update(&mut self, data: &[u8]) {
        self.inner.update(data);
    }

    /// Finalises the digest and returns the little-endian XXH3/64 output.
    ///
    /// # Examples
    ///
    /// ```
    /// use checksums::strong::Xxh3;
    ///
    /// let mut hasher = Xxh3::new(0);
    /// hasher.update(b"finalize me");
    /// let digest = hasher.finalize();
    ///
    /// // Convert to u64 if needed
    /// let _hash_value = u64::from_le_bytes(digest);
    /// ```
    #[must_use]
    pub fn finalize(self) -> [u8; 8] {
        self.inner.digest().to_le_bytes()
    }

    /// Convenience helper that computes the XXH3/64 digest for `data` in one shot.
    ///
    /// Uses the `xxh3` crate with runtime SIMD detection to automatically use
    /// AVX2 (x86_64) or NEON (aarch64) when available, with a scalar fallback.
    ///
    /// # Examples
    ///
    /// ```
    /// use checksums::strong::Xxh3;
    ///
    /// let digest = Xxh3::digest(0, b"fast one-shot hash");
    /// assert_eq!(digest.len(), 8);
    /// ```
    #[must_use]
    pub fn digest(seed: u64, data: &[u8]) -> [u8; 8] {
        xxh3::hash64_with_seed(data, seed).to_le_bytes()
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
/// The one-shot [`digest`](Self::digest) method uses the `xxh3` crate with
/// runtime SIMD detection (AVX2/NEON) and automatic scalar fallback.
/// Streaming operations use `xxhash-rust` as the `xxh3` crate lacks streaming support.
///
/// # Examples
///
/// One-shot hashing:
///
/// ```
/// use checksums::strong::Xxh3_128;
///
/// let digest = Xxh3_128::digest(0, b"data for 128-bit hash");
/// assert_eq!(digest.len(), 16); // XXH3-128 produces 128-bit output
/// ```
///
/// Streaming hashing:
///
/// ```
/// use checksums::strong::Xxh3_128;
///
/// let seed: u64 = 12345;
/// let mut hasher = Xxh3_128::new(seed);
///
/// hasher.update(b"first part");
/// hasher.update(b"second part");
///
/// let digest = hasher.finalize();
/// assert_eq!(digest.len(), 16);
/// ```
///
/// When to use XXH3-128 vs XXH3-64:
///
/// ```
/// use checksums::strong::{Xxh3, Xxh3_128};
///
/// // XXH3-64 is faster and sufficient for most use cases
/// let fast_hash = Xxh3::digest(0, b"data");
///
/// // XXH3-128 provides lower collision probability for large datasets
/// let strong_hash = Xxh3_128::digest(0, b"data");
///
/// assert_eq!(fast_hash.len(), 8);   // 64 bits
/// assert_eq!(strong_hash.len(), 16); // 128 bits
/// ```
#[derive(Clone)]
pub struct Xxh3_128 {
    inner: xxhash_rust::xxh3::Xxh3,
}

impl Xxh3_128 {
    /// Creates a hasher with the supplied seed.
    ///
    /// # Examples
    ///
    /// ```
    /// use checksums::strong::Xxh3_128;
    ///
    /// let mut hasher = Xxh3_128::new(0);
    /// hasher.update(b"data");
    /// let digest = hasher.finalize();
    /// assert_eq!(digest.len(), 16);
    /// ```
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self {
            inner: xxhash_rust::xxh3::Xxh3::with_seed(seed),
        }
    }

    /// Feeds additional bytes into the digest state.
    ///
    /// # Examples
    ///
    /// ```
    /// use checksums::strong::Xxh3_128;
    ///
    /// let mut hasher = Xxh3_128::new(42);
    /// hasher.update(b"part 1");
    /// hasher.update(b"part 2");
    /// let digest = hasher.finalize();
    /// ```
    pub fn update(&mut self, data: &[u8]) {
        self.inner.update(data);
    }

    /// Finalises the digest and returns the little-endian XXH3/128 output.
    ///
    /// # Examples
    ///
    /// ```
    /// use checksums::strong::Xxh3_128;
    ///
    /// let mut hasher = Xxh3_128::new(0);
    /// hasher.update(b"data");
    /// let digest = hasher.finalize();
    ///
    /// // Convert to u128 if needed
    /// let _hash_value = u128::from_le_bytes(digest);
    /// ```
    #[must_use]
    pub fn finalize(self) -> [u8; 16] {
        self.inner.digest128().to_le_bytes()
    }

    /// Convenience helper that computes the XXH3/128 digest for `data` in one shot.
    ///
    /// Uses the `xxh3` crate with runtime SIMD detection to automatically use
    /// AVX2 (x86_64) or NEON (aarch64) when available, with a scalar fallback.
    ///
    /// # Examples
    ///
    /// ```
    /// use checksums::strong::Xxh3_128;
    ///
    /// let digest = Xxh3_128::digest(0, b"128-bit hash");
    /// assert_eq!(digest.len(), 16);
    /// ```
    #[must_use]
    pub fn digest(seed: u64, data: &[u8]) -> [u8; 16] {
        xxh3::hash128_with_seed(data, seed).to_le_bytes()
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
/// Always returns `true` because the `xxh3` crate (which provides runtime
/// AVX2/NEON detection with a scalar fallback) is always compiled in. One-shot
/// [`Xxh3::digest`] and [`Xxh3_128::digest`] calls automatically use the
/// fastest available instruction set at runtime.
///
/// Streaming operations (using `update`/`finalize`) always use `xxhash-rust`
/// which relies on compile-time SIMD detection.
///
/// # Examples
///
/// ```
/// use checksums::strong::xxh3_simd_available;
///
/// assert!(xxh3_simd_available()); // always true
/// ```
#[must_use]
pub const fn xxh3_simd_available() -> bool {
    true
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
        assert!(
            xxh3_simd_available(),
            "xxh3 crate is always compiled in, should report true"
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
