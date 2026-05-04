use digest::Digest;

use super::StrongDigest;

/// Streaming SHA-1 hasher used by upstream rsync when negotiated with peers.
///
/// SHA-1 produces a 160-bit (20-byte) digest. While collision attacks are
/// known, it remains useful for interoperability with peers that negotiate
/// SHA-1 as a stronger alternative to MD4/MD5.
///
/// # Examples
///
/// One-shot hashing:
///
/// ```
/// use checksums::strong::Sha1;
///
/// let digest = Sha1::digest(b"hello world");
/// assert_eq!(digest.len(), 20);
/// ```
///
/// Incremental hashing:
///
/// ```
/// use checksums::strong::Sha1;
///
/// let mut hasher = Sha1::new();
/// hasher.update(b"hello ");
/// hasher.update(b"world");
/// let digest = hasher.finalize();
/// assert_eq!(digest, Sha1::digest(b"hello world"));
/// ```
#[derive(Clone, Debug)]
pub struct Sha1 {
    inner: sha1::Sha1,
}

impl Default for Sha1 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha1 {
    /// Creates a hasher with an empty state.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: sha1::Sha1::new(),
        }
    }

    /// Feeds additional bytes into the digest state.
    pub fn update(&mut self, data: &[u8]) {
        self.inner.update(data);
    }

    /// Finalises the digest and returns the 160-bit SHA-1 output.
    #[must_use]
    pub fn finalize(self) -> [u8; 20] {
        self.inner.finalize().into()
    }

    /// Convenience helper that computes the SHA-1 digest for `data` in one shot.
    #[must_use]
    pub fn digest(data: &[u8]) -> [u8; 20] {
        <Self as StrongDigest>::digest(data)
    }
}

impl StrongDigest for Sha1 {
    type Seed = ();
    type Digest = [u8; 20];
    const DIGEST_LEN: usize = 20;

    fn with_seed((): Self::Seed) -> Self {
        Sha1::new()
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
    fn sha1_streaming_matches_rfc_vectors() {
        let vectors = [
            (b"".as_slice(), "da39a3ee5e6b4b0d3255bfef95601890afd80709"),
            (b"a".as_slice(), "86f7e437faa5a7fce15d1ddcb9eaeaea377667b8"),
            (
                b"abc".as_slice(),
                "a9993e364706816aba3e25717850c26c9cd0d89d",
            ),
            (
                b"message digest".as_slice(),
                "c12252ceda8be8994d5fa0290a47231c1d16aae3",
            ),
        ];

        for (input, expected_hex) in vectors {
            let mut hasher = Sha1::new();
            let mid = input.len() / 2;
            hasher.update(&input[..mid]);
            hasher.update(&input[mid..]);
            let digest = hasher.finalize();
            assert_eq!(to_hex(&digest), expected_hex);

            let one_shot = Sha1::digest(input);
            assert_eq!(to_hex(&one_shot), expected_hex);
        }
    }

    #[test]
    fn empty_input_known_hash() {
        let digest = Sha1::digest(b"");
        assert_eq!(to_hex(&digest), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
    }

    #[test]
    fn abc_known_hash() {
        let digest = Sha1::digest(b"abc");
        assert_eq!(to_hex(&digest), "a9993e364706816aba3e25717850c26c9cd0d89d");
    }

    #[test]
    fn streaming_matches_one_shot() {
        let data = b"The quick brown fox jumps over the lazy dog";

        let one_shot = Sha1::digest(data);

        let mut hasher = Sha1::new();
        hasher.update(&data[..10]);
        hasher.update(&data[10..20]);
        hasher.update(&data[20..]);
        let streaming = hasher.finalize();

        assert_eq!(one_shot, streaming);
    }

    #[test]
    fn byte_at_a_time_matches_one_shot() {
        let data = b"incremental SHA-1 input";
        let expected = Sha1::digest(data);

        let mut hasher = Sha1::new();
        for &byte in data.iter() {
            hasher.update(&[byte]);
        }
        assert_eq!(hasher.finalize(), expected);
    }

    #[test]
    fn different_data_different_hashes() {
        assert_ne!(Sha1::digest(b"aaa"), Sha1::digest(b"bbb"));
    }

    #[test]
    fn large_data_consistent() {
        // Exercise the hash with > 1 MiB of cyclic data to walk multiple
        // 64-byte SHA-1 compression blocks and verify deterministic output.
        let data: Vec<u8> = (0u8..=255).cycle().take(1024 * 1024 + 17).collect();
        let first = Sha1::digest(&data);
        let second = Sha1::digest(&data);
        assert_eq!(first, second);
    }

    #[test]
    fn incremental_chunks_consistent() {
        let data: Vec<u8> = (0u8..=255).cycle().take(8192).collect();
        let expected = Sha1::digest(&data);

        for chunk_size in [1usize, 7, 13, 64, 1000] {
            let mut hasher = Sha1::new();
            for chunk in data.chunks(chunk_size) {
                hasher.update(chunk);
            }
            assert_eq!(hasher.finalize(), expected, "chunk_size={chunk_size}");
        }
    }

    #[test]
    fn hash_function_is_deterministic() {
        let data = b"deterministic input";
        assert_eq!(Sha1::digest(data), Sha1::digest(data));
    }

    #[test]
    fn default_trait_matches_new() {
        let a = Sha1::new().finalize();
        let b = Sha1::default().finalize();
        assert_eq!(a, b);
    }

    #[test]
    fn clone_preserves_state() {
        let mut hasher = Sha1::new();
        hasher.update(b"partial state");
        let cloned = hasher.clone();

        assert_eq!(hasher.finalize(), cloned.finalize());
    }

    #[test]
    fn length_extension_protection() {
        // hash(empty) must differ from hash([0u8]) - a single null byte is not
        // the same as no input.
        assert_ne!(Sha1::digest(b""), Sha1::digest(&[0u8]));
    }

    #[test]
    fn hex_output_format_matches_lowercase() {
        let digest = Sha1::digest(b"abc");
        let hex = to_hex(&digest);
        assert_eq!(hex.len(), 40);
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(hex.chars().all(|c| !c.is_ascii_uppercase()));
    }

    #[test]
    fn strong_digest_trait_matches_inherent_api() {
        let data = b"trait dispatch parity";

        let inherent = Sha1::digest(data);
        let via_trait = <Sha1 as StrongDigest>::digest(data);
        assert_eq!(inherent, via_trait);

        let mut hasher = <Sha1 as StrongDigest>::with_seed(());
        StrongDigest::update(&mut hasher, data);
        let trait_streaming = StrongDigest::finalize(hasher);
        assert_eq!(trait_streaming, inherent);

        assert_eq!(<Sha1 as StrongDigest>::DIGEST_LEN, 20);
    }
}
