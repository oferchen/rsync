//! Client-side trust for the QUIC transport.
//!
//! The bytes inside a QUIC session are the unmodified daemon protocol; this
//! module only decides *whether to trust the server's certificate*, then hands
//! the decision to [`QuicConnector`](super::QuicConnector) as a [`QuicTrust`].
//!
//! There is exactly one trust source: the private CA bundle named by
//! `--quic-ca` ([`load_private_ca`]/[`private_ca_file`]). The server chain must
//! verify against it - issuer, SAN and validity period - or the handshake
//! fails. There is no default trust: with no `--quic-ca`, [`resolve`] refuses
//! before any packet is sent, and there is no fallback to the platform trust
//! store.
//!
//! See `docs/design/quic-security-model.md` (section 7.1).

use std::fmt;
use std::fs;
use std::io;
use std::path::Path;

use rustls::RootCertStore;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

use super::{QuicTrust, io_err};

/// Loads a private CA bundle from the PEM file at `path` into a
/// [`RootCertStore`] (QUIC-5b, `--quic-ca`).
///
/// This is the only client trust source: an operator points `--quic-ca` at the
/// certificate that anchors their daemon's chain (a corporate, internal or
/// self-made CA), and the server chain is verified against it. Only `CERTIFICATE` PEM
/// sections are consumed; any other section kind is ignored, matching how
/// openssl/stunnel treat a CA bundle.
///
/// Fails loudly rather than trusting nothing: an unreadable file, a malformed
/// PEM certificate, a certificate rustls rejects, or a file with no
/// certificate at all each returns a distinct error naming `path`. Callers map
/// the [`io::Error`] to the appropriate exit code at the CLI boundary.
///
/// # Errors
///
/// Returns an error when the file cannot be read, contains no certificates, or a certificate is
/// rejected.
pub fn load_private_ca(path: &Path) -> io::Result<RootCertStore> {
    let pem = fs::read(path).map_err(|err| {
        io_err(format!(
            "quic: reading --quic-ca file {}: {err}",
            path.display()
        ))
    })?;
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
/// Convenience wrapper over [`load_private_ca`] that yields the
/// [`QuicTrust::Roots`] the connector consumes. The connector verifies the
/// chain with rustls' WebPKI verifier, so the check is strict: a leaf from
/// another issuer, a SAN that does not name the dialed host, or a certificate
/// outside its validity period fails the handshake.
///
/// # Errors
///
/// Returns the error from `load_private_ca`.
pub fn private_ca_file(path: &Path) -> io::Result<QuicTrust> {
    Ok(QuicTrust::Roots(load_private_ca(path)?))
}

/// Loads a PEM certificate chain (leaf first) and its private key from `cert`
/// and `key`.
///
/// The single owner of PEM chain+key loading for the QUIC transport: both the
/// daemon's server identity ([`QuicServerIdentity::PemFiles`](super::QuicServerIdentity))
/// and the client's mutual-TLS certificate ([`ClientAuth::from_pem_files`]) go
/// through here, so the two sides accept identical file shapes and surface
/// identical errors. The key is PKCS#8, PKCS#1, or SEC1 (the encodings the ring
/// provider accepts). Fails loudly - a missing/unreadable file, a malformed PEM
/// certificate, a chain with no certificate, or an unreadable key each return an
/// [`io::Error`] naming the offending path - rather than yielding an empty chain
/// that would fail opaquely later in the handshake.
///
/// # Errors
///
/// Returns an error when either PEM file cannot be read or parsed, or the chain is empty.
pub fn load_cert_chain_and_key(
    cert: &Path,
    key: &Path,
) -> io::Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    let chain: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(cert)
        .map_err(io_err)?
        .collect::<Result<_, _>>()
        .map_err(io_err)?;
    if chain.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("no certificate found in {}", cert.display()),
        ));
    }
    let key = PrivateKeyDer::from_pem_file(key).map_err(io_err)?;
    Ok((chain, key))
}

/// A client certificate chain and its private key, presented to the daemon in
/// the QUIC handshake for mutual TLS (`--quic-cert` / `--quic-key`).
///
/// Loaded from operator-supplied PEM files through the shared
/// [`load_cert_chain_and_key`] loader, so it accepts exactly the same chain/key
/// shapes as the daemon's own [`QuicServerIdentity::PemFiles`](super::QuicServerIdentity).
/// Handed to [`QuicConnector::with_trust_tuned_client_auth`](super::QuicConnector::with_trust_tuned_client_auth),
/// which installs it via rustls `with_client_auth_cert`. Absent it, the client
/// presents no certificate and the handshake is byte-identical to today's.
pub struct ClientAuth {
    pub(super) chain: Vec<CertificateDer<'static>>,
    pub(super) key: PrivateKeyDer<'static>,
}

impl ClientAuth {
    /// Loads the client certificate chain (leaf first) from `cert` and its
    /// private key from `key`.
    ///
    /// Delegates to [`load_cert_chain_and_key`], so a missing file, a malformed
    /// certificate, or an unreadable key fails loudly with a path-naming error
    /// rather than silently presenting no certificate.
    ///
    /// # Errors
    ///
    /// Returns the error from `load_cert_chain_and_key`.
    pub fn from_pem_files(cert: &Path, key: &Path) -> io::Result<Self> {
        let (chain, key) = load_cert_chain_and_key(cert, key)?;
        Ok(Self { chain, key })
    }
}

impl fmt::Debug for ClientAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientAuth")
            .field("chain_len", &self.chain.len())
            .finish_non_exhaustive()
    }
}

/// Resolves the client trust source from `--quic-ca`.
///
/// `Some(path)` loads the bundle through [`private_ca_file`]. `None` is an
/// error: QUIC has no default trust, so a client without `--quic-ca` refuses
/// before dialing rather than trusting a certificate it cannot verify.
pub fn resolve(ca: Option<&Path>) -> io::Result<QuicTrust> {
    match ca {
        Some(path) => private_ca_file(path),
        None => Err(io_err(
            "quic: no trust anchor configured; pass --quic-ca <PATH> naming the CA \
             that signed the daemon certificate (QUIC has no default trust)",
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use base64::Engine as _;
    use rustls::client::danger::ServerCertVerifier;
    use rustls::pki_types::{ServerName, UnixTime};

    use super::super::{QuicConnector, QuicTrust, ring_provider};
    use super::*;

    /// A fresh self-signed certificate, unrelated to any test CA.
    fn synthetic_cert() -> CertificateDer<'static> {
        rcgen::generate_simple_self_signed(vec!["host.example".to_owned()])
            .expect("generate self-signed")
            .cert
            .der()
            .clone()
    }

    /// PEM-encodes a certificate DER (`-----BEGIN CERTIFICATE-----` framing,
    /// standard base64 wrapped at 64 columns).
    fn cert_to_pem(der: &CertificateDer<'_>) -> String {
        use base64::engine::general_purpose::STANDARD;
        let mut pem = String::from("-----BEGIN CERTIFICATE-----\n");
        for chunk in STANDARD.encode(der.as_ref()).as_bytes().chunks(64) {
            pem.push_str(std::str::from_utf8(chunk).expect("base64 is ascii"));
            pem.push('\n');
        }
        pem.push_str("-----END CERTIFICATE-----\n");
        pem
    }

    /// `load_private_ca` parses a PEM bundle into a usable root store the
    /// connector accepts, and fails loudly on a missing file or one with no
    /// certificate - a mistyped or empty `--quic-ca` must never silently
    /// degrade trust.
    #[test]
    fn load_private_ca_reads_pem_and_rejects_empty() {
        let dir = test_support::create_tempdir();

        let issued =
            rcgen::generate_simple_self_signed(vec!["ca.example".to_owned()]).expect("gen cert");
        let pem_path = dir.path().join("ca.pem");
        std::fs::write(&pem_path, cert_to_pem(issued.cert.der())).expect("write pem");

        let roots = load_private_ca(&pem_path).expect("load ca bundle");
        assert!(!roots.is_empty(), "the bundle must yield a trust anchor");
        QuicConnector::with_trust(QuicTrust::Roots(roots))
            .expect("connector accepts the loaded CA");

        let missing = dir.path().join("does-not-exist.pem");
        assert!(
            load_private_ca(&missing).is_err(),
            "missing file must error"
        );

        let empty = dir.path().join("empty.pem");
        std::fs::write(&empty, b"not a certificate\n").expect("write empty");
        assert!(
            load_private_ca(&empty).is_err(),
            "a bundle with no certificate must error"
        );
    }

    /// With no `--quic-ca` there is no trust source at all, so `resolve`
    /// refuses and names the flag. Encodes WHY: QUIC has no default trust, and
    /// the refusal must tell the operator what to configure rather than fail
    /// later in the handshake.
    #[test]
    fn resolve_without_ca_refuses_and_names_the_flag() {
        let err = resolve(None).expect_err("no --quic-ca must not yield a trust source");
        assert!(
            err.to_string().contains("--quic-ca"),
            "the refusal must name --quic-ca: {err}"
        );
    }

    /// With `--quic-ca`, `resolve` yields the root store the connector hands to
    /// the rustls WebPKI verifier, so the bundle is the whole trust decision.
    #[test]
    fn resolve_with_ca_yields_a_strict_root_store() {
        let (ca_pem, _leaf) = private_ca_and_leaf();
        let dir = test_support::create_tempdir();
        let ca_path = dir.path().join("private-ca.pem");
        std::fs::write(&ca_path, ca_pem).expect("write ca pem");

        let trust = resolve(Some(&ca_path)).expect("resolve with CA");
        assert!(
            matches!(trust, QuicTrust::Roots(_)),
            "--quic-ca must resolve to the strict root store"
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
        let dir = test_support::create_tempdir();
        let ca_path = dir.path().join("private-ca.pem");
        std::fs::write(&ca_path, ca_pem).expect("write ca pem");

        let roots = load_private_ca(&ca_path).expect("load private ca");
        assert_eq!(
            roots.len(),
            1,
            "one CA certificate becomes one trust anchor"
        );

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

    /// The standalone `private_ca_file` builder loads a real `--quic-ca` bundle
    /// into the `QuicTrust::Roots` source the connector consumes directly.
    #[test]
    fn private_ca_file_yields_a_roots_trust_source() {
        let (ca_pem, _leaf) = private_ca_and_leaf();
        let dir = test_support::create_tempdir();
        let ca_path = dir.path().join("private-ca.pem");
        std::fs::write(&ca_path, ca_pem).expect("write ca pem");

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
        let dir = test_support::create_tempdir();

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
            err.to_string().contains("--quic-ca")
                && err.to_string().contains("parsing certificate"),
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
