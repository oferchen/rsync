//! Daemon mode authentication matching upstream rsync 3.4.1.
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
//! 1. **Server generates challenge**: A unique base64-encoded random string
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
//! - **Challenge generation**: Uses cryptographically secure random + timestamp + PID
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
//! The server advertises supported algorithms in the `@RSYNCD:` greeting, and clients
//! select the strongest mutually supported algorithm.
//!
//! # Examples
//!
//! ## Server-side authentication
//!
//! ```no_run
//! use daemon::auth::{ChallengeGenerator, SecretsFile, verify_client_response};
//! use std::net::IpAddr;
//! use std::path::Path;
//!
//! # fn example() -> std::io::Result<()> {
//! // Load secrets file
//! let secrets = SecretsFile::from_file(Path::new("/etc/rsyncd.secrets"))?;
//!
//! // Generate challenge
//! let peer_ip: IpAddr = "192.168.1.1".parse().unwrap();
//! let challenge = ChallengeGenerator::generate(peer_ip);
//!
//! // Client sends: "alice <response>"
//! let client_response = "alice dGVzdHJlc3BvbnNl"; // Base64-encoded hash
//!
//! // Verify
//! if let Some(password) = secrets.lookup("alice") {
//!     if verify_client_response(password.as_bytes(), &challenge, "dGVzdHJlc3BvbnNl") {
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
//! - **Constant-time comparison**: Prevents timing attacks during response verification
//! - **Cryptographically secure random**: Uses `getrandom` for challenge generation
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
//! - [`daemon::sections::module_access`]: Server-side authentication flow during module requests
//! - This module: High-level documentation and helper utilities
//!
//! Challenge generation and response verification are implemented directly in
//! `daemon::sections::module_access` to minimize coupling and keep the authentication
//! flow co-located with the module request handling code.

pub use core::auth::{
    compute_daemon_auth_response as compute_auth_response,
    verify_daemon_auth_response as verify_client_response,
    DaemonAuthDigest, SUPPORTED_DAEMON_DIGESTS,
};

use std::collections::HashMap;
use std::fs;
use std::io;
use std::net::IpAddr;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::STANDARD_NO_PAD;
use base64::Engine as _;
use checksums::strong::Md5;

/// Generates authentication challenges for daemon mode.
///
/// Challenges are created by hashing a combination of:
/// - Client IP address
/// - Current timestamp (seconds and microseconds)
/// - Process ID
///
/// This ensures challenges are unique across sessions and resistant to prediction.
pub struct ChallengeGenerator;

impl ChallengeGenerator {
    /// Generates a unique authentication challenge for the given peer.
    ///
    /// The challenge is a base64-encoded MD5 hash of:
    /// - Peer IP address (up to 16 bytes)
    /// - Unix timestamp seconds (4 bytes)
    /// - Timestamp microseconds (4 bytes)
    /// - Process ID (4 bytes)
    ///
    /// # Examples
    ///
    /// ```
    /// use daemon::auth::ChallengeGenerator;
    /// use std::net::IpAddr;
    ///
    /// let peer_ip: IpAddr = "192.168.1.1".parse().unwrap();
    /// let challenge = ChallengeGenerator::generate(peer_ip);
    ///
    /// // Challenge is base64-encoded MD5 (22 characters without padding)
    /// assert_eq!(challenge.len(), 22);
    /// ```
    #[must_use]
    pub fn generate(peer_ip: IpAddr) -> String {
        let mut input = [0u8; 32];

        // First 16 bytes: IP address
        let address_text = peer_ip.to_string();
        let address_bytes = address_text.as_bytes();
        let copy_len = address_bytes.len().min(16);
        input[..copy_len].copy_from_slice(&address_bytes[..copy_len]);

        // Next 8 bytes: timestamp
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let seconds = (timestamp.as_secs() & u64::from(u32::MAX)) as u32;
        let micros = timestamp.subsec_micros();
        input[16..20].copy_from_slice(&seconds.to_le_bytes());
        input[20..24].copy_from_slice(&micros.to_le_bytes());

        // Last 4 bytes: process ID
        let pid = std::process::id();
        input[24..28].copy_from_slice(&pid.to_le_bytes());

        // Hash and encode
        let mut hasher = Md5::new();
        hasher.update(&input);
        let digest = hasher.finalize();
        STANDARD_NO_PAD.encode(digest)
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
/// On Unix systems, the secrets file must have mode 0600 (readable only by owner).
/// This is enforced by [`SecretsFile::from_file`].
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

            // Skip empty lines and comments
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            // Parse username:password
            if let Some((user, password)) = line.split_once(':') {
                entries.insert(user.to_string(), password.to_string());
            } else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("invalid secrets file line (missing ':'): {}", line),
                ));
            }
        }

        Ok(Self { entries })
    }

    /// Loads a secrets file from disk and verifies permissions.
    ///
    /// # Security
    ///
    /// On Unix systems, this function checks that the file has mode 0600
    /// (readable and writable only by owner). World-readable or group-readable
    /// secrets files are rejected to prevent password disclosure.
    ///
    /// On Windows, permission checks are skipped (matching upstream rsync).
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The file cannot be read
    /// - The file has incorrect permissions (Unix only)
    /// - The file contains invalid entries
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use daemon::auth::SecretsFile;
    /// use std::path::Path;
    ///
    /// # fn example() -> std::io::Result<()> {
    /// let secrets = SecretsFile::from_file(Path::new("/etc/rsyncd.secrets"))?;
    /// if let Some(password) = secrets.lookup("alice") {
    ///     println!("Found password for alice");
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn from_file(path: &Path) -> io::Result<Self> {
        // Check permissions before reading
        Self::check_permissions(path)?;

        // Read and parse
        let content = fs::read_to_string(path)?;
        Self::parse(&content)
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
    #[must_use]
    pub fn lookup(&self, username: &str) -> Option<&str> {
        self.entries.get(username).map(|s| s.as_str())
    }

    /// Checks that the secrets file has correct permissions.
    ///
    /// On Unix: File must be mode 0600 (owner read/write only)
    /// On Windows: No checks performed
    ///
    /// # Errors
    ///
    /// Returns an error if the file is world-readable or group-readable on Unix.
    #[cfg(unix)]
    pub fn check_permissions(path: &Path) -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt;

        let metadata = fs::metadata(path)?;
        let permissions = metadata.permissions();
        let mode = permissions.mode();

        // Check for world-readable (bit 2) or group-readable (bit 5)
        if (mode & 0o044) != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "secrets file '{}' must not be readable by group or others (mode {:o})",
                    path.display(),
                    mode & 0o777
                ),
            ));
        }

        Ok(())
    }

    /// No-op permission check on Windows (matching upstream rsync).
    #[cfg(not(unix))]
    pub fn check_permissions(_path: &Path) -> io::Result<()> {
        Ok(())
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

    #[test]
    fn challenge_generator_produces_valid_base64() {
        let peer_ip: IpAddr = "192.168.1.1".parse().unwrap();
        let challenge = ChallengeGenerator::generate(peer_ip);

        // MD5 hash is 16 bytes, base64 without padding is 22 characters
        assert_eq!(challenge.len(), 22);

        // Should be valid base64
        assert!(challenge
            .chars()
            .all(|c| c.is_alphanumeric() || c == '+' || c == '/'));
    }

    #[test]
    fn challenge_generator_produces_unique_values() {
        let peer_ip: IpAddr = "10.0.0.1".parse().unwrap();
        let challenge1 = ChallengeGenerator::generate(peer_ip);

        // Small delay to ensure different timestamp
        std::thread::sleep(std::time::Duration::from_millis(10));

        let challenge2 = ChallengeGenerator::generate(peer_ip);

        // Challenges should differ due to timestamp
        assert_ne!(challenge1, challenge2);
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
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("missing ':'"));
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

        // Compute response using MD5
        let response = compute_auth_response(password, challenge, DaemonAuthDigest::Md5);

        // Verify should succeed
        assert!(verify_client_response(password, challenge, &response));
    }

    #[test]
    fn verify_client_response_wrong_password() {
        let password = b"correct";
        let wrong_password = b"incorrect";
        let challenge = "test_challenge";

        let response = compute_auth_response(password, challenge, DaemonAuthDigest::Md5);

        // Verification with wrong password should fail
        assert!(!verify_client_response(wrong_password, challenge, &response));
    }

    #[test]
    fn verify_client_response_wrong_challenge() {
        let password = b"secret";
        let challenge1 = "challenge1";
        let challenge2 = "challenge2";

        let response = compute_auth_response(password, challenge1, DaemonAuthDigest::Md5);

        // Verification with different challenge should fail
        assert!(!verify_client_response(password, challenge2, &response));
    }

    #[test]
    fn verify_client_response_supports_multiple_algorithms() {
        let password = b"test";
        let challenge = "ch";

        // Test all supported algorithms
        for &digest in SUPPORTED_DAEMON_DIGESTS {
            let response = compute_auth_response(password, challenge, digest);
            assert!(
                verify_client_response(password, challenge, &response),
                "Failed for digest: {:?}",
                digest
            );
        }
    }
}
