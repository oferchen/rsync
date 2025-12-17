use digest::Digest;
use std::fmt;

use super::StrongDigest;
#[cfg(feature = "openssl")]
use super::openssl_support;

/// Seed configuration for MD5 checksum calculation.
///
/// Rsync uses seeded MD5 checksums for file list validation. The
/// `CHECKSUM_SEED_FIX` compatibility flag (protocol 30+) determines whether the
/// seed is hashed before or after the file data:
///
/// - `proper_order = true` (new behavior): hash seed, then data
/// - `proper_order = false` (old behavior): hash data, then seed
///
/// This ordering difference affects checksum compatibility between protocol
/// versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Md5Seed {
    /// Seed value to mix into the hash.
    pub value: Option<i32>,
    /// Whether to use proper ordering (seed-before-data) vs old ordering (seed-after-data).
    pub proper_order: bool,
}

impl Md5Seed {
    /// Creates a seed configuration with no seed value.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            value: None,
            proper_order: true,
        }
    }

    /// Creates a seed configuration with seed-before-data ordering (protocol 30+ with CHECKSUM_SEED_FIX).
    #[must_use]
    pub const fn proper(value: i32) -> Self {
        Self {
            value: Some(value),
            proper_order: true,
        }
    }

    /// Creates a seed configuration with seed-after-data ordering (legacy protocols).
    #[must_use]
    pub const fn legacy(value: i32) -> Self {
        Self {
            value: Some(value),
            proper_order: false,
        }
    }
}

impl Default for Md5Seed {
    fn default() -> Self {
        Self::none()
    }
}

/// Streaming MD5 hasher used by rsync when backward compatibility demands it.
///
/// Supports optional seeded hashing with configurable ordering for protocol
/// compatibility. See [`Md5Seed`] for details on seed ordering behavior.
#[derive(Clone)]
pub struct Md5 {
    inner: Md5Backend,
    /// Seed to hash AFTER data (when proper_order is false).
    pending_seed: Option<i32>,
}

#[derive(Clone)]
enum Md5Backend {
    #[cfg(feature = "openssl")]
    OpenSsl(openssl::hash::Hasher),
    Rust(md5::Md5),
}

impl Md5Backend {
    fn new() -> Self {
        #[cfg(feature = "openssl")]
        {
            if let Some(hasher) = openssl_support::new_md5_hasher() {
                return Self::OpenSsl(hasher);
            }
        }

        Self::Rust(md5::Md5::new())
    }
}

impl fmt::Debug for Md5 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Md5").field("backend", &self.inner).finish()
    }
}

impl fmt::Debug for Md5Backend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            #[cfg(feature = "openssl")]
            Md5Backend::OpenSsl(_) => f.write_str("OpenSsl"),
            Md5Backend::Rust(_) => f.write_str("Rust"),
        }
    }
}

impl Default for Md5 {
    fn default() -> Self {
        Self::new()
    }
}

impl Md5 {
    /// Creates a hasher with an empty state and no seed.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Md5Backend::new(),
            pending_seed: None,
        }
    }

    /// Feeds additional bytes into the digest state.
    pub fn update(&mut self, data: &[u8]) {
        match &mut self.inner {
            #[cfg(feature = "openssl")]
            Md5Backend::OpenSsl(hasher) => {
                hasher.update(data).expect("OpenSSL MD5 update failed");
            }
            Md5Backend::Rust(hasher) => hasher.update(data),
        }
    }

    /// Finalises the digest and returns the 128-bit MD5 output.
    ///
    /// If a seed was configured with `proper_order = false` (legacy ordering),
    /// the seed bytes are hashed after the data before finalizing.
    #[must_use]
    pub fn finalize(mut self) -> [u8; 16] {
        // If we have a pending seed (legacy order), hash it AFTER the data
        if let Some(seed) = self.pending_seed {
            self.update(&seed.to_le_bytes());
        }

        match self.inner {
            #[cfg(feature = "openssl")]
            Md5Backend::OpenSsl(mut hasher) => {
                let mut output = [0_u8; 16];
                let bytes = hasher.finish().expect("OpenSSL MD5 finalisation failed");
                output.copy_from_slice(bytes.as_ref());
                output
            }
            Md5Backend::Rust(hasher) => hasher.finalize().into(),
        }
    }

    /// Convenience helper that computes the MD5 digest for `data` in one shot.
    #[must_use]
    pub fn digest(data: &[u8]) -> [u8; 16] {
        <Self as StrongDigest>::digest(data)
    }
}

impl StrongDigest for Md5 {
    type Seed = Md5Seed;
    type Digest = [u8; 16];
    const DIGEST_LEN: usize = 16;

    fn with_seed(seed: Self::Seed) -> Self {
        let mut md5 = Self {
            inner: Md5Backend::new(),
            pending_seed: None,
        };

        if let Some(value) = seed.value {
            if seed.proper_order {
                // Hash seed BEFORE data (proper order, protocol 30+ with CHECKSUM_SEED_FIX)
                md5.update(&value.to_le_bytes());
            } else {
                // Store seed to hash AFTER data (legacy order)
                md5.pending_seed = Some(value);
            }
        }

        md5
    }

    fn update(&mut self, data: &[u8]) {
        self.update(data);
    }

    fn finalize(mut self) -> Self::Digest {
        // If we have a pending seed (legacy order), hash it AFTER the data
        if let Some(seed) = self.pending_seed {
            self.update(&seed.to_le_bytes());
        }

        // Now finalize the hash
        match self.inner {
            #[cfg(feature = "openssl")]
            Md5Backend::OpenSsl(mut hasher) => {
                let mut output = [0_u8; 16];
                let bytes = hasher.finish().expect("OpenSSL MD5 finalisation failed");
                output.copy_from_slice(bytes.as_ref());
                output
            }
            Md5Backend::Rust(hasher) => hasher.finalize().into(),
        }
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

    #[test]
    fn md5_seeded_proper_order_hashes_seed_before_data() {
        // Proper order: hash seed bytes THEN data bytes
        let seed_value: i32 = 0x12345678;
        let data = b"test data";

        // Hash with seed in proper order
        let mut with_seed = Md5::with_seed(Md5Seed::proper(seed_value));
        with_seed.update(data);
        let seeded_digest = with_seed.finalize();

        // Manual construction: hash seed, then data
        let mut manual = Md5::new();
        manual.update(&seed_value.to_le_bytes());
        manual.update(data);
        let manual_digest = manual.finalize();

        assert_eq!(
            to_hex(&seeded_digest),
            to_hex(&manual_digest),
            "Proper order should hash seed before data"
        );

        // Should NOT match unseeded hash
        let unseeded_digest = Md5::digest(data);
        assert_ne!(
            to_hex(&seeded_digest),
            to_hex(&unseeded_digest),
            "Seeded hash should differ from unseeded"
        );
    }

    #[test]
    fn md5_seeded_legacy_order_hashes_seed_after_data() {
        // Legacy order: hash data bytes THEN seed bytes
        let seed_value: i32 = 0x12345678;
        let data = b"test data";

        // Hash with seed in legacy order
        let mut with_seed = Md5::with_seed(Md5Seed::legacy(seed_value));
        with_seed.update(data);
        let seeded_digest = with_seed.finalize();

        // Manual construction: hash data, then seed
        let mut manual = Md5::new();
        manual.update(data);
        manual.update(&seed_value.to_le_bytes());
        let manual_digest = manual.finalize();

        assert_eq!(
            to_hex(&seeded_digest),
            to_hex(&manual_digest),
            "Legacy order should hash seed after data"
        );
    }

    #[test]
    fn md5_seed_ordering_produces_different_hashes() {
        // Verify that proper vs legacy ordering produce different results
        let seed_value: i32 = 0x1BCDEF01u32 as i32;
        let data = b"same data for both";

        let mut proper = Md5::with_seed(Md5Seed::proper(seed_value));
        proper.update(data);
        let proper_digest = proper.finalize();

        let mut legacy = Md5::with_seed(Md5Seed::legacy(seed_value));
        legacy.update(data);
        let legacy_digest = legacy.finalize();

        assert_ne!(
            to_hex(&proper_digest),
            to_hex(&legacy_digest),
            "Proper and legacy order should produce different hashes"
        );
    }

    #[test]
    fn md5_no_seed_matches_unseeded_hash() {
        let data = b"unseeded test";

        let mut with_none_seed = Md5::with_seed(Md5Seed::none());
        with_none_seed.update(data);
        let none_seeded = with_none_seed.finalize();

        let unseeded = Md5::digest(data);

        assert_eq!(
            to_hex(&none_seeded),
            to_hex(&unseeded),
            "Md5Seed::none() should match unseeded hash"
        );
    }

    #[test]
    fn md5_default_seed_is_none() {
        let data = b"default test";

        let mut with_default = Md5::with_seed(Md5Seed::default());
        with_default.update(data);
        let default_digest = with_default.finalize();

        let unseeded = Md5::digest(data);

        assert_eq!(
            to_hex(&default_digest),
            to_hex(&unseeded),
            "Default seed should match unseeded hash"
        );
    }
}
