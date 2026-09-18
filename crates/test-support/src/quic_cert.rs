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
