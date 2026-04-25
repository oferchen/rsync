use digest::Digest;

use super::StrongDigest;

/// Streaming SHA-256 hasher used by rsync when peers negotiate stronger daemon authentication digests.
///
/// SHA-256 produces a 256-bit (32-byte) digest and is cryptographically
/// secure. It is used for daemon authentication and high-security transfers.
/// Hardware acceleration via SHA-NI (x86_64) or crypto extensions (aarch64)
/// is used automatically when compiled with appropriate target features.
///
/// # Examples
///
/// One-shot hashing:
///
/// ```
/// use checksums::strong::Sha256;
///
/// let digest = Sha256::digest(b"secure data");
/// assert_eq!(digest.len(), 32);
/// ```
///
/// Incremental hashing:
///
/// ```
/// use checksums::strong::Sha256;
///
/// let mut hasher = Sha256::new();
/// hasher.update(b"part one");
/// hasher.update(b"part two");
/// let digest = hasher.finalize();
/// assert_eq!(digest, Sha256::digest(b"part onepart two"));
/// ```
#[derive(Clone, Debug)]
pub struct Sha256 {
    inner: sha2::Sha256,
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha256 {
    /// Creates a hasher with an empty state.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: sha2::Sha256::new(),
        }
    }

    /// Feeds additional bytes into the digest state.
    pub fn update(&mut self, data: &[u8]) {
        self.inner.update(data);
    }

    /// Finalises the digest and returns the 256-bit SHA-256 output.
    #[must_use]
    pub fn finalize(self) -> [u8; 32] {
        self.inner.finalize().into()
    }

    /// Convenience helper that computes the SHA-256 digest for `data` in one shot.
    #[must_use]
    pub fn digest(data: &[u8]) -> [u8; 32] {
        <Self as StrongDigest>::digest(data)
    }
}

impl StrongDigest for Sha256 {
    type Seed = ();
    type Digest = [u8; 32];
    const DIGEST_LEN: usize = 32;

    fn with_seed((): Self::Seed) -> Self {
        Sha256::new()
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
    fn sha256_streaming_matches_rfc_vectors() {
        let vectors = [
            (
                b"".as_slice(),
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            ),
            (
                b"abc".as_slice(),
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            ),
            (
                b"message digest".as_slice(),
                "f7846f55cf23e14eebeab5b4e1550cad5b509e3348fbc4efa3a1413d393cb650",
            ),
        ];

        for (input, expected_hex) in vectors {
            let mut hasher = Sha256::new();
            let mid = input.len() / 2;
            hasher.update(&input[..mid]);
            hasher.update(&input[mid..]);
            let digest = hasher.finalize();
            assert_eq!(to_hex(&digest), expected_hex);

            let one_shot = Sha256::digest(input);
            assert_eq!(to_hex(&one_shot), expected_hex);
        }
    }

    #[test]
    fn empty_input_known_hash() {
        let digest = Sha256::digest(b"");
        assert_eq!(
            to_hex(&digest),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn abc_known_hash() {
        let digest = Sha256::digest(b"abc");
        assert_eq!(
            to_hex(&digest),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn streaming_matches_one_shot() {
        let data = b"The quick brown fox jumps over the lazy dog";

        let one_shot = Sha256::digest(data);

        let mut hasher = Sha256::new();
        hasher.update(&data[..10]);
        hasher.update(&data[10..20]);
        hasher.update(&data[20..]);
        let streaming = hasher.finalize();

        assert_eq!(one_shot, streaming);
    }

    #[test]
    fn byte_at_a_time_matches_one_shot() {
        let data = b"incremental SHA-256 input";
        let expected = Sha256::digest(data);

        let mut hasher = Sha256::new();
        for &byte in data.iter() {
            hasher.update(&[byte]);
        }
        assert_eq!(hasher.finalize(), expected);
    }

    #[test]
    fn different_data_different_hashes() {
        assert_ne!(Sha256::digest(b"aaa"), Sha256::digest(b"bbb"));
    }

    #[test]
    fn large_data_consistent() {
        // Walk the 64-byte SHA-256 compression function across many blocks.
        let data: Vec<u8> = (0u8..=255).cycle().take(1024 * 1024 + 17).collect();
        let first = Sha256::digest(&data);
        let second = Sha256::digest(&data);
        assert_eq!(first, second);
    }

    #[test]
    fn incremental_chunks_consistent() {
        let data: Vec<u8> = (0u8..=255).cycle().take(8192).collect();
        let expected = Sha256::digest(&data);

        for chunk_size in [1usize, 7, 13, 64, 1000] {
            let mut hasher = Sha256::new();
            for chunk in data.chunks(chunk_size) {
                hasher.update(chunk);
            }
            assert_eq!(hasher.finalize(), expected, "chunk_size={chunk_size}");
        }
    }

    #[test]
    fn hash_function_is_deterministic() {
        let data = b"deterministic input";
        assert_eq!(Sha256::digest(data), Sha256::digest(data));
    }

    #[test]
    fn default_trait_matches_new() {
        let a = Sha256::new().finalize();
        let b = Sha256::default().finalize();
        assert_eq!(a, b);
    }

    #[test]
    fn clone_preserves_state() {
        let mut hasher = Sha256::new();
        hasher.update(b"partial state");
        let cloned = hasher.clone();

        assert_eq!(hasher.finalize(), cloned.finalize());
    }

    #[test]
    fn length_extension_protection() {
        assert_ne!(Sha256::digest(b""), Sha256::digest(&[0u8]));
    }

    #[test]
    fn hex_output_format_matches_lowercase() {
        let digest = Sha256::digest(b"abc");
        let hex = to_hex(&digest);
        assert_eq!(hex.len(), 64);
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(hex.chars().all(|c| !c.is_ascii_uppercase()));
    }

    #[test]
    fn strong_digest_trait_matches_inherent_api() {
        let data = b"trait dispatch parity";

        let inherent = Sha256::digest(data);
        let via_trait = <Sha256 as StrongDigest>::digest(data);
        assert_eq!(inherent, via_trait);

        let mut hasher = <Sha256 as StrongDigest>::with_seed(());
        StrongDigest::update(&mut hasher, data);
        let trait_streaming = StrongDigest::finalize(hasher);
        assert_eq!(trait_streaming, inherent);

        assert_eq!(<Sha256 as StrongDigest>::DIGEST_LEN, 32);
    }
}
