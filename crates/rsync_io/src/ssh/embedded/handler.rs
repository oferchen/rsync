//! SSH client handler implementing host key verification.
//!
//! Provides `SshClientHandler` which implements `russh::client::Handler`
//! with configurable host key checking behavior mirroring OpenSSH's
//! `StrictHostKeyChecking` option.

use std::io::Write;
use std::path::PathBuf;

use is_terminal::IsTerminal;
use russh_keys::key::PublicKey;

use super::error::SshError;
use super::types::StrictHostKeyChecking;

/// SSH client handler that verifies server host keys against a known hosts file.
///
/// Behavior depends on the configured `StrictHostKeyChecking` mode:
/// - `Yes` - reject unknown or changed keys immediately.
/// - `Ask` - prompt the user on a TTY; reject if no TTY is available.
/// - `No` - accept unknown keys with a warning (changed keys are always rejected).
pub struct SshClientHandler {
    strict_host_key_checking: StrictHostKeyChecking,
    known_hosts_file: Option<PathBuf>,
    host: String,
    port: u16,
}

impl SshClientHandler {
    /// Create a new handler for the given host and port.
    ///
    /// When `known_hosts_file` is `None`, the default `~/.ssh/known_hosts`
    /// location is used (via `russh_keys::check_known_hosts`).
    pub fn new(
        host: String,
        port: u16,
        strict_host_key_checking: StrictHostKeyChecking,
        known_hosts_file: Option<PathBuf>,
    ) -> Self {
        Self {
            strict_host_key_checking,
            known_hosts_file,
            host,
            port,
        }
    }

    /// Check the server key against the known hosts file.
    ///
    /// Returns `Ok(true)` to accept, `Ok(false)` to reject, or an error
    /// for key mismatches (potential MITM).
    fn verify_host_key(&self, server_public_key: &PublicKey) -> Result<bool, SshError> {
        let check_result = match &self.known_hosts_file {
            Some(path) => {
                russh_keys::check_known_hosts_path(&self.host, self.port, server_public_key, path)
            }
            None => russh_keys::check_known_hosts(&self.host, self.port, server_public_key),
        };

        match check_result {
            Ok(true) => Ok(true),
            Ok(false) => self.handle_unknown_host(server_public_key),
            Err(russh_keys::Error::KeyChanged { line }) => {
                emit_key_changed_warning(&self.host, self.port, server_public_key, line);
                Err(SshError::HostKeyMismatch {
                    host: self.host.clone(),
                })
            }
            Err(e) => {
                // File-not-found or parse errors - treat as unknown host.
                logging::debug_log!(
                    Io,
                    1,
                    "known_hosts check error for {}:{}: {}",
                    self.host,
                    self.port,
                    e
                );
                self.handle_unknown_host(server_public_key)
            }
        }
    }

    /// Decide whether to accept an unknown host key based on the configured policy.
    fn handle_unknown_host(&self, server_public_key: &PublicKey) -> Result<bool, SshError> {
        match self.strict_host_key_checking {
            StrictHostKeyChecking::Yes => Err(SshError::UnknownHost {
                host: self.host.clone(),
            }),
            StrictHostKeyChecking::No => {
                eprintln!(
                    "Warning: Permanently added '{}' ({}) to the list of known hosts.",
                    self.host,
                    server_public_key.name(),
                );
                self.learn_host_key(server_public_key)?;
                Ok(true)
            }
            StrictHostKeyChecking::Ask => self.prompt_user(server_public_key),
        }
    }

    /// Prompt on stderr/stdin for host key acceptance (requires a TTY).
    fn prompt_user(&self, server_public_key: &PublicKey) -> Result<bool, SshError> {
        if !std::io::stdin().is_terminal() {
            eprintln!(
                "Host key verification failed: no terminal available to prompt for {}.",
                self.host,
            );
            return Err(SshError::UnknownHost {
                host: self.host.clone(),
            });
        }

        let fingerprint = server_public_key.fingerprint();
        eprint!(
            "The authenticity of host '{}' ({}) can't be established.\n\
             {} key fingerprint is {}.\n\
             Are you sure you want to continue connecting (yes/no)? ",
            self.host,
            format_host_port(&self.host, self.port),
            server_public_key.name(),
            fingerprint,
        );
        std::io::stderr().flush().ok();

        let mut response = String::new();
        std::io::stdin()
            .read_line(&mut response)
            .map_err(SshError::Io)?;

        let answer = response.trim().to_lowercase();
        if answer == "yes" {
            eprintln!(
                "Warning: Permanently added '{}' ({}) to the list of known hosts.",
                self.host,
                server_public_key.name(),
            );
            self.learn_host_key(server_public_key)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Append the server's public key to the known hosts file.
    fn learn_host_key(&self, server_public_key: &PublicKey) -> Result<(), SshError> {
        match &self.known_hosts_file {
            Some(path) => {
                russh_keys::learn_known_hosts_path(&self.host, self.port, server_public_key, path)
                    .map_err(|e| SshError::Io(std::io::Error::other(e.to_string())))
            }
            None => russh_keys::learn_known_hosts(&self.host, self.port, server_public_key)
                .map_err(|e| SshError::Io(std::io::Error::other(e.to_string()))),
        }
    }
}

#[async_trait::async_trait]
impl russh::client::Handler for SshClientHandler {
    type Error = SshError;

    async fn check_server_key(&mut self, server_public_key: &PublicKey) -> Result<bool, SshError> {
        self.verify_host_key(server_public_key)
    }
}

/// Format host:port for display, omitting port 22.
fn format_host_port(host: &str, port: u16) -> String {
    if port == 22 {
        host.to_string()
    } else {
        format!("[{host}]:{port}")
    }
}

/// Print the MITM warning to stderr, matching OpenSSH's format.
fn emit_key_changed_warning(host: &str, port: u16, key: &PublicKey, line: usize) {
    eprintln!(
        "@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@\n\
         @    WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!     @\n\
         @@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@\n\
         IT IS POSSIBLE THAT SOMEONE IS DOING SOMETHING NASTY!\n\
         Someone could be eavesdropping on you right now (man-in-the-middle attack)!\n\
         It is also possible that a host key has just been changed.\n\
         The fingerprint for the {} key sent by the remote host is\n\
         {}.\n\
         Please contact your system administrator.\n\
         Add correct host key in known_hosts to get rid of this message.\n\
         Offending key in known_hosts:{}\n\
         Host key for {} has changed and you have requested strict checking.\n\
         Host key verification failed.",
        key.name(),
        key.fingerprint(),
        line,
        format_host_port(host, port),
    );
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    /// Verify handler creation with each strict host key checking mode.
    #[test]
    fn handler_creation_modes() {
        let h = SshClientHandler::new("example.com".into(), 22, StrictHostKeyChecking::Yes, None);
        assert_eq!(h.strict_host_key_checking, StrictHostKeyChecking::Yes);
        assert_eq!(h.host, "example.com");
        assert_eq!(h.port, 22);
        assert!(h.known_hosts_file.is_none());

        let h = SshClientHandler::new(
            "example.com".into(),
            2222,
            StrictHostKeyChecking::No,
            Some(PathBuf::from("/tmp/kh")),
        );
        assert_eq!(h.strict_host_key_checking, StrictHostKeyChecking::No);
        assert_eq!(h.port, 2222);
        assert_eq!(
            h.known_hosts_file.as_deref(),
            Some(std::path::Path::new("/tmp/kh"))
        );
    }

    /// Verify host:port formatting omits port 22.
    #[test]
    fn format_host_port_display() {
        assert_eq!(format_host_port("example.com", 22), "example.com");
        assert_eq!(format_host_port("example.com", 2222), "[example.com]:2222");
    }

    /// Generate a deterministic Ed25519 keypair for tests.
    fn test_ed25519_pubkey() -> PublicKey {
        let keypair = russh_keys::key::KeyPair::generate_ed25519();
        match keypair {
            Some(kp) => kp
                .clone_public_key()
                .expect("ed25519 key should have public key"),
            None => panic!("failed to generate test ed25519 keypair"),
        }
    }

    /// Known host entry matches - verification should succeed.
    #[test]
    fn known_host_match() {
        let pubkey = test_ed25519_pubkey();
        let dir = tempfile::tempdir().expect("tempdir");
        let kh_path = dir.path().join("known_hosts");

        russh_keys::learn_known_hosts_path("testhost.example", 22, &pubkey, &kh_path)
            .expect("learn");

        let handler = SshClientHandler::new(
            "testhost.example".into(),
            22,
            StrictHostKeyChecking::Yes,
            Some(kh_path),
        );

        let result = handler.verify_host_key(&pubkey);
        assert!(result.is_ok());
        assert!(result.unwrap());
    }

    /// Unknown host with StrictHostKeyChecking::Yes should be rejected.
    #[test]
    fn unknown_host_strict_yes() {
        let pubkey = test_ed25519_pubkey();
        let dir = tempfile::tempdir().expect("tempdir");
        let kh_path = dir.path().join("known_hosts");

        std::fs::File::create(&kh_path).expect("create");

        let handler = SshClientHandler::new(
            "unknown.example".into(),
            22,
            StrictHostKeyChecking::Yes,
            Some(kh_path),
        );

        let result = handler.verify_host_key(&pubkey);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, SshError::UnknownHost { ref host } if host == "unknown.example"),
            "expected UnknownHost, got: {err:?}",
        );
    }

    /// Unknown host with StrictHostKeyChecking::No should be accepted and learned.
    #[test]
    fn unknown_host_strict_no_accepts_and_learns() {
        let pubkey = test_ed25519_pubkey();
        let dir = tempfile::tempdir().expect("tempdir");
        let kh_path = dir.path().join("known_hosts");

        std::fs::File::create(&kh_path).expect("create");

        let handler = SshClientHandler::new(
            "auto.example".into(),
            22,
            StrictHostKeyChecking::No,
            Some(kh_path.clone()),
        );

        let result = handler.verify_host_key(&pubkey);
        assert!(result.is_ok());
        assert!(result.unwrap());

        // Verify the key was persisted.
        let check = russh_keys::check_known_hosts_path("auto.example", 22, &pubkey, &kh_path);
        assert!(check.is_ok());
        assert!(check.unwrap());
    }

    /// Mismatched key is always rejected, even with StrictHostKeyChecking::No.
    #[test]
    fn key_mismatch_always_rejected() {
        let original_key = test_ed25519_pubkey();
        let different_key = test_ed25519_pubkey();

        let dir = tempfile::tempdir().expect("tempdir");
        let kh_path = dir.path().join("known_hosts");

        // Learn the original key.
        russh_keys::learn_known_hosts_path("mismatch.example", 22, &original_key, &kh_path)
            .expect("learn");

        // Verify with a different key - should fail even with No mode.
        let handler = SshClientHandler::new(
            "mismatch.example".into(),
            22,
            StrictHostKeyChecking::No,
            Some(kh_path),
        );

        let result = handler.verify_host_key(&different_key);
        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), SshError::HostKeyMismatch { ref host } if host == "mismatch.example"),
        );
    }

    /// Non-standard port known hosts entries are distinct from port 22.
    #[test]
    fn non_standard_port_isolation() {
        let pubkey = test_ed25519_pubkey();
        let dir = tempfile::tempdir().expect("tempdir");
        let kh_path = dir.path().join("known_hosts");

        // Learn on port 2222.
        russh_keys::learn_known_hosts_path("porttest.example", 2222, &pubkey, &kh_path)
            .expect("learn");

        // Should match on port 2222.
        let handler = SshClientHandler::new(
            "porttest.example".into(),
            2222,
            StrictHostKeyChecking::Yes,
            Some(kh_path.clone()),
        );
        assert!(handler.verify_host_key(&pubkey).unwrap());

        // Should NOT match on port 22 (unknown host).
        let handler22 = SshClientHandler::new(
            "porttest.example".into(),
            22,
            StrictHostKeyChecking::Yes,
            Some(kh_path),
        );
        assert!(handler22.verify_host_key(&pubkey).is_err());
    }

    /// Missing known_hosts file with StrictHostKeyChecking::No should still accept.
    #[test]
    fn missing_known_hosts_file_no_mode() {
        let pubkey = test_ed25519_pubkey();
        let dir = tempfile::tempdir().expect("tempdir");
        let kh_path = dir.path().join("nonexistent");

        let handler = SshClientHandler::new(
            "nofile.example".into(),
            22,
            StrictHostKeyChecking::No,
            Some(kh_path),
        );

        // Should accept because StrictHostKeyChecking::No treats errors as unknown.
        let result = handler.verify_host_key(&pubkey);
        assert!(result.is_ok());
        assert!(result.unwrap());
    }

    /// Malformed known_hosts file is treated as unknown host.
    #[test]
    fn malformed_known_hosts_treated_as_unknown() {
        let pubkey = test_ed25519_pubkey();
        let dir = tempfile::tempdir().expect("tempdir");
        let kh_path = dir.path().join("known_hosts");

        let mut f = std::fs::File::create(&kh_path).expect("create");
        writeln!(f, "not a valid known_hosts line !!! garbage").expect("write");
        drop(f);

        let handler = SshClientHandler::new(
            "badfile.example".into(),
            22,
            StrictHostKeyChecking::Yes,
            Some(kh_path),
        );

        // Should be treated as unknown host - strict Yes rejects.
        let result = handler.verify_host_key(&pubkey);
        assert!(result.is_err());
    }
}
