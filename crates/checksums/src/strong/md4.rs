use digest::Digest;

use super::StrongDigest;

/// Streaming MD4 hasher mirroring upstream rsync's default strong checksum.
#[derive(Clone, Debug)]
pub struct Md4 {
    inner: md4::Md4,
}

impl Default for Md4 {
    fn default() -> Self {
        Self::new()
    }
}

impl Md4 {
    /// Creates a hasher with an empty state.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: md4::Md4::new(),
        }
    }

    /// Feeds additional bytes into the digest state.
    pub fn update(&mut self, data: &[u8]) {
        self.inner.update(data);
    }

    /// Finalises the digest and returns the 128-bit MD4 output.
    #[must_use]
    pub fn finalize(self) -> [u8; 16] {
        self.inner.finalize().into()
    }

    /// Convenience helper that computes the MD4 digest for `data` in one shot.
    #[must_use]
    pub fn digest(data: &[u8]) -> [u8; 16] {
        <Self as StrongDigest>::digest(data)
    }
}

impl StrongDigest for Md4 {
    type Seed = ();
    type Digest = [u8; 16];
    const DIGEST_LEN: usize = 16;

    fn with_seed((): Self::Seed) -> Self {
        Md4::new()
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
    fn md4_streaming_matches_rfc_vectors() {
        let vectors = [
            (b"".as_slice(), "31d6cfe0d16ae931b73c59d7e0c089c0"),
            (b"a".as_slice(), "bde52cb31de33e46245e05fbdbd6fb24"),
            (b"abc".as_slice(), "a448017aaf21d8525fc10ae87aa6729d"),
            (
                b"message digest".as_slice(),
                "d9130a8164549fe818874806e1c7014b",
            ),
        ];

        for (input, expected_hex) in vectors {
            let mut hasher = Md4::new();
            let mid = input.len() / 2;
            hasher.update(&input[..mid]);
            hasher.update(&input[mid..]);
            let digest = hasher.finalize();
            assert_eq!(to_hex(&digest), expected_hex);

            let one_shot = Md4::digest(input);
            assert_eq!(to_hex(&one_shot), expected_hex);
        }
    }
}
