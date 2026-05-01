use digest::Digest;
use std::fmt;

use super::StrongDigest;
#[cfg(feature = "openssl")]
use super::openssl_support;

/// Streaming MD4 hasher mirroring upstream rsync's default strong checksum.
///
/// MD4 produces a 128-bit (16-byte) digest. It is used by rsync protocol
/// versions below 30 as the default strong checksum for block matching.
/// When the `openssl` feature is enabled, an OpenSSL-backed implementation
/// is used for improved throughput; otherwise a pure-Rust implementation
/// is used.
///
/// # Upstream Reference
///
/// - `checksum.c:get_checksum2()` - strong checksum computation using MD4
/// - `match.c:hash_search()` - verifies rolling checksum matches with MD4
///
/// # Examples
///
/// One-shot hashing:
///
/// ```
/// use checksums::strong::Md4;
///
/// let digest = Md4::digest(b"legacy data");
/// assert_eq!(digest.len(), 16);
/// ```
///
/// Incremental hashing:
///
/// ```
/// use checksums::strong::Md4;
///
/// let mut hasher = Md4::new();
/// hasher.update(b"part 1");
/// hasher.update(b"part 2");
/// let digest = hasher.finalize();
/// assert_eq!(digest, Md4::digest(b"part 1part 2"));
/// ```
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

    /// Computes the MD4 digest for `data` with rsync's checksum seed applied.
    ///
    /// When `seed` is non-zero, the seed value is appended to the data as four
    /// little-endian bytes before hashing. A `seed` of `0` is treated as
    /// "no seed" and produces an identical digest to [`Md4::digest`].
    ///
    /// This mirrors upstream rsync's `get_checksum2()` MD4 path, which appends
    /// `SIVAL(buf1, len, checksum_seed)` after the data and before digesting:
    ///
    /// ```text
    /// memcpy(buf1, buf, len);
    /// if (checksum_seed) {
    ///     SIVAL(buf1, len, checksum_seed);
    ///     len += 4;
    /// }
    /// ```
    ///
    /// upstream: checksum.c:358-396 `get_checksum2()` MD4 branch.
    ///
    /// # Examples
    ///
    /// ```
    /// use checksums::strong::Md4;
    ///
    /// let data = b"block";
    /// let seed: i32 = 0x12345678;
    ///
    /// let seeded = Md4::digest_with_seed(seed, data);
    ///
    /// // Equivalent: append seed bytes after data, then plain MD4.
    /// let mut combined = data.to_vec();
    /// combined.extend_from_slice(&seed.to_le_bytes());
    /// assert_eq!(seeded, Md4::digest(&combined));
    ///
    /// // Zero seed produces the same digest as unseeded MD4.
    /// assert_eq!(Md4::digest_with_seed(0, data), Md4::digest(data));
    /// ```
    #[must_use]
    pub fn digest_with_seed(seed: i32, data: &[u8]) -> [u8; 16] {
        let mut hasher = Md4::new();
        hasher.update(data);
        // upstream: checksum.c:377-380 - SIVAL(buf1, len, checksum_seed) appends
        // the seed as a 32-bit little-endian value when seed != 0.
        if seed != 0 {
            hasher.update(&seed.to_le_bytes());
        }
        hasher.finalize()
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
/// SIMD instructions (AVX2/AVX-512/NEON) via runtime CPUID detection.
/// Falls back to scalar computation when no SIMD instructions are available.
///
/// All implementations maintain RFC 1320 compatibility.
///
/// # Examples
///
/// ```
/// use checksums::strong::md4_digest_batch;
///
/// let inputs = [b"block1".as_slice(), b"block2", b"block3"];
/// let digests = md4_digest_batch(&inputs);
///
/// assert_eq!(digests.len(), 3);
/// for digest in &digests {
///     assert_eq!(digest.len(), 16);
/// }
/// ```
pub fn digest_batch<T: AsRef<[u8]>>(inputs: &[T]) -> Vec<[u8; 16]> {
    crate::simd_batch::md4::digest_batch(inputs)
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
    fn md4_seeded_appends_seed_after_data() {
        // upstream: checksum.c:376-380 - SIVAL(buf1, len, checksum_seed)
        let seed: i32 = 0x12345678;
        let data = b"rsync block payload";

        let seeded = Md4::digest_with_seed(seed, data);

        // Reference: build (data || seed_le_bytes) then plain MD4.
        let mut reference_buf = data.to_vec();
        reference_buf.extend_from_slice(&seed.to_le_bytes());
        let reference = Md4::digest(&reference_buf);

        assert_eq!(
            to_hex(&seeded),
            to_hex(&reference),
            "seeded MD4 must match upstream's append-seed-after-data semantics"
        );

        // Seeded digest must differ from unseeded for non-zero seed.
        assert_ne!(
            to_hex(&seeded),
            to_hex(&Md4::digest(data)),
            "non-zero seed must change the digest"
        );
    }

    #[test]
    fn md4_seeded_zero_seed_matches_unseeded() {
        // upstream: checksum.c:377 - `if (checksum_seed)` skips seed append on 0
        let data = b"zero seed produces unseeded digest";
        assert_eq!(
            Md4::digest_with_seed(0, data),
            Md4::digest(data),
            "zero seed must produce identical digest to unseeded MD4"
        );
    }

    #[test]
    fn md4_seeded_negative_seed_is_le_two_complement() {
        // i32::to_le_bytes preserves two's complement; verify wire equivalence.
        let seed: i32 = -1;
        let data = b"negative seed";

        let seeded = Md4::digest_with_seed(seed, data);

        let mut reference_buf = data.to_vec();
        reference_buf.extend_from_slice(&[0xff, 0xff, 0xff, 0xff]);
        assert_eq!(seeded, Md4::digest(&reference_buf));
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
