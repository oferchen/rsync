use digest::Digest;
use std::fmt;

use super::StrongDigest;
#[cfg(feature = "openssl")]
use super::openssl_support;

/// Streaming MD4 hasher mirroring upstream rsync's default strong checksum.
#[derive(Clone)]
pub struct Md4 {
    inner: Md4Backend,
}

#[derive(Clone)]
enum Md4Backend {
    #[cfg(feature = "openssl")]
    OpenSsl(openssl::hash::Hasher),
    Rust(md4::Md4),
}

impl Md4Backend {
    fn new() -> Self {
        #[cfg(feature = "openssl")]
        {
            if let Some(hasher) = openssl_support::new_md4_hasher() {
                return Self::OpenSsl(hasher);
            }
        }

        Self::Rust(md4::Md4::new())
    }
}

impl fmt::Debug for Md4 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Md4").field("backend", &self.inner).finish()
    }
}

impl fmt::Debug for Md4Backend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            #[cfg(feature = "openssl")]
            Md4Backend::OpenSsl(_) => f.write_str("OpenSsl"),
            Md4Backend::Rust(_) => f.write_str("Rust"),
        }
    }
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
            inner: Md4Backend::new(),
        }
    }

    /// Feeds additional bytes into the digest state.
    pub fn update(&mut self, data: &[u8]) {
        match &mut self.inner {
            #[cfg(feature = "openssl")]
            Md4Backend::OpenSsl(hasher) => {
                hasher.update(data).expect("OpenSSL MD4 update failed");
            }
            Md4Backend::Rust(hasher) => hasher.update(data),
        }
    }

    /// Finalises the digest and returns the 128-bit MD4 output.
    #[must_use]
    pub fn finalize(self) -> [u8; 16] {
        match self.inner {
            #[cfg(feature = "openssl")]
            Md4Backend::OpenSsl(mut hasher) => {
                let mut output = [0_u8; 16];
                let bytes = hasher.finish().expect("OpenSSL MD4 finalisation failed");
                output.copy_from_slice(bytes.as_ref());
                output
            }
            Md4Backend::Rust(hasher) => hasher.finalize().into(),
        }
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
        self.update(data);
    }

    fn finalize(self) -> Self::Digest {
        self.finalize()
    }
}

/// Batch compute MD4 digests using SIMD when available.
///
/// This function computes MD4 digests for multiple inputs in parallel using
/// SIMD instructions (AVX2/AVX-512/NEON) when the `md5-simd` feature is enabled.
/// Falls back to sequential computation when SIMD is unavailable.
#[cfg(feature = "md5-simd")]
pub fn digest_batch<T: AsRef<[u8]>>(inputs: &[T]) -> Vec<[u8; 16]> {
    md5_simd::md4::digest_batch(inputs)
}

/// Batch compute MD4 digests (sequential fallback when SIMD unavailable).
#[cfg(not(feature = "md5-simd"))]
pub fn digest_batch<T: AsRef<[u8]>>(inputs: &[T]) -> Vec<[u8; 16]> {
    inputs.iter().map(|i| Md4::digest(i.as_ref())).collect()
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

    #[test]
    fn md4_batch_matches_sequential() {
        let inputs: &[&[u8]] = &[
            b"",
            b"a",
            b"abc",
            b"message digest",
            b"abcdefghijklmnopqrstuvwxyz",
        ];

        let batch_results = super::digest_batch(inputs);
        let sequential_results: Vec<[u8; 16]> = inputs.iter().map(|i| Md4::digest(i)).collect();

        assert_eq!(batch_results.len(), sequential_results.len());
        for (i, (batch, seq)) in batch_results
            .iter()
            .zip(sequential_results.iter())
            .enumerate()
        {
            assert_eq!(to_hex(batch), to_hex(seq), "Mismatch at index {i}");
        }
    }
}
