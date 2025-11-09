//! Strong checksum implementations backed by well-known hash algorithms.
//!
//! Upstream rsync negotiates the strong checksum algorithm based on the protocol
//! version and compile-time feature set. This module exposes streaming wrappers
//! for MD4, MD5, XXH64, XXH3/64, and XXH3/128 so higher layers can compose the
//! desired strategy without reimplementing the hashing primitives.

mod md4;
mod md5;
#[cfg(feature = "openssl")]
mod openssl_support;
mod sha1;
mod sha256;
mod sha512;
mod xxhash;

pub use md4::Md4;
pub use md5::Md5;
#[cfg(feature = "openssl")]
pub use openssl_support::openssl_acceleration_available;
#[cfg(not(feature = "openssl"))]
#[inline]
pub const fn openssl_acceleration_available() -> bool {
    false
}
pub use sha1::Sha1;
pub use sha256::Sha256;
pub use sha512::Sha512;
pub use xxhash::{Xxh3, Xxh3_128, Xxh64};

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
}
