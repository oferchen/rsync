use digest::Digest;

use super::StrongDigest;

/// Streaming MD5 hasher used by rsync when backward compatibility demands it.
#[derive(Clone, Debug)]
pub struct Md5 {
    inner: md5::Md5,
}

impl Default for Md5 {
    fn default() -> Self {
        Self::new()
    }
}

impl Md5 {
    /// Creates a hasher with an empty state.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: md5::Md5::new(),
        }
    }

    /// Feeds additional bytes into the digest state.
    pub fn update(&mut self, data: &[u8]) {
        self.inner.update(data);
    }

    /// Finalises the digest and returns the 128-bit MD5 output.
    #[must_use]
    pub fn finalize(self) -> [u8; 16] {
        self.inner.finalize().into()
    }

    /// Convenience helper that computes the MD5 digest for `data` in one shot.
    #[must_use]
    pub fn digest(data: &[u8]) -> [u8; 16] {
        <Self as StrongDigest>::digest(data)
    }
}

impl StrongDigest for Md5 {
    type Seed = ();
    type Digest = [u8; 16];
    const DIGEST_LEN: usize = 16;

    fn with_seed((): Self::Seed) -> Self {
        Md5::new()
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
    fn md5_streaming_matches_rfc_vectors() {
        let vectors = [
            (b"".as_slice(), "d41d8cd98f00b204e9800998ecf8427e"),
            (b"a".as_slice(), "0cc175b9c0f1b6a831c399e269772661"),
            (b"abc".as_slice(), "900150983cd24fb0d6963f7d28e17f72"),
            (
                b"message digest".as_slice(),
                "f96b697d7cb7938d525a2f31aaf161d0",
            ),
        ];

        for (input, expected_hex) in vectors {
            let mut hasher = Md5::new();
            let mid = input.len() / 2;
            hasher.update(&input[..mid]);
            hasher.update(&input[mid..]);
            let digest = hasher.finalize();
            assert_eq!(to_hex(&digest), expected_hex);

            let one_shot = Md5::digest(input);
            assert_eq!(to_hex(&one_shot), expected_hex);
        }
    }
}
