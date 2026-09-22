//! SSH client handler implementing host key verification.
//!
//! Provides `SshClientHandler` which implements `russh::client::Handler`
//! with configurable host key checking behavior mirroring OpenSSH's
//! `StrictHostKeyChecking` option.

use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use base64::Engine as _;
use hmac::{Hmac, KeyInit, Mac};
use is_terminal::IsTerminal;
use russh::keys::{HashAlg, PublicKey, PublicKeyOrCertificate, known_hosts};
use sha1::Sha1;

use super::error::SshError;
use super::types::StrictHostKeyChecking;

/// HMAC-SHA1, the MAC OpenSSH uses for hashed `known_hosts` entries
/// (openssh/hostfile.c `host_hash`, `HASH_MAGIC "|1|"`).
type HmacSha1 = Hmac<Sha1>;

/// How one name (hostname or IP) verified against the configured sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NameVerdict {
    /// A known_hosts entry recorded this exact key for the name.
    Known,
    /// The name is recorded with a DIFFERENT key at the given line - a
    /// potential MITM, refused under every policy.
    Changed(usize),
    /// No entry recorded the name.
    Unknown,
}

/// The host-key verification inputs resolved from an `SshConfig`, mirroring
/// the OpenSSH `known_hosts` decision surface: the ordered file list, the
/// learn target, hashing, the lookup alias, the optional IP identity and the
/// revoked-keys file.
pub struct HostKeyOptions {
    /// The `StrictHostKeyChecking` policy.
    pub strict_host_key_checking: StrictHostKeyChecking,
    /// Files consulted for verification, in order. A `None` element means
    /// russh's default `~/.ssh/known_hosts` location.
    pub check_sources: Vec<Option<PathBuf>>,
    /// Where a newly learned key is written: `Some(target)` where `target`
    /// is a file path (or `None` for russh's default). The outer `None`
    /// disables learning to a file (`UserKnownHostsFile none`).
    pub learn_target: Option<Option<PathBuf>>,
    /// Whether a newly learned entry is written hashed (`|1|salt|hash`).
    pub hash_known_hosts: bool,
    /// The name looked up and stored: `HostKeyAlias` if set, else the host.
    pub lookup_name: String,
    /// The connection host, used for diagnostics and error context.
    pub host: String,
    /// The connection port.
    pub port: u16,
    /// The resolved server IP, present only when `CheckHostIP` is on and the
    /// address is known (a direct dial). `None` skips the IP identity.
    pub server_ip: Option<String>,
    /// The `RevokedHostKeys` file; a listed server key is rejected.
    pub revoked_host_keys: Option<PathBuf>,
}

/// SSH client handler that verifies server host keys against known_hosts files.
///
/// Behavior depends on the configured `StrictHostKeyChecking` mode:
/// - `Yes` - reject unknown or changed keys immediately.
/// - `Ask` - prompt the user on a TTY; reject if no TTY is available.
/// - `No` - accept unknown keys with a warning (changed keys are always rejected).
///
/// The verification surface mirrors OpenSSH: an ordered list of known_hosts
/// files (`UserKnownHostsFile`/`GlobalKnownHostsFile`), an optional
/// `HostKeyAlias` used in place of the hostname, `HashKnownHosts` for hashed
/// entries, `CheckHostIP` for treating the resolved IP as a second identity,
/// and a `RevokedHostKeys` file whose keys are refused outright.
pub struct SshClientHandler {
    strict_host_key_checking: StrictHostKeyChecking,
    check_sources: Vec<Option<PathBuf>>,
    learn_target: Option<Option<PathBuf>>,
    hash_known_hosts: bool,
    lookup_name: String,
    host: String,
    port: u16,
    server_ip: Option<String>,
    revoked_host_keys: Option<PathBuf>,
}

impl SshClientHandler {
    /// Create a handler that consults a single known_hosts file.
    ///
    /// When `known_hosts_file` is `None`, the default `~/.ssh/known_hosts`
    /// location is used (via `russh::keys::known_hosts::check_known_hosts`).
    /// The host is its own lookup name; hashing, the IP identity and the
    /// revoked list are off. Richer configurations use [`Self::with_options`].
    pub fn new(
        host: String,
        port: u16,
        strict_host_key_checking: StrictHostKeyChecking,
        known_hosts_file: Option<PathBuf>,
    ) -> Self {
        Self {
            strict_host_key_checking,
            check_sources: vec![known_hosts_file.clone()],
            learn_target: Some(known_hosts_file),
            hash_known_hosts: false,
            lookup_name: host.clone(),
            host,
            port,
            server_ip: None,
            revoked_host_keys: None,
        }
    }

    /// Create a handler from the full resolved [`HostKeyOptions`].
    pub fn with_options(opts: HostKeyOptions) -> Self {
        Self {
            strict_host_key_checking: opts.strict_host_key_checking,
            check_sources: opts.check_sources,
            learn_target: opts.learn_target,
            hash_known_hosts: opts.hash_known_hosts,
            lookup_name: opts.lookup_name,
            host: opts.host,
            port: opts.port,
            server_ip: opts.server_ip,
            revoked_host_keys: opts.revoked_host_keys,
        }
    }

    /// Verify the server key against the revoked list, the known_hosts files,
    /// and - when `CheckHostIP` is on - the resolved IP.
    ///
    /// Returns `Ok(true)` to accept, `Ok(false)` to reject, or an error for a
    /// changed or revoked key. A revoked key is refused before any
    /// known_hosts check and under every policy (openssh/sshconnect.c:1050).
    fn verify_host_key(&self, server_public_key: &PublicKey) -> Result<bool, SshError> {
        if let Some(ref path) = self.revoked_host_keys
            && key_is_revoked(path, server_public_key)
        {
            return Err(SshError::HostKeyRevoked {
                host: self.host.clone(),
            });
        }

        let mut verdict = self.verify_name(&self.lookup_name, server_public_key);
        // `CheckHostIP`: the resolved IP is a second recognised identity.
        // A hostname match still governs (upstream treats a differing IP
        // entry as a warning, not a hard failure, openssh/sshconnect.c:1233),
        // so `Known` wins over `Changed` in the merge.
        if let Some(ref ip) = self.server_ip {
            verdict = merge_verdicts(verdict, self.verify_name(ip, server_public_key));
        }

        match verdict {
            NameVerdict::Known => Ok(true),
            NameVerdict::Changed(line) => {
                emit_key_changed_warning(&self.host, self.port, server_public_key, line);
                Err(SshError::HostKeyMismatch {
                    host: self.host.clone(),
                })
            }
            NameVerdict::Unknown => self.handle_unknown_host(server_public_key),
        }
    }

    /// Verify one name against every configured source, first match wins.
    fn verify_name(&self, name: &str, key: &PublicKey) -> NameVerdict {
        let mut changed = None;
        for source in &self.check_sources {
            match self.check_one(name, key, source.as_deref()) {
                NameVerdict::Known => return NameVerdict::Known,
                NameVerdict::Changed(line) => changed = changed.or(Some(line)),
                NameVerdict::Unknown => {}
            }
        }
        changed.map_or(NameVerdict::Unknown, NameVerdict::Changed)
    }

    /// Check `name`'s key against one source (`None` = russh default file).
    fn check_one(&self, name: &str, key: &PublicKey, file: Option<&Path>) -> NameVerdict {
        let result = match file {
            Some(path) => known_hosts::check_known_hosts_path(name, self.port, key, path),
            None => known_hosts::check_known_hosts(name, self.port, key),
        };
        match result {
            Ok(true) => NameVerdict::Known,
            Ok(false) => NameVerdict::Unknown,
            Err(russh::keys::Error::KeyChanged { line }) => NameVerdict::Changed(line),
            Err(e) => {
                // File-not-found or parse errors - treat as unknown host.
                logging::debug_log!(
                    Io,
                    1,
                    "known_hosts check error for {}:{}: {}",
                    name,
                    self.port,
                    e
                );
                NameVerdict::Unknown
            }
        }
    }

    /// Decide whether to accept an unknown host key based on the configured policy.
    fn handle_unknown_host(&self, server_public_key: &PublicKey) -> Result<bool, SshError> {
        match self.strict_host_key_checking {
            StrictHostKeyChecking::Yes => Err(SshError::UnknownHost {
                host: self.host.clone(),
            }),
            // `no` and `accept-new` share the unknown-host arm: both learn
            // the key without prompting. Upstream reaches this arm the same
            // way, by testing only for YES and ASK and letting OFF and NEW
            // fall through together (openssh/sshconnect.c:1169-1181). They diverge
            // only on a CHANGED key, which never reaches here - that path is
            // the Changed branch in verify_host_key, which refuses under
            // every policy.
            StrictHostKeyChecking::No | StrictHostKeyChecking::AcceptNew => {
                eprintln!(
                    "Warning: Permanently added '{}' ({}) to the list of known hosts.",
                    self.host,
                    server_public_key.algorithm(),
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

        let fingerprint = server_public_key.fingerprint(HashAlg::Sha256);
        eprint!(
            "The authenticity of host '{}' ({}) can't be established.\n\
             {} key fingerprint is {}.\n\
             Are you sure you want to continue connecting (yes/no)? ",
            self.host,
            format_host_port(&self.host, self.port),
            server_public_key.algorithm(),
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
                server_public_key.algorithm(),
            );
            self.learn_host_key(server_public_key)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Append the server's public key to the learn target, under the lookup
    /// name and - when `CheckHostIP` is on - the resolved IP as well.
    ///
    /// A `learn_target` of `None` (an explicit `UserKnownHostsFile none`)
    /// disables writing entirely, matching upstream, which has no user file
    /// to append to.
    fn learn_host_key(&self, server_public_key: &PublicKey) -> Result<(), SshError> {
        let Some(target) = self.learn_target.as_ref() else {
            return Ok(());
        };
        self.learn_name(&self.lookup_name, server_public_key, target.as_deref())?;
        if let Some(ref ip) = self.server_ip {
            self.learn_name(ip, server_public_key, target.as_deref())?;
        }
        Ok(())
    }

    /// Write one `name -> key` entry to `file` (`None` = russh default),
    /// hashed when `HashKnownHosts` is set and a concrete path is available.
    fn learn_name(&self, name: &str, key: &PublicKey, file: Option<&Path>) -> Result<(), SshError> {
        match (self.hash_known_hosts, file) {
            (true, Some(path)) => learn_hashed(name, self.port, key, path),
            (false, Some(path)) => known_hosts::learn_known_hosts_path(name, self.port, key, path)
                .map_err(|e| SshError::Io(std::io::Error::other(e.to_string()))),
            // No concrete path (russh default): russh's own writer, which is
            // plaintext-only, so hashing cannot be honoured here.
            (_, None) => known_hosts::learn_known_hosts(name, self.port, key)
                .map_err(|e| SshError::Io(std::io::Error::other(e.to_string()))),
        }
    }
}

/// Merge a hostname verdict with an IP verdict: a `Known` on either wins,
/// then a `Changed`, else `Unknown`. Mirrors upstream treating a hostname
/// match as authoritative even when the IP entry differs
/// (openssh/sshconnect.c:1233-1249).
fn merge_verdicts(host: NameVerdict, ip: NameVerdict) -> NameVerdict {
    match (host, ip) {
        (NameVerdict::Known, _) | (_, NameVerdict::Known) => NameVerdict::Known,
        (NameVerdict::Changed(line), _) | (_, NameVerdict::Changed(line)) => {
            NameVerdict::Changed(line)
        }
        _ => NameVerdict::Unknown,
    }
}

/// Whether `key` appears in the `RevokedHostKeys` file.
///
/// The file is read as a list of public keys, one per line, in either
/// `authorized_keys` form (`<algo> <base64> [comment]`) or `known_hosts`
/// form (`<host-field> <algo> <base64>`); a leading host field is stripped
/// and the key retried. Comparison is on key material only, so a differing
/// comment or host field does not hide a revocation. Comment lines and blanks
/// are ignored. A missing or unreadable file revokes nothing.
///
/// upstream reads a KRL or a plaintext key list here (openssh/sshconnect.c:
/// 1050 `sshkey_check_revoked`); oc supports the plaintext form.
fn key_is_revoked(path: &Path, key: &PublicKey) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    let target = key.key_data();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(parsed) = parse_revoked_key(line)
            && parsed.key_data() == target
        {
            return true;
        }
    }
    false
}

/// Parse one revoked-list line into a public key, tolerating a leading
/// `known_hosts` host field.
fn parse_revoked_key(line: &str) -> Option<PublicKey> {
    if let Ok(key) = PublicKey::from_openssh(line) {
        return Some(key);
    }
    let (_host, rest) = line.split_once(char::is_whitespace)?;
    PublicKey::from_openssh(rest.trim()).ok()
}

/// Append a hashed `known_hosts` entry (`|1|salt|hash <algo> <base64>`),
/// the format `russh::keys::known_hosts` reads back and OpenSSH writes under
/// `HashKnownHosts yes` (openssh/hostfile.c `host_hash`, `HASH_MAGIC "|1|"`,
/// `HASH_DELIM '|'`). The hash is `HMAC-SHA1(salt, host_port)` and both salt
/// and hash are base64-encoded, matching russh's reader (`Hmac::<Sha1>` over
/// the `[host]:port` form).
fn learn_hashed(name: &str, port: u16, key: &PublicKey, path: &Path) -> Result<(), SshError> {
    let host_port = if port == 22 {
        name.to_owned()
    } else {
        format!("[{name}]:{port}")
    };
    let salt: [u8; 20] = rand::random();
    let mut mac = HmacSha1::new_from_slice(&salt).expect("HMAC accepts any key length");
    mac.update(host_port.as_bytes());
    let digest = mac.finalize().into_bytes();
    let b64 = base64::engine::general_purpose::STANDARD;
    let hashed_host = format!("|1|{}|{}", b64.encode(salt), b64.encode(digest));
    let openssh = key
        .to_openssh()
        .map_err(|e| SshError::Io(std::io::Error::other(e.to_string())))?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .open(path)?;
    // Prepend a newline when the file does not already end in one, mirroring
    // russh's `learn_known_hosts_path` so an entry never joins the last line.
    let mut last = [0u8; 1];
    let mut ends_in_newline = false;
    if file.seek(SeekFrom::End(-1)).is_ok() {
        file.read_exact(&mut last)?;
        ends_in_newline = last[0] == b'\n';
    }
    file.seek(SeekFrom::End(0))?;
    if !ends_in_newline {
        file.write_all(b"\n")?;
    }
    writeln!(file, "{hashed_host} {openssh}")?;
    Ok(())
}

impl russh::client::Handler for SshClientHandler {
    type Error = SshError;

    /// Verify the key the server authenticated with.
    ///
    /// russh hands over either a plain public key or an OpenSSH host
    /// certificate. Only the plain key can be checked against `known_hosts`.
    /// A certificate is refused: it stands in place of the key check rather
    /// than adding to it, so honouring one requires a trusted certificate
    /// authority, and unwrapping the key it carries would verify a key the
    /// client was never told to trust.
    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKeyOrCertificate,
    ) -> Result<bool, SshError> {
        match server_public_key {
            PublicKeyOrCertificate::PublicKey { key, .. } => self.verify_host_key(key),
            PublicKeyOrCertificate::Certificate(_) => Err(SshError::HostCertificateUnsupported {
                host: self.host.clone(),
            }),
        }
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
        key.algorithm(),
        key.fingerprint(HashAlg::Sha256),
        line,
        format_host_port(host, port),
    );
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use russh::client::Handler;
    use russh::keys::ssh_key::certificate::{Builder, CertType};
    use russh::keys::{Algorithm, PrivateKey};

    use super::*;

    /// A host certificate is refused, never unwrapped to the key it carries.
    ///
    /// The fixture is deliberately the one a naive implementation passes: the
    /// host's plain key IS in `known_hosts`, so unwrapping the certificate and
    /// checking the key inside would return `Ok(true)`. Upstream russh states
    /// the rule at its call site - a certificate replaces the key check rather
    /// than adding to it, so the key it carries was never something the client
    /// was told to trust.
    #[tokio::test]
    async fn a_host_certificate_is_refused_rather_than_unwrapped() {
        let ca = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).expect("ca keypair");
        let host = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).expect("host keypair");

        let mut builder = Builder::new_with_random_nonce(
            &mut rand::rng(),
            host.public_key().key_data().clone(),
            0,
            u64::MAX,
        )
        .expect("certificate builder");
        builder.cert_type(CertType::Host).expect("cert type");
        builder
            .valid_principal("cert.example")
            .expect("valid principal");
        let cert = builder.sign(&ca).expect("sign certificate");

        let dir = tempfile::tempdir().expect("tempdir");
        let kh_path = dir.path().join("known_hosts");
        std::fs::File::create(&kh_path).expect("create");
        known_hosts::learn_known_hosts_path("cert.example", 22, host.public_key(), &kh_path)
            .expect("learn host key");

        let mut handler = SshClientHandler::new(
            "cert.example".into(),
            22,
            StrictHostKeyChecking::Yes,
            Some(kh_path),
        );

        // Non-vacuity: the same entry point accepts the plain key, so a refusal
        // below cannot be an artefact of the fixture failing to verify at all.
        let plain = handler
            .check_server_key(&PublicKeyOrCertificate::PublicKey {
                key: host.public_key().clone(),
                hash_alg: None,
            })
            .await;
        assert!(
            matches!(plain, Ok(true)),
            "the plain host key must still verify: {plain:?}",
        );

        let err = handler
            .check_server_key(&PublicKeyOrCertificate::Certificate(cert))
            .await
            .expect_err("a host certificate must be refused");
        assert!(
            matches!(err, SshError::HostCertificateUnsupported { ref host } if host == "cert.example"),
            "expected HostCertificateUnsupported, got: {err:?}",
        );
    }

    /// Verify handler creation with each strict host key checking mode. The
    /// single-file `new` maps the file into one check source plus the same
    /// learn target, its host doubling as the lookup name.
    #[test]
    fn handler_creation_modes() {
        let h = SshClientHandler::new("example.com".into(), 22, StrictHostKeyChecking::Yes, None);
        assert_eq!(h.strict_host_key_checking, StrictHostKeyChecking::Yes);
        assert_eq!(h.host, "example.com");
        assert_eq!(h.lookup_name, "example.com");
        assert_eq!(h.port, 22);
        assert_eq!(h.check_sources, vec![None]);
        assert_eq!(h.learn_target, Some(None));
        assert!(!h.hash_known_hosts);
        assert!(h.server_ip.is_none());
        assert!(h.revoked_host_keys.is_none());

        let h = SshClientHandler::new(
            "example.com".into(),
            2222,
            StrictHostKeyChecking::No,
            Some(PathBuf::from("/tmp/kh")),
        );
        assert_eq!(h.strict_host_key_checking, StrictHostKeyChecking::No);
        assert_eq!(h.port, 2222);
        assert_eq!(h.check_sources, vec![Some(PathBuf::from("/tmp/kh"))]);
        assert_eq!(h.learn_target, Some(Some(PathBuf::from("/tmp/kh"))));
    }

    /// Verify host:port formatting omits port 22.
    #[test]
    fn format_host_port_display() {
        assert_eq!(format_host_port("example.com", 22), "example.com");
        assert_eq!(format_host_port("example.com", 2222), "[example.com]:2222");
    }

    /// Generate an Ed25519 public key for tests.
    fn test_ed25519_pubkey() -> PublicKey {
        let private = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)
            .expect("ed25519 keypair generation");
        private.public_key().clone()
    }

    /// Known host entry matches - verification should succeed.
    #[test]
    fn known_host_match() {
        let pubkey = test_ed25519_pubkey();
        let dir = tempfile::tempdir().expect("tempdir");
        let kh_path = dir.path().join("known_hosts");

        known_hosts::learn_known_hosts_path("testhost.example", 22, &pubkey, &kh_path)
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
        let check = known_hosts::check_known_hosts_path("auto.example", 22, &pubkey, &kh_path);
        assert!(check.is_ok());
        assert!(check.unwrap());
    }

    /// An unattended run under `accept-new` must learn an unknown key without
    /// reaching the prompt, and `ask` on the same fixture must not.
    ///
    /// The control is what makes this behavioural rather than structural:
    /// `prompt_user` refuses when stdin is not a terminal (which it never is
    /// under the test runner), so `Ask` fails on exactly the fixture
    /// `AcceptNew` succeeds on. Before `AcceptNew` existed, an operator who
    /// wrote `accept-new` got the `Ask` row - the failure this pins.
    ///
    /// upstream: openssh/sshconnect.c:1169-1181 - the unknown-host arm tests only for
    /// YES and ASK, so NEW and OFF both fall through to learning the key.
    #[test]
    fn unknown_host_accept_new_learns_without_prompting() {
        let pubkey = test_ed25519_pubkey();
        let dir = tempfile::tempdir().expect("tempdir");
        let kh_path = dir.path().join("known_hosts");
        std::fs::File::create(&kh_path).expect("create");

        let handler = SshClientHandler::new(
            "new.example".into(),
            22,
            StrictHostKeyChecking::AcceptNew,
            Some(kh_path.clone()),
        );
        assert!(handler.verify_host_key(&pubkey).expect("accepted"));
        assert!(
            known_hosts::check_known_hosts_path("new.example", 22, &pubkey, &kh_path)
                .expect("check"),
            "accept-new must persist the learned key",
        );

        // Control: the same unknown host under `ask` cannot be answered.
        let asking = SshClientHandler::new(
            "ask.example".into(),
            22,
            StrictHostKeyChecking::Ask,
            Some(kh_path),
        );
        assert!(
            matches!(
                asking.verify_host_key(&pubkey),
                Err(SshError::UnknownHost { .. })
            ),
            "ask must not silently accept an unknown host unattended",
        );
    }

    /// `accept-new` rejects a CHANGED key - that is the whole difference
    /// between it and `no`.
    ///
    /// upstream: openssh/sshconnect.c:1272-1274 / :1329-1331 refuse a changed key for
    /// every policy except `off`/`no`; oc refuses under all of them, so this
    /// pins the stricter side for the new variant specifically.
    #[test]
    fn accept_new_still_rejects_a_changed_key() {
        let original_key = test_ed25519_pubkey();
        let different_key = test_ed25519_pubkey();

        let dir = tempfile::tempdir().expect("tempdir");
        let kh_path = dir.path().join("known_hosts");
        known_hosts::learn_known_hosts_path("changed.example", 22, &original_key, &kh_path)
            .expect("learn");

        let handler = SshClientHandler::new(
            "changed.example".into(),
            22,
            StrictHostKeyChecking::AcceptNew,
            Some(kh_path),
        );
        assert!(matches!(
            handler.verify_host_key(&different_key),
            Err(SshError::HostKeyMismatch { ref host }) if host == "changed.example"
        ));
    }

    /// Mismatched key is always rejected, even with StrictHostKeyChecking::No.
    #[test]
    fn key_mismatch_always_rejected() {
        let original_key = test_ed25519_pubkey();
        let different_key = test_ed25519_pubkey();

        let dir = tempfile::tempdir().expect("tempdir");
        let kh_path = dir.path().join("known_hosts");

        // Learn the original key.
        known_hosts::learn_known_hosts_path("mismatch.example", 22, &original_key, &kh_path)
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
        known_hosts::learn_known_hosts_path("porttest.example", 2222, &pubkey, &kh_path)
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

    // --- Host-key verification family (task 237m) ---

    /// Build options for a single check/learn file with the family knobs.
    fn opts(
        host: &str,
        strict: StrictHostKeyChecking,
        check: Vec<Option<PathBuf>>,
        learn: Option<Option<PathBuf>>,
    ) -> HostKeyOptions {
        HostKeyOptions {
            strict_host_key_checking: strict,
            check_sources: check,
            learn_target: learn,
            hash_known_hosts: false,
            lookup_name: host.to_owned(),
            host: host.to_owned(),
            port: 22,
            server_ip: None,
            revoked_host_keys: None,
        }
    }

    /// `HashKnownHosts yes` writes a hashed entry (no plaintext hostname on
    /// disk) that russh still reads back as a match; the `no` control writes
    /// the hostname in the clear. This is the behavioural pin: the same
    /// learned key verifies either way, but only the plaintext form exposes
    /// the hostname.
    #[test]
    fn hash_known_hosts_hashes_the_learned_entry() {
        let pubkey = test_ed25519_pubkey();

        // hash = yes
        let dir = tempfile::tempdir().expect("tempdir");
        let kh = dir.path().join("known_hosts");
        std::fs::File::create(&kh).expect("create");
        let hashed = SshClientHandler::with_options(HostKeyOptions {
            hash_known_hosts: true,
            ..opts(
                "secret.example",
                StrictHostKeyChecking::No,
                vec![Some(kh.clone())],
                Some(Some(kh.clone())),
            )
        });
        assert!(
            hashed.verify_host_key(&pubkey).expect("learns"),
            "unknown host under No must be learned",
        );
        let on_disk = std::fs::read_to_string(&kh).expect("read");
        assert!(
            on_disk.contains("|1|"),
            "expected a hashed entry: {on_disk}"
        );
        assert!(
            !on_disk.contains("secret.example"),
            "the hostname must not appear in the clear: {on_disk}",
        );
        // russh reads the hashed entry back as a match.
        assert!(
            known_hosts::check_known_hosts_path("secret.example", 22, &pubkey, &kh).expect("check"),
            "the hashed entry must verify",
        );

        // control: hash = no writes the hostname in the clear
        let dir2 = tempfile::tempdir().expect("tempdir");
        let kh2 = dir2.path().join("known_hosts");
        std::fs::File::create(&kh2).expect("create");
        let plain = SshClientHandler::with_options(opts(
            "secret.example",
            StrictHostKeyChecking::No,
            vec![Some(kh2.clone())],
            Some(Some(kh2.clone())),
        ));
        assert!(plain.verify_host_key(&pubkey).expect("learns"));
        let clear = std::fs::read_to_string(&kh2).expect("read");
        assert!(
            clear.contains("secret.example") && !clear.contains("|1|"),
            "plaintext control must record the hostname: {clear}",
        );
    }

    /// The user known_hosts files are consulted in order: a key recorded only
    /// in the second file still verifies. The control is the first file,
    /// which does not contain it, so a single-file check would miss it.
    #[test]
    fn user_known_hosts_files_are_consulted_in_order() {
        let pubkey = test_ed25519_pubkey();
        let dir = tempfile::tempdir().expect("tempdir");
        let first = dir.path().join("first");
        let second = dir.path().join("second");
        std::fs::File::create(&first).expect("create");
        known_hosts::learn_known_hosts_path("multi.example", 22, &pubkey, &second).expect("learn");

        let handler = SshClientHandler::with_options(opts(
            "multi.example",
            StrictHostKeyChecking::Yes,
            vec![Some(first.clone()), Some(second)],
            None,
        ));
        assert!(
            handler
                .verify_host_key(&pubkey)
                .expect("second file matches"),
            "a key in the second file must verify",
        );

        // Control: only the first (empty) file - the same strict handler now
        // cannot find the key.
        let only_first = SshClientHandler::with_options(opts(
            "multi.example",
            StrictHostKeyChecking::Yes,
            vec![Some(first)],
            None,
        ));
        assert!(only_first.verify_host_key(&pubkey).is_err());
    }

    /// `HostKeyAlias` makes the key store and look up under the alias, not
    /// the connection host. The learned entry is keyed on the alias, and a
    /// lookup under the real host would miss it.
    #[test]
    fn host_key_alias_keys_the_entry_on_the_alias() {
        let pubkey = test_ed25519_pubkey();
        let dir = tempfile::tempdir().expect("tempdir");
        let kh = dir.path().join("known_hosts");
        std::fs::File::create(&kh).expect("create");

        let handler = SshClientHandler::with_options(HostKeyOptions {
            lookup_name: "alias.name".to_owned(),
            ..opts(
                "real.host",
                StrictHostKeyChecking::No,
                vec![Some(kh.clone())],
                Some(Some(kh.clone())),
            )
        });
        assert!(handler.verify_host_key(&pubkey).expect("learns"));
        // Stored under the alias, not the real host.
        assert!(
            known_hosts::check_known_hosts_path("alias.name", 22, &pubkey, &kh).expect("check"),
        );
        assert!(
            !known_hosts::check_known_hosts_path("real.host", 22, &pubkey, &kh).expect("check"),
            "the entry must not be keyed on the connection host",
        );
    }

    /// A key listed in `RevokedHostKeys` is refused before any known_hosts
    /// check and under every policy - even `No`, and even when the key is
    /// ALSO a valid known_hosts entry (the control: without the revoked file
    /// the same key verifies).
    #[test]
    fn revoked_host_key_is_refused_under_every_policy() {
        let pubkey = test_ed25519_pubkey();
        let dir = tempfile::tempdir().expect("tempdir");
        let kh = dir.path().join("known_hosts");
        known_hosts::learn_known_hosts_path("revoke.example", 22, &pubkey, &kh).expect("learn");
        let revoked = dir.path().join("revoked");
        std::fs::write(&revoked, pubkey.to_openssh().expect("openssh")).expect("write revoked");

        let handler = SshClientHandler::with_options(HostKeyOptions {
            revoked_host_keys: Some(revoked),
            ..opts(
                "revoke.example",
                StrictHostKeyChecking::No,
                vec![Some(kh.clone())],
                Some(Some(kh.clone())),
            )
        });
        assert!(
            matches!(
                handler.verify_host_key(&pubkey),
                Err(SshError::HostKeyRevoked { ref host }) if host == "revoke.example"
            ),
            "a revoked key must be refused even under No",
        );

        // Control: the identical setup without the revoked file accepts the
        // key (it is a valid known_hosts entry), proving the refusal is the
        // revoked list and not the fixture.
        let ok = SshClientHandler::with_options(opts(
            "revoke.example",
            StrictHostKeyChecking::Yes,
            vec![Some(kh)],
            None,
        ));
        assert!(ok.verify_host_key(&pubkey).expect("valid"));
    }

    /// `CheckHostIP` makes the resolved IP a second recognised identity: a
    /// key stored only under the IP verifies when `server_ip` is set, and the
    /// control with no IP identity treats the same host as unknown.
    #[test]
    fn check_host_ip_treats_the_ip_as_a_second_identity() {
        let pubkey = test_ed25519_pubkey();
        let dir = tempfile::tempdir().expect("tempdir");
        let kh = dir.path().join("known_hosts");
        // The key is recorded ONLY under the IP, never the hostname.
        known_hosts::learn_known_hosts_path("192.0.2.10", 22, &pubkey, &kh).expect("learn");

        let with_ip = SshClientHandler::with_options(HostKeyOptions {
            server_ip: Some("192.0.2.10".to_owned()),
            ..opts(
                "ip.example",
                StrictHostKeyChecking::Yes,
                vec![Some(kh.clone())],
                None,
            )
        });
        assert!(
            with_ip
                .verify_host_key(&pubkey)
                .expect("ip identity matches"),
            "the IP entry must satisfy verification",
        );

        // Control: no IP identity (CheckHostIP off) - the hostname is unknown.
        let no_ip = SshClientHandler::with_options(opts(
            "ip.example",
            StrictHostKeyChecking::Yes,
            vec![Some(kh)],
            None,
        ));
        assert!(no_ip.verify_host_key(&pubkey).is_err());
    }

    /// `UserKnownHostsFile none` (a `None` learn target) disables writing:
    /// an unknown host under `No` is still accepted, but nothing is recorded.
    #[test]
    fn a_disabled_learn_target_accepts_without_writing() {
        let pubkey = test_ed25519_pubkey();
        let dir = tempfile::tempdir().expect("tempdir");
        let kh = dir.path().join("known_hosts");
        std::fs::File::create(&kh).expect("create");

        let handler = SshClientHandler::with_options(opts(
            "nowrite.example",
            StrictHostKeyChecking::No,
            vec![Some(kh.clone())],
            None, // learning disabled
        ));
        assert!(handler.verify_host_key(&pubkey).expect("accepts"));
        assert_eq!(
            std::fs::read_to_string(&kh).expect("read"),
            "",
            "a disabled learn target must not write",
        );
    }
}
