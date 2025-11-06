use digest::Digest;

use super::StrongDigest;

/// Streaming SHA-1 hasher used by upstream rsync when negotiated with peers.
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
}
