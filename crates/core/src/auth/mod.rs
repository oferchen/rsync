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
use protocol::AdvertisedDigests;

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

/// Parses the whitespace-separated digest list advertised in a greeting.
///
/// Names this build does not implement are dropped while the peer's ordering is
/// preserved, mirroring how `parse_negotiate_str()` (compat.c:333-356) skips
/// entries `get_nni_by_name()` cannot resolve.
#[must_use]
pub fn parse_daemon_digest_list(list: &str) -> Vec<DaemonAuthDigest> {
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

/// Negotiates the daemon-auth digest from the client's point of view.
///
/// `server_digests` is the list the daemon advertised in its `@RSYNCD:`
/// greeting (upstream's `daemon_auth_choices`, clientserver.c:199-203).
///
/// upstream: compat.c:848 `negotiate_daemon_auth(f_out, 1)` from
/// `auth_client()`. With `am_server == 0`, `parse_negotiate_str()`
/// (compat.c:333-356) walks the *server's* list to completion and keeps the
/// entry with the lowest ordinal in *our* table, so our preference order
/// decides. An absent list is replaced by `protocol_version >= 30 ? "md5" :
/// "md4"` (compat.c:859-862), which always negotiates.
///
/// # Errors
///
/// Returns [`NoMutualDaemonAuthDigest`] when the server advertised a list -
/// empty included - that names no digest this build implements. Upstream aborts
/// there rather than substituting a default: `recv_negotiate_str()` reports
/// `Failed to negotiate a daemon auth checksum choice.` and calls
/// `exit_cleanup(RERR_UNSUPPORTED)` (compat.c:383-406).
pub fn negotiate_client_daemon_digest(
    server_digests: AdvertisedDigests<'_>,
    protocol_version: u8,
) -> Result<DaemonAuthDigest, NoMutualDaemonAuthDigest> {
    let Some(list) = server_digests.names() else {
        return Ok(default_legacy_digest(protocol_version));
    };

    let advertised = parse_daemon_digest_list(list);
    SUPPORTED_DAEMON_DIGESTS
        .iter()
        .copied()
        .find(|preferred| advertised.contains(preferred))
        .ok_or(NoMutualDaemonAuthDigest)
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
/// This is the *only* advertised-list producer: the `@RSYNCD:` greeting, the
/// daemon's refusal line, and the client's abort diagnostic all render it.
///
/// upstream: compat.c:462 `get_default_nno_list()` walks
/// `valid_auth_checksums_items[]` (checksum.c:71-84) in table order and joins the
/// names with a single space. The list is never filtered by protocol version -
/// `output_daemon_greeting()` (compat.c:838-842) emits it verbatim, so an
/// rsync 3.4.4 daemon forced to `--protocol=28` still greets with
/// `@RSYNCD: 28.0 sha512 sha256 sha1 md5 md4`.
///
/// The names live in `protocol` because the client's `@RSYNCD:` greeting is built
/// there and `protocol` sits below this crate. Deriving the string here instead
/// would give the wire two producers that could drift apart;
/// `advertised_digest_names_match_the_wire_table` pins them together.
#[must_use]
pub fn supported_daemon_digest_list() -> String {
    protocol::daemon_auth_digest_list()
}

/// The peer advertised a digest list naming nothing this build implements.
///
/// Both roles treat this as fatal with `RERR_UNSUPPORTED`, differing only in the
/// diagnostic: the daemon writes `@ERROR: your client does not support one of
/// our daemon-auth checksums: <list>` (compat.c:871-875) and the client prints
/// `Failed to negotiate a daemon auth checksum choice.` (compat.c:383-406).
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("failed to negotiate a daemon auth checksum choice")]
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
/// An *empty* advertised list is not the same as an absent one: upstream keeps
/// `strdup("")` non-NULL, skips the substitution, and walks a list that matches
/// nothing. Hence [`AdvertisedDigests`] rather than an `Option<&str>` whose
/// `Some("")` reads like `None`.
///
/// # Errors
///
/// Returns [`NoMutualDaemonAuthDigest`] when the client offered a list - empty
/// included - none of whose names is supported here.
pub fn negotiate_server_daemon_digest(
    client_digests: AdvertisedDigests<'_>,
    protocol_version: u8,
) -> Result<DaemonAuthDigest, NoMutualDaemonAuthDigest> {
    let Some(list) = client_digests.names() else {
        return Ok(default_legacy_digest(protocol_version));
    };

    // `parse_daemon_digest_list` drops names this build does not implement while
    // preserving the client's order, so the first survivor is exactly upstream's
    // "first acceptable client choice".
    parse_daemon_digest_list(list)
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
        let list = parse_daemon_digest_list("sha512 sponge sha1 md5");
        assert_eq!(
            list,
            [
                DaemonAuthDigest::Sha512,
                DaemonAuthDigest::Sha1,
                DaemonAuthDigest::Md5
            ]
        );
    }

    // upstream: compat.c:333-356 with `am_server == 0` - the client scans the
    // whole server list and keeps the entry ranked highest in OUR table, so our
    // preference decides rather than the server's ordering.
    #[test]
    fn client_negotiation_prefers_our_strongest_mutual_digest() {
        assert_eq!(
            negotiate_client_daemon_digest(AdvertisedDigests::Present("md5 sha1 sha256"), 31),
            Ok(DaemonAuthDigest::Sha256)
        );
    }

    // upstream: compat.c:859-862 - only an ABSENT list is substituted, and the
    // substitute is protocol-keyed. `md4_is_old` is set on that path, so the
    // pre-protocol-30 outcome is the seeded CSUM_MD4_OLD (compat.c:879-881).
    #[test]
    fn client_negotiation_substitutes_only_for_an_absent_list() {
        assert_eq!(
            negotiate_client_daemon_digest(AdvertisedDigests::Absent, 30),
            Ok(DaemonAuthDigest::Md5)
        );
        assert_eq!(
            negotiate_client_daemon_digest(AdvertisedDigests::Absent, 32),
            Ok(DaemonAuthDigest::Md5)
        );
        assert_eq!(
            negotiate_client_daemon_digest(AdvertisedDigests::Absent, 29),
            Ok(DaemonAuthDigest::Md4Old)
        );
        assert_eq!(
            negotiate_client_daemon_digest(AdvertisedDigests::Absent, 28),
            Ok(DaemonAuthDigest::Md4Old)
        );
    }

    // upstream: compat.c:383-406 - a server list with no mutual name makes the
    // client ABORT with RERR_UNSUPPORTED. Falling back to the protocol-keyed
    // default would send a hash upstream never sends, and at protocol < 30 that
    // hash is the seeded CSUM_MD4_OLD. Verified against rsync 3.4.4, which
    // answers `Failed to negotiate a daemon auth checksum choice.` + exit 4.
    #[test]
    fn client_negotiation_aborts_when_the_server_list_has_no_mutual_name() {
        for protocol in [28u8, 29, 30, 31, 32] {
            assert_eq!(
                negotiate_client_daemon_digest(
                    AdvertisedDigests::Present("bogus1 bogus2"),
                    protocol
                ),
                Err(NoMutualDaemonAuthDigest),
                "protocol {protocol} must abort rather than fall back",
            );
            assert_eq!(
                negotiate_client_daemon_digest(AdvertisedDigests::Present(""), protocol),
                Err(NoMutualDaemonAuthDigest),
                "protocol {protocol} must abort on an empty server list",
            );
        }
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
        assert_eq!(parse_daemon_digest_list("md4"), [DaemonAuthDigest::Md4]);
        assert_eq!(
            negotiate_server_daemon_digest(AdvertisedDigests::Present("md4"), 29),
            Ok(DaemonAuthDigest::Md4)
        );
        assert_eq!(
            negotiate_client_daemon_digest(AdvertisedDigests::Present("md4"), 29),
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
            negotiate_server_daemon_digest(AdvertisedDigests::Present("md5 sha512"), 32),
            Ok(DaemonAuthDigest::Md5)
        );
        assert_eq!(
            negotiate_server_daemon_digest(AdvertisedDigests::Present("sha1 md5"), 32),
            Ok(DaemonAuthDigest::Sha1)
        );
    }

    #[test]
    fn server_negotiation_skips_unsupported_client_names() {
        assert_eq!(
            negotiate_server_daemon_digest(
                AdvertisedDigests::Present("sponge blake3 sha256 md5"),
                32
            ),
            Ok(DaemonAuthDigest::Sha256)
        );
    }

    #[test]
    fn server_negotiation_rejects_a_wholly_foreign_client_list() {
        assert_eq!(
            negotiate_server_daemon_digest(AdvertisedDigests::Present("sponge blake3"), 32),
            Err(NoMutualDaemonAuthDigest)
        );
    }

    #[test]
    fn server_negotiation_falls_back_when_the_client_offered_no_list() {
        // upstream: compat.c:860 - the absent-list substitute is protocol-keyed.
        assert_eq!(
            negotiate_server_daemon_digest(AdvertisedDigests::Absent, 31),
            Ok(DaemonAuthDigest::Md5)
        );
        assert_eq!(
            negotiate_server_daemon_digest(AdvertisedDigests::Absent, 29),
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

    // upstream: compat.c:838-842 `output_daemon_greeting()` renders
    // `get_default_nno_list(&valid_auth_checksums, ...)` with no protocol
    // filtering whatsoever, so this single producer serves the greeting, the
    // daemon refusal, and the client abort alike. Confirmed against rsync
    // 3.4.4: `--daemon --protocol=28` still greets
    // `@RSYNCD: 28.0 sha512 sha256 sha1 md5 md4`.
    #[test]
    fn the_advertised_list_is_never_filtered_by_protocol() {
        let rendered = supported_daemon_digest_list();
        assert_eq!(rendered, "sha512 sha256 sha1 md5 md4");

        // Every name we advertise must round-trip back through the parser;
        // advertising a name we would then reject is what makes two producers
        // dangerous.
        assert_eq!(
            parse_daemon_digest_list(&rendered),
            SUPPORTED_DAEMON_DIGESTS.as_slice(),
        );
    }
}
