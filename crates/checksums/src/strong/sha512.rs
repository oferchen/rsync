use digest::Digest;

use super::StrongDigest;

/// Streaming SHA-512 hasher used by rsync when peers negotiate the strongest daemon authentication digest.
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
}
