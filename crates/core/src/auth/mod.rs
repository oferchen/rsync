//! Shared helpers for daemon authentication digests.
//!
//! The rsync daemon supports multiple challenge/response hash algorithms that are negotiated via
//! the legacy `@RSYNCD:` greeting. Both the client and daemon use this module to select the
//! strongest mutually supported digest, compute base64-encoded responses, and validate incoming
//! credentials without duplicating algorithm tables across crates.

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
#[must_use]
pub fn verify_daemon_auth_response(secret: &[u8], challenge: &str, response: &str) -> bool {
    digests_for_response(response)
        .iter()
        .filter(|digest| SUPPORTED_DAEMON_DIGESTS.contains(digest))
        .any(|digest| compute_daemon_auth_response(secret, challenge, *digest) == response)
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
}
