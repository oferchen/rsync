//! Client-side trust backends for the QUIC transport (policy B).
//!
//! The bytes inside a QUIC session are the unmodified daemon protocol; this
//! module only decides *whether to trust the server's certificate*, then hands
//! the decision to [`QuicConnector`](super::QuicConnector) as a [`QuicTrust`].
//! Three sources make up the client verification ladder:
//!
//! - **System roots** ([`system_roots`], QUIC-5c) - the zero-config default.
//!   Verifies the server chain against the live platform trust store, the
//!   openssl/stunnel behaviour upstream `rsync-ssl` relies on when
//!   `RSYNC_SSL_CA_CERT` is unset.
//! - **TOFU `quic_known_hosts`** ([`tofu`]/[`TofuVerifier`], QUIC-5d) - the
//!   self-signed path. On first contact with an authority the server's cert
//!   fingerprint is pinned into a known-hosts file (mirroring SSH); a changed
//!   fingerprint for a known authority aborts the handshake loudly. There is no
//!   blanket insecure escape hatch.
//! - **Private CA** ([`load_private_ca`]/[`private_ca_file`], `--quic-ca`,
//!   QUIC-5b) - a PEM CA bundle parsed into a [`RootCertStore`] and used as the
//!   highest-precedence [`QuicTrust::Roots`], for a daemon whose chain is
//!   anchored by an internal or corporate CA rather than the platform store.
//!
//! [`resolve`] selects one source per policy-B precedence so the CLI wiring
//! (`--quic-ca` > system roots > TOFU) composes these backends without
//! re-deriving the order. Dependency inversion keeps the TOFU verifier testable:
//! it pins through a [`KnownHostsStore`] trait, not a hard-wired file, so tests
//! inject a temp-path store and no network or real trust store is touched.
//!
//! See `docs/design/quic-transport-policy.md` (Decision B) and
//! `docs/design/quic-transport-integration.md` (Phase 3).

use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD_NO_PAD;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, Error as TlsError, RootCertStore, SignatureScheme};
use sha2::{Digest, Sha256};

use super::{QuicTrust, io_err, ring_provider};

/// SHA-256 fingerprint of a presented server certificate (the whole DER
/// encoding), the identity a TOFU pin records.
///
/// Hashing the certificate DER - rather than the SubjectPublicKeyInfo - is the
/// `cert SHA-256` option the policy note explicitly allows: the daemon's
/// zero-config identity is a *persisted* self-signed certificate (policy A), so
/// the certificate bytes are as stable across restarts as the key would be,
/// and a whole-certificate digest needs no ASN.1 parser to extract a subfield.
/// The wire/handshake is untouched either way - this is a local trust check.
#[derive(Clone, PartialEq, Eq)]
pub struct Fingerprint([u8; 32]);

impl Fingerprint {
    /// Computes the fingerprint of `cert`.
    #[must_use]
    pub fn of_certificate(cert: &CertificateDer<'_>) -> Self {
        Self(Sha256::digest(cert.as_ref()).into())
    }

    /// The `SHA256:<base64>` token stored in `quic_known_hosts` and shown in
    /// diagnostics, matching the unpadded-base64 form SSH prints for a SHA-256
    /// key fingerprint.
    fn token(&self) -> String {
        format!("SHA256:{}", STANDARD_NO_PAD.encode(self.0))
    }

    /// Parses a `SHA256:<base64>` token, returning `None` on any malformed
    /// input so a corrupt line is skipped rather than trusted.
    fn parse_token(token: &str) -> Option<Self> {
        let encoded = token.strip_prefix("SHA256:")?;
        let bytes = STANDARD_NO_PAD.decode(encoded).ok()?;
        let array: [u8; 32] = bytes.try_into().ok()?;
        Some(Self(array))
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.token())
    }
}

impl fmt::Debug for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fingerprint({})", self.token())
    }
}

/// Persistent record of the fingerprints pinned per authority - the seam the
/// TOFU verifier depends on instead of a concrete file.
///
/// Inverting this dependency is what makes [`TofuVerifier`] testable offline: a
/// test injects a store over a temp path (or an in-memory fake) and drives the
/// pin/accept/reject decisions without a real `~/.config` file or a network.
pub trait KnownHostsStore: fmt::Debug + Send + Sync {
    /// Returns the fingerprint pinned for `authority`, or `None` on first
    /// contact.
    fn pinned(&self, authority: &str) -> io::Result<Option<Fingerprint>>;

    /// Records `fingerprint` as the pin for `authority`. Called once, on first
    /// contact; never overwrites a differing pin (that is the caller's abort
    /// path).
    fn pin(&self, authority: &str, fingerprint: &Fingerprint) -> io::Result<()>;
}

/// A `quic_known_hosts` file, one `authority SHA256:<base64>` record per line.
///
/// Mirrors SSH `known_hosts`: a per-user trust store keyed by `host:port`,
/// created `0700`/`0600`, written atomically (temp file in the same directory
/// then rename) so a concurrent reader never sees a half-written file.
#[derive(Debug, Clone)]
pub struct KnownHostsFile {
    path: PathBuf,
}

impl KnownHostsFile {
    /// Binds the store to `path`. The file and its parent directory are created
    /// lazily on the first [`pin`](KnownHostsStore::pin).
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl KnownHostsStore for KnownHostsFile {
    fn pinned(&self, authority: &str) -> io::Result<Option<Fingerprint>> {
        let contents = match fs::read_to_string(&self.path) {
            Ok(contents) => contents,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err),
        };
        Ok(contents
            .lines()
            .find_map(|line| parse_record(line, authority)))
    }

    fn pin(&self, authority: &str, fingerprint: &Fingerprint) -> io::Result<()> {
        if let Some(parent) = self.path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
            restrict_dir(parent)?;
        }
        let mut contents = match fs::read_to_string(&self.path) {
            Ok(contents) => contents,
            Err(err) if err.kind() == io::ErrorKind::NotFound => String::new(),
            Err(err) => return Err(err),
        };
        if !contents.is_empty() && !contents.ends_with('\n') {
            contents.push('\n');
        }
        contents.push_str(&format!("{authority} {}\n", fingerprint.token()));
        atomic_write(&self.path, contents.as_bytes())
    }
}

/// Parses one `known_hosts` line, returning the fingerprint when it records
/// `authority`. Blank lines, `#` comments, and malformed tokens are ignored.
fn parse_record(line: &str, authority: &str) -> Option<Fingerprint> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let mut fields = line.split_whitespace();
    let recorded = fields.next()?;
    if recorded != authority {
        return None;
    }
    Fingerprint::parse_token(fields.next()?)
}

/// Writes `bytes` to `path` atomically: a temp file in the same directory,
/// `0600` on unix, is persisted over the target by rename.
fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty());
    let mut builder = tempfile::Builder::new();
    builder.prefix(".quic_known_hosts").suffix(".tmp");
    let mut temp = match dir {
        Some(dir) => builder.tempfile_in(dir)?,
        None => builder.tempfile()?,
    };
    temp.write_all(bytes)?;
    temp.flush()?;
    restrict_file(temp.as_file())?;
    temp.persist(path).map_err(|err| err.error)?;
    Ok(())
}

#[cfg(unix)]
fn restrict_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn restrict_dir(_dir: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn restrict_file(file: &fs::File) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn restrict_file(_file: &fs::File) -> io::Result<()> {
    Ok(())
}

/// A rustls [`ServerCertVerifier`] implementing trust-on-first-use.
///
/// Keyed by a `host:port` `authority` fixed at construction (rustls hands the
/// verifier only the certificate and the TLS `ServerName`, not the port, so the
/// pin key mirrors SSH's `host:port` and comes from the connect target). On
/// first contact the presented certificate's fingerprint is pinned; afterward
/// the same authority must present the same fingerprint or the handshake fails.
/// Handshake-signature verification still runs against the crypto provider, so
/// a peer must prove possession of the pinned certificate's private key - the
/// pin alone is not enough.
pub struct TofuVerifier {
    authority: String,
    store: Box<dyn KnownHostsStore>,
    provider: Arc<CryptoProvider>,
}

impl TofuVerifier {
    /// Builds a verifier pinning `authority` through `store`.
    #[must_use]
    pub fn new(authority: impl Into<String>, store: Box<dyn KnownHostsStore>) -> Self {
        Self {
            authority: authority.into(),
            store,
            provider: ring_provider(),
        }
    }
}

impl fmt::Debug for TofuVerifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TofuVerifier")
            .field("authority", &self.authority)
            .field("store", &self.store)
            .finish_non_exhaustive()
    }
}

impl ServerCertVerifier for TofuVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        let presented = Fingerprint::of_certificate(end_entity);
        match self.store.pinned(&self.authority) {
            Err(err) => Err(TlsError::General(format!(
                "quic: reading known-hosts for {}: {err}",
                self.authority
            ))),
            Ok(Some(pinned)) if pinned == presented => Ok(ServerCertVerified::assertion()),
            Ok(Some(pinned)) => {
                emit_key_changed_warning(&self.authority, &pinned, &presented);
                Err(TlsError::General(format!(
                    "quic host key mismatch for {}: pinned {pinned} got {presented}",
                    self.authority
                )))
            }
            Ok(None) => match self.store.pin(&self.authority, &presented) {
                Ok(()) => {
                    emit_pinned_notice(&self.authority, &presented);
                    Ok(ServerCertVerified::assertion())
                }
                Err(err) => Err(TlsError::General(format!(
                    "quic: pinning host key for {}: {err}",
                    self.authority
                ))),
            },
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Informational first-contact notice (not fatal), matching the policy note.
fn emit_pinned_notice(authority: &str, fingerprint: &Fingerprint) {
    let _ = writeln!(
        io::stderr(),
        "oc-rsync: quic: pinning new host key for {authority} ({fingerprint}); \
         add --quic-ca or --quic-known-key to verify out of band"
    );
}

/// Loud MITM-shaped warning on a changed key, deliberately echoing SSH's
/// `REMOTE HOST IDENTIFICATION HAS CHANGED` so operator muscle memory transfers.
fn emit_key_changed_warning(authority: &str, pinned: &Fingerprint, presented: &Fingerprint) {
    let _ = writeln!(
        io::stderr(),
        "@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@\n\
         @    WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!     @\n\
         @@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@\n\
         IT IS POSSIBLE THAT SOMEONE IS DOING SOMETHING NASTY!\n\
         Someone could be eavesdropping on you right now (man-in-the-middle attack)!\n\
         The quic host key for {authority} has changed.\n\
         Pinned fingerprint: {pinned}\n\
         Offered fingerprint: {presented}\n\
         Remove the offending line for {authority} from your quic_known_hosts to proceed."
    );
}

/// Builds the system-roots trust source (QUIC-5c): verify the server chain
/// against the live platform trust store.
///
/// This is the zero-config default of the client ladder and the carried-forward
/// `RSYNC_SSL_CA_CERT`-unset behaviour ("default CA set AND verify"). Errors if
/// the platform store yields no usable certificate, so a broken trust store
/// fails loudly rather than silently trusting nothing.
pub fn system_roots() -> io::Result<QuicTrust> {
    let loaded = rustls_native_certs::load_native_certs();
    let mut roots = RootCertStore::empty();
    let (added, _ignored) = roots.add_parsable_certificates(loaded.certs);
    if added == 0 {
        let detail = loaded.errors.first().map_or_else(
            || "empty platform trust store".to_owned(),
            ToString::to_string,
        );
        return Err(io_err(format!(
            "no usable certificates in the system trust store: {detail}"
        )));
    }
    Ok(QuicTrust::Roots(roots))
}

/// Loads a private CA bundle from the PEM file at `path` into a
/// [`RootCertStore`] (QUIC-5b, `--quic-ca`).
///
/// This is the highest-precedence source of the client-verification ladder: an
/// operator points `--quic-ca` at the certificate that anchors their daemon's
/// chain (a corporate or internal CA), and the server chain is verified against
/// it instead of the platform store or a TOFU pin. Only `CERTIFICATE` PEM
/// sections are consumed; any other section kind is ignored, matching how
/// openssl/stunnel treat a CA bundle.
///
/// Fails loudly rather than trusting nothing: an unreadable file, a malformed
/// PEM certificate, a certificate rustls rejects, or a file with no
/// certificate at all each returns a distinct error naming `path`. Callers map
/// the [`io::Error`] to the appropriate exit code at the CLI boundary.
pub fn load_private_ca(path: &Path) -> io::Result<RootCertStore> {
    let pem = fs::read(path)
        .map_err(|err| io_err(format!("quic: reading --quic-ca file {}: {err}", path.display())))?;
    let mut roots = RootCertStore::empty();
    let mut added = 0usize;
    for (index, cert) in CertificateDer::pem_slice_iter(&pem).enumerate() {
        let cert = cert.map_err(|err| {
            io_err(format!(
                "quic: parsing certificate #{} in --quic-ca file {}: {err}",
                index + 1,
                path.display()
            ))
        })?;
        roots.add(cert).map_err(|err| {
            io_err(format!(
                "quic: certificate #{} in --quic-ca file {} is not a usable trust anchor: {err}",
                index + 1,
                path.display()
            ))
        })?;
        added += 1;
    }
    if added == 0 {
        return Err(io_err(format!(
            "quic: no certificates found in --quic-ca file {}",
            path.display()
        )));
    }
    Ok(roots)
}

/// Builds a private-CA trust source (QUIC-5b) from the PEM file at `path`.
///
/// Convenience wrapper over [`load_private_ca`] mirroring [`tofu_file`] and
/// [`system_roots`]: it yields the [`QuicTrust::Roots`] the connector consumes,
/// so the CLI wiring feeds `--quic-ca` through the same connect path as the
/// system-roots default.
pub fn private_ca_file(path: &Path) -> io::Result<QuicTrust> {
    Ok(QuicTrust::Roots(load_private_ca(path)?))
}

/// Builds a TOFU trust source (QUIC-5d) pinning `authority` through `store`.
#[must_use]
pub fn tofu(authority: impl Into<String>, store: Box<dyn KnownHostsStore>) -> QuicTrust {
    QuicTrust::Verifier(Arc::new(TofuVerifier::new(authority, store)))
}

/// Builds a TOFU trust source backed by a `quic_known_hosts` file at `path`.
#[must_use]
pub fn tofu_file(authority: impl Into<String>, path: PathBuf) -> QuicTrust {
    tofu(authority, Box::new(KnownHostsFile::new(path)))
}

/// Default `quic_known_hosts` location: `$XDG_CONFIG_HOME/oc-rsync/` when set,
/// else the per-user config dir (`~/.config` on unix, `%APPDATA%` on Windows).
///
/// Kept inside oc-rsync's own config namespace so it never collides with or
/// resembles an upstream path. Returns `None` when no home/config dir resolves.
#[must_use]
pub fn default_known_hosts_path() -> Option<PathBuf> {
    const FILE: &str = "quic_known_hosts";
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(xdg).join("oc-rsync").join(FILE));
    }
    config_dir().map(|dir| dir.join("oc-rsync").join(FILE))
}

#[cfg(unix)]
fn config_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(|home| PathBuf::from(home).join(".config"))
}

#[cfg(windows)]
fn config_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

#[cfg(not(any(unix, windows)))]
fn config_dir() -> Option<PathBuf> {
    None
}

/// One resolved trust source, in policy-B precedence order.
///
/// Constructed by the CLI wiring (a private CA from `--quic-ca`, a TOFU target
/// from a self-signed endpoint, or the system-roots default) and converted to a
/// [`QuicTrust`] the connector consumes.
pub enum TrustPolicy {
    /// Private CA bundle (`--quic-ca`), already parsed into a root store.
    PrivateCa(RootCertStore),
    /// Platform trust store (zero-config default).
    SystemRoots,
    /// Trust-on-first-use, keyed by `authority`; `None` uses the default
    /// `quic_known_hosts` location.
    Tofu {
        /// `host:port` the pin is keyed by.
        authority: String,
        /// Explicit known-hosts path, or `None` for [`default_known_hosts_path`].
        known_hosts: Option<PathBuf>,
    },
}

impl TrustPolicy {
    /// Materializes the trust source into a [`QuicTrust`].
    pub fn into_trust(self) -> io::Result<QuicTrust> {
        match self {
            Self::PrivateCa(roots) => Ok(QuicTrust::Roots(roots)),
            Self::SystemRoots => system_roots(),
            Self::Tofu {
                authority,
                known_hosts,
            } => {
                let path = known_hosts
                    .or_else(default_known_hosts_path)
                    .ok_or_else(|| {
                        io_err("cannot resolve a quic_known_hosts path (no HOME/config dir)")
                    })?;
                Ok(tofu_file(authority, path))
            }
        }
    }
}

/// Selects the effective client trust source per policy-B precedence
/// (`--quic-ca` > system roots > TOFU) and materializes it into a [`QuicTrust`].
///
/// An explicit private CA wins; otherwise an explicit TOFU target engages the
/// self-signed path; otherwise the system-roots default applies. This is the
/// composition seam for the CLI wiring: it supplies `ca`/`tofu` from the parsed
/// flags and receives one ready trust source, without re-encoding the order.
pub fn resolve(
    ca: Option<RootCertStore>,
    tofu: Option<(String, Option<PathBuf>)>,
) -> io::Result<QuicTrust> {
    let policy = match (ca, tofu) {
        (Some(roots), _) => TrustPolicy::PrivateCa(roots),
        (None, Some((authority, known_hosts))) => TrustPolicy::Tofu {
            authority,
            known_hosts,
        },
        (None, None) => TrustPolicy::SystemRoots,
    };
    policy.into_trust()
}

#[cfg(test)]
mod tests {
    use rustls::pki_types::{ServerName, UnixTime};

    use super::super::{QuicConnector, QuicTrust};
    use super::*;

    const AUTHORITY: &str = "host.example:873";

    /// A fresh self-signed certificate; each call has a distinct key, so two
    /// certificates yield distinct fingerprints - the "changed host key" case.
    fn synthetic_cert() -> CertificateDer<'static> {
        rcgen::generate_simple_self_signed(vec!["host.example".to_owned()])
            .expect("generate self-signed")
            .cert
            .der()
            .clone()
    }

    fn store_at(path: &Path) -> Box<dyn KnownHostsStore> {
        Box::new(KnownHostsFile::new(path.to_path_buf()))
    }

    fn verify(cert: &CertificateDer<'_>, path: &Path) -> Result<ServerCertVerified, TlsError> {
        let verifier = TofuVerifier::new(AUTHORITY, store_at(path));
        verifier.verify_server_cert(
            cert,
            &[],
            &ServerName::try_from("host.example").expect("server name"),
            &[],
            UnixTime::now(),
        )
    }

    /// The system-roots default (QUIC-5c) must arrive as a `QuicTrust::Roots`
    /// the existing connector accepts - the whole point is that the platform
    /// store feeds the unchanged connect path. A trust-store-less environment
    /// must fail loudly with the documented message, never silently trust
    /// nothing.
    #[test]
    fn system_roots_feeds_the_connector_or_reports_empty() {
        match system_roots() {
            Ok(trust) => {
                assert!(matches!(trust, QuicTrust::Roots(_)));
                QuicConnector::with_trust(trust).expect("system-roots connector");
            }
            Err(err) => assert!(
                err.to_string().contains("system trust store"),
                "unexpected error: {err}"
            ),
        }
    }

    /// TOFU end to end: first contact pins the fingerprint and persists it, the
    /// same certificate is accepted on a later contact, and a changed
    /// certificate for the known authority is rejected. Encodes WHY: TOFU's
    /// security property is exactly "pin once, then require the same key" - an
    /// accept on first sight plus a hard reject on a changed key is what
    /// distinguishes it from a blanket insecure flag.
    #[test]
    fn tofu_pins_then_accepts_same_and_rejects_changed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state").join("quic_known_hosts");
        let cert = synthetic_cert();
        let rogue = synthetic_cert();
        assert_ne!(
            Fingerprint::of_certificate(&cert),
            Fingerprint::of_certificate(&rogue),
            "distinct certs must fingerprint distinctly for this test to mean anything"
        );

        // First contact: no pin yet, so the cert is accepted and recorded.
        assert!(!path.exists(), "no known-hosts file before first contact");
        verify(&cert, &path).expect("first contact pins and accepts");
        let recorded = std::fs::read_to_string(&path).expect("known-hosts written");
        assert!(
            recorded.contains(AUTHORITY) && recorded.contains("SHA256:"),
            "first contact must persist an authority/fingerprint record: {recorded:?}"
        );

        // Second contact with the same identity: the pin matches, so accept.
        verify(&cert, &path).expect("same fingerprint accepted");

        // Changed identity for a known authority: MITM-shaped, must reject.
        let err = verify(&rogue, &path).expect_err("changed fingerprint must be rejected");
        assert!(
            err.to_string().contains("host key mismatch"),
            "rejection must name the mismatch: {err}"
        );

        // The reject must not have overwritten the original pin.
        let after = std::fs::read_to_string(&path).expect("known-hosts still present");
        assert_eq!(recorded, after, "a rejected key must never repin the host");
    }

    /// The `quic_known_hosts` file is created with owner-only permissions and a
    /// `0700` parent, mirroring SSH - a world-readable trust store would let a
    /// local user tamper with pins.
    #[cfg(unix)]
    #[test]
    fn known_hosts_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let parent = dir.path().join("state");
        let path = parent.join("quic_known_hosts");
        let store = KnownHostsFile::new(path.clone());
        store
            .pin(AUTHORITY, &Fingerprint::of_certificate(&synthetic_cert()))
            .expect("pin");

        assert_eq!(
            std::fs::metadata(&path)
                .expect("file meta")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(&parent)
                .expect("dir meta")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }

    /// The store round-trips a pin: after `pin`, `pinned` returns the same
    /// fingerprint, and an unknown authority reads back as `None`.
    #[test]
    fn store_round_trips_pins() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("quic_known_hosts");
        let store = KnownHostsFile::new(path);
        let fp = Fingerprint::of_certificate(&synthetic_cert());

        assert!(store.pinned(AUTHORITY).expect("read empty").is_none());
        store.pin(AUTHORITY, &fp).expect("pin");
        assert_eq!(store.pinned(AUTHORITY).expect("read back"), Some(fp));
        assert!(
            store
                .pinned("other:873")
                .expect("unknown authority")
                .is_none(),
            "an unrelated authority must not match"
        );
    }

    /// The resolver honours policy-B precedence: an explicit CA outranks a TOFU
    /// target, and TOFU outranks the system-roots default. Encodes WHY: the CLI
    /// wiring (#50) relies on this order to compose `--quic-ca`, TOFU, and the
    /// default without re-deriving it.
    #[test]
    fn resolve_precedence_ca_over_tofu() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("quic_known_hosts");

        let tofu_target = Some((AUTHORITY.to_owned(), Some(path.clone())));
        let trust = resolve(None, tofu_target).expect("tofu resolves");
        assert!(
            matches!(trust, QuicTrust::Verifier(_)),
            "TOFU target -> verifier"
        );

        let ca = RootCertStore::empty();
        let both =
            resolve(Some(ca), Some((AUTHORITY.to_owned(), Some(path)))).expect("ca resolves");
        assert!(
            matches!(both, QuicTrust::Roots(_)),
            "explicit CA outranks TOFU"
        );
    }

    /// Encodes a DER certificate as a PEM `CERTIFICATE` block, base64 wrapped at
    /// 64 columns - the on-disk shape `--quic-ca` is pointed at. Kept local so
    /// the loader tests own their fixtures without pulling rcgen's optional
    /// `pem` feature into the build.
    fn der_to_pem(der: &CertificateDer<'_>) -> String {
        use base64::engine::general_purpose::STANDARD;
        let encoded = STANDARD.encode(der.as_ref());
        let mut body = String::new();
        for line in encoded.as_bytes().chunks(64) {
            body.push_str(std::str::from_utf8(line).expect("base64 is ascii"));
            body.push('\n');
        }
        format!("-----BEGIN CERTIFICATE-----\n{body}-----END CERTIFICATE-----\n")
    }

    /// Builds a private CA and a leaf certificate signed by it (SAN
    /// `host.example`, `serverAuth` EKU), returning `(ca_pem, leaf_der)`. The
    /// leaf is what a QUIC daemon behind the private CA would present; the CA
    /// PEM is what `--quic-ca` loads to anchor it.
    fn private_ca_and_leaf() -> (String, CertificateDer<'static>) {
        use rcgen::{
            BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
            KeyUsagePurpose,
        };

        let ca_key = KeyPair::generate().expect("ca key");
        let mut ca_params =
            CertificateParams::new(vec!["oc-rsync test ca".to_owned()]).expect("ca params");
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params.key_usages = vec![
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::CrlSign,
        ];
        let ca_cert = ca_params.self_signed(&ca_key).expect("self-sign ca");
        let ca_pem = der_to_pem(ca_cert.der());
        let issuer = Issuer::new(ca_params, ca_key);

        let leaf_key = KeyPair::generate().expect("leaf key");
        let mut leaf_params =
            CertificateParams::new(vec!["host.example".to_owned()]).expect("leaf params");
        leaf_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let leaf = leaf_params
            .signed_by(&leaf_key, &issuer)
            .expect("sign leaf")
            .der()
            .clone();
        (ca_pem, leaf)
    }

    /// The end-to-end trust property of `--quic-ca`: a certificate signed by the
    /// loaded CA is accepted, an unrelated self-signed certificate is rejected.
    /// Drives rustls' `WebPkiServerVerifier` over the exact [`RootCertStore`]
    /// `load_private_ca` produces - the same store `QuicConnector::with_trust`
    /// installs for a [`QuicTrust::Roots`] - so this asserts the connector's
    /// trust decision, not a reimplementation of it. Encodes WHY: `--quic-ca`
    /// is only meaningful if a chain it anchors verifies while an off-CA chain
    /// does not; a loader that accepted anything (or nothing) would pass a
    /// weaker test.
    #[test]
    fn private_ca_trusts_signed_leaf_and_rejects_unrelated() {
        let (ca_pem, leaf) = private_ca_and_leaf();
        let rogue = synthetic_cert();
        let dir = tempfile::tempdir().expect("tempdir");
        let ca_path = dir.path().join("private-ca.pem");
        std::fs::write(&ca_path, ca_pem).expect("write ca pem");

        let roots = load_private_ca(&ca_path).expect("load private ca");
        assert_eq!(roots.len(), 1, "one CA certificate becomes one trust anchor");

        let verifier = rustls::client::WebPkiServerVerifier::builder_with_provider(
            Arc::new(roots),
            ring_provider(),
        )
        .build()
        .expect("build webpki verifier");
        let name = ServerName::try_from("host.example").expect("server name");

        verifier
            .verify_server_cert(&leaf, &[], &name, &[], UnixTime::now())
            .expect("a leaf signed by the loaded CA must be trusted");
        verifier
            .verify_server_cert(&rogue, &[], &name, &[], UnixTime::now())
            .expect_err("a certificate not signed by the CA must be rejected");
    }

    /// `--quic-ca` pointed at a private CA outranks both the system-roots
    /// default and a TOFU target, and it arrives as the `QuicTrust::Roots` the
    /// connector consumes. Threads the real loader through [`resolve`] so the
    /// precedence assertion covers the actual `--quic-ca` bytes, not a stand-in
    /// empty store.
    #[test]
    fn loaded_private_ca_outranks_tofu_via_resolve() {
        let (ca_pem, _leaf) = private_ca_and_leaf();
        let dir = tempfile::tempdir().expect("tempdir");
        let ca_path = dir.path().join("private-ca.pem");
        std::fs::write(&ca_path, ca_pem).expect("write ca pem");
        let known_hosts = dir.path().join("quic_known_hosts");

        let ca = load_private_ca(&ca_path).expect("load private ca");
        let trust = resolve(Some(ca), Some((AUTHORITY.to_owned(), Some(known_hosts))))
            .expect("resolve with ca and tofu");
        assert!(
            matches!(trust, QuicTrust::Roots(_)),
            "a loaded --quic-ca must win over a TOFU target"
        );

        let private = private_ca_file(&ca_path).expect("private_ca_file");
        assert!(
            matches!(private, QuicTrust::Roots(_)),
            "private_ca_file yields a Roots trust source"
        );
    }

    /// The loader fails loudly on each way a `--quic-ca` file can be unusable:
    /// a missing file, a non-PEM file, and a well-formed PEM with no
    /// certificate section. Every error names `--quic-ca` so the operator knows
    /// which input to fix, and none silently yields an empty (trust-nothing)
    /// store. Encodes WHY: a private-CA flag that swallowed a bad bundle would
    /// either abort every later handshake with an opaque TLS error or, worse,
    /// trust nothing while looking configured.
    #[test]
    fn load_private_ca_reports_unusable_inputs() {
        let dir = tempfile::tempdir().expect("tempdir");

        let missing = dir.path().join("absent.pem");
        let err = load_private_ca(&missing).expect_err("missing file must error");
        assert!(
            err.to_string().contains("--quic-ca") && err.to_string().contains("reading"),
            "missing-file error must name the flag and the failed read: {err}"
        );

        let garbage = dir.path().join("garbage.pem");
        std::fs::write(
            &garbage,
            b"-----BEGIN CERTIFICATE-----\n@@@ not base64 @@@\n-----END CERTIFICATE-----\n",
        )
        .expect("write malformed cert");
        let err = load_private_ca(&garbage).expect_err("malformed certificate must error");
        assert!(
            err.to_string().contains("--quic-ca") && err.to_string().contains("parsing certificate"),
            "malformed-PEM error must name the flag and the parse failure: {err}"
        );

        let empty = dir.path().join("empty.pem");
        std::fs::write(
            &empty,
            b"-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n",
        )
        .expect("write cert-less pem");
        let err = load_private_ca(&empty).expect_err("cert-less PEM must error");
        assert!(
            err.to_string().contains("no certificates found"),
            "a PEM without a certificate must report none found: {err}"
        );
    }
}
