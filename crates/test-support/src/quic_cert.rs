//! Self-signed QUIC certificate generation for loopback end-to-end tests.
//!
//! The QUIC daemon listener has no ephemeral/self-signed fallback: a request
//! without both `quic cert file` and `quic key file` is a fatal
//! misconfiguration (see `daemon/.../runtime_options/quic_identity.rs`). A test
//! that stands up a real QUIC daemon must therefore supply an operator-shaped
//! PEM cert/key pair, and the dialing client must trust it. This helper is the
//! single owner of that fixture so every QUIC e2e test mints an identity the
//! same way rather than reinventing the cert plumbing.
//!
//! The certificate carries a `localhost` subject-alternative name, so a client
//! dialing `quic://localhost` passes rustls hostname verification. The same
//! leaf PEM serves two roles: the daemon's `quic cert file`, and the client's
//! `--quic-ca` trust anchor - a self-signed leaf is its own root, the shape
//! `rsync_io`'s `round_trip_via_roots_trust` loopback test already relies on.

/// A PEM-encoded self-signed certificate and its private key, both carrying a
/// `localhost` subject-alternative name.
pub struct SelfSignedPem {
    /// PEM certificate (leaf). Usable as the daemon `quic cert file` and,
    /// unchanged, as the client `--quic-ca` trust anchor.
    pub cert_pem: String,
    /// PEM PKCS#8 private key for the daemon `quic key file`.
    pub key_pem: String,
}

/// Generates a fresh `localhost` self-signed certificate/key pair for a
/// loopback QUIC daemon fixture.
///
/// # Panics
///
/// Panics if `rcgen` cannot generate the certificate, which in a test context
/// is an environment failure the caller cannot recover from.
#[must_use]
pub fn localhost_self_signed() -> SelfSignedPem {
    let issued = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()])
        .expect("rcgen: generate self-signed localhost certificate");
    SelfSignedPem {
        cert_pem: pem_block("CERTIFICATE", issued.cert.der()),
        key_pem: pem_block("PRIVATE KEY", &issued.signing_key.serialize_der()),
    }
}

/// A `localhost` leaf certificate signed by a freshly minted private CA, with
/// the CA certificate kept separate - the shape of a daemon behind an internal
/// CA, as opposed to [`localhost_self_signed`]'s self-anchored leaf.
pub struct CaSignedPem {
    /// PEM certificate of the issuing CA, for the client `--quic-ca`.
    pub ca_pem: String,
    /// PEM leaf certificate (SAN `localhost`, `serverAuth`), for the daemon.
    pub cert_pem: String,
    /// PEM PKCS#8 private key of the leaf, for the daemon.
    pub key_pem: String,
}

/// Mints a private CA and a `localhost` server leaf it signs.
///
/// Each call creates a new CA, so two calls give two unrelated trust anchors:
/// one fixture's CA does not verify the other fixture's leaf.
///
/// # Panics
///
/// Panics if `rcgen` cannot generate a key or certificate, which in a test
/// context is an environment failure the caller cannot recover from.
#[must_use]
pub fn localhost_ca_signed() -> CaSignedPem {
    use rcgen::{
        BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
        KeyUsagePurpose,
    };

    let ca_key = KeyPair::generate().expect("rcgen: generate CA key");
    let mut ca_params =
        CertificateParams::new(vec!["oc-rsync test ca".to_owned()]).expect("rcgen: CA params");
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let ca_cert = ca_params.self_signed(&ca_key).expect("rcgen: self-sign CA");
    let issuer = Issuer::new(ca_params, ca_key);

    let leaf_key = KeyPair::generate().expect("rcgen: generate leaf key");
    let mut leaf_params =
        CertificateParams::new(vec!["localhost".to_owned()]).expect("rcgen: leaf params");
    leaf_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let leaf = leaf_params
        .signed_by(&leaf_key, &issuer)
        .expect("rcgen: sign leaf");

    CaSignedPem {
        ca_pem: pem_block("CERTIFICATE", ca_cert.der()),
        cert_pem: pem_block("CERTIFICATE", leaf.der()),
        key_pem: pem_block("PRIVATE KEY", &leaf_key.serialize_der()),
    }
}

/// Wraps DER bytes in a PEM block with `label` (e.g. `CERTIFICATE`,
/// `PRIVATE KEY`), 64-character base64 lines. `serialize_der` emits a PKCS#8
/// key, so `PRIVATE KEY` is the correct label for the key block.
fn pem_block(label: &str, der: &[u8]) -> String {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;

    let mut out = format!("-----BEGIN {label}-----\n");
    for chunk in STANDARD.encode(der).as_bytes().chunks(64) {
        out.push_str(std::str::from_utf8(chunk).expect("base64 output is ASCII"));
        out.push('\n');
    }
    out.push_str(&format!("-----END {label}-----\n"));
    out
}
