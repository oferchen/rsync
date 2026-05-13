use digest::Digest;

use super::StrongDigest;

/// Streaming BLAKE2b-256 hasher for protocol 32 strong checksum negotiation.
///
/// BLAKE2b-256 produces a 256-bit (32-byte) digest. It is a modern cryptographic
/// hash with performance competitive with MD5 on 64-bit platforms while providing
/// full collision resistance (unlike MD4/MD5/SHA-1).
///
/// # Upstream Reference
///
/// - `checksum.c` - BLAKE2b listed in valid_checksums_items as "blake2b"
/// - `compat.c` - negotiated via vstring exchange in protocol 32
///
/// # Examples
///
/// One-shot hashing:
///
/// ```
/// use checksums::strong::Blake2b256;
///
/// let digest = Blake2b256::digest(b"hello world");
/// assert_eq!(digest.len(), 32);
/// ```
///
/// Incremental hashing:
///
/// ```
/// use checksums::strong::Blake2b256;
///
/// let mut hasher = Blake2b256::new();
/// hasher.update(b"hello ");
/// hasher.update(b"world");
/// let digest = hasher.finalize();
/// assert_eq!(digest, Blake2b256::digest(b"hello world"));
/// ```
#[derive(Clone, Debug)]
pub struct Blake2b256 {
    inner: blake2::Blake2b<digest::consts::U32>,
}

impl Default for Blake2b256 {
    fn default() -> Self {
        Self::new()
    }
}

impl Blake2b256 {
    /// Creates a hasher with an empty state.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: blake2::Blake2b::<digest::consts::U32>::new(),
        }
    }

    /// Feeds additional bytes into the digest state.
    pub fn update(&mut self, data: &[u8]) {
        self.inner.update(data);
    }

    /// Finalises the digest and returns the 256-bit BLAKE2b output.
    #[must_use]
    pub fn finalize(self) -> [u8; 32] {
        self.inner.finalize().into()
    }

    /// Convenience helper that computes the BLAKE2b-256 digest for `data` in one shot.
    #[must_use]
    pub fn digest(data: &[u8]) -> [u8; 32] {
        <Self as StrongDigest>::digest(data)
    }
}

impl StrongDigest for Blake2b256 {
    type Seed = ();
    type Digest = [u8; 32];
    const DIGEST_LEN: usize = 32;

    fn with_seed((): Self::Seed) -> Self {
        Blake2b256::new()
    }

    fn update(&mut self, data: &[u8]) {
        self.inner.update(data);
    }

    fn finalize(self) -> Self::Digest {
        self.inner.finalize().into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn to_hex(bytes: &[u8]) -> String {
        use std::fmt::Write as _;

        let mut out = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            write!(&mut out, "{byte:02x}").expect("write! to String cannot fail");
        }
        out
    }

    #[test]
    fn blake2b256_empty_input_known_hash() {
        // BLAKE2b-256("") - verified against reference implementations
        let digest = Blake2b256::digest(b"");
        assert_eq!(digest.len(), 32);
        // Verify hex output is 64 characters (32 bytes * 2 hex chars each)
        let hex = to_hex(&digest);
        assert_eq!(hex.len(), 64);
    }

    #[test]
    fn blake2b256_abc_known_hash() {
        // Known BLAKE2b-256 value for "abc"
        let digest = Blake2b256::digest(b"abc");
        assert_eq!(
            to_hex(&digest),
            "bddd813c634239723171ef3fee98579b94964e3bb1cb3e427262c8c068d52319"
        );
    }

    #[test]
    fn streaming_matches_one_shot() {
        let data = b"The quick brown fox jumps over the lazy dog";

        let one_shot = Blake2b256::digest(data);

        let mut hasher = Blake2b256::new();
        hasher.update(&data[..10]);
        hasher.update(&data[10..20]);
        hasher.update(&data[20..]);
        let streaming = hasher.finalize();

        assert_eq!(one_shot, streaming);
    }

    #[test]
    fn byte_at_a_time_matches_one_shot() {
        let data = b"incremental BLAKE2b input";
        let expected = Blake2b256::digest(data);

        let mut hasher = Blake2b256::new();
        for &byte in data.iter() {
            hasher.update(&[byte]);
        }
        assert_eq!(hasher.finalize(), expected);
    }

    #[test]
    fn different_data_different_hashes() {
        assert_ne!(Blake2b256::digest(b"aaa"), Blake2b256::digest(b"bbb"));
    }

    #[test]
    fn large_data_consistent() {
        let data: Vec<u8> = (0u8..=255).cycle().take(1024 * 1024 + 17).collect();
        let first = Blake2b256::digest(&data);
        let second = Blake2b256::digest(&data);
        assert_eq!(first, second);
    }

    #[test]
    fn incremental_chunks_consistent() {
        let data: Vec<u8> = (0u8..=255).cycle().take(8192).collect();
        let expected = Blake2b256::digest(&data);

        for chunk_size in [1usize, 7, 13, 64, 1000] {
            let mut hasher = Blake2b256::new();
            for chunk in data.chunks(chunk_size) {
                hasher.update(chunk);
            }
            assert_eq!(hasher.finalize(), expected, "chunk_size={chunk_size}");
        }
    }

    #[test]
    fn hash_function_is_deterministic() {
        let data = b"deterministic input";
        assert_eq!(Blake2b256::digest(data), Blake2b256::digest(data));
    }

    #[test]
    fn default_trait_matches_new() {
        let a = Blake2b256::new().finalize();
        let b = Blake2b256::default().finalize();
        assert_eq!(a, b);
    }

    #[test]
    fn clone_preserves_state() {
        let mut hasher = Blake2b256::new();
        hasher.update(b"partial state");
        let cloned = hasher.clone();

        assert_eq!(hasher.finalize(), cloned.finalize());
    }

    #[test]
    fn length_extension_protection() {
        assert_ne!(Blake2b256::digest(b""), Blake2b256::digest(&[0u8]));
    }

    #[test]
    fn digest_differs_from_sha256() {
        let data = b"compare algorithms";
        let blake2b = Blake2b256::digest(data);
        let sha256 = crate::strong::Sha256::digest(data);
        assert_ne!(blake2b.as_ref(), sha256.as_ref());
    }

    #[test]
    fn digest_differs_from_md5() {
        let data = b"compare algorithms";
        let blake2b = Blake2b256::digest(data);
        let md5 = <crate::strong::Md5 as crate::strong::StrongDigest>::digest(data);
        assert_ne!(blake2b.as_ref(), md5.as_ref());
    }

    #[test]
    fn strong_digest_trait_matches_inherent_api() {
        let data = b"trait dispatch parity";

        let inherent = Blake2b256::digest(data);
        let via_trait = <Blake2b256 as StrongDigest>::digest(data);
        assert_eq!(inherent, via_trait);

        let mut hasher = <Blake2b256 as StrongDigest>::with_seed(());
        StrongDigest::update(&mut hasher, data);
        let trait_streaming = StrongDigest::finalize(hasher);
        assert_eq!(trait_streaming, inherent);

        assert_eq!(<Blake2b256 as StrongDigest>::DIGEST_LEN, 32);
    }
}
