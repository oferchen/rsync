//! Checksum verification for delta transfer integrity.
//!
//! Uses enum dispatch for zero-allocation runtime algorithm selection.
//! Mirrors upstream rsync's checksum verification in `receiver.c`.

use checksums::strong::{Md4, Md5, Sha1, StrongDigest, Xxh3, Xxh3_128, Xxh64};
use protocol::{ChecksumAlgorithm, CompatibilityFlags, NegotiationResult, ProtocolVersion};

/// Checksum verifier for delta transfer integrity verification.
///
/// Uses enum dispatch for zero-allocation runtime algorithm selection.
/// Mirrors upstream rsync's checksum verification in `receiver.c`.
pub enum ChecksumVerifier {
    /// No checksum - `CSUM_NONE` negotiated. 1-byte placeholder digest
    /// matching upstream `receiver.c` behaviour.
    None,
    /// MD4 checksum (legacy, protocol < 30).
    Md4(Md4),
    /// MD5 checksum (protocol 30+ default).
    Md5(Md5),
    /// SHA1 checksum.
    Sha1(Sha1),
    /// XXH64 checksum (fast non-cryptographic).
    Xxh64(Xxh64),
    /// XXH3 64-bit checksum (fastest non-cryptographic).
    Xxh3(Xxh3),
    /// XXH3 128-bit checksum.
    Xxh128(Xxh3_128),
}

impl ChecksumVerifier {
    /// Creates a verifier based on negotiated parameters.
    ///
    /// For protocol < 30 (no binary negotiation), the verifier uses MD4 with
    /// the checksum seed prepended as the first 4 bytes of input - matching
    /// upstream's `CSUM_MD4_OLD` behaviour in `checksum.c:605-612`.
    /// Protocol 30+ uses MD5 (no seed) or negotiated algorithms.
    ///
    /// # Upstream Reference
    ///
    /// - `checksum.c:559-620` - `sum_init()` seeds legacy MD4 variants
    /// - `checksum.c:125-126` - protocol 27-29 uses `CSUM_MD4_OLD`
    #[must_use]
    pub fn new(
        negotiated: Option<&NegotiationResult>,
        protocol: ProtocolVersion,
        seed: i32,
        _compat_flags: Option<&CompatibilityFlags>,
    ) -> Self {
        negotiated
            .map(|n| Self::for_algorithm(n.checksum))
            .unwrap_or_else(|| {
                if protocol.uses_varint_encoding() {
                    Self::Md5(Md5::new())
                } else {
                    // upstream: checksum.c:125 - protocol >= 27 uses CSUM_MD4_OLD
                    // which prepends the 4-byte seed before file data.
                    let mut verifier = Self::Md4(Md4::new());
                    verifier.update(&seed.to_le_bytes());
                    verifier
                }
            })
    }

    /// Creates a verifier for a specific algorithm with seed prepended.
    ///
    /// For legacy MD4 (protocol < 30), the seed must be prepended to match
    /// upstream `CSUM_MD4_OLD`. For all other algorithms, the seed is not
    /// prepended - matching upstream `sum_init()` which only seeds the
    /// legacy MD4 variants.
    ///
    /// # Upstream Reference
    ///
    /// - `checksum.c:605-612` - only `CSUM_MD4_OLD`/`BUSTED`/`ARCHAIC` prepend seed
    #[must_use]
    pub fn for_algorithm_seeded(algorithm: ChecksumAlgorithm, seed: i32) -> Self {
        let mut verifier = Self::for_algorithm(algorithm);
        if algorithm == ChecksumAlgorithm::MD4 {
            verifier.update(&seed.to_le_bytes());
        }
        verifier
    }

    /// Creates a verifier for a specific algorithm.
    #[must_use]
    pub fn for_algorithm(algorithm: ChecksumAlgorithm) -> Self {
        match algorithm {
            ChecksumAlgorithm::None => Self::None,
            ChecksumAlgorithm::MD4 => Self::Md4(Md4::new()),
            ChecksumAlgorithm::MD5 => Self::Md5(Md5::new()),
            ChecksumAlgorithm::SHA1 => Self::Sha1(Sha1::new()),
            ChecksumAlgorithm::XXH64 => Self::Xxh64(Xxh64::with_seed(0)),
            ChecksumAlgorithm::XXH3 => Self::Xxh3(Xxh3::with_seed(0)),
            ChecksumAlgorithm::XXH128 => Self::Xxh128(Xxh3_128::with_seed(0)),
        }
    }

    /// Returns the checksum algorithm used by this verifier.
    #[inline]
    #[must_use]
    pub const fn algorithm(&self) -> ChecksumAlgorithm {
        match self {
            Self::None => ChecksumAlgorithm::None,
            Self::Md4(_) => ChecksumAlgorithm::MD4,
            Self::Md5(_) => ChecksumAlgorithm::MD5,
            Self::Sha1(_) => ChecksumAlgorithm::SHA1,
            Self::Xxh64(_) => ChecksumAlgorithm::XXH64,
            Self::Xxh3(_) => ChecksumAlgorithm::XXH3,
            Self::Xxh128(_) => ChecksumAlgorithm::XXH128,
        }
    }

    /// Updates the hasher with data.
    #[inline]
    pub fn update(&mut self, data: &[u8]) {
        match self {
            Self::None => {}
            Self::Md4(h) => h.update(data),
            Self::Md5(h) => h.update(data),
            Self::Sha1(h) => h.update(data),
            Self::Xxh64(h) => h.update(data),
            Self::Xxh3(h) => h.update(data),
            Self::Xxh128(h) => h.update(data),
        }
    }

    /// Maximum digest length across all supported algorithms (SHA1 = 20 bytes).
    pub const MAX_DIGEST_LEN: usize = 20;

    /// Returns the digest length for the current algorithm.
    #[inline]
    #[must_use]
    pub const fn digest_len(&self) -> usize {
        match self {
            Self::None => 1,
            Self::Md4(_) | Self::Md5(_) | Self::Xxh128(_) => 16,
            Self::Sha1(_) => 20,
            Self::Xxh64(_) | Self::Xxh3(_) => 8,
        }
    }

    /// Finalizes the digest into a caller-provided stack buffer.
    ///
    /// Returns the number of bytes written (equals `digest_len()`).
    /// Avoids heap allocation, suitable for hot paths.
    #[inline]
    pub fn finalize_into(self, buf: &mut [u8; Self::MAX_DIGEST_LEN]) -> usize {
        let len = self.digest_len();
        match self {
            Self::None => buf[0] = 0,
            Self::Md4(h) => buf[..len].copy_from_slice(h.finalize().as_ref()),
            Self::Md5(h) => buf[..len].copy_from_slice(h.finalize().as_ref()),
            Self::Sha1(h) => buf[..len].copy_from_slice(h.finalize().as_ref()),
            Self::Xxh64(h) => buf[..len].copy_from_slice(h.finalize().as_ref()),
            Self::Xxh3(h) => buf[..len].copy_from_slice(h.finalize().as_ref()),
            Self::Xxh128(h) => buf[..len].copy_from_slice(h.finalize().as_ref()),
        }
        len
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifier_digest_lengths() {
        assert_eq!(
            ChecksumVerifier::for_algorithm(ChecksumAlgorithm::MD4).digest_len(),
            16
        );
        assert_eq!(
            ChecksumVerifier::for_algorithm(ChecksumAlgorithm::MD5).digest_len(),
            16
        );
        assert_eq!(
            ChecksumVerifier::for_algorithm(ChecksumAlgorithm::SHA1).digest_len(),
            20
        );
        assert_eq!(
            ChecksumVerifier::for_algorithm(ChecksumAlgorithm::XXH64).digest_len(),
            8
        );
        assert_eq!(
            ChecksumVerifier::for_algorithm(ChecksumAlgorithm::XXH3).digest_len(),
            8
        );
        assert_eq!(
            ChecksumVerifier::for_algorithm(ChecksumAlgorithm::XXH128).digest_len(),
            16
        );
    }

    #[test]
    fn verifier_update_and_finalize() {
        let mut v = ChecksumVerifier::for_algorithm(ChecksumAlgorithm::MD5);
        v.update(b"hello");
        v.update(b" world");
        let mut buf = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
        assert_eq!(v.finalize_into(&mut buf), 16);
    }

    #[test]
    fn verifier_protocol_defaults() {
        let v29 = ChecksumVerifier::new(None, ProtocolVersion::try_from(29u8).unwrap(), 0, None);
        assert_eq!(v29.digest_len(), 16); // MD4

        let v30 = ChecksumVerifier::new(None, ProtocolVersion::try_from(30u8).unwrap(), 0, None);
        assert_eq!(v30.digest_len(), 16); // MD5
    }

    #[test]
    fn verifier_for_algorithm_none_is_noop() {
        let v = ChecksumVerifier::for_algorithm(ChecksumAlgorithm::None);
        assert_eq!(v.algorithm(), ChecksumAlgorithm::None);
        assert_eq!(v.digest_len(), 1); // 1-byte placeholder per upstream
        let mut buf = [0xFFu8; ChecksumVerifier::MAX_DIGEST_LEN];
        let len = v.finalize_into(&mut buf);
        assert_eq!(len, 1);
        assert_eq!(buf[0], 0);
    }

    #[test]
    fn verifier_update_empty_data() {
        let mut v = ChecksumVerifier::for_algorithm(ChecksumAlgorithm::MD5);
        v.update(&[]);
        let mut buf = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
        // Should still produce valid MD5 (of empty string)
        assert_eq!(v.finalize_into(&mut buf), 16);
    }

    #[test]
    fn verifier_xxh64_produces_correct_length() {
        let mut v = ChecksumVerifier::for_algorithm(ChecksumAlgorithm::XXH64);
        v.update(b"test");
        let mut buf = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
        assert_eq!(v.finalize_into(&mut buf), 8);
    }

    #[test]
    fn verifier_xxh3_produces_correct_length() {
        let mut v = ChecksumVerifier::for_algorithm(ChecksumAlgorithm::XXH3);
        v.update(b"test");
        let mut buf = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
        assert_eq!(v.finalize_into(&mut buf), 8);
    }

    #[test]
    fn verifier_xxh128_produces_correct_length() {
        let mut v = ChecksumVerifier::for_algorithm(ChecksumAlgorithm::XXH128);
        v.update(b"test");
        let mut buf = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
        assert_eq!(v.finalize_into(&mut buf), 16);
    }

    #[test]
    fn verifier_sha1_produces_correct_length() {
        let mut v = ChecksumVerifier::for_algorithm(ChecksumAlgorithm::SHA1);
        v.update(b"test");
        let mut buf = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
        assert_eq!(v.finalize_into(&mut buf), 20);
    }
}
