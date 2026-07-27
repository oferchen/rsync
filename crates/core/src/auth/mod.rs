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
use protocol::ProtocolVersion;

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
    /// MD4 seeded with four zero bytes, upstream's `CSUM_MD4_OLD`.
    ///
    /// Not an advertisable name: upstream's table carries a single `md4` entry
    /// and `negotiate_daemon_auth()` rewrites the *negotiated* item to
    /// `CSUM_MD4_OLD` (compat.c:879-881) only when the peer sent no digest list
    /// and the protocol-keyed fallback landed on `md4`. `sum_init()`
    /// (checksum.c:604-610) then prefixes the seed as four bytes - zero here,
    /// because both `gen_challenge()` and `generate_hash()` pass seed `0` - which
    /// an explicitly negotiated `md4` does not get.
    Md4Old,
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
            // upstream keeps `nni->name` as "md4" when it rewrites `num` to
            // CSUM_MD4_OLD, so both spell the same wire token.
            Self::Md4 | Self::Md4Old => "md4",
        }
    }

    /// Returns the expected length of the base64-encoded digest without padding.
    #[must_use]
    pub const fn base64_len(self) -> usize {
        match self {
            Self::Sha512 => 86,
            Self::Sha256 => 43,
            Self::Sha1 => 27,
            Self::Md5 | Self::Md4 | Self::Md4Old => 22,
        }
    }

    /// Computes the raw digest bytes over the concatenation of `parts`.
    fn digest_bytes(self, parts: &[&[u8]]) -> Vec<u8> {
        macro_rules! digest {
            ($hasher:ty) => {{
                let mut hasher = <$hasher>::new();
                for part in parts {
                    hasher.update(part);
                }
                hasher.finalize().to_vec()
            }};
        }

        match self {
            Self::Sha512 => digest!(Sha512),
            Self::Sha256 => digest!(Sha256),
            Self::Sha1 => digest!(Sha1),
            Self::Md5 => digest!(Md5),
            Self::Md4 => digest!(Md4),
            // upstream: checksum.c:604-610 - the MD4_OLD family seeds the digest
            // with `SIVAL(s, 0, seed); sum_update(s, 4)`. Daemon auth always
            // passes seed 0 (authenticate.c:76, :90), so the prefix is four zero
            // bytes.
            Self::Md4Old => {
                let mut hasher = Md4::new();
                hasher.update(&[0u8; 4]);
                for part in parts {
                    hasher.update(part);
                }
                hasher.finalize().to_vec()
            }
        }
    }

    /// Returns the unpadded base64 encoding of this digest over `parts`.
    ///
    /// upstream: authenticate.c:80,95 - both `gen_challenge()` and
    /// `generate_hash()` finish with `base64_encode(digest, len, out, 0)`, whose
    /// trailing `0` suppresses `=` padding.
    #[must_use]
    pub fn base64_digest(self, parts: &[&[u8]]) -> String {
        STANDARD_NO_PAD.encode(self.digest_bytes(parts))
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

/// Returns the authentication digests appropriate for the given protocol version.
///
/// upstream: csprotocol.txt - digest negotiation rules depend on protocol era:
///
/// - **Protocol >= 31**: all digests (SHA-512, SHA-256, SHA-1, MD5, MD4).
///   Rsync 3.2.7 introduced the digest list in the greeting for protocol 31+.
/// - **Protocol 30**: MD5 and MD4. SHA-family digests were not available before
///   protocol 31.
/// - **Protocol < 30**: MD4 only. MD5 was introduced in protocol 30.
#[must_use]
pub fn digests_for_protocol(version: ProtocolVersion) -> &'static [DaemonAuthDigest] {
    let v = version.as_u8();
    if v >= 31 {
        SUPPORTED_DAEMON_DIGESTS
    } else if v >= 30 {
        PROTOCOL_30_DIGESTS
    } else {
        LEGACY_DIGESTS
    }
}

/// Digests available for protocol 30 (MD5 + MD4).
const PROTOCOL_30_DIGESTS: &[DaemonAuthDigest] = &[DaemonAuthDigest::Md5, DaemonAuthDigest::Md4];

/// Digests available for protocols before 30 (MD4 only).
const LEGACY_DIGESTS: &[DaemonAuthDigest] = &[DaemonAuthDigest::Md4];

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
///
/// When no advertised digest matches, falls back based on `protocol_version`:
/// MD5 for protocol >= 30, MD4 for protocol < 30.
///
/// upstream: compat.c:858 - `protocol_version >= 30 ? "md5" : "md4"`
#[must_use]
pub fn select_daemon_digest(
    advertised: &[DaemonAuthDigest],
    protocol_version: u8,
) -> DaemonAuthDigest {
    for preferred in SUPPORTED_DAEMON_DIGESTS.iter().copied() {
        if advertised.contains(&preferred) {
            return preferred;
        }
    }

    // upstream: compat.c:858 - when no daemon_auth_choices are configured the default
    // digest depends on the negotiated protocol version.
    default_legacy_digest(protocol_version)
}

/// Returns the digest to use when the peer advertised no list at all.
///
/// upstream: compat.c:860 - the substitute list is
/// `protocol_version >= 30 ? "md5" : "md4"`, and because that branch also sets
/// `md4_is_old`, the `md4` outcome is rewritten to `CSUM_MD4_OLD`
/// (compat.c:879-881). An `md4` the peer named explicitly stays plain `CSUM_MD4`.
#[must_use]
pub const fn default_legacy_digest(protocol_version: u8) -> DaemonAuthDigest {
    if protocol_version >= 30 {
        DaemonAuthDigest::Md5
    } else {
        DaemonAuthDigest::Md4Old
    }
}

/// Computes the base64-encoded daemon authentication response using the provided digest.
///
/// upstream: authenticate.c:88-96 `generate_hash()` hashes the secret followed by
/// the challenge with the negotiated digest.
#[must_use]
pub fn compute_daemon_auth_response(
    secret: &[u8],
    challenge: &str,
    digest: DaemonAuthDigest,
) -> String {
    digest.base64_digest(&[secret, challenge.as_bytes()])
}

/// Renders the daemon-auth digest list this implementation advertises.
///
/// upstream: compat.c:462 `get_default_nno_list()` walks
/// `valid_auth_checksums_items[]` (checksum.c:71-84) in table order and joins the
/// names with a single space. The list is never filtered by protocol version.
///
/// The names live in `protocol` because the client's `@RSYNCD:` greeting is built
/// there and `protocol` sits below this crate. Deriving the string here instead
/// would give the wire two producers that could drift apart;
/// `advertised_digest_names_match_the_wire_table` pins them together.
#[must_use]
pub fn supported_daemon_digest_list() -> String {
    protocol::daemon_auth_digest_list()
}

/// No digest offered by the client is supported by this implementation.
///
/// upstream: compat.c:871-875 - when `parse_negotiate_str()` finds no mutual
/// choice the daemon writes `@ERROR: your client does not support one of our
/// daemon-auth checksums: <list>` and calls `exit_cleanup(RERR_UNSUPPORTED)`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("your client does not support one of our daemon-auth checksums")]
pub struct NoMutualDaemonAuthDigest;

/// Negotiates the daemon-auth digest from the server's point of view.
///
/// `client_digests` is the raw digest name list captured from the client's
/// `@RSYNCD:` greeting (upstream's `daemon_auth_choices`, clientserver.c:199-211).
///
/// upstream: compat.c:848 `negotiate_daemon_auth(f_out, 0)`. With `am_server`
/// set, `parse_negotiate_str()` (compat.c:333-356) walks the *client's* list and
/// stops at the first name the server also supports - `if (best == 1 || am_server)
/// break;` - so client preference order decides. When the client advertised no
/// list at all (only reachable at protocol <= 31) upstream substitutes
/// `protocol_version >= 30 ? "md5" : "md4"` (compat.c:860) and negotiates against
/// that, which always succeeds because both names are compiled in.
///
/// # Errors
///
/// Returns [`NoMutualDaemonAuthDigest`] when the client offered a list but none of
/// its names is supported here.
pub fn negotiate_server_daemon_digest(
    client_digests: Option<&str>,
    protocol_version: u8,
) -> Result<DaemonAuthDigest, NoMutualDaemonAuthDigest> {
    let Some(list) = client_digests else {
        return Ok(default_legacy_digest(protocol_version));
    };

    // `parse_daemon_digest_list` drops names this build does not implement while
    // preserving the client's order, so the first survivor is exactly upstream's
    // "first acceptable client choice".
    parse_daemon_digest_list(Some(list))
        .first()
        .copied()
        .ok_or(NoMutualDaemonAuthDigest)
}

/// Verifies a daemon authentication response against the secret and challenge.
///
/// `digest` must be the digest fixed by [`negotiate_server_daemon_digest`]: upstream
/// derives both halves of the exchange from the single `valid_auth_checksums.
/// negotiated_nni` (authenticate.c:76 `gen_challenge`, :90 `generate_hash`), so the
/// challenge and the verification can never disagree. Inferring the algorithm from
/// the response instead would let a client pick which digest it is checked against.
///
/// # Security
///
/// This function uses constant-time comparison to prevent timing attacks. An attacker
/// cannot determine how many bytes of their response matched by measuring response time.
///
/// Reference: Upstream rsync uses `strcmp()` which is timing-vulnerable. This implementation
/// improves upon upstream by using constant-time comparison for cryptographic security.
#[must_use]
pub fn verify_daemon_auth_response(
    secret: &[u8],
    challenge: &str,
    response: &str,
    digest: DaemonAuthDigest,
) -> bool {
    let expected = compute_daemon_auth_response(secret, challenge, digest);
    constant_time_eq(expected.as_bytes(), response.as_bytes())
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

    // Fold over every byte pair so the loop runs to completion regardless of
    // where the first difference occurs; any divergent byte sets bits in the
    // accumulator.
    let diff = a
        .iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y));

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
        assert_eq!(select_daemon_digest(&digests, 31), DaemonAuthDigest::Sha256);
    }

    #[test]
    fn selection_falls_back_to_md5_for_protocol_30_plus() {
        assert_eq!(select_daemon_digest(&[], 30), DaemonAuthDigest::Md5);
        assert_eq!(select_daemon_digest(&[], 31), DaemonAuthDigest::Md5);
        assert_eq!(select_daemon_digest(&[], 32), DaemonAuthDigest::Md5);
    }

    #[test]
    fn selection_falls_back_to_seeded_md4_for_protocol_below_30() {
        // upstream: compat.c:879-881 - the absent-list `md4` fallback is rewritten
        // to CSUM_MD4_OLD, which seeds the digest with four zero bytes.
        assert_eq!(select_daemon_digest(&[], 29), DaemonAuthDigest::Md4Old);
        assert_eq!(select_daemon_digest(&[], 28), DaemonAuthDigest::Md4Old);
    }

    #[test]
    fn seeded_md4_differs_from_plain_md4_and_is_never_advertised() {
        let secret = b"pw";
        let challenge = "challenge";
        assert_ne!(
            compute_daemon_auth_response(secret, challenge, DaemonAuthDigest::Md4),
            compute_daemon_auth_response(secret, challenge, DaemonAuthDigest::Md4Old),
        );
        assert_eq!(DaemonAuthDigest::Md4Old.name(), "md4");
        assert!(!SUPPORTED_DAEMON_DIGESTS.contains(&DaemonAuthDigest::Md4Old));
        // An explicitly named `md4` stays plain, at every protocol version.
        assert_eq!(
            parse_daemon_digest_list(Some("md4")),
            [DaemonAuthDigest::Md4]
        );
        assert_eq!(
            negotiate_server_daemon_digest(Some("md4"), 29),
            Ok(DaemonAuthDigest::Md4)
        );
    }

    #[test]
    fn compute_and_verify_round_trip_for_sha512() {
        let secret = b"secret";
        let challenge = "challenge";
        let response = compute_daemon_auth_response(secret, challenge, DaemonAuthDigest::Sha512);
        assert!(verify_daemon_auth_response(
            secret,
            challenge,
            &response,
            DaemonAuthDigest::Sha512
        ));
    }

    #[test]
    fn advertised_digest_list_matches_upstream_order() {
        assert_eq!(supported_daemon_digest_list(), "sha512 sha256 sha1 md5 md4");
    }

    // The advertised names are spelled in `protocol` so the client greeting can
    // reach them, while negotiation preference lives here in
    // `SUPPORTED_DAEMON_DIGESTS`. The two orderings are the same fact, and a
    // digest added to one but not the other would either be advertised and then
    // refused, or supported and never offered. Neither is detectable from the
    // list string alone, so compare the tables element by element.
    #[test]
    fn advertised_digest_names_match_the_wire_table() {
        let preferred: Vec<&str> = SUPPORTED_DAEMON_DIGESTS
            .iter()
            .map(|digest| digest.name())
            .collect();
        assert_eq!(preferred, protocol::DAEMON_AUTH_DIGEST_NAMES.to_vec());
    }

    #[test]
    fn server_negotiation_takes_the_first_client_choice_it_supports() {
        // upstream: compat.c:354 - `if (best == 1 || am_server) break;`, so the
        // server honours the client's ordering rather than its own preference.
        assert_eq!(
            negotiate_server_daemon_digest(Some("md5 sha512"), 32),
            Ok(DaemonAuthDigest::Md5)
        );
        assert_eq!(
            negotiate_server_daemon_digest(Some("sha1 md5"), 32),
            Ok(DaemonAuthDigest::Sha1)
        );
    }

    #[test]
    fn server_negotiation_skips_unsupported_client_names() {
        assert_eq!(
            negotiate_server_daemon_digest(Some("sponge blake3 sha256 md5"), 32),
            Ok(DaemonAuthDigest::Sha256)
        );
    }

    #[test]
    fn server_negotiation_rejects_a_wholly_foreign_client_list() {
        assert_eq!(
            negotiate_server_daemon_digest(Some("sponge blake3"), 32),
            Err(NoMutualDaemonAuthDigest)
        );
    }

    #[test]
    fn server_negotiation_falls_back_when_the_client_offered_no_list() {
        // upstream: compat.c:860 - the absent-list substitute is protocol-keyed.
        assert_eq!(
            negotiate_server_daemon_digest(None, 31),
            Ok(DaemonAuthDigest::Md5)
        );
        assert_eq!(
            negotiate_server_daemon_digest(None, 29),
            Ok(DaemonAuthDigest::Md4Old)
        );
    }

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
        assert!(!constant_time_eq(b"\x00", b"\x01"));
        assert!(!constant_time_eq(b"\xff", b"\xfe"));
    }

    #[test]
    fn verify_rejects_wrong_response() {
        let secret = b"mysecret";
        let challenge = "mychallenge";
        let correct = compute_daemon_auth_response(secret, challenge, DaemonAuthDigest::Sha256);

        let mut tampered = correct.clone();
        if let Some(c) = tampered.pop() {
            tampered.push(if c == 'A' { 'B' } else { 'A' });
        }

        assert!(verify_daemon_auth_response(
            secret,
            challenge,
            &correct,
            DaemonAuthDigest::Sha256
        ));
        assert!(!verify_daemon_auth_response(
            secret,
            challenge,
            &tampered,
            DaemonAuthDigest::Sha256
        ));
    }

    #[test]
    fn verify_rejects_empty_response() {
        let secret = b"secret";
        let challenge = "challenge";
        assert!(!verify_daemon_auth_response(
            secret,
            challenge,
            "",
            DaemonAuthDigest::Md5
        ));
    }

    #[test]
    fn verify_rejects_wrong_length_response() {
        let secret = b"secret";
        let challenge = "challenge";
        assert!(!verify_daemon_auth_response(
            secret,
            challenge,
            "tooshort",
            DaemonAuthDigest::Md5
        ));
        assert!(!verify_daemon_auth_response(
            secret,
            challenge,
            &"A".repeat(100),
            DaemonAuthDigest::Md5,
        ));
    }

    #[test]
    fn verify_is_pinned_to_the_negotiated_digest() {
        // The response length no longer selects the algorithm: a client that
        // negotiated SHA-512 cannot be verified with anything else, and a
        // 22-character MD4 response is not accepted where MD5 was negotiated
        // even though the two share a length.
        let secret = b"test_secret";
        let challenge = "test_challenge";

        for negotiated in *SUPPORTED_DAEMON_DIGESTS {
            let response = compute_daemon_auth_response(secret, challenge, negotiated);
            assert!(verify_daemon_auth_response(
                secret, challenge, &response, negotiated
            ));

            for other in SUPPORTED_DAEMON_DIGESTS.iter().copied() {
                assert_eq!(
                    verify_daemon_auth_response(secret, challenge, &response, other),
                    other == negotiated,
                    "a {} response must not verify under {}",
                    negotiated.name(),
                    other.name(),
                );
            }
        }
    }

    #[test]
    fn digests_for_protocol_32_returns_all() {
        let digests = digests_for_protocol(ProtocolVersion::V32);
        assert_eq!(digests.len(), 5);
        assert_eq!(digests[0], DaemonAuthDigest::Sha512);
        assert_eq!(digests[4], DaemonAuthDigest::Md4);
    }

    #[test]
    fn digests_for_protocol_31_returns_all() {
        let digests = digests_for_protocol(ProtocolVersion::V31);
        assert_eq!(digests.len(), 5);
        assert_eq!(digests[0], DaemonAuthDigest::Sha512);
    }

    #[test]
    fn digests_for_protocol_30_returns_md5_and_md4() {
        let digests = digests_for_protocol(ProtocolVersion::V30);
        assert_eq!(digests.len(), 2);
        assert_eq!(digests[0], DaemonAuthDigest::Md5);
        assert_eq!(digests[1], DaemonAuthDigest::Md4);
    }

    #[test]
    fn digests_for_protocol_29_returns_md4_only() {
        let digests = digests_for_protocol(ProtocolVersion::V29);
        assert_eq!(digests.len(), 1);
        assert_eq!(digests[0], DaemonAuthDigest::Md4);
    }

    #[test]
    fn digests_for_protocol_28_returns_md4_only() {
        let digests = digests_for_protocol(ProtocolVersion::V28);
        assert_eq!(digests.len(), 1);
        assert_eq!(digests[0], DaemonAuthDigest::Md4);
    }
}
