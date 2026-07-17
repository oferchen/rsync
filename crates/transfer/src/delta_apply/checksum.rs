//! Checksum verification for delta transfer integrity.
//!
//! upstream: `receiver.c` end-of-file digest verification.

use checksums::strong::{Md4, Md5, Sha1, StrongDigest, Xxh3, Xxh3_128, Xxh64};
use protocol::{ChecksumAlgorithm, CompatibilityFlags, NegotiationResult, ProtocolVersion};

/// Whole-file checksum verifier with enum dispatch for zero-allocation runtime
/// algorithm selection.
///
/// Mirrors upstream rsync's whole-file checksum verification in `receiver.c`.
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
            .map(|n| Self::for_algorithm_seeded(n.checksum, seed, protocol))
            .unwrap_or_else(|| {
                if protocol.uses_varint_encoding() {
                    Self::Md5(Md5::new())
                } else {
                    // upstream: checksum.c:125 - protocol >= 27 uses CSUM_MD4_OLD
                    // which prepends the 4-byte seed before file data.
                    Self::for_algorithm_seeded(ChecksumAlgorithm::MD4, seed, protocol)
                }
            })
    }

    /// Creates a verifier for a specific algorithm, gating the seed prepend on
    /// the protocol version to match upstream's whole-file `sum_init()`.
    ///
    /// The whole-file end-of-file digest prepends the 4-byte LE seed before the
    /// file data ONLY for the legacy MD4 variants (`CSUM_MD4_OLD`/`BUSTED`/
    /// `ARCHAIC`, protocol below 30). The modern `CSUM_MD4` negotiated at
    /// protocol 30 or newer (e.g. `--checksum-choice=md4`) is unseeded, as are
    /// MD5, SHA1, and the XXH variants. This is distinct from the per-block
    /// `get_checksum2()` path, which appends the seed after the data for all MD4
    /// variants.
    ///
    /// # Upstream Reference
    ///
    /// - `checksum.c:600-611` `sum_init()` - only `CSUM_MD4_OLD`/`BUSTED`/
    ///   `ARCHAIC` (the protocol < 30 forms) call `sum_update(seed, 4)`; the
    ///   modern `CSUM_MD4` and `CSUM_MD5` cases seed nothing.
    #[must_use]
    pub fn for_algorithm_seeded(
        algorithm: ChecksumAlgorithm,
        seed: i32,
        protocol: ProtocolVersion,
    ) -> Self {
        let mut verifier = Self::for_algorithm(algorithm);
        // Only the legacy protocol < 30 MD4 whole-file sum prepends the seed.
        if algorithm == ChecksumAlgorithm::MD4 && !protocol.uses_varint_encoding() {
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

    /// Returns `true` when the verifier is the `None` variant - no whole-file
    /// digest will be computed.
    ///
    /// Used by performance fast paths that can skip the per-byte `update`
    /// callback when no checksum is being accumulated (e.g. the IUD-10
    /// `copy_file_range` delta-apply path).
    #[inline]
    #[must_use]
    pub const fn is_noop(&self) -> bool {
        matches!(self, Self::None)
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

    fn proto(v: u8) -> ProtocolVersion {
        ProtocolVersion::try_from(v).expect("protocol version")
    }

    fn digest(mut v: ChecksumVerifier, data: &[u8]) -> Vec<u8> {
        v.update(data);
        let mut buf = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
        let len = v.finalize_into(&mut buf);
        buf[..len].to_vec()
    }

    /// WHY: the whole-file end-of-file digest must be byte-compatible with a real
    /// upstream 3.4.4 binary. Upstream `checksum.c:600-611` `sum_init()` prepends
    /// the 4-byte LE seed ONLY for the legacy MD4 variants (CSUM_MD4_OLD/BUSTED/
    /// ARCHAIC, protocol < 30). The modern CSUM_MD4 negotiated at protocol >= 30
    /// (e.g. `--checksum-choice=md4`) is unseeded. Seeding it would corrupt the
    /// digest and make every MD4 transfer fail verification against upstream.
    #[test]
    fn md4_seed_prepend_is_gated_on_protocol() {
        let data = b"the quick brown fox";
        let seed: i32 = 0x1234_5678;

        // Reference digests computed with the base (unseeded) verifier.
        let unseeded = digest(
            ChecksumVerifier::for_algorithm(ChecksumAlgorithm::MD4),
            data,
        );
        let seeded = {
            let mut v = ChecksumVerifier::for_algorithm(ChecksumAlgorithm::MD4);
            v.update(&seed.to_le_bytes());
            digest(v, data)
        };
        assert_ne!(
            unseeded, seeded,
            "a non-zero seed must change the MD4 digest"
        );

        // protocol >= 30: modern CSUM_MD4, no seed prepend (upstream sum_init).
        let p32 = ChecksumVerifier::for_algorithm_seeded(ChecksumAlgorithm::MD4, seed, proto(32));
        assert_eq!(
            digest(p32, data),
            unseeded,
            "MD4 at protocol >= 30 must NOT prepend the seed (upstream checksum.c:600)"
        );

        // protocol < 30: legacy CSUM_MD4_OLD, seed prepended before file data.
        let p29 = ChecksumVerifier::for_algorithm_seeded(ChecksumAlgorithm::MD4, seed, proto(29));
        assert_eq!(
            digest(p29, data),
            seeded,
            "MD4 at protocol < 30 must prepend the seed (upstream checksum.c:604-611)"
        );

        // MD5 never seeds via this path, regardless of protocol version.
        let md5_ref = digest(
            ChecksumVerifier::for_algorithm(ChecksumAlgorithm::MD5),
            data,
        );
        for v in [29u8, 32u8] {
            let m = ChecksumVerifier::for_algorithm_seeded(ChecksumAlgorithm::MD5, seed, proto(v));
            assert_eq!(
                digest(m, data),
                md5_ref,
                "MD5 must never be seeded by for_algorithm_seeded (proto {v})"
            );
        }
    }

    #[test]
    fn verifier_sha1_produces_correct_length() {
        let mut v = ChecksumVerifier::for_algorithm(ChecksumAlgorithm::SHA1);
        v.update(b"test");
        let mut buf = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
        assert_eq!(v.finalize_into(&mut buf), 20);
    }
}
