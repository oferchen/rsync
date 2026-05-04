use digest::Digest;

use super::StrongDigest;

/// Streaming SHA-512 hasher used by rsync when peers negotiate the strongest daemon authentication digest.
///
/// SHA-512 produces a 512-bit (64-byte) digest and provides the maximum
/// security level among the supported algorithms. It is used for daemon
/// authentication when maximum collision resistance is required.
///
/// # Examples
///
/// One-shot hashing:
///
/// ```
/// use checksums::strong::Sha512;
///
/// let digest = Sha512::digest(b"important data");
/// assert_eq!(digest.len(), 64);
/// ```
///
/// Incremental hashing:
///
/// ```
/// use checksums::strong::Sha512;
///
/// let mut hasher = Sha512::new();
/// hasher.update(b"chunk a");
/// hasher.update(b"chunk b");
/// let digest = hasher.finalize();
/// assert_eq!(digest, Sha512::digest(b"chunk achunk b"));
/// ```
#[derive(Clone, Debug)]
pub struct Sha512 {
    inner: sha2::Sha512,
}

impl Default for Sha512 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha512 {
    /// Creates a hasher with an empty state.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: sha2::Sha512::new(),
        }
    }

    /// Feeds additional bytes into the digest state.
    pub fn update(&mut self, data: &[u8]) {
        self.inner.update(data);
    }

    /// Finalises the digest and returns the 512-bit SHA-512 output.
    #[must_use]
    pub fn finalize(self) -> [u8; 64] {
        self.inner.finalize().into()
    }

    /// Convenience helper that computes the SHA-512 digest for `data` in one shot.
    #[must_use]
    pub fn digest(data: &[u8]) -> [u8; 64] {
        <Self as StrongDigest>::digest(data)
    }
}

impl StrongDigest for Sha512 {
    type Seed = ();
    type Digest = [u8; 64];
    const DIGEST_LEN: usize = 64;

    fn with_seed((): Self::Seed) -> Self {
        Sha512::new()
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
    fn sha512_streaming_matches_rfc_vectors() {
        let vectors = [
            (
                b"".as_slice(),
                "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e",
            ),
            (
                b"abc".as_slice(),
                "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f",
            ),
            (
                b"message digest".as_slice(),
                "107dbf389d9e9f71a3a95f6c055b9251bc5268c2be16d6c13492ea45b0199f3309e16455ab1e96118e8a905d5597b72038ddb372a89826046de66687bb420e7c",
            ),
        ];

        for (input, expected_hex) in vectors {
            let mut hasher = Sha512::new();
            let mid = input.len() / 2;
            hasher.update(&input[..mid]);
            hasher.update(&input[mid..]);
            let digest = hasher.finalize();
            assert_eq!(to_hex(&digest), expected_hex);

            let one_shot = Sha512::digest(input);
            assert_eq!(to_hex(&one_shot), expected_hex);
        }
    }

    #[test]
    fn empty_input_known_hash() {
        let digest = Sha512::digest(b"");
        assert_eq!(
            to_hex(&digest),
            "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e"
        );
    }

    #[test]
    fn abc_known_hash() {
        let digest = Sha512::digest(b"abc");
        assert_eq!(
            to_hex(&digest),
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
        );
    }

    #[test]
    fn streaming_matches_one_shot() {
        let data = b"The quick brown fox jumps over the lazy dog";

        let one_shot = Sha512::digest(data);

        let mut hasher = Sha512::new();
        hasher.update(&data[..10]);
        hasher.update(&data[10..20]);
        hasher.update(&data[20..]);
        let streaming = hasher.finalize();

        assert_eq!(one_shot, streaming);
    }

    #[test]
    fn byte_at_a_time_matches_one_shot() {
        let data = b"incremental SHA-512 input";
        let expected = Sha512::digest(data);

        let mut hasher = Sha512::new();
        for &byte in data.iter() {
            hasher.update(&[byte]);
        }
        assert_eq!(hasher.finalize(), expected);
    }

    #[test]
    fn different_data_different_hashes() {
        assert_ne!(Sha512::digest(b"aaa"), Sha512::digest(b"bbb"));
    }

    #[test]
    fn large_data_consistent() {
        // Walk the 128-byte SHA-512 compression function across many blocks.
        let data: Vec<u8> = (0u8..=255).cycle().take(1024 * 1024 + 17).collect();
        let first = Sha512::digest(&data);
        let second = Sha512::digest(&data);
        assert_eq!(first, second);
    }

    #[test]
    fn incremental_chunks_consistent() {
        let data: Vec<u8> = (0u8..=255).cycle().take(8192).collect();
        let expected = Sha512::digest(&data);

        for chunk_size in [1usize, 7, 13, 128, 1000] {
            let mut hasher = Sha512::new();
            for chunk in data.chunks(chunk_size) {
                hasher.update(chunk);
            }
            assert_eq!(hasher.finalize(), expected, "chunk_size={chunk_size}");
        }
    }

    #[test]
    fn hash_function_is_deterministic() {
        let data = b"deterministic input";
        assert_eq!(Sha512::digest(data), Sha512::digest(data));
    }

    #[test]
    fn default_trait_matches_new() {
        let a = Sha512::new().finalize();
        let b = Sha512::default().finalize();
        assert_eq!(a, b);
    }

    #[test]
    fn clone_preserves_state() {
        let mut hasher = Sha512::new();
        hasher.update(b"partial state");
        let cloned = hasher.clone();

        assert_eq!(hasher.finalize(), cloned.finalize());
    }

    #[test]
    fn length_extension_protection() {
        assert_ne!(Sha512::digest(b""), Sha512::digest(&[0u8]));
    }

    #[test]
    fn hex_output_format_matches_lowercase() {
        let digest = Sha512::digest(b"abc");
        let hex = to_hex(&digest);
        assert_eq!(hex.len(), 128);
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(hex.chars().all(|c| !c.is_ascii_uppercase()));
    }

    #[test]
    fn strong_digest_trait_matches_inherent_api() {
        let data = b"trait dispatch parity";

        let inherent = Sha512::digest(data);
        let via_trait = <Sha512 as StrongDigest>::digest(data);
        assert_eq!(inherent, via_trait);

        let mut hasher = <Sha512 as StrongDigest>::with_seed(());
        StrongDigest::update(&mut hasher, data);
        let trait_streaming = StrongDigest::finalize(hasher);
        assert_eq!(trait_streaming, inherent);

        assert_eq!(<Sha512 as StrongDigest>::DIGEST_LEN, 64);
    }
}
