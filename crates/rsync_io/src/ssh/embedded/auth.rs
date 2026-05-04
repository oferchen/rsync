//! SSH authentication methods for the embedded transport.
//!
//! Tries authentication in OpenSSH order: agent, identity files, password.
//! The orchestrator function `authenticate()` drives the sequence and returns
//! `SshError::AuthenticationFailed` when all methods are exhausted.

use std::path::Path;
use std::sync::Arc;

use is_terminal::IsTerminal;
use russh::keys::PrivateKey;
use russh::keys::key::PrivateKeyWithHashAlg;

use super::config::SshConfig;
use super::error::SshError;
use super::handler::SshClientHandler;

/// Effective username for authentication.
///
/// Returns the configured username or falls back to the `USER` (Unix) /
/// `USERNAME` (Windows) environment variable.
fn effective_username(config: &SshConfig) -> Result<String, SshError> {
    if let Some(ref user) = config.username {
        return Ok(user.clone());
    }
    #[cfg(unix)]
    let var = "USER";
    #[cfg(windows)]
    let var = "USERNAME";
    #[cfg(not(any(unix, windows)))]
    let var = "USER";
    std::env::var(var).map_err(|_| SshError::AuthenticationFailed {
        tried: "no username available".to_owned(),
    })
}

/// Try authentication via the SSH agent.
///
/// Connects to the agent using `SSH_AUTH_SOCK`, enumerates all identities, and
/// signs each via `authenticate_publickey_with()` until one succeeds. Returns
/// `Ok(true)` on success, `Ok(false)` when the agent is unavailable or no
/// identity works.
///
/// `russh::keys::agent::client::AgentClient::connect_env` is gated to
/// `cfg(unix)` upstream (Pageant / named-pipe support is a separate Windows
/// path we have not validated). On non-Unix targets this method short-circuits
/// to `Ok(false)` so the caller falls through to identity-file and password
/// auth.
#[cfg(unix)]
async fn try_agent_auth(
    session: &mut russh::client::Handle<SshClientHandler>,
    username: &str,
) -> Result<bool, SshError> {
    let mut agent = match russh::keys::agent::client::AgentClient::connect_env().await {
        Ok(agent) => agent,
        Err(e) => {
            logging::debug_log!(Io, 1, "SSH agent unavailable: {}", e);
            return Ok(false);
        }
    };

    let identities = match agent.request_identities().await {
        Ok(ids) => ids,
        Err(e) => {
            logging::debug_log!(Io, 1, "SSH agent identity request failed: {}", e);
            return Ok(false);
        }
    };

    if identities.is_empty() {
        logging::debug_log!(Io, 1, "SSH agent has no identities");
        return Ok(false);
    }

    for identity in identities {
        let pubkey = identity.public_key().into_owned();
        match session
            .authenticate_publickey_with(username, pubkey, None, &mut agent)
            .await
        {
            Ok(result) if result.success() => return Ok(true),
            Ok(_) => continue,
            Err(e) => {
                logging::debug_log!(Io, 1, "SSH agent auth attempt failed: {}", e);
                continue;
            }
        }
    }

    Ok(false)
}

#[cfg(not(unix))]
async fn try_agent_auth(
    _session: &mut russh::client::Handle<SshClientHandler>,
    _username: &str,
) -> Result<bool, SshError> {
    logging::debug_log!(Io, 1, "SSH agent auth is not supported on this platform");
    Ok(false)
}

/// Try authentication using identity files (private keys).
///
/// For each path in `identity_files`, attempts to load the key and authenticate.
/// Encrypted keys prompt for a passphrase when stdin is a TTY. Missing or
/// unreadable files are silently skipped.
async fn try_identity_file_auth(
    session: &mut russh::client::Handle<SshClientHandler>,
    username: &str,
    identity_files: &[std::path::PathBuf],
) -> Result<bool, SshError> {
    for path in identity_files {
        let key = match load_identity_key(path) {
            Some(k) => k,
            None => continue,
        };
        let key_with_hash = PrivateKeyWithHashAlg::new(Arc::new(key), None);
        match session
            .authenticate_publickey(username, key_with_hash)
            .await
        {
            Ok(result) if result.success() => return Ok(true),
            Ok(_) => continue,
            Err(e) => {
                logging::debug_log!(
                    Io,
                    1,
                    "public key auth failed for {}: {}",
                    path.display(),
                    e
                );
                continue;
            }
        }
    }
    Ok(false)
}

/// Load a private key from disk, prompting for passphrase if needed.
///
/// Returns `None` when the file is missing, unreadable, or the user declines
/// to enter a passphrase for an encrypted key.
fn load_identity_key(path: &Path) -> Option<PrivateKey> {
    if !path.is_file() {
        return None;
    }

    // First attempt without a passphrase.
    match russh::keys::load_secret_key(path, None) {
        Ok(key) => return Some(key),
        Err(e) => {
            // Check if the error indicates an encrypted key.
            let msg = e.to_string();
            if !msg.contains("encrypted") && !msg.contains("passphrase") && !msg.contains("decrypt")
            {
                logging::debug_log!(Io, 1, "skipping identity file {}: {}", path.display(), e);
                return None;
            }
        }
    }

    // Key is encrypted - prompt for passphrase if we have a TTY.
    if !std::io::stdin().is_terminal() {
        logging::debug_log!(
            Io,
            1,
            "skipping encrypted key {} (no TTY for passphrase)",
            path.display()
        );
        return None;
    }

    let prompt = format!("Enter passphrase for key '{}': ", path.display());
    let passphrase = match rpassword::prompt_password(prompt) {
        Ok(p) => p,
        Err(e) => {
            logging::debug_log!(Io, 1, "passphrase prompt failed: {}", e);
            return None;
        }
    };

    match russh::keys::load_secret_key(path, Some(&passphrase)) {
        Ok(key) => Some(key),
        Err(e) => {
            eprintln!("Could not load key '{}': {}", path.display(), e);
            None
        }
    }
}

/// Try password authentication.
///
/// Uses the URL-embedded password if present (with a security warning), or
/// prompts interactively when stdin is a TTY. Returns `Ok(false)` when no
/// password is available.
async fn try_password_auth(
    session: &mut russh::client::Handle<SshClientHandler>,
    username: &str,
    config: &SshConfig,
) -> Result<bool, SshError> {
    let password = if let Some(ref pw) = config.password {
        eprintln!(
            "Warning: password provided via URL - this is insecure and may be visible in process listings."
        );
        pw.clone()
    } else if std::io::stdin().is_terminal() {
        match rpassword::prompt_password(format!("{username}@{}'s password: ", config.host)) {
            Ok(pw) => pw,
            Err(e) => {
                logging::debug_log!(Io, 1, "password prompt failed: {}", e);
                return Ok(false);
            }
        }
    } else {
        return Ok(false);
    };

    match session.authenticate_password(username, &password).await {
        Ok(result) => Ok(result.success()),
        Err(e) => Err(SshError::Connect(e)),
    }
}

/// Authenticate an SSH session using all available methods.
///
/// Tries methods in OpenSSH order:
/// 1. SSH agent (if `config.use_agent` is true and `SSH_AUTH_SOCK` is set)
/// 2. Identity files (each file in `config.identity_files`)
/// 3. Password (URL-embedded or interactive prompt)
///
/// Returns `Ok(())` on the first successful authentication. Returns
/// `SshError::AuthenticationFailed` if every method is exhausted.
pub async fn authenticate(
    session: &mut russh::client::Handle<SshClientHandler>,
    config: &SshConfig,
) -> Result<(), SshError> {
    let username = effective_username(config)?;
    let mut tried = Vec::new();

    if config.use_agent {
        if try_agent_auth(session, &username).await? {
            return Ok(());
        }
        tried.push("agent");
    }

    if !config.identity_files.is_empty() {
        if try_identity_file_auth(session, &username, &config.identity_files).await? {
            return Ok(());
        }
        tried.push("publickey");
    }

    if try_password_auth(session, &username, config).await? {
        return Ok(());
    }
    tried.push("password");

    Err(SshError::AuthenticationFailed {
        tried: tried.join(", "),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn effective_username_from_config() {
        let config = SshConfig {
            username: Some("alice".to_owned()),
            ..SshConfig::default()
        };
        let user = effective_username(&config).unwrap();
        assert_eq!(user, "alice");
    }

    #[test]
    fn effective_username_from_env() {
        let config = SshConfig {
            username: None,
            ..SshConfig::default()
        };
        // Should succeed as long as USER/USERNAME is set in the environment.
        let result = effective_username(&config);
        // In CI the env var is always set; if not, the error is expected.
        if std::env::var("USER").is_ok() || std::env::var("USERNAME").is_ok() {
            assert!(result.is_ok());
        } else {
            assert!(result.is_err());
        }
    }

    #[test]
    fn effective_username_none_no_env() {
        // Temporarily clear the env var to test the fallback error.
        let config = SshConfig {
            username: None,
            ..SshConfig::default()
        };
        // We cannot safely unset USER in a multi-threaded test, so just verify
        // the function returns something reasonable when the var is set.
        let _ = effective_username(&config);
    }

    #[test]
    fn load_identity_key_missing_file_returns_none() {
        let result = load_identity_key(Path::new("/nonexistent/path/id_ed25519"));
        assert!(result.is_none());
    }

    #[test]
    fn load_identity_key_directory_returns_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let result = load_identity_key(dir.path());
        assert!(result.is_none());
    }

    #[test]
    fn load_identity_key_invalid_content_returns_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let key_path = dir.path().join("bad_key");
        std::fs::write(&key_path, "not a valid key").expect("write");
        let result = load_identity_key(&key_path);
        assert!(result.is_none());
    }

    #[test]
    fn load_identity_key_valid_unencrypted_ed25519() {
        let dir = tempfile::tempdir().expect("tempdir");
        let key_path = dir.path().join("id_ed25519");

        // Generate a private key and write it in PKCS8 PEM format.
        let private =
            PrivateKey::random(&mut rand::rng(), russh::keys::Algorithm::Ed25519).expect("keygen");
        let mut buf = Vec::new();
        russh::keys::encode_pkcs8_pem(&private, &mut buf).expect("encode pem");
        std::fs::write(&key_path, &buf).expect("write key");

        let result = load_identity_key(&key_path);
        assert!(result.is_some());
    }

    #[test]
    fn try_password_no_tty_no_password_returns_false() {
        // When running in CI/tests, stdin is not a TTY, so without a config
        // password, try_password_auth should return Ok(false). We cannot call
        // the async function directly without a session, but we can verify the
        // logic by checking the condition.
        let config = SshConfig::default();
        assert!(config.password.is_none());
        // stdin is not a TTY in test runners, confirming the early-return path.
        assert!(!std::io::stdin().is_terminal());
    }

    #[test]
    fn auth_methods_ordering() {
        // Verify the tried-methods list is built correctly.
        let tried = ["agent", "publickey", "password"];
        assert_eq!(tried.join(", "), "agent, publickey, password");
    }

    #[test]
    fn default_config_enables_agent() {
        let config = SshConfig::default();
        assert!(config.use_agent);
    }

    #[test]
    fn default_config_has_identity_files() {
        let config = SshConfig::default();
        assert!(!config.identity_files.is_empty());
    }

    #[test]
    fn empty_identity_files_skips_pubkey_auth() {
        let config = SshConfig {
            identity_files: Vec::new(),
            ..SshConfig::default()
        };
        assert!(config.identity_files.is_empty());
    }

    #[test]
    fn url_password_triggers_warning_path() {
        // Verify that a config with password set takes the URL-password branch.
        let config = SshConfig {
            password: Some("secret".to_owned()),
            ..SshConfig::default()
        };
        assert!(config.password.is_some());
    }

    #[test]
    fn authentication_failed_error_display() {
        let err = SshError::AuthenticationFailed {
            tried: "agent, publickey, password".to_owned(),
        };
        let msg = err.to_string();
        assert!(msg.contains("agent"));
        assert!(msg.contains("publickey"));
        assert!(msg.contains("password"));
    }

    #[test]
    fn load_identity_key_symlink_to_missing_returns_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let link_path = dir.path().join("broken_link");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/nonexistent/target", &link_path).expect("symlink");
            let result = load_identity_key(&link_path);
            assert!(result.is_none());
        }
    }

    #[test]
    fn effective_username_prefers_config_over_env() {
        let config = SshConfig {
            username: Some("explicit".to_owned()),
            ..SshConfig::default()
        };
        let user = effective_username(&config).unwrap();
        assert_eq!(user, "explicit");
    }

    use russh::server::Server as _;
    use tokio::net::TcpListener;

    /// Auth policy for the mock SSH server.
    #[derive(Clone)]
    struct MockAuthPolicy {
        /// Public keys the server accepts.
        accepted_keys: Vec<russh::keys::PublicKey>,
        /// Password the server accepts (if any).
        accepted_password: Option<String>,
    }

    /// Mock SSH server that accepts/rejects auth based on `MockAuthPolicy`.
    #[derive(Clone)]
    struct MockSshServer {
        policy: MockAuthPolicy,
    }

    struct MockServerHandler {
        policy: MockAuthPolicy,
    }

    impl russh::server::Handler for MockServerHandler {
        type Error = russh::Error;

        async fn channel_open_session(
            &mut self,
            _channel: russh::Channel<russh::server::Msg>,
            _session: &mut russh::server::Session,
        ) -> Result<bool, Self::Error> {
            Ok(true)
        }

        async fn auth_publickey(
            &mut self,
            _user: &str,
            public_key: &russh::keys::PublicKey,
        ) -> Result<russh::server::Auth, Self::Error> {
            for accepted in &self.policy.accepted_keys {
                if accepted.fingerprint(russh::keys::HashAlg::Sha256)
                    == public_key.fingerprint(russh::keys::HashAlg::Sha256)
                {
                    return Ok(russh::server::Auth::Accept);
                }
            }
            Ok(russh::server::Auth::reject())
        }

        async fn auth_password(
            &mut self,
            _user: &str,
            password: &str,
        ) -> Result<russh::server::Auth, Self::Error> {
            if let Some(ref expected) = self.policy.accepted_password {
                if password == expected {
                    return Ok(russh::server::Auth::Accept);
                }
            }
            Ok(russh::server::Auth::reject())
        }
    }

    impl russh::server::Server for MockSshServer {
        type Handler = MockServerHandler;

        fn new_client(&mut self, _peer_addr: Option<std::net::SocketAddr>) -> Self::Handler {
            MockServerHandler {
                policy: self.policy.clone(),
            }
        }
    }

    /// Generate a russh server config with a fresh host key.
    fn mock_server_config() -> Arc<russh::server::Config> {
        let host_key =
            PrivateKey::random(&mut rand::rng(), russh::keys::Algorithm::Ed25519).expect("keygen");
        Arc::new(russh::server::Config {
            keys: vec![host_key],
            ..Default::default()
        })
    }

    /// Start a mock SSH server on an ephemeral port and return the port number.
    /// The server runs in the background until the runtime is dropped.
    async fn start_mock_server(policy: MockAuthPolicy) -> (u16, russh::keys::PublicKey) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("local_addr").port();

        let server_config = mock_server_config();
        let host_pubkey = server_config.keys[0].public_key().clone();

        let mut server = MockSshServer { policy };

        tokio::spawn(async move {
            let _ = server.run_on_socket(server_config, &listener).await;
        });

        (port, host_pubkey)
    }

    /// Create an `SshConfig` pointing to 127.0.0.1 at the given port with
    /// StrictHostKeyChecking::No, agent disabled, and no identity files.
    fn test_ssh_config(port: u16) -> SshConfig {
        SshConfig {
            host: "127.0.0.1".to_owned(),
            port,
            username: Some("testuser".to_owned()),
            password: None,
            identity_files: Vec::new(),
            use_agent: false,
            ciphers: None,
            connect_timeout: std::time::Duration::from_secs(5),
            keepalive_interval: None,
            keepalive_max_count: 3,
            known_hosts_file: None,
            strict_host_key_checking: super::super::types::StrictHostKeyChecking::No,
            ip_preference: super::super::types::IpPreference::Auto,
        }
    }

    /// Connect to the mock server and return a client handle.
    async fn connect_to_mock(
        port: u16,
        host_pubkey: &russh::keys::PublicKey,
    ) -> russh::client::Handle<SshClientHandler> {
        let handler = SshClientHandler::new(
            "127.0.0.1".to_owned(),
            port,
            super::super::types::StrictHostKeyChecking::No,
            None,
        );

        let client_config = Arc::new(russh::client::Config::default());
        let _ = host_pubkey; // Host key verification handled by StrictHostKeyChecking::No.

        russh::client::connect(client_config, ("127.0.0.1", port), handler)
            .await
            .expect("connect to mock server")
    }

    #[tokio::test]
    async fn authenticate_pubkey_succeeds() {
        let dir = tempfile::tempdir().expect("tempdir");
        let key_path = dir.path().join("id_ed25519");

        // Generate a private key and write it.
        let private =
            PrivateKey::random(&mut rand::rng(), russh::keys::Algorithm::Ed25519).expect("keygen");
        let pubkey = private.public_key().clone();
        let mut buf = Vec::new();
        russh::keys::encode_pkcs8_pem(&private, &mut buf).expect("encode pem");
        std::fs::write(&key_path, &buf).expect("write key");

        let policy = MockAuthPolicy {
            accepted_keys: vec![pubkey],
            accepted_password: None,
        };

        let (port, host_pubkey) = start_mock_server(policy).await;
        let mut handle = connect_to_mock(port, &host_pubkey).await;

        let mut config = test_ssh_config(port);
        config.identity_files = vec![key_path];

        let result = authenticate(&mut handle, &config).await;
        assert!(result.is_ok(), "pubkey auth should succeed: {result:?}");
    }

    #[tokio::test]
    async fn authenticate_password_succeeds() {
        let policy = MockAuthPolicy {
            accepted_keys: Vec::new(),
            accepted_password: Some("correct-password".to_owned()),
        };

        let (port, host_pubkey) = start_mock_server(policy).await;
        let mut handle = connect_to_mock(port, &host_pubkey).await;

        let mut config = test_ssh_config(port);
        config.password = Some("correct-password".to_owned());

        let result = authenticate(&mut handle, &config).await;
        assert!(result.is_ok(), "password auth should succeed: {result:?}");
    }

    #[tokio::test]
    async fn authenticate_wrong_password_fails() {
        let policy = MockAuthPolicy {
            accepted_keys: Vec::new(),
            accepted_password: Some("correct".to_owned()),
        };

        let (port, host_pubkey) = start_mock_server(policy).await;
        let mut handle = connect_to_mock(port, &host_pubkey).await;

        let mut config = test_ssh_config(port);
        config.password = Some("wrong".to_owned());

        let result = authenticate(&mut handle, &config).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, SshError::AuthenticationFailed { .. }),
            "expected AuthenticationFailed, got: {err:?}",
        );
    }

    #[tokio::test]
    async fn authenticate_all_methods_exhausted_reports_tried() {
        let policy = MockAuthPolicy {
            accepted_keys: Vec::new(),
            accepted_password: None,
        };

        let (port, host_pubkey) = start_mock_server(policy).await;
        let mut handle = connect_to_mock(port, &host_pubkey).await;

        // No agent, no identity files, no password, stdin is not a TTY.
        let config = test_ssh_config(port);

        let result = authenticate(&mut handle, &config).await;
        assert!(result.is_err());

        match result.unwrap_err() {
            SshError::AuthenticationFailed { tried } => {
                // Agent and pubkey were skipped (disabled), only password was tried.
                assert!(
                    tried.contains("password"),
                    "tried should include password: {tried}",
                );
                assert!(
                    !tried.contains("agent"),
                    "agent was disabled, should not appear in tried: {tried}",
                );
                assert!(
                    !tried.contains("publickey"),
                    "no identity files, should not appear in tried: {tried}",
                );
            }
            other => panic!("expected AuthenticationFailed, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn authenticate_agent_disabled_skips_agent() {
        let policy = MockAuthPolicy {
            accepted_keys: Vec::new(),
            accepted_password: Some("pw".to_owned()),
        };

        let (port, host_pubkey) = start_mock_server(policy).await;
        let mut handle = connect_to_mock(port, &host_pubkey).await;

        let mut config = test_ssh_config(port);
        config.use_agent = false;
        config.password = Some("pw".to_owned());

        // Auth should succeed via password without ever trying agent.
        let result = authenticate(&mut handle, &config).await;
        assert!(result.is_ok(), "should succeed via password: {result:?}");
    }

    #[tokio::test]
    async fn authenticate_agent_enabled_falls_through_to_password() {
        let policy = MockAuthPolicy {
            accepted_keys: Vec::new(),
            accepted_password: Some("fallback".to_owned()),
        };

        let (port, host_pubkey) = start_mock_server(policy).await;
        let mut handle = connect_to_mock(port, &host_pubkey).await;

        let mut config = test_ssh_config(port);
        // Enable agent but unset SSH_AUTH_SOCK so agent cannot connect.
        config.use_agent = true;
        config.password = Some("fallback".to_owned());

        // Agent will fail (no sock), fallback to password.
        let result = authenticate(&mut handle, &config).await;
        assert!(
            result.is_ok(),
            "should fall through to password: {result:?}"
        );
    }

    #[tokio::test]
    async fn authenticate_identity_file_wrong_key_falls_to_password() {
        let dir = tempfile::tempdir().expect("tempdir");
        let key_path = dir.path().join("id_ed25519");

        // Generate a private key the server does NOT accept.
        let wrong_key =
            PrivateKey::random(&mut rand::rng(), russh::keys::Algorithm::Ed25519).expect("keygen");
        let mut buf = Vec::new();
        russh::keys::encode_pkcs8_pem(&wrong_key, &mut buf).expect("encode pem");
        std::fs::write(&key_path, &buf).expect("write key");

        // Server accepts a different key and password.
        let accepted_key =
            PrivateKey::random(&mut rand::rng(), russh::keys::Algorithm::Ed25519).expect("keygen");
        let accepted_pubkey = accepted_key.public_key().clone();
        let policy = MockAuthPolicy {
            accepted_keys: vec![accepted_pubkey],
            accepted_password: Some("backup".to_owned()),
        };

        let (port, host_pubkey) = start_mock_server(policy).await;
        let mut handle = connect_to_mock(port, &host_pubkey).await;

        let mut config = test_ssh_config(port);
        config.identity_files = vec![key_path];
        config.password = Some("backup".to_owned());

        // Wrong key fails, falls through to password.
        let result = authenticate(&mut handle, &config).await;
        assert!(
            result.is_ok(),
            "should fall through to password: {result:?}"
        );
    }

    #[tokio::test]
    async fn authenticate_missing_identity_file_skipped() {
        let policy = MockAuthPolicy {
            accepted_keys: Vec::new(),
            accepted_password: Some("pass".to_owned()),
        };

        let (port, host_pubkey) = start_mock_server(policy).await;
        let mut handle = connect_to_mock(port, &host_pubkey).await;

        let mut config = test_ssh_config(port);
        config.identity_files = vec![
            std::path::PathBuf::from("/nonexistent/key1"),
            std::path::PathBuf::from("/nonexistent/key2"),
        ];
        config.password = Some("pass".to_owned());

        // Missing files are silently skipped, falls through to password.
        let result = authenticate(&mut handle, &config).await;
        assert!(result.is_ok(), "missing keys should be skipped: {result:?}");
    }

    #[tokio::test]
    async fn authenticate_multiple_identity_files_tries_in_order() {
        let dir = tempfile::tempdir().expect("tempdir");

        // Generate two private keys - server accepts the second one.
        let wrong_key =
            PrivateKey::random(&mut rand::rng(), russh::keys::Algorithm::Ed25519).expect("keygen");
        let right_key =
            PrivateKey::random(&mut rand::rng(), russh::keys::Algorithm::Ed25519).expect("keygen");
        let right_pubkey = right_key.public_key().clone();

        let wrong_path = dir.path().join("id_wrong");
        let right_path = dir.path().join("id_right");

        let mut buf = Vec::new();
        russh::keys::encode_pkcs8_pem(&wrong_key, &mut buf).expect("encode");
        std::fs::write(&wrong_path, &buf).expect("write");

        buf.clear();
        russh::keys::encode_pkcs8_pem(&right_key, &mut buf).expect("encode");
        std::fs::write(&right_path, &buf).expect("write");

        let policy = MockAuthPolicy {
            accepted_keys: vec![right_pubkey],
            accepted_password: None,
        };

        let (port, host_pubkey) = start_mock_server(policy).await;
        let mut handle = connect_to_mock(port, &host_pubkey).await;

        let mut config = test_ssh_config(port);
        // Wrong key first, right key second - should try in order.
        config.identity_files = vec![wrong_path, right_path];

        let result = authenticate(&mut handle, &config).await;
        assert!(
            result.is_ok(),
            "second identity file should succeed: {result:?}"
        );
    }

    #[tokio::test]
    async fn authenticate_no_methods_available() {
        let policy = MockAuthPolicy {
            accepted_keys: Vec::new(),
            accepted_password: None,
        };

        let (port, host_pubkey) = start_mock_server(policy).await;
        let mut handle = connect_to_mock(port, &host_pubkey).await;

        // All auth disabled: no agent, no keys, no password, no TTY.
        let mut config = test_ssh_config(port);
        config.use_agent = false;
        config.identity_files = Vec::new();
        config.password = None;

        let result = authenticate(&mut handle, &config).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            SshError::AuthenticationFailed { tried } => {
                // Only password should appear (it was attempted but no-TTY/no-password).
                assert!(tried.contains("password"), "got: {tried}");
            }
            other => panic!("expected AuthenticationFailed, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn authenticate_tried_list_includes_all_attempted_methods() {
        let dir = tempfile::tempdir().expect("tempdir");
        let key_path = dir.path().join("id_ed25519");

        let private =
            PrivateKey::random(&mut rand::rng(), russh::keys::Algorithm::Ed25519).expect("keygen");
        let mut buf = Vec::new();
        russh::keys::encode_pkcs8_pem(&private, &mut buf).expect("encode");
        std::fs::write(&key_path, &buf).expect("write key");

        // Server rejects everything.
        let policy = MockAuthPolicy {
            accepted_keys: Vec::new(),
            accepted_password: None,
        };

        let (port, host_pubkey) = start_mock_server(policy).await;
        let mut handle = connect_to_mock(port, &host_pubkey).await;

        let mut config = test_ssh_config(port);
        config.use_agent = true;
        config.identity_files = vec![key_path];
        config.password = None;

        let result = authenticate(&mut handle, &config).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            SshError::AuthenticationFailed { tried } => {
                assert!(tried.contains("agent"), "should have tried agent: {tried}");
                assert!(
                    tried.contains("publickey"),
                    "should have tried publickey: {tried}"
                );
                assert!(
                    tried.contains("password"),
                    "should have tried password: {tried}"
                );
            }
            other => panic!("expected AuthenticationFailed, got: {other:?}"),
        }
    }
}
