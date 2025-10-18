use super::StrongDigest;

/// Streaming XXH64 hasher used by rsync when negotiated by newer protocols.
#[derive(Clone)]
pub struct Xxh64 {
    inner: xxhash_rust::xxh64::Xxh64,
}

impl Xxh64 {
    /// Creates a hasher with the supplied seed.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self {
            inner: xxhash_rust::xxh64::Xxh64::new(seed),
        }
    }

    /// Feeds additional bytes into the digest state.
    pub fn update(&mut self, data: &[u8]) {
        self.inner.update(data);
    }

    /// Finalises the digest and returns the little-endian XXH64 output.
    #[must_use]
    pub fn finalize(self) -> [u8; 8] {
        self.inner.digest().to_le_bytes()
    }

    /// Convenience helper that computes the XXH64 digest for `data` in one shot.
    #[must_use]
    pub fn digest(seed: u64, data: &[u8]) -> [u8; 8] {
        <Self as StrongDigest>::digest_with_seed(seed, data)
    }
}

impl StrongDigest for Xxh64 {
    type Seed = u64;
    type Digest = [u8; 8];
    const DIGEST_LEN: usize = 8;

    fn with_seed(seed: Self::Seed) -> Self {
        Xxh64::new(seed)
    }

    fn update(&mut self, data: &[u8]) {
        self.inner.update(data);
    }

    fn finalize(self) -> Self::Digest {
        self.inner.digest().to_le_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xxh64_matches_reference_values() {
        let vectors = [
            (0, b"".as_slice()),
            (0, b"a".as_slice()),
            (0, b"The quick brown fox jumps over the lazy dog".as_slice()),
            (123, b"rsync".as_slice()),
        ];

        for (seed, input) in vectors {
            let mut hasher = Xxh64::new(seed);
            let mid = input.len() / 2;
            hasher.update(&input[..mid]);
            hasher.update(&input[mid..]);
            let digest = hasher.finalize();
            let expected = xxhash_rust::xxh64::xxh64(input, seed).to_le_bytes();
            assert_eq!(digest, expected);

            let one_shot = Xxh64::digest(seed, input);
            assert_eq!(one_shot, expected);
        }
    }
}
