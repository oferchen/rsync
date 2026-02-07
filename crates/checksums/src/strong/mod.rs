//! Strong checksum implementations backed by well-known hash algorithms.
//!
//! Upstream rsync negotiates the strong checksum algorithm based on the protocol
//! version and compile-time feature set. This module exposes streaming wrappers
//! for MD4, MD5, XXH64, XXH3/64, and XXH3/128 so higher layers can compose the
//! desired strategy without reimplementing the hashing primitives.
//!
//! # Algorithm Examples
//!
//! ## Cryptographic Hashes (MD4, MD5, SHA family)
//!
//! ```
//! use checksums::strong::{Md4, Md5, Sha1, Sha256, StrongDigest};
//!
//! // MD4 - legacy algorithm for rsync protocol compatibility
//! let md4 = Md4::digest(b"data");
//! assert_eq!(md4.as_ref().len(), 16);
//!
//! // MD5 - common for protocol versions < 30
//! let md5 = Md5::digest(b"data");
//! assert_eq!(md5.as_ref().len(), 16);
//!
//! // SHA-1 and SHA-256 for higher security
//! let sha1 = Sha1::digest(b"data");
//! assert_eq!(sha1.as_ref().len(), 20);
//!
//! let sha256 = Sha256::digest(b"data");
//! assert_eq!(sha256.as_ref().len(), 32);
//! ```
//!
//! ## XXHash (Fast Non-Cryptographic)
//!
//! XXHash variants support seeding for protocol-specific initialization:
//!
//! ```
//! use checksums::strong::{Xxh64, Xxh3, Xxh3_128, StrongDigest};
//!
//! // XXH64 with seed (used by rsync protocol >= 30)
//! let seed: u64 = 0x12345678;
//! let xxh64 = Xxh64::digest_with_seed(seed, b"data");
//! assert_eq!(xxh64.as_ref().len(), 8);
//!
//! // XXH3 64-bit - faster on modern CPUs
//! let xxh3 = Xxh3::digest_with_seed(seed, b"data");
//! assert_eq!(xxh3.as_ref().len(), 8);
//!
//! // XXH3 128-bit for higher collision resistance
//! let xxh3_128 = Xxh3_128::digest_with_seed(seed, b"data");
//! assert_eq!(xxh3_128.as_ref().len(), 16);
//! ```

mod md4;
mod md5;
#[cfg(feature = "openssl")]
mod openssl_support;
mod sha1;
mod sha256;
mod sha512;
pub mod strategy;
mod xxhash;

/// MD4 streaming hasher and batch digest function.
///
/// `md4_digest_batch` computes MD4 digests for multiple inputs, using SIMD
/// acceleration when the `simd-batch` feature is enabled.
pub use md4::{Md4, digest_batch as md4_digest_batch};

/// MD5 streaming hasher, seed configuration, and batch digest function.
///
/// `md5_digest_batch` computes MD5 digests for multiple inputs, using SIMD
/// acceleration when the `simd-batch` feature is enabled.
pub use md5::{Md5, Md5Seed, digest_batch as md5_digest_batch};

#[cfg(feature = "openssl")]
pub use openssl_support::openssl_acceleration_available;

#[cfg(not(feature = "openssl"))]
#[inline]
/// Returns `false` when the `openssl` feature is not enabled, indicating that
/// OpenSSL-backed strong checksum acceleration is unavailable on this build.
///
/// This keeps the public API identical across platforms and feature sets so
/// callers can unconditionally query for OpenSSL support.
pub const fn openssl_acceleration_available() -> bool {
    false
}

/// Streaming SHA-1 hasher (160-bit output).
pub use sha1::Sha1;
/// Streaming SHA-256 hasher (256-bit output).
pub use sha256::Sha256;
/// Streaming SHA-512 hasher (512-bit output).
pub use sha512::Sha512;
/// XXHash streaming hashers and runtime SIMD detection query.
///
/// - [`Xxh3`] -- 64-bit XXH3 with optional SIMD acceleration on one-shot calls
/// - [`Xxh3_128`] -- 128-bit XXH3 with optional SIMD acceleration on one-shot calls
/// - [`Xxh64`] -- 64-bit XXH64
/// - [`xxh3_simd_available`] -- returns `true` when the `xxh3-simd` feature is enabled
pub use xxhash::{Xxh3, Xxh3_128, Xxh64, xxh3_simd_available};

/// Trait implemented by strong checksum algorithms used by rsync.
///
/// Implementors provide a streaming interface that mirrors upstream rsync's
/// usage: callers feed data incrementally via [`Self::update`] and then obtain
/// the final digest through [`Self::finalize`]. The associated
/// [`DIGEST_LEN`](Self::DIGEST_LEN) constant exposes the byte width of the
/// resulting hash so higher layers can size buffers without hard-coding
/// algorithm-specific knowledge.
///
/// # Examples
///
/// Compute an MD5 digest through the trait without depending on the concrete
/// hasher type.
///
/// ```
/// use checksums::strong::{Md5, StrongDigest};
///
/// let mut hasher = Md5::new();
/// hasher.update(b"example");
/// let digest = hasher.finalize();
/// assert_eq!(digest.as_ref().len(), Md5::DIGEST_LEN);
/// ```
pub trait StrongDigest: Sized {
    /// Type used to parameterise a new digest instance.
    type Seed: Default;

    /// Type returned when finalising the digest.
    type Digest: AsRef<[u8]> + Copy;

    /// Length of the final digest in bytes.
    const DIGEST_LEN: usize;

    /// Creates a new hasher with an empty state.
    #[must_use]
    fn new() -> Self {
        Self::with_seed(Default::default())
    }

    /// Creates a new hasher using the provided seed value.
    fn with_seed(seed: Self::Seed) -> Self;

    /// Feeds additional bytes into the digest state.
    fn update(&mut self, data: &[u8]);

    /// Finalises the digest and returns the resulting hash.
    fn finalize(self) -> Self::Digest;

    /// Convenience helper that hashes `data` in a single call.
    fn digest(data: &[u8]) -> Self::Digest {
        Self::digest_with_seed(Default::default(), data)
    }

    /// Convenience helper that hashes `data` using an explicit seed value.
    fn digest_with_seed(seed: Self::Seed, data: &[u8]) -> Self::Digest {
        let mut hasher = Self::with_seed(seed);
        hasher.update(data);
        hasher.finalize()
    }
}

#[cfg(test)]
mod tests {
    use super::{Md4, Md5, Sha1, Sha256, Sha512, StrongDigest, Xxh3, Xxh3_128, Xxh64};

    #[cfg(feature = "openssl")]
    #[test]
    fn openssl_detection_succeeds_when_feature_enabled() {
        assert!(super::openssl_acceleration_available());
    }

    #[test]
    fn md5_trait_round_trip_matches_inherent_api() {
        let input = b"trait-check";

        let mut via_trait = Md5::new();
        via_trait.update(input);
        let trait_digest = via_trait.finalize();

        assert_eq!(trait_digest.as_ref(), Md5::digest(input).as_ref());
    }

    #[test]
    fn md4_trait_digest_matches_inherent_helper() {
        let input = b"weak-md4";

        let digest = Md4::digest(input);
        assert_eq!(
            digest.as_ref(),
            <Md4 as StrongDigest>::digest(input).as_ref()
        );
    }

    #[test]
    fn xxh64_trait_supports_seeds() {
        let seed = 123_u64;
        let input = b"seeded";

        let digest = Xxh64::digest(seed, input);
        assert_eq!(
            digest.as_ref(),
            <Xxh64 as StrongDigest>::digest_with_seed(seed, input).as_ref()
        );
    }

    #[test]
    fn xxh3_trait_matches_inherent_api() {
        let seed = 77_u64;
        let input = b"xxh3-64";

        let mut via_trait: Xxh3 = StrongDigest::with_seed(seed);
        via_trait.update(input);
        let trait_digest = via_trait.finalize();

        assert_eq!(trait_digest.as_ref(), Xxh3::digest(seed, input).as_ref());
    }

    #[test]
    fn xxh3_128_trait_matches_inherent_api() {
        let seed = 987_u64;
        let input = b"xxh3-128";

        let mut via_trait: Xxh3_128 = StrongDigest::with_seed(seed);
        via_trait.update(input);
        let trait_digest = via_trait.finalize();

        assert_eq!(
            trait_digest.as_ref(),
            Xxh3_128::digest(seed, input).as_ref()
        );
    }

    #[test]
    fn sha1_trait_matches_inherent_api() {
        let input = b"sha1-check";

        let mut via_trait = Sha1::new();
        via_trait.update(input);
        let trait_digest = via_trait.finalize();

        assert_eq!(trait_digest.as_ref(), Sha1::digest(input).as_ref());
    }

    #[test]
    fn sha256_trait_matches_inherent_api() {
        let input = b"sha256-check";

        let mut via_trait = Sha256::new();
        via_trait.update(input);
        let trait_digest = via_trait.finalize();

        assert_eq!(trait_digest.as_ref(), Sha256::digest(input).as_ref());
    }

    #[test]
    fn sha512_trait_matches_inherent_api() {
        let input = b"sha512-check";

        let mut via_trait = Sha512::new();
        via_trait.update(input);
        let trait_digest = via_trait.finalize();

        assert_eq!(trait_digest.as_ref(), Sha512::digest(input).as_ref());
    }

    #[test]
    fn md5_digest_len_is_16() {
        assert_eq!(Md5::DIGEST_LEN, 16);
        let digest = Md5::digest(b"test");
        assert_eq!(digest.as_ref().len(), 16);
    }

    #[test]
    fn md4_digest_len_is_16() {
        assert_eq!(Md4::DIGEST_LEN, 16);
        let digest = Md4::digest(b"test");
        assert_eq!(digest.as_ref().len(), 16);
    }

    #[test]
    fn sha1_digest_len_is_20() {
        assert_eq!(Sha1::DIGEST_LEN, 20);
        let digest = Sha1::digest(b"test");
        assert_eq!(digest.as_ref().len(), 20);
    }

    #[test]
    fn sha256_digest_len_is_32() {
        assert_eq!(Sha256::DIGEST_LEN, 32);
        let digest = Sha256::digest(b"test");
        assert_eq!(digest.as_ref().len(), 32);
    }

    #[test]
    fn sha512_digest_len_is_64() {
        assert_eq!(Sha512::DIGEST_LEN, 64);
        let digest = Sha512::digest(b"test");
        assert_eq!(digest.as_ref().len(), 64);
    }

    #[test]
    fn xxh64_digest_len_is_8() {
        assert_eq!(Xxh64::DIGEST_LEN, 8);
        let digest = Xxh64::digest(0, b"test");
        assert_eq!(digest.as_ref().len(), 8);
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
    fn md5_multiple_updates() {
        let mut hasher = Md5::new();
        hasher.update(b"hello");
        hasher.update(b" ");
        hasher.update(b"world");
        let split_digest = hasher.finalize();

        let combined_digest = Md5::digest(b"hello world");
        assert_eq!(split_digest.as_ref(), combined_digest.as_ref());
    }

    #[test]
    fn sha256_multiple_updates() {
        let mut hasher = Sha256::new();
        hasher.update(b"foo");
        hasher.update(b"bar");
        let split_digest = hasher.finalize();

        let combined_digest = Sha256::digest(b"foobar");
        assert_eq!(split_digest.as_ref(), combined_digest.as_ref());
    }

    #[test]
    fn xxh64_different_seeds_different_digests() {
        let input = b"test input";
        let digest1 = Xxh64::digest(0, input);
        let digest2 = Xxh64::digest(1, input);
        assert_ne!(digest1.as_ref(), digest2.as_ref());
    }

    #[test]
    fn xxh3_different_seeds_different_digests() {
        let input = b"test input";
        let digest1 = Xxh3::digest(0, input);
        let digest2 = Xxh3::digest(1, input);
        assert_ne!(digest1.as_ref(), digest2.as_ref());
    }

    #[test]
    fn xxh3_128_different_seeds_different_digests() {
        let input = b"test input";
        let digest1 = Xxh3_128::digest(0, input);
        let digest2 = Xxh3_128::digest(1, input);
        assert_ne!(digest1.as_ref(), digest2.as_ref());
    }

    #[test]
    fn empty_input_produces_valid_digests() {
        let empty = b"";
        assert_eq!(Md4::digest(empty).as_ref().len(), Md4::DIGEST_LEN);
        assert_eq!(Md5::digest(empty).as_ref().len(), Md5::DIGEST_LEN);
        assert_eq!(Sha1::digest(empty).as_ref().len(), Sha1::DIGEST_LEN);
        assert_eq!(Sha256::digest(empty).as_ref().len(), Sha256::DIGEST_LEN);
        assert_eq!(Sha512::digest(empty).as_ref().len(), Sha512::DIGEST_LEN);
        assert_eq!(Xxh64::digest(0, empty).as_ref().len(), Xxh64::DIGEST_LEN);
        assert_eq!(Xxh3::digest(0, empty).as_ref().len(), Xxh3::DIGEST_LEN);
        assert_eq!(
            Xxh3_128::digest(0, empty).as_ref().len(),
            Xxh3_128::DIGEST_LEN
        );
    }

    #[test]
    fn same_input_same_digest() {
        let input = b"deterministic";
        assert_eq!(Md5::digest(input).as_ref(), Md5::digest(input).as_ref());
        assert_eq!(
            Sha256::digest(input).as_ref(),
            Sha256::digest(input).as_ref()
        );
    }

    #[test]
    fn different_input_different_digest() {
        let input1 = b"input1";
        let input2 = b"input2";
        assert_ne!(Md5::digest(input1).as_ref(), Md5::digest(input2).as_ref());
        assert_ne!(
            Sha256::digest(input1).as_ref(),
            Sha256::digest(input2).as_ref()
        );
    }
}
