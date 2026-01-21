//! Shared helpers for daemon authentication digests.
//!
//! The rsync daemon supports multiple challenge/response hash algorithms that are negotiated via
//! the legacy `@RSYNCD:` greeting. Both the client and daemon use this module to select the
//! strongest mutually supported digest, compute base64-encoded responses, and validate incoming
//! credentials without duplicating algorithm tables across crates.
//!
//! # Security
//!
//! Authentication verification uses constant-time comparison to prevent timing attacks.
//! See `verify_daemon_auth_response` for details.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD_NO_PAD;
use checksums::strong::{Md4, Md5, Sha1, Sha256, Sha512};

/// Digest algorithms supported for daemon challenge/response authentication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DaemonAuthDigest {
    /// SHA-512, the strongest algorithm supported by upstream rsync.
    Sha512,
    /// SHA-256, preferred when SHA-512 is unavailable.
    Sha256,
    /// SHA-1, retained for compatibility with older daemons.
    Sha1,
    /// MD5, the historical default.
    Md5,
    /// MD4, accepted for compatibility with very old clients.
    Md4,
}

impl DaemonAuthDigest {
    /// Returns the canonical token used in daemon greetings for this digest.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Sha512 => "sha512",
            Self::Sha256 => "sha256",
            Self::Sha1 => "sha1",
            Self::Md5 => "md5",
            Self::Md4 => "md4",
        }
    }

    /// Returns the expected length of the base64-encoded digest without padding.
    #[must_use]
    pub const fn base64_len(self) -> usize {
        match self {
            Self::Sha512 => 86,
            Self::Sha256 => 43,
            Self::Sha1 => 27,
            Self::Md5 | Self::Md4 => 22,
        }
    }

    /// Computes the raw digest bytes for the provided secret and challenge.
    fn digest_bytes(self, secret: &[u8], challenge: &[u8]) -> Vec<u8> {
        match self {
            Self::Sha512 => {
                let mut hasher = Sha512::new();
                hasher.update(secret);
                hasher.update(challenge);
                hasher.finalize().to_vec()
            }
            Self::Sha256 => {
                let mut hasher = Sha256::new();
                hasher.update(secret);
                hasher.update(challenge);
                hasher.finalize().to_vec()
            }
            Self::Sha1 => {
                let mut hasher = Sha1::new();
                hasher.update(secret);
                hasher.update(challenge);
                hasher.finalize().to_vec()
            }
            Self::Md5 => {
                let mut hasher = Md5::new();
                hasher.update(secret);
                hasher.update(challenge);
                hasher.finalize().to_vec()
            }
            Self::Md4 => {
                let mut hasher = Md4::new();
                hasher.update(secret);
                hasher.update(challenge);
                hasher.finalize().to_vec()
            }
        }
    }
}

/// Ordered list of authentication digests supported by this implementation.
///
/// The order reflects preference from strongest to weakest.
pub const SUPPORTED_DAEMON_DIGESTS: &[DaemonAuthDigest; 5] = &[
    DaemonAuthDigest::Sha512,
    DaemonAuthDigest::Sha256,
    DaemonAuthDigest::Sha1,
    DaemonAuthDigest::Md5,
    DaemonAuthDigest::Md4,
];

/// Parses the whitespace-separated digest list advertised by a daemon greeting.
#[must_use]
pub fn parse_daemon_digest_list(list: Option<&str>) -> Vec<DaemonAuthDigest> {
    let Some(list) = list else {
        return Vec::new();
    };

    list.split_whitespace()
        .filter_map(|token| match token.to_ascii_lowercase().as_str() {
            "sha512" => Some(DaemonAuthDigest::Sha512),
            "sha256" => Some(DaemonAuthDigest::Sha256),
            "sha1" => Some(DaemonAuthDigest::Sha1),
            "md5" => Some(DaemonAuthDigest::Md5),
            "md4" => Some(DaemonAuthDigest::Md4),
            _ => None,
        })
        .collect()
}

/// Selects the strongest mutually supported digest between the local implementation and the advertised list.
#[must_use]
pub fn select_daemon_digest(advertised: &[DaemonAuthDigest]) -> DaemonAuthDigest {
    for preferred in SUPPORTED_DAEMON_DIGESTS.iter().copied() {
        if advertised.contains(&preferred) {
            return preferred;
        }
    }

    // No explicit match; fall back to the historical default so older daemons without an
    // advertised list continue working.
    DaemonAuthDigest::Md5
}

/// Computes the base64-encoded daemon authentication response using the provided digest.
#[must_use]
pub fn compute_daemon_auth_response(
    secret: &[u8],
    challenge: &str,
    digest: DaemonAuthDigest,
) -> String {
    let bytes = digest.digest_bytes(secret, challenge.as_bytes());
    STANDARD_NO_PAD.encode(bytes)
}

/// Returns the supported digest candidates that match the supplied response length.
#[must_use]
pub const fn digests_for_response(response: &str) -> &'static [DaemonAuthDigest] {
    match response.len() {
        len if len == DaemonAuthDigest::Sha512.base64_len() => SHA512_ONLY,
        len if len == DaemonAuthDigest::Sha256.base64_len() => SHA256_ONLY,
        len if len == DaemonAuthDigest::Sha1.base64_len() => SHA1_ONLY,
        len if len == DaemonAuthDigest::Md5.base64_len() => MD_LEGACY,
        _ => &[],
    }
}

const SHA512_ONLY: &[DaemonAuthDigest] = &[DaemonAuthDigest::Sha512];
const SHA256_ONLY: &[DaemonAuthDigest] = &[DaemonAuthDigest::Sha256];
const SHA1_ONLY: &[DaemonAuthDigest] = &[DaemonAuthDigest::Sha1];
const MD_LEGACY: &[DaemonAuthDigest] = &[DaemonAuthDigest::Md5, DaemonAuthDigest::Md4];

/// Verifies whether the supplied daemon authentication response matches the secret and challenge.
///
/// # Security
///
/// This function uses constant-time comparison to prevent timing attacks. An attacker
/// cannot determine how many bytes of their response matched by measuring response time.
///
/// Reference: Upstream rsync uses `strcmp()` which is timing-vulnerable. This implementation
/// improves upon upstream by using constant-time comparison for cryptographic security.
#[must_use]
pub fn verify_daemon_auth_response(secret: &[u8], challenge: &str, response: &str) -> bool {
    digests_for_response(response)
        .iter()
        .filter(|digest| SUPPORTED_DAEMON_DIGESTS.contains(digest))
        .any(|digest| {
            let expected = compute_daemon_auth_response(secret, challenge, *digest);
            constant_time_eq(expected.as_bytes(), response.as_bytes())
        })
}

/// Compares two byte slices in constant time to prevent timing attacks.
///
/// Returns `true` if and only if the slices are equal. The comparison time
/// depends only on the length of the slices, not their contents.
///
/// # Implementation
///
/// Uses XOR accumulation to compare all bytes regardless of early differences.
/// The use of `wrapping_sub` and bitwise operations ensures no short-circuit
/// evaluation occurs, making the comparison constant-time.
#[must_use]
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }

    // XOR all bytes together - any difference sets bits in the accumulator.
    // Using fold ensures all iterations happen regardless of intermediate values.
    let diff = a
        .iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y));

    // Convert to bool without branching on intermediate values.
    // wrapping_sub(1) on 0 gives 255 (0xFF), on any non-zero gives < 255.
    // Right-shifting by 8 bits gives 0 for 0xFF and 0 for anything else that
    // doesn't overflow. Instead, we use the fact that 0u8 == 0 is true.
    // The fold above ensures constant-time iteration.
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advertised_list_preserves_order_and_filters_unknown() {
        let list = parse_daemon_digest_list(Some("sha512 sponge sha1 md5"));
        assert_eq!(
            list,
            [
                DaemonAuthDigest::Sha512,
                DaemonAuthDigest::Sha1,
                DaemonAuthDigest::Md5
            ]
        );
    }

    #[test]
    fn selection_prefers_strongest_supported_digest() {
        let digests = [
            DaemonAuthDigest::Md5,
            DaemonAuthDigest::Sha1,
            DaemonAuthDigest::Sha256,
        ];
        assert_eq!(select_daemon_digest(&digests), DaemonAuthDigest::Sha256);
    }

    #[test]
    fn selection_falls_back_to_md5_when_list_empty() {
        assert_eq!(select_daemon_digest(&[]), DaemonAuthDigest::Md5);
    }

    #[test]
    fn compute_and_verify_round_trip_for_sha512() {
        let secret = b"secret";
        let challenge = "challenge";
        let response = compute_daemon_auth_response(secret, challenge, DaemonAuthDigest::Sha512);
        assert!(verify_daemon_auth_response(secret, challenge, &response));
    }

    #[test]
    fn digests_for_response_disambiguates_md4_and_md5() {
        let len = DaemonAuthDigest::Md5.base64_len();
        assert_eq!(digests_for_response(&"A".repeat(len)), MD_LEGACY);
    }

    // Tests for constant_time_eq

    #[test]
    fn constant_time_eq_returns_true_for_equal_slices() {
        assert!(constant_time_eq(b"hello", b"hello"));
        assert!(constant_time_eq(b"", b""));
        assert!(constant_time_eq(b"\x00\x00\x00", b"\x00\x00\x00"));
    }

    #[test]
    fn constant_time_eq_returns_false_for_unequal_slices() {
        assert!(!constant_time_eq(b"hello", b"world"));
        assert!(!constant_time_eq(b"hello", b"hellO"));
        assert!(!constant_time_eq(b"abc", b"abd"));
    }

    #[test]
    fn constant_time_eq_returns_false_for_different_lengths() {
        assert!(!constant_time_eq(b"hello", b"hell"));
        assert!(!constant_time_eq(b"hi", b"hello"));
        assert!(!constant_time_eq(b"", b"a"));
    }

    #[test]
    fn constant_time_eq_handles_single_byte_difference() {
        // Test that even a single bit difference is detected
        assert!(!constant_time_eq(b"\x00", b"\x01"));
        assert!(!constant_time_eq(b"\xff", b"\xfe"));
    }

    #[test]
    fn verify_rejects_wrong_response() {
        let secret = b"mysecret";
        let challenge = "mychallenge";
        let correct = compute_daemon_auth_response(secret, challenge, DaemonAuthDigest::Sha256);

        // Tamper with the response
        let mut tampered = correct.clone();
        if let Some(c) = tampered.pop() {
            tampered.push(if c == 'A' { 'B' } else { 'A' });
        }

        assert!(verify_daemon_auth_response(secret, challenge, &correct));
        assert!(!verify_daemon_auth_response(secret, challenge, &tampered));
    }

    #[test]
    fn verify_rejects_empty_response() {
        let secret = b"secret";
        let challenge = "challenge";
        assert!(!verify_daemon_auth_response(secret, challenge, ""));
    }

    #[test]
    fn verify_rejects_wrong_length_response() {
        let secret = b"secret";
        let challenge = "challenge";
        // Response length doesn't match any known digest
        assert!(!verify_daemon_auth_response(secret, challenge, "tooshort"));
        assert!(!verify_daemon_auth_response(
            secret,
            challenge,
            &"A".repeat(100)
        ));
    }
}
