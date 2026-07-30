// Persisted self-signed QUIC identity (docs/design/quic-transport-policy.md,
// decision A).
//
// When the operator sets neither `quic cert file` nor `quic key file`, the
// daemon needs an identity to present on the QUIC listener. Rather than minting
// a fresh self-signed certificate on every start - which makes each restart
// look like a new host to a trust-on-first-use client - the daemon generates
// the pair once and persists it under a `quic/` subdirectory of the config
// directory, then reuses it verbatim on subsequent starts. This file resolves
// that pair; building the listener from it lands under a later QUIC task.

/// Subdirectory under the daemon config directory that holds the persisted
/// self-signed QUIC identity. Co-located with the config that enables QUIC,
/// matching where an operator already looks for daemon state (the daemon has no
/// dedicated state-dir constant today).
const QUIC_STATE_SUBDIR: &str = "quic";
/// Basename of the persisted self-signed certificate (PEM).
const QUIC_SELF_SIGNED_CERT: &str = "self-signed.pem";
/// Basename of the persisted self-signed private key (PEM).
const QUIC_SELF_SIGNED_KEY: &str = "self-signed.key";

/// Ensures a persisted self-signed QUIC identity exists under `state_dir` and
/// returns the resolved `(cert_path, key_path)` pair.
///
/// On first call the pair is generated with `rcgen` and written to
/// `state_dir/self-signed.{pem,key}`. On every later call an existing complete
/// pair is reused untouched, so the daemon presents a stable identity across
/// restarts - the property a trust-on-first-use client depends on. A partial
/// pair (only one file present) is regenerated so the certificate and key stay
/// matched.
///
/// The state directory is created `0700` and the private key `0600` on Unix so
/// the key is never briefly world-readable; the certificate is public.
#[allow(dead_code)] // REASON: consumed by the QUIC listener build-out (QUIC-6c); exercised by tests now
pub(crate) fn persist_self_signed_quic_identity(
    state_dir: &Path,
) -> io::Result<(PathBuf, PathBuf)> {
    let cert_path = state_dir.join(QUIC_SELF_SIGNED_CERT);
    let key_path = state_dir.join(QUIC_SELF_SIGNED_KEY);

    // Stable identity: an existing complete pair is authoritative and must not
    // be regenerated, or every restart would rotate the key.
    if cert_path.is_file() && key_path.is_file() {
        return Ok((cert_path, key_path));
    }

    fs::create_dir_all(state_dir)?;
    #[cfg(unix)]
    fs::set_permissions(state_dir, fs::Permissions::from_mode(0o700))?;

    let issued = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()])
        .map_err(io::Error::other)?;
    let cert_pem = issued.cert.pem();
    let key_pem = issued.signing_key.serialize_pem();

    // Write the key first (restrictive perms) so a concurrent reader never sees
    // a certificate without its key.
    write_private_file(&key_path, key_pem.as_bytes())?;
    fs::write(&cert_path, cert_pem.as_bytes())?;

    Ok((cert_path, key_path))
}

/// Writes `contents` to `path`, creating the file `0600` on Unix so a private
/// key is never momentarily readable by other users.
fn write_private_file(path: &Path, contents: &[u8]) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(contents)?;
    file.flush()
}

impl RuntimeOptions {
    /// Resolves the certificate/key pair the QUIC listener presents.
    ///
    /// When the operator set `quic cert file` / `quic key file` the configured
    /// paths are returned verbatim - the operator owns the identity and nothing
    /// is generated. Otherwise the daemon falls back to a persisted self-signed
    /// pair under a `quic/` subdirectory of the config directory, generating it
    /// on first run and reusing it thereafter (decision A).
    #[allow(dead_code)] // REASON: consumed by the QUIC listener build-out (QUIC-6c); exercised by tests now
    pub(crate) fn resolve_quic_identity(&self) -> io::Result<(PathBuf, PathBuf)> {
        if let (Some(cert), Some(key)) = (&self.quic_cert_file, &self.quic_key_file) {
            return Ok((cert.clone(), key.clone()));
        }
        let state_dir = self.quic_state_dir()?;
        persist_self_signed_quic_identity(&state_dir)
    }

    /// Locates the directory holding the persisted self-signed QUIC identity: a
    /// `quic/` subdirectory beside the loaded config file. Requires a config
    /// file to anchor the location; a CLI-only daemon must set the directives
    /// explicitly.
    fn quic_state_dir(&self) -> io::Result<PathBuf> {
        let config_path = self.config_path.as_deref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "cannot resolve a persisted QUIC identity without a config file; \
                 set 'quic cert file' / 'quic key file' explicitly",
            )
        })?;
        let config_dir = config_path.parent().unwrap_or_else(|| Path::new("."));
        Ok(config_dir.join(QUIC_STATE_SUBDIR))
    }
}
