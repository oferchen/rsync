//! Daemon mode authentication matching upstream rsync 3.5.1.
//!
//! This module implements the rsync daemon challenge-response authentication protocol
//! that protects modules configured with `auth users`. The implementation mirrors
//! upstream rsync's `authenticate.c` while using constant-time comparison for improved
//! security.
//!
//! # Protocol Overview
//!
//! The daemon authentication follows this sequence:
//!
//! 1. **Server generates challenge**: A base64-encoded digest of peer address,
//!    time and PID
//! 2. **Server sends**: `@RSYNCD: AUTHREQD <challenge>\n`
//! 3. **Client computes response**: Base64(MD4/MD5(password + challenge))
//! 4. **Client sends**: `<username> <response>\n`
//! 5. **Server verifies**: Looks up password in secrets file, computes expected response
//! 6. **Server replies**: `@RSYNCD: OK\n` or `@ERROR: access denied\n`
//!
//! # Secrets File Format
//!
//! The secrets file (`/etc/rsyncd.secrets` by default) contains username:password pairs:
//!
//! ```text
//! # Comments are allowed
//! alice:secret123
//! bob:another_password
//! ```
//!
//! ## Security Requirements
//!
//! - **File permissions**: Must be readable only by owner (mode 0600 on Unix)
//! - **Password storage**: Plain text (matching upstream rsync behavior)
//! - **Challenge generation**: 32 bytes from the kernel CSPRNG followed by the
//!   legacy peer address + timestamp + PID block, hashed with the negotiated
//!   digest - upstream's `gen_challenge()` (authenticate.c:88-119). If the RNG is
//!   unavailable the legacy block alone is hashed and the daemon logs upstream's
//!   warning, so a failure degrades to (never below) the pre-3.5 behaviour.
//!
//! # Supported Hash Algorithms
//!
//! The implementation supports multiple hash algorithms negotiated via the daemon greeting:
//!
//! - SHA-512 (strongest, preferred)
//! - SHA-256
//! - SHA-1
//! - MD5 (historical default)
//! - MD4 (legacy compatibility)
//!
//! Both sides advertise their list in the `@RSYNCD:` greeting. A client picks the
//! entry of the server's list that ranks highest in its own; this server takes the
//! *first* entry of the client's list that it supports, so client preference wins.
//! Deviation from upstream: 3.5.1's `parse_negotiate_str()` (compat.c:333-364)
//! keeps the mutual entry ranked highest in the server's own list on both sides.
//! The single winner then drives both the challenge and the verification.
//!
//! # Examples
//!
//! ## Server-side authentication
//!
//! ```no_run
//! use core::auth::{DaemonAuthDigest, negotiate_server_daemon_digest};
//! use daemon::auth::{ChallengeGenerator, SecretsFile, verify_client_response};
//! use protocol::AdvertisedDigests;
//! use std::net::IpAddr;
//!
//! # fn example() -> std::io::Result<()> {
//! // Parse the module's secrets file contents
//! let secrets = SecretsFile::parse("alice:secret123\n")?;
//!
//! // Pin one digest for the whole exchange, from the client's greeting list.
//! let digest = negotiate_server_daemon_digest(AdvertisedDigests::Present("sha512 md5"), 31)
//!     .expect("client offered a supported digest");
//!
//! // Generate challenge
//! let peer_ip: IpAddr = "192.168.1.1".parse().unwrap();
//! let challenge = ChallengeGenerator::generate(peer_ip, digest).text;
//!
//! // Client sends: "alice <response>"
//! let client_response = "alice dGVzdHJlc3BvbnNl"; // Base64-encoded hash
//!
//! // Verify with the same negotiated digest
//! if let Some(password) = secrets.lookup("alice") {
//!     if verify_client_response(password.as_bytes(), &challenge, "dGVzdHJlc3BvbnNl", digest) {
//!         println!("Authentication successful");
//!     }
//! }
//! # Ok(())
//! # }
//! ```
//!
//! ## Client-side authentication
//!
//! ```no_run
//! use daemon::auth::compute_auth_response;
//! use core::auth::DaemonAuthDigest;
//!
//! // Receive challenge from server: "@RSYNCD: AUTHREQD dGVzdGNoYWxsZW5nZQ"
//! let challenge = "dGVzdGNoYWxsZW5nZQ";
//! let password = b"secret123";
//!
//! // Compute response using MD5 (default)
//! let response = compute_auth_response(password, challenge, DaemonAuthDigest::Md5);
//!
//! // Send to server: "alice <response>"
//! println!("alice {}", response);
//! ```
//!
//! # Security Considerations
//!
//! ## Improvements over upstream rsync
//!
//! - **Constant-time comparison**: Prevents timing attacks during response
//!   verification, where upstream uses a plain `strcmp()`. Not wire-observable.
//!
//! ## Known limitations (matching upstream)
//!
//! - **Plain text passwords**: Secrets file stores passwords in plain text
//! - **No salt**: Password hash doesn't use a per-user salt
//! - **Challenge replay**: No protection against challenge replay within the same session
//!
//! # Implementation Notes
//!
//! The authentication logic is split across multiple modules:
//!
//! - [`core::auth`]: Shared hash computation and verification (used by both client and server)
//! - `daemon::sections::module_access`: Server-side authentication flow during module requests
//! - This module: High-level documentation and helper utilities
//!
//! `ChallengeGenerator` is the sole challenge implementation; the module-access
//! authentication flow calls it rather than keeping a second copy.

pub use core::auth::{
    DaemonAuthDigest, NoMutualDaemonAuthDigest, SUPPORTED_DAEMON_DIGESTS,
    compute_daemon_auth_response as compute_auth_response, negotiate_server_daemon_digest,
    supported_daemon_digest_list, verify_daemon_auth_response as verify_client_response,
};

use std::collections::HashMap;
use std::io;
use std::net::IpAddr;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Daemon log line written when the kernel RNG cannot seed a challenge.
///
/// upstream: authenticate.c:113-114 - `rprintf(FWARNING, ...)`, which a daemon
/// routes to its log file.
pub const KERNEL_RNG_UNAVAILABLE_WARNING: &str =
    "gen_challenge: /dev/urandom unavailable, falling back to time-based challenge";

/// A daemon authentication challenge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Challenge {
    /// Base64 text sent after `@RSYNCD: AUTHREQD `.
    pub text: String,
    /// `false` when the kernel RNG failed and only the legacy time-based input
    /// was hashed; the caller then logs [`KERNEL_RNG_UNAVAILABLE_WARNING`].
    pub kernel_random: bool,
}

/// Generates authentication challenges for daemon mode.
///
/// This is the single challenge implementation; the daemon's authentication
/// handler calls it directly rather than keeping a private copy.
pub struct ChallengeGenerator;

impl ChallengeGenerator {
    /// Generates an unpredictable authentication challenge for the given peer.
    ///
    /// The digest input is 32 bytes from the kernel CSPRNG followed by a
    /// 32-byte legacy block:
    /// - Peer IP address text (up to 16 bytes)
    /// - Unix timestamp seconds (4 bytes)
    /// - Timestamp microseconds (4 bytes)
    /// - Process ID (4 bytes)
    ///
    /// The legacy block alone is roughly 35 bits of entropy, which lets a
    /// network observer enumerate the preimage offline; the random prefix gives
    /// the digest full entropy. When the RNG fails, only the legacy block is
    /// hashed and [`Challenge::kernel_random`] is `false`.
    ///
    /// `digest` is the algorithm fixed by [`negotiate_server_daemon_digest`]; the
    /// same one must verify the response.
    ///
    /// upstream: authenticate.c:88-119 `gen_challenge()` - `sum_update(rnd, 32)`
    /// when `get_random_bytes()` succeeds, then `sum_update(input, 32)` over the
    /// zeroed buffer holding `strlcpy(input, addr, 17)`, `SIVAL(input, 16,
    /// tv_sec)`, `SIVAL(input, 20, tv_usec)` and `SIVAL(input, 24, getpid())`,
    /// hashed with `valid_auth_checksums.negotiated_nni`. `SIVAL` is
    /// little-endian by definition, so `to_le_bytes` reproduces upstream's bytes.
    /// Upstream reads `/dev/urandom` directly (authenticate.c:71-85); the
    /// `getrandom` crate reaches the same kernel CSPRNG on every platform,
    /// including Windows, which has no `/dev/urandom`.
    ///
    /// # Examples
    ///
    /// ```
    /// use core::auth::DaemonAuthDigest;
    /// use daemon::auth::ChallengeGenerator;
    /// use std::net::IpAddr;
    ///
    /// let peer_ip: IpAddr = "192.168.1.1".parse().unwrap();
    /// let challenge = ChallengeGenerator::generate(peer_ip, DaemonAuthDigest::Md5);
    ///
    /// // Challenge is base64-encoded hash (22 characters without padding)
    /// assert_eq!(challenge.text.len(), 22);
    /// ```
    #[must_use]
    pub fn generate(peer_ip: IpAddr, digest: DaemonAuthDigest) -> Challenge {
        let since_epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        Self::generate_from(
            peer_ip,
            digest,
            since_epoch,
            std::process::id(),
            getrandom::fill,
        )
    }

    fn generate_from(
        peer_ip: IpAddr,
        digest: DaemonAuthDigest,
        since_epoch: Duration,
        pid: u32,
        fill_random: impl FnOnce(&mut [u8]) -> Result<(), getrandom::Error>,
    ) -> Challenge {
        let mut input = [0u8; 32];
        let address_text = peer_ip.to_string();
        let address_bytes = address_text.as_bytes();
        let copy_len = address_bytes.len().min(16);
        input[..copy_len].copy_from_slice(&address_bytes[..copy_len]);
        let seconds = (since_epoch.as_secs() & u64::from(u32::MAX)) as u32;
        input[16..20].copy_from_slice(&seconds.to_le_bytes());
        input[20..24].copy_from_slice(&since_epoch.subsec_micros().to_le_bytes());
        input[24..28].copy_from_slice(&pid.to_le_bytes());

        let mut random = [0u8; 32];
        let kernel_random = fill_random(&mut random).is_ok();
        let text = if kernel_random {
            digest.base64_digest(&[&random, &input])
        } else {
            digest.base64_digest(&[&input])
        };
        Challenge {
            text,
            kernel_random,
        }
    }
}

/// Parses and manages daemon secrets files.
///
/// A secrets file contains `username:password` entries, one per line.
/// Lines starting with `#` are treated as comments and ignored.
///
/// # Format
///
/// ```text
/// # This is a comment
/// alice:secret123
/// bob:another_password
/// charlie:yet_another
/// ```
///
/// # Security
///
/// On Unix systems, the secrets file must not be other-accessible (mode `& 06`);
/// group access such as mode 0640 is allowed. Enforced by [`SecretsFile::check_permissions`].
#[derive(Debug, Clone)]
pub struct SecretsFile {
    entries: HashMap<String, String>,
}

impl SecretsFile {
    /// Creates a new empty secrets file.
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    /// Parses a secrets file from its contents.
    ///
    /// # Format
    ///
    /// - Each line should be `username:password`
    /// - Lines starting with `#` are comments
    /// - Empty lines are ignored
    /// - Carriage returns (`\r`) are stripped
    ///
    /// # Errors
    ///
    /// Returns an error if a non-comment line doesn't contain a `:` separator.
    ///
    /// # Examples
    ///
    /// ```
    /// use daemon::auth::SecretsFile;
    ///
    /// let content = "# Comment\nalice:secret\nbob:password\n";
    /// let secrets = SecretsFile::parse(content).unwrap();
    ///
    /// assert_eq!(secrets.lookup("alice"), Some("secret"));
    /// assert_eq!(secrets.lookup("bob"), Some("password"));
    /// assert_eq!(secrets.lookup("charlie"), None);
    /// ```
    pub fn parse(content: &str) -> io::Result<Self> {
        let mut entries = HashMap::new();

        for raw_line in content.lines() {
            let line = raw_line.trim_end_matches('\r');

            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            if let Some((user, password)) = line.split_once(':') {
                entries.insert(user.to_string(), password.to_string());
            } else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("invalid secrets file line (missing ':'): {line}"),
                ));
            }
        }

        Ok(Self { entries })
    }

    /// Looks up the password for a given username.
    ///
    /// # Returns
    ///
    /// - `Some(password)` if the username exists
    /// - `None` if the username is not found
    ///
    /// # Examples
    ///
    /// ```
    /// use daemon::auth::SecretsFile;
    ///
    /// let content = "alice:secret123\nbob:password\n";
    /// let secrets = SecretsFile::parse(content).unwrap();
    ///
    /// assert_eq!(secrets.lookup("alice"), Some("secret123"));
    /// assert_eq!(secrets.lookup("bob"), Some("password"));
    /// assert_eq!(secrets.lookup("charlie"), None);
    /// ```
    pub fn lookup(&self, username: &str) -> Option<&str> {
        self.entries.get(username).map(|s| s.as_str())
    }

    /// Checks the secrets file against upstream's strict-modes rules.
    ///
    /// On Unix the file must not be accessible to others (mode 0640 is
    /// allowed), and a daemon running as root refuses a file that root does
    /// not own. On other platforms no check is performed, as upstream.
    ///
    /// upstream: authenticate.c:172-177 check_secret(). Delegates to
    /// `platform::secrets::check_secrets_file_permissions`, the same rule the
    /// daemon's authentication path applies.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be inspected or breaks either rule.
    pub fn check_permissions(path: &Path) -> io::Result<()> {
        platform::secrets::check_secrets_file_permissions(path)
    }

    /// Returns the number of username/password entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns true if the secrets file contains no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Default for SecretsFile {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WHY (security, deliberate no-op - pinned): on Windows the secrets-file
    /// permission check is intentionally a no-op because NTFS exposes no POSIX
    /// mode bits to inspect, matching upstream rsync (which only calls `stat`
    /// mode checks under Unix). This pins that the skip is a conscious decision,
    /// not an accidental gap: a secrets file is accepted regardless of its
    /// on-disk permissions. On Unix the same file (created without tightening)
    /// would be rejected as other-accessible. If Windows ACL hardening is ever
    /// added, this test must be updated deliberately, surfacing the change.
    #[cfg(not(unix))]
    #[test]
    fn check_permissions_is_a_deliberate_noop_off_unix() {
        let file = tempfile::NamedTempFile::new().expect("temp secrets file");
        assert!(
            SecretsFile::check_permissions(file.path()).is_ok(),
            "Windows deliberately accepts any-permission secrets files"
        );
    }

    /// upstream: authenticate.c:175-177 - a root daemon refuses a secrets file
    /// root does not own. Only observable when the test itself runs as root;
    /// the rule is unit-tested without root in `platform::secrets`.
    #[cfg(unix)]
    #[test]
    fn check_permissions_refuses_a_non_root_owned_file_when_root() {
        use std::os::unix::fs::PermissionsExt;
        if platform::privilege::effective_uid() != 0 {
            return;
        }
        let file = tempfile::NamedTempFile::new().expect("temp secrets file");
        std::fs::set_permissions(file.path(), std::fs::Permissions::from_mode(0o600))
            .expect("chmod secrets");
        std::os::unix::fs::chown(file.path(), Some(65534), None).expect("chown to nobody");
        let err = SecretsFile::check_permissions(file.path())
            .expect_err("root must refuse a secrets file it does not own");
        assert!(err.to_string().contains("must be owned by root"));
    }

    #[test]
    fn challenge_generator_produces_valid_base64() {
        let peer_ip: IpAddr = "192.168.1.1".parse().unwrap();
        let challenge = ChallengeGenerator::generate(peer_ip, DaemonAuthDigest::Md5).text;

        // MD5 hash is 16 bytes, base64 without padding is 22 characters
        assert_eq!(challenge.len(), 22);

        // Should be valid base64
        assert!(
            challenge
                .chars()
                .all(|c| c.is_alphanumeric() || c == '+' || c == '/')
        );
    }

    #[test]
    fn challenge_generator_produces_valid_base64_legacy_protocol() {
        let peer_ip: IpAddr = "192.168.1.1".parse().unwrap();
        let challenge = ChallengeGenerator::generate(peer_ip, DaemonAuthDigest::Md4).text;

        // MD4 hash is also 16 bytes, base64 without padding is 22 characters
        assert_eq!(challenge.len(), 22);

        assert!(
            challenge
                .chars()
                .all(|c| c.is_alphanumeric() || c == '+' || c == '/')
        );
    }

    const FIXED_PEER: &str = "10.0.0.1";
    const FIXED_TIME: Duration = Duration::from_micros(1_700_000_000_123_456);
    const FIXED_PID: u32 = 4242;

    /// The legacy block upstream builds, spelled out byte by byte so a layout
    /// change in `generate_from` cannot hide behind a shared helper.
    fn expected_legacy_input() -> [u8; 32] {
        let mut input = [0u8; 32];
        input[..8].copy_from_slice(b"10.0.0.1");
        input[16..20].copy_from_slice(&1_700_000_000u32.to_le_bytes());
        input[20..24].copy_from_slice(&123_456u32.to_le_bytes());
        input[24..28].copy_from_slice(&FIXED_PID.to_le_bytes());
        input
    }

    fn fixed_challenge(
        fill_random: impl FnOnce(&mut [u8]) -> Result<(), getrandom::Error>,
    ) -> Challenge {
        ChallengeGenerator::generate_from(
            FIXED_PEER.parse().unwrap(),
            DaemonAuthDigest::Md5,
            FIXED_TIME,
            FIXED_PID,
            fill_random,
        )
    }

    /// The security property: an observer who knows the peer address, the
    /// clock and the daemon pid still cannot predict the challenge, because
    /// kernel randomness enters the digest.
    ///
    /// upstream: authenticate.c:110-111 - the 32 random bytes are hashed in.
    #[test]
    fn challenges_from_identical_legacy_inputs_differ() {
        let first = fixed_challenge(getrandom::fill);
        let second = fixed_challenge(getrandom::fill);
        assert!(first.kernel_random && second.kernel_random);
        assert_ne!(
            first.text, second.text,
            "challenge is predictable from address, time and pid alone"
        );
    }

    /// The random bytes are hashed BEFORE the legacy block, as upstream does,
    /// and the legacy block keeps upstream's exact layout.
    #[test]
    fn kernel_bytes_are_hashed_before_the_legacy_input() {
        let challenge = fixed_challenge(|buf| {
            buf.fill(0x5a);
            Ok(())
        });
        let expected =
            DaemonAuthDigest::Md5.base64_digest(&[&[0x5a; 32], &expected_legacy_input()]);
        assert!(challenge.kernel_random);
        assert_eq!(challenge.text, expected);
    }

    /// upstream: authenticate.c:112-115 - when the RNG fails, upstream warns and
    /// hashes the legacy block alone instead of refusing the connection.
    #[test]
    fn rng_failure_falls_back_to_the_legacy_input_alone() {
        let challenge = fixed_challenge(|_| Err(getrandom::Error::UNSUPPORTED));
        assert!(
            !challenge.kernel_random,
            "fallback must be reported for the warning"
        );
        assert_eq!(
            challenge.text,
            DaemonAuthDigest::Md5.base64_digest(&[&expected_legacy_input()])
        );
    }

    #[test]
    fn rng_unavailable_warning_matches_upstream_text() {
        assert_eq!(
            KERNEL_RNG_UNAVAILABLE_WARNING,
            "gen_challenge: /dev/urandom unavailable, falling back to time-based challenge"
        );
    }

    #[test]
    fn secrets_file_parse_basic() {
        let content = "alice:password123\nbob:secret\n";
        let secrets = SecretsFile::parse(content).unwrap();

        assert_eq!(secrets.lookup("alice"), Some("password123"));
        assert_eq!(secrets.lookup("bob"), Some("secret"));
        assert_eq!(secrets.lookup("charlie"), None);
    }

    #[test]
    fn secrets_file_parse_with_comments() {
        let content = "# This is a comment\nalice:pass\n# Another comment\nbob:word\n";
        let secrets = SecretsFile::parse(content).unwrap();

        assert_eq!(secrets.lookup("alice"), Some("pass"));
        assert_eq!(secrets.lookup("bob"), Some("word"));
    }

    #[test]
    fn secrets_file_parse_empty_lines() {
        let content = "alice:pass\n\nbob:word\n\n";
        let secrets = SecretsFile::parse(content).unwrap();

        assert_eq!(secrets.lookup("alice"), Some("pass"));
        assert_eq!(secrets.lookup("bob"), Some("word"));
    }

    #[test]
    fn secrets_file_parse_strips_carriage_returns() {
        let content = "alice:pass\r\nbob:word\r\n";
        let secrets = SecretsFile::parse(content).unwrap();

        assert_eq!(secrets.lookup("alice"), Some("pass"));
        assert_eq!(secrets.lookup("bob"), Some("word"));
    }

    #[test]
    fn secrets_file_parse_rejects_malformed() {
        let content = "alice:pass\ninvalid_line_without_colon\nbob:word\n";
        let result = SecretsFile::parse(content);

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("missing ':'"));
    }

    #[test]
    fn secrets_file_len_and_is_empty() {
        let empty = SecretsFile::new();
        assert_eq!(empty.len(), 0);
        assert!(empty.is_empty());

        let content = "alice:pass\nbob:word\n";
        let secrets = SecretsFile::parse(content).unwrap();
        assert_eq!(secrets.len(), 2);
        assert!(!secrets.is_empty());
    }

    #[test]
    fn verify_client_response_roundtrip() {
        let password = b"mysecret";
        let challenge = "test_challenge";

        let response = compute_auth_response(password, challenge, DaemonAuthDigest::Md5);

        assert!(verify_client_response(
            password,
            challenge,
            &response,
            DaemonAuthDigest::Md5
        ));
    }

    #[test]
    fn verify_client_response_wrong_password() {
        let password = b"correct";
        let wrong_password = b"incorrect";
        let challenge = "test_challenge";

        let response = compute_auth_response(password, challenge, DaemonAuthDigest::Md5);

        // Verification with wrong password should fail
        assert!(!verify_client_response(
            wrong_password,
            challenge,
            &response,
            DaemonAuthDigest::Md5,
        ));
    }

    #[test]
    fn verify_client_response_wrong_challenge() {
        let password = b"secret";
        let challenge1 = "challenge1";
        let challenge2 = "challenge2";

        let response = compute_auth_response(password, challenge1, DaemonAuthDigest::Md5);

        // Verification with different challenge should fail
        assert!(!verify_client_response(
            password,
            challenge2,
            &response,
            DaemonAuthDigest::Md5
        ));
    }

    /// Every advertised digest must complete a full challenge/response round
    /// trip when it is the one negotiation picked.
    #[test]
    fn verify_client_response_supports_every_negotiated_algorithm() {
        let password = b"daemon_secret";

        for &digest in SUPPORTED_DAEMON_DIGESTS {
            let challenge = ChallengeGenerator::generate("10.0.0.1".parse().unwrap(), digest).text;
            assert_eq!(challenge.len(), digest.base64_len());

            let response = compute_auth_response(password, &challenge, digest);
            assert!(
                verify_client_response(password, &challenge, &response, digest),
                "round trip failed for {digest:?}"
            );
        }
    }

    /// A client cannot pick the algorithm it is checked against by choosing its
    /// response length: MD4 and MD5 share a base64 length, yet only the digest
    /// negotiation settled on may verify.
    ///
    /// upstream: authenticate.c:104,128 - both halves use
    /// `valid_auth_checksums.negotiated_nni`.
    #[test]
    fn verify_client_response_ignores_a_non_negotiated_digest() {
        let password = b"secret";
        let challenge =
            ChallengeGenerator::generate("10.0.0.1".parse().unwrap(), DaemonAuthDigest::Md5).text;

        let md4_resp = compute_auth_response(password, &challenge, DaemonAuthDigest::Md4);
        assert!(!verify_client_response(
            password,
            &challenge,
            &md4_resp,
            DaemonAuthDigest::Md5
        ));

        let sha512_resp = compute_auth_response(password, &challenge, DaemonAuthDigest::Sha512);
        assert!(!verify_client_response(
            password,
            &challenge,
            &sha512_resp,
            DaemonAuthDigest::Md5
        ));
    }

    // upstream: compat.c:844-853 `output_daemon_greeting()` prints
    // `get_default_nno_list(&valid_auth_checksums, ...)` verbatim into
    // `@RSYNCD: %d.%d %s\n` with no protocol filtering, so there is exactly one
    // advertised list and every protocol era sees it: `--daemon --protocol=28`
    // greets `@RSYNCD: 28.0 sha512 sha256 sha1 md5 md4`.
    #[test]
    fn the_daemon_greeting_advertises_the_same_list_at_every_protocol() {
        use protocol::ProtocolVersion;

        let expected = format!(" {}", supported_daemon_digest_list());
        for version in [
            ProtocolVersion::V28,
            ProtocolVersion::V29,
            ProtocolVersion::V30,
            ProtocolVersion::V31,
            ProtocolVersion::V32,
        ] {
            let greeting = crate::daemon::legacy_daemon_greeting_for_protocol(version);
            assert!(
                greeting.ends_with(&format!("{expected}\n")),
                "protocol {} greeting must carry the unfiltered list: {greeting:?}",
                version.as_u8(),
            );
        }
    }
}
