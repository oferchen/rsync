//! SSH authentication methods for the embedded transport.
//!
//! Tries authentication in OpenSSH order: agent, identity files, password.
//! The orchestrator function `authenticate()` drives the sequence and returns
//! `SshError::AuthenticationFailed` when all methods are exhausted.

use std::path::Path;
use std::sync::Arc;

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

/// Which SSH agent a resolved `IdentityAgent` value selects.
///
/// Mirrors OpenSSH's special-casing in `readconf.c`: the literal
/// `SSH_AUTH_SOCK` means "use the environment variable", `none` disables the
/// agent, and any other value is a Unix-domain socket path.
#[cfg(unix)]
#[derive(Debug, PartialEq, Eq)]
enum AgentSource<'a> {
    /// `IdentityAgent none` - agent authentication is disabled.
    Disabled,
    /// No directive or the literal `SSH_AUTH_SOCK` - use `$SSH_AUTH_SOCK`.
    Env,
    /// An explicit Unix-domain socket path.
    Socket(&'a str),
}

/// Classifies an `IdentityAgent` value into the agent source to connect to.
#[cfg(unix)]
fn classify_identity_agent(identity_agent: Option<&str>) -> AgentSource<'_> {
    match identity_agent {
        Some(value) if value.eq_ignore_ascii_case("none") => AgentSource::Disabled,
        Some("SSH_AUTH_SOCK") => AgentSource::Env,
        Some(path) => AgentSource::Socket(path),
        None => AgentSource::Env,
    }
}

/// Public keys of the configured identity files, forming the match set that
/// `IdentitiesOnly` restricts the agent to.
///
/// Each path contributes at most one key, derived the way OpenSSH derives it:
/// the path read as a public key file, else `<path>.pub`, else the cleartext
/// public half stored inside the private key file. A path that yields nothing
/// contributes no match, mirroring upstream leaving `identity_keys[i]` NULL -
/// `sshkey_equal()` then never matches it.
/// upstream: openssh/authfile.c:263 `sshkey_load_public()`,
/// openssh/ssh.c:2431 which fills `identity_keys[]` from it.
///
/// The private key is never decrypted here: upstream reaches the same set
/// without a passphrase, and prompting during key selection would be a new
/// interactive step in the middle of a transfer.
#[cfg(unix)]
fn configured_public_keys(
    identity_files: &[std::path::PathBuf],
) -> Vec<russh::keys::ssh_key::public::KeyData> {
    identity_files
        .iter()
        .filter_map(|path| {
            russh::keys::load_public_key(path)
                .ok()
                .or_else(|| {
                    let mut with_suffix = path.clone().into_os_string();
                    with_suffix.push(".pub");
                    russh::keys::load_public_key(Path::new(&with_suffix)).ok()
                })
                .map(|key| key.key_data().clone())
                .or_else(|| {
                    // The OpenSSH private-key format stores the public half in
                    // cleartext, so an encrypted key still yields its match key.
                    PrivateKey::read_openssh_file(path)
                        .ok()
                        .or_else(|| russh::keys::load_secret_key(path, None).ok())
                        .map(|key| key.public_key().key_data().clone())
                })
        })
        .collect()
}

/// Whether an agent identity may be offered to the server.
///
/// With `IdentitiesOnly` off - the upstream default - every agent key is
/// offered. With it on, only an agent key equal to one of the configured
/// identities is, compared on the public key blob rather than on a filename or
/// the agent's comment.
/// upstream: openssh/sshconnect2.c:1745 matches with `sshkey_equal()` and
/// openssh/sshconnect2.c:1753 keeps an unmatched agent key only when
/// `!options.identities_only`.
#[cfg(unix)]
fn agent_key_is_offered(
    identities_only: bool,
    configured: &[russh::keys::ssh_key::public::KeyData],
    agent_key: &russh::keys::PublicKey,
) -> bool {
    !identities_only || configured.iter().any(|key| key == agent_key.key_data())
}

/// Try authentication via the SSH agent.
///
/// Connects to the agent selected by `config.identity_agent` (an
/// `IdentityAgent` directive or `SSH_AUTH_SOCK` when unset), enumerates all
/// identities, and signs each via `authenticate_publickey_with()` until one
/// succeeds. Returns `Ok(true)` on success, `Ok(false)` when the agent is
/// disabled, unavailable, or no identity works.
///
/// When `config.identities_only` is set, an agent key is offered only if it
/// matches one of `config.identity_files`; see [`agent_key_is_offered`].
///
/// `russh::keys::agent::client::AgentClient::connect_env` is gated to
/// `cfg(unix)` upstream (Pageant / named-pipe support is a separate Windows
/// path we have not validated). On non-Unix targets this method short-circuits
/// to `Ok(false)` so the caller falls through to identity-file and password
/// auth; `IdentityAgent` is therefore honoured only on Unix.
#[cfg(unix)]
async fn try_agent_auth(
    session: &mut russh::client::Handle<SshClientHandler>,
    username: &str,
    config: &SshConfig,
) -> Result<bool, SshError> {
    use russh::keys::agent::client::AgentClient;

    let connect = match classify_identity_agent(config.identity_agent.as_deref()) {
        AgentSource::Disabled => {
            logging::debug_log!(Io, 1, "IdentityAgent=none: SSH agent disabled");
            return Ok(false);
        }
        AgentSource::Env => AgentClient::connect_env().await,
        AgentSource::Socket(path) => AgentClient::connect_uds(path).await,
    };
    let mut agent = match connect {
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

    // Built only when the restriction is on, so the default path reads no
    // identity files at all.
    let configured = if config.identities_only {
        configured_public_keys(&config.identity_files)
    } else {
        Vec::new()
    };

    for identity in identities {
        let pubkey = identity.public_key().into_owned();
        if !agent_key_is_offered(config.identities_only, &configured, &pubkey) {
            logging::debug_log!(
                Io,
                1,
                "IdentitiesOnly=yes: not offering agent key {} (no configured identity matches it)",
                identity.comment()
            );
            continue;
        }
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
    _config: &SshConfig,
) -> Result<bool, SshError> {
    logging::debug_log!(Io, 1, "SSH agent auth is not supported on this platform");
    Ok(false)
}

/// Try authentication using identity files (private keys).
///
/// For each path in `identity_files`, attempts to load the key and authenticate.
/// Encrypted keys prompt for a passphrase on the controlling terminal. Missing
/// or unreadable files are silently skipped.
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

/// Whether an interactive passphrase/password prompt can reach the user.
///
/// OpenSSH prompts on the *controlling terminal* (`/dev/tty`), not on stdin:
/// rsync always redirects stdin/stdout for its protocol pipe, so stdin is never
/// a tty during a transfer. Gating on `stdin().is_terminal()` therefore skips
/// the prompt on every real transfer. Mirror OpenSSH by probing `/dev/tty`
/// instead. upstream: OpenSSH `readpass.c:read_passphrase()` opens `/dev/tty`.
#[cfg(unix)]
fn has_controlling_terminal() -> bool {
    // Opening /dev/tty succeeds only when a controlling terminal exists
    // (fails in cron/daemon/detached contexts, where we skip the prompt).
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .is_ok()
}

/// Windows has no `/dev/tty`; fall back to the console check on stdin.
#[cfg(not(unix))]
fn has_controlling_terminal() -> bool {
    use is_terminal::IsTerminal;
    std::io::stdin().is_terminal()
}

/// Number of passphrase attempts before giving up on an encrypted key.
///
/// Mirrors OpenSSH's default of 3 tries in `sshconnect2.c`.
const MAX_PASSPHRASE_ATTEMPTS: usize = 3;

/// Load a private key from disk, prompting for passphrase if needed.
///
/// Returns `None` when the file is missing, unreadable, or the user declines
/// to enter a passphrase for an encrypted key.
fn load_identity_key(path: &Path) -> Option<PrivateKey> {
    load_identity_key_with(path, has_controlling_terminal())
}

/// Inner implementation of [`load_identity_key`] with the terminal-availability
/// decision injected, so the gate can be tested without a real `/dev/tty`.
fn load_identity_key_with(path: &Path, terminal_available: bool) -> Option<PrivateKey> {
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

    // Key is encrypted - prompt on the controlling terminal like OpenSSH, not
    // on stdin (which rsync redirects for its protocol pipe).
    if !terminal_available {
        logging::debug_log!(
            Io,
            1,
            "skipping encrypted key {} (no controlling terminal for passphrase)",
            path.display()
        );
        return None;
    }

    // Retry on a wrong passphrase, as OpenSSH does, up to a fixed limit.
    for _ in 0..MAX_PASSPHRASE_ATTEMPTS {
        let prompt = format!("Enter passphrase for key '{}': ", path.display());
        let passphrase = match rpassword::prompt_password(prompt) {
            Ok(p) => p,
            Err(e) => {
                logging::debug_log!(Io, 1, "passphrase prompt failed: {}", e);
                return None;
            }
        };

        match russh::keys::load_secret_key(path, Some(&passphrase)) {
            Ok(key) => return Some(key),
            Err(e) => {
                eprintln!("Could not load key '{}': {}", path.display(), e);
            }
        }
    }
    None
}

/// Try password authentication.
///
/// Uses the URL-embedded password if present (with a security warning), or
/// prompts interactively on the controlling terminal. Returns `Ok(false)` when
/// no password is available.
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
    } else if has_controlling_terminal() {
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
/// 1. SSH agent (if `config.use_agent` is true; `config.identity_agent`
///    selects the socket, defaulting to `SSH_AUTH_SOCK`; `identities_only`
///    restricts which of its keys may be offered)
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
        if try_agent_auth(session, &username, config).await? {
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
    use is_terminal::IsTerminal;

    #[cfg(unix)]
    #[test]
    fn classify_identity_agent_defaults_to_env() {
        // No directive means the agent comes from $SSH_AUTH_SOCK, preserving
        // the pre-IdentityAgent behaviour.
        assert_eq!(classify_identity_agent(None), AgentSource::Env);
    }

    #[cfg(unix)]
    #[test]
    fn classify_identity_agent_literal_sock_uses_env() {
        // OpenSSH treats the literal token SSH_AUTH_SOCK as "use the env var",
        // not as a filesystem path named SSH_AUTH_SOCK.
        assert_eq!(
            classify_identity_agent(Some("SSH_AUTH_SOCK")),
            AgentSource::Env
        );
    }

    #[cfg(unix)]
    #[test]
    fn classify_identity_agent_none_disables() {
        // IdentityAgent none must switch the agent off, not connect to a socket
        // literally named "none".
        assert_eq!(classify_identity_agent(Some("none")), AgentSource::Disabled);
        assert_eq!(classify_identity_agent(Some("None")), AgentSource::Disabled);
    }

    #[cfg(unix)]
    #[test]
    fn classify_identity_agent_path_is_socket() {
        // Any other value is a socket path the agent connects to via connect_uds.
        assert_eq!(
            classify_identity_agent(Some("/run/agent.sock")),
            AgentSource::Socket("/run/agent.sock")
        );
    }

    #[test]
    fn has_controlling_terminal_does_not_panic() {
        // The result depends on the environment (CI has no tty), but the probe
        // must never panic. On Unix it opens /dev/tty; on Windows it checks the
        // stdin console handle.
        let _ = has_controlling_terminal();
    }

    #[test]
    fn load_identity_key_encrypted_without_terminal_is_skipped() {
        // WHY: OpenSSH prompts for the passphrase on /dev/tty, not stdin,
        // because rsync redirects stdin/stdout for its protocol pipe. When no
        // controlling terminal is available (cron/daemon), the encrypted key
        // must be skipped rather than blocking on an unreachable prompt.
        let dir = tempfile::tempdir().expect("tempdir");
        let key_path = dir.path().join("id_ed25519");

        let private =
            PrivateKey::random(&mut rand::rng(), russh::keys::Algorithm::Ed25519).expect("keygen");
        let mut buf = Vec::new();
        russh::keys::encode_pkcs8_pem_encrypted(&private, b"hunter2", 16, &mut buf)
            .expect("encode encrypted");
        std::fs::write(&key_path, &buf).expect("write key");

        // terminal_available = false must take the skip branch, never prompting.
        let result = load_identity_key_with(&key_path, false);
        assert!(result.is_none());
    }

    #[test]
    fn load_identity_key_unencrypted_loads_regardless_of_terminal() {
        // An unencrypted key never needs a prompt, so the terminal gate must
        // not affect it: it loads whether or not a terminal is available.
        let dir = tempfile::tempdir().expect("tempdir");
        let key_path = dir.path().join("id_ed25519");

        let private =
            PrivateKey::random(&mut rand::rng(), russh::keys::Algorithm::Ed25519).expect("keygen");
        let mut buf = Vec::new();
        russh::keys::encode_pkcs8_pem(&private, &mut buf).expect("encode pem");
        std::fs::write(&key_path, &buf).expect("write key");

        assert!(load_identity_key_with(&key_path, false).is_some());
        assert!(load_identity_key_with(&key_path, true).is_some());
    }

    #[test]
    fn load_identity_key_missing_file_ignores_terminal() {
        assert!(load_identity_key_with(Path::new("/nonexistent/id"), true).is_none());
        assert!(load_identity_key_with(Path::new("/nonexistent/id"), false).is_none());
    }

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
        let config = SshConfig {
            username: None,
            ..SshConfig::default()
        };
        // Cannot safely unset USER in a multi-threaded test - just smoke-check
        // that the function returns without panicking when the var is set.
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

    #[cfg(unix)]
    #[test]
    fn load_identity_key_symlink_to_missing_returns_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let link_path = dir.path().join("broken_link");
        std::os::unix::fs::symlink("/nonexistent/target", &link_path).expect("symlink");
        let result = load_identity_key(&link_path);
        assert!(result.is_none());
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
        offered: OfferLog,
    }

    /// Every public key the client offered, in offer order.
    type OfferLog = Arc<std::sync::Mutex<Vec<russh::keys::PublicKey>>>;

    struct MockServerHandler {
        policy: MockAuthPolicy,
        offered: OfferLog,
    }

    impl russh::server::Handler for MockServerHandler {
        type Error = russh::Error;

        /// Records the `publickey` probe, which the client sends for every key
        /// it decides to offer before any signature is requested. Returning
        /// `Accept` reproduces the trait default, so the recorder changes no
        /// existing test's auth flow.
        async fn auth_publickey_offered(
            &mut self,
            _user: &str,
            public_key: &russh::keys::PublicKey,
        ) -> Result<russh::server::Auth, Self::Error> {
            self.offered
                .lock()
                .expect("offer log lock")
                .push(public_key.clone());
            Ok(russh::server::Auth::Accept)
        }

        async fn channel_open_session(
            &mut self,
            _channel: russh::Channel<russh::server::Msg>,
            reply: russh::server::ChannelOpenHandle,
            _session: &mut russh::server::Session,
        ) -> Result<(), Self::Error> {
            reply.accept().await;
            Ok(())
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
                offered: Arc::clone(&self.offered),
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
        let (port, host_pubkey, _offered) = start_recording_mock_server(policy).await;
        (port, host_pubkey)
    }

    /// Variant of [`start_mock_server`] that also hands back the log of every
    /// public key the client offers, so a test can assert on which keys were
    /// put on the wire rather than only on the auth outcome.
    async fn start_recording_mock_server(
        policy: MockAuthPolicy,
    ) -> (u16, russh::keys::PublicKey, OfferLog) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("local_addr").port();

        let server_config = mock_server_config();
        let host_pubkey = server_config.keys[0].public_key().clone();

        let offered: OfferLog = Arc::default();
        let mut server = MockSshServer {
            policy,
            offered: Arc::clone(&offered),
        };

        tokio::spawn(async move {
            let _ = server.run_on_socket(server_config, &listener).await;
        });

        (port, host_pubkey, offered)
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
            identities_only: false,
            use_agent: false,
            identity_agent: None,
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
    ///
    /// Uses an isolated temporary known_hosts file so the test never reads
    /// or conflicts with the system `~/.ssh/known_hosts`.
    async fn connect_to_mock(
        port: u16,
        host_pubkey: &russh::keys::PublicKey,
    ) -> russh::client::Handle<SshClientHandler> {
        // Create an empty known_hosts in a temp dir to avoid conflicts with
        // the system known_hosts (which may already have a different key for
        // 127.0.0.1 at this port from prior test runs or CI jobs).
        let kh_dir = tempfile::tempdir().expect("tempdir for known_hosts");
        let kh_path = kh_dir.path().join("known_hosts");

        let handler = SshClientHandler::new(
            "127.0.0.1".to_owned(),
            port,
            super::super::types::StrictHostKeyChecking::No,
            Some(kh_path),
        );

        let client_config = Arc::new(russh::client::Config::default());
        let _ = host_pubkey; // Host key verification handled by StrictHostKeyChecking::No.

        russh::client::connect(client_config, ("127.0.0.1", port), handler)
            .await
            .expect("connect to mock server")

        // kh_dir is dropped at end of scope, cleaning up the temp file.
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

    /// Appends an SSH `string`: a `uint32` length followed by the bytes.
    #[cfg(unix)]
    fn put_ssh_string(out: &mut Vec<u8>, bytes: &[u8]) {
        let len = u32::try_from(bytes.len()).expect("ssh string length");
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(bytes);
    }

    /// Splits the leading SSH `string` off `buf`, returning it and the remainder.
    #[cfg(unix)]
    fn take_ssh_string(buf: &[u8]) -> Option<(&[u8], &[u8])> {
        let (len_bytes, rest) = buf.split_at_checked(4)?;
        let len = u32::from_be_bytes(len_bytes.try_into().ok()?) as usize;
        rest.split_at_checked(len)
    }

    /// Comment the fake agent reports for every identity.
    ///
    /// Deliberately unlike any on-disk filename or `.pub` comment, so a test
    /// that matches an agent key against a configured identity can only be
    /// passing on the public key blob.
    #[cfg(unix)]
    const FAKE_AGENT_COMMENT: &str = "held-only-by-the-agent";

    /// Starts an in-process ssh-agent on a Unix socket and returns its
    /// temporary directory (kept alive by the caller) and socket path.
    ///
    /// Implements the two requests the embedded transport issues:
    /// `SSH_AGENTC_REQUEST_IDENTITIES` (11) and `SSH_AGENTC_SIGN_REQUEST`
    /// (13). Building an agent rather than consulting `$SSH_AUTH_SOCK` keeps
    /// the agent leg of `authenticate()` exercised on every machine, so these
    /// cells never degrade into a silent skip.
    #[cfg(unix)]
    async fn start_fake_agent(keys: Vec<PrivateKey>) -> (tempfile::TempDir, String) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        const REQUEST_IDENTITIES: u8 = 11;
        const IDENTITIES_ANSWER: u8 = 12;
        const SIGN_REQUEST: u8 = 13;
        const AGENT_FAILURE: u8 = 5;

        let dir = tempfile::tempdir().expect("tempdir for agent socket");
        let sock_path = dir.path().join("s");
        let listener = tokio::net::UnixListener::bind(&sock_path).expect("bind agent socket");
        let sock = sock_path.to_string_lossy().into_owned();

        let mut answer = vec![IDENTITIES_ANSWER];
        answer.extend_from_slice(&u32::try_from(keys.len()).expect("key count").to_be_bytes());
        for key in &keys {
            let blob = key.public_key().to_bytes().expect("agent public key blob");
            put_ssh_string(&mut answer, &blob);
            put_ssh_string(&mut answer, FAKE_AGENT_COMMENT.as_bytes());
        }

        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let answer = answer.clone();
                let keys = keys.clone();
                tokio::spawn(async move {
                    loop {
                        let mut len_buf = [0u8; 4];
                        if stream.read_exact(&mut len_buf).await.is_err() {
                            return;
                        }
                        let mut request = vec![0u8; u32::from_be_bytes(len_buf) as usize];
                        if stream.read_exact(&mut request).await.is_err() {
                            return;
                        }
                        let reply = match request.split_first() {
                            Some((&REQUEST_IDENTITIES, _)) => answer.clone(),
                            Some((&SIGN_REQUEST, body)) => {
                                fake_agent_sign(&keys, body).unwrap_or_else(|| vec![AGENT_FAILURE])
                            }
                            _ => vec![AGENT_FAILURE],
                        };
                        let mut framed = u32::try_from(reply.len())
                            .expect("reply length")
                            .to_be_bytes()
                            .to_vec();
                        framed.extend_from_slice(&reply);
                        if stream.write_all(&framed).await.is_err() {
                            return;
                        }
                    }
                });
            }
        });

        (dir, sock)
    }

    /// Answers an `SSH_AGENTC_SIGN_REQUEST` body (`string key_blob`,
    /// `string data`, `uint32 flags`) with an `SSH_AGENT_SIGN_RESPONSE`.
    #[cfg(unix)]
    fn fake_agent_sign(keys: &[PrivateKey], body: &[u8]) -> Option<Vec<u8>> {
        let (blob, rest) = take_ssh_string(body)?;
        let (data, _) = take_ssh_string(rest)?;
        let key = keys
            .iter()
            .find(|k| k.public_key().to_bytes().is_ok_and(|b| b == blob))?;
        let signature: russh::keys::ssh_key::Signature =
            russh::keys::signature::Signer::try_sign(key, data).ok()?;
        let mut signature_blob = Vec::new();
        russh::keys::ssh_encoding::Encode::encode(&signature, &mut signature_blob).ok()?;
        const SIGN_RESPONSE: u8 = 14;
        let mut reply = vec![SIGN_RESPONSE];
        put_ssh_string(&mut reply, &signature_blob);
        Some(reply)
    }

    /// Writes `key` to `path` as an unencrypted private key file.
    #[cfg(unix)]
    fn write_private_key(path: &Path, key: &PrivateKey) {
        let mut buf = Vec::new();
        russh::keys::encode_pkcs8_pem(key, &mut buf).expect("encode private key");
        std::fs::write(path, &buf).expect("write private key");
    }

    /// Shared fixture for the `IdentitiesOnly` cells: an in-process agent
    /// holding `agent_key`, a mock server that records every offer and accepts
    /// nothing, and a config whose `IdentityAgent` points at that agent.
    ///
    /// The returned `TempDir` owns the agent socket and must outlive the call
    /// to `authenticate()`.
    #[cfg(unix)]
    async fn identities_only_fixture(
        agent_key: &PrivateKey,
    ) -> (
        russh::client::Handle<SshClientHandler>,
        SshConfig,
        OfferLog,
        tempfile::TempDir,
    ) {
        let (agent_dir, agent_sock) = start_fake_agent(vec![agent_key.clone()]).await;
        let policy = MockAuthPolicy {
            accepted_keys: Vec::new(),
            accepted_password: None,
        };
        let (port, host_pubkey, offered) = start_recording_mock_server(policy).await;
        let handle = connect_to_mock(port, &host_pubkey).await;

        let mut config = test_ssh_config(port);
        config.use_agent = true;
        config.identity_agent = Some(agent_sock);

        (handle, config, offered, agent_dir)
    }

    /// Whether the client offered `key`, compared on the public key blob so
    /// the comment the agent attaches cannot influence the answer.
    #[cfg(unix)]
    fn offered_contains(log: &OfferLog, key: &russh::keys::PublicKey) -> bool {
        log.lock()
            .expect("offer log lock")
            .iter()
            .any(|candidate| candidate.key_data() == key.key_data())
    }

    /// Generates a fresh Ed25519 key.
    #[cfg(unix)]
    fn fresh_key() -> PrivateKey {
        PrivateKey::random(&mut rand::rng(), russh::keys::Algorithm::Ed25519).expect("keygen")
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn identities_only_withholds_an_agent_key_outside_the_configured_identities() {
        // The point of `IdentitiesOnly` is to stop a loaded - often forwarded -
        // agent from parading every key it holds at a host the operator scoped
        // to one identity. Clearing the identity-file list does not do that:
        // the restriction has to reach the agent's key list.
        // upstream: openssh/sshconnect2.c:1753 appends an agent key that
        // matched no configured identity only when `!options.identities_only`.
        let agent_key = fresh_key();
        let configured = fresh_key();

        let dir = tempfile::tempdir().expect("tempdir");
        let configured_path = dir.path().join("configured_ed25519");
        write_private_key(&configured_path, &configured);

        let (mut handle, mut config, offered, _agent_dir) =
            identities_only_fixture(&agent_key).await;
        config.identity_files = vec![configured_path];
        config.identities_only = true;

        let _ = authenticate(&mut handle, &config).await;

        assert!(
            !offered_contains(&offered, agent_key.public_key()),
            "IdentitiesOnly=yes offered an agent key that is not a configured identity",
        );
        // Non-vacuity: the fixture really reached the offer path, and the
        // restriction is not a blanket refusal to offer anything.
        assert!(
            offered_contains(&offered, configured.public_key()),
            "the configured identity itself was never offered - fixture did not reach the offer path",
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn agent_key_outside_the_configured_identities_is_offered_by_default() {
        // Control for the cell above. With `IdentitiesOnly` off - the upstream
        // default (openssh/readconf.c:2905) - the same agent key must still be
        // offered, so a fix that simply stopped offering agent keys fails here.
        let agent_key = fresh_key();
        let configured = fresh_key();

        let dir = tempfile::tempdir().expect("tempdir");
        let configured_path = dir.path().join("configured_ed25519");
        write_private_key(&configured_path, &configured);

        let (mut handle, mut config, offered, _agent_dir) =
            identities_only_fixture(&agent_key).await;
        config.identity_files = vec![configured_path];
        config.identities_only = false;

        let _ = authenticate(&mut handle, &config).await;

        assert!(
            offered_contains(&offered, agent_key.public_key()),
            "IdentitiesOnly is off, so the agent key must still be offered",
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn identities_only_still_offers_an_agent_key_matching_a_configured_identity() {
        // The match key is the public key blob, not the filename and not the
        // agent's comment: the agent reports FAKE_AGENT_COMMENT while the
        // configured identity is a path with no relation to it.
        // upstream: openssh/sshconnect2.c:1745 `sshkey_equal()`.
        let agent_key = fresh_key();

        let dir = tempfile::tempdir().expect("tempdir");
        let configured_path = dir.path().join("unrelated_name");
        write_private_key(&configured_path, &agent_key);

        let (mut handle, mut config, offered, _agent_dir) =
            identities_only_fixture(&agent_key).await;
        config.identity_files = vec![configured_path];
        config.identities_only = true;

        let _ = authenticate(&mut handle, &config).await;

        assert!(
            offered_contains(&offered, agent_key.public_key()),
            "an agent key that IS a configured identity must still be offered",
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn identities_only_matches_a_configured_identity_that_is_public_key_only() {
        // The canonical agent deployment: the private key lives in the agent
        // and only `<IdentityFile>.pub` is on disk. Deriving the match key from
        // private key files alone would withhold every key in that setup.
        // upstream: openssh/authfile.c:263 `sshkey_load_public()` tries the
        // path, then `<path>.pub`, then the private file's cleartext public
        // half.
        let agent_key = fresh_key();

        let dir = tempfile::tempdir().expect("tempdir");
        let configured_path = dir.path().join("agent_only_ed25519");
        let public_path = dir.path().join("agent_only_ed25519.pub");
        std::fs::write(
            &public_path,
            agent_key
                .public_key()
                .to_openssh()
                .expect("encode public key"),
        )
        .expect("write public key");
        assert!(
            !configured_path.exists(),
            "the private key must be absent for this cell to mean anything",
        );

        let (mut handle, mut config, offered, _agent_dir) =
            identities_only_fixture(&agent_key).await;
        config.identity_files = vec![configured_path];
        config.identities_only = true;

        let _ = authenticate(&mut handle, &config).await;

        assert!(
            offered_contains(&offered, agent_key.public_key()),
            "a configured identity present only as a .pub file must still match the agent key",
        );
    }
}
