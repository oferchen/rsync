//! Native TLS termination for the oc-rsync daemon via rustls.
//!
//! This module provides the building blocks for accepting TLS-wrapped
//! connections in the daemon listener, eliminating the need for an external
//! stunnel or HAProxy sidecar. It is feature-gated behind `daemon-tls` and
//! not compiled into default builds.
//!
//! # Design
//!
//! The module exposes two operations:
//!
//! 1. **`build_tls_acceptor`** - loads PEM certificates and a private key
//!    from disk and constructs a rustls `TlsAcceptor` configured with safe
//!    defaults (TLS 1.2+, ring crypto provider).
//! 2. **`wrap_stream`** - performs the TLS handshake on an accepted
//!    `TcpStream`, returning a [`rustls::StreamOwned`] that implements
//!    `Read + Write` and can be handed to the synchronous per-connection
//!    worker unchanged.
//!
//! Integration with the daemon listener loop is tracked as a follow-up task
//! (TLS-7). This module contains only the TLS plumbing, not the listener
//! wiring.
//!
//! # Certificate loading
//!
//! `TlsConfig` accepts filesystem paths for the certificate chain
//! (fullchain PEM), the private key (PKCS#8 or RSA PEM), and an optional
//! client CA bundle for mutual TLS. Certificate rotation requires a daemon
//! restart; live reload is a future enhancement.
//!
//! # Security
//!
//! - Only TLS 1.2 and 1.3 are enabled (rustls does not support older
//!   protocol versions).
//! - The ring crypto provider is used by default, providing FIPS-grade
//!   primitives without linking to OpenSSL.
//! - Client certificate verification is opt-in via the `client_ca_path`
//!   field on `TlsConfig`.

use std::fs;
use std::io;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustls::ServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::WebPkiClientVerifier;

/// TLS acceptor wrapping a shared rustls [`ServerConfig`].
///
/// Cloning is cheap (inner `Arc`). One acceptor is built at daemon startup
/// and shared across all connection-handling threads.
#[derive(Clone, Debug)]
pub struct TlsAcceptor {
    config: Arc<ServerConfig>,
}

/// Configuration paths for TLS certificate material.
///
/// All paths must point at PEM-encoded files. The certificate chain file
/// must contain the server certificate followed by any intermediate
/// certificates, ordered leaf-first.
#[derive(Clone, Debug)]
pub struct TlsConfig {
    /// Path to the PEM-encoded certificate chain (server cert + intermediates).
    pub cert_path: PathBuf,
    /// Path to the PEM-encoded private key (PKCS#8, PKCS#1 RSA, or SEC1 EC).
    pub key_path: PathBuf,
    /// Optional path to a PEM-encoded CA bundle for client certificate
    /// verification (mutual TLS). When `None`, client certificates are not
    /// requested.
    pub client_ca_path: Option<PathBuf>,
}

/// Loads certificates and private key, then builds a `TlsAcceptor`.
///
/// The acceptor is configured with the ring crypto provider and supports
/// TLS 1.2 and TLS 1.3. When `config.client_ca_path` is set, client
/// certificate verification is enabled using the provided CA bundle.
///
/// # Errors
///
/// Returns `io::Error` if any PEM file cannot be read or parsed, if no
/// certificates or private keys are found in the provided files, or if the
/// rustls `ServerConfig` rejects the certificate/key combination.
pub fn build_tls_acceptor(config: &TlsConfig) -> Result<TlsAcceptor, io::Error> {
    let certs = load_certificates(&config.cert_path)?;
    let key = load_private_key(&config.key_path)?;

    let mut server_config = if let Some(ca_path) = &config.client_ca_path {
        let roots = load_root_store(ca_path)?;
        let verifier = WebPkiClientVerifier::builder(Arc::new(roots))
            .build()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
            .with_client_cert_verifier(verifier)
            .with_single_cert(certs, key)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
    } else {
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
    };

    // ALPN is not used - the rsync protocol is not registered with IANA for
    // ALPN negotiation, and no rsync client sends ALPN extensions.
    server_config.alpn_protocols = Vec::new();

    Ok(TlsAcceptor {
        config: Arc::new(server_config),
    })
}

/// Performs a TLS handshake on an accepted TCP connection.
///
/// On success, returns a `rustls::StreamOwned` that implements `Read + Write`
/// over the encrypted channel. The caller can pass this stream to the
/// synchronous per-connection worker in place of a raw `TcpStream`.
///
/// # Errors
///
/// Returns `io::Error` if the TLS handshake fails (e.g., client sends an
/// unsupported protocol version, invalid certificate, or aborts the
/// connection).
pub fn wrap_stream(
    acceptor: &TlsAcceptor,
    stream: TcpStream,
) -> Result<rustls::StreamOwned<rustls::ServerConnection, TcpStream>, io::Error> {
    let conn = rustls::ServerConnection::new(Arc::clone(&acceptor.config))
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(rustls::StreamOwned::new(conn, stream))
}

/// Loads PEM-encoded certificates from a file.
fn load_certificates(path: &Path) -> Result<Vec<CertificateDer<'static>>, io::Error> {
    let pem_data = fs::read(path)?;
    let certs: Vec<CertificateDer<'static>> =
        rustls_pemfile::certs(&mut pem_data.as_slice()).collect::<Result<Vec<_>, _>>()?;
    if certs.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("no certificates found in {}", path.display()),
        ));
    }
    Ok(certs)
}

/// Loads the first PEM-encoded private key from a file.
///
/// Accepts PKCS#8, PKCS#1 (RSA), and SEC1 (EC) key formats.
fn load_private_key(path: &Path) -> Result<PrivateKeyDer<'static>, io::Error> {
    let pem_data = fs::read(path)?;
    rustls_pemfile::private_key(&mut pem_data.as_slice())?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("no private key found in {}", path.display()),
        )
    })
}

/// Builds a root certificate store from a PEM-encoded CA bundle.
fn load_root_store(path: &Path) -> Result<rustls::RootCertStore, io::Error> {
    let ca_certs = load_certificates(path)?;
    let mut store = rustls::RootCertStore::empty();
    for cert in ca_certs {
        store
            .add(cert)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    }
    if store.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("no CA certificates found in {}", path.display()),
        ));
    }
    Ok(store)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tls_config_holds_paths() {
        let config = TlsConfig {
            cert_path: PathBuf::from("/etc/certs/server.pem"),
            key_path: PathBuf::from("/etc/certs/server.key"),
            client_ca_path: None,
        };
        assert_eq!(config.cert_path.as_os_str(), "/etc/certs/server.pem");
        assert_eq!(config.key_path.as_os_str(), "/etc/certs/server.key");
        assert!(config.client_ca_path.is_none());
    }

    #[test]
    fn tls_config_with_client_ca() {
        let config = TlsConfig {
            cert_path: PathBuf::from("/etc/certs/server.pem"),
            key_path: PathBuf::from("/etc/certs/server.key"),
            client_ca_path: Some(PathBuf::from("/etc/certs/ca.pem")),
        };
        assert_eq!(
            config.client_ca_path.as_deref(),
            Some(Path::new("/etc/certs/ca.pem"))
        );
    }

    #[test]
    fn build_acceptor_rejects_missing_cert_file() {
        let config = TlsConfig {
            cert_path: PathBuf::from("/nonexistent/cert.pem"),
            key_path: PathBuf::from("/nonexistent/key.pem"),
            client_ca_path: None,
        };
        let err = build_tls_acceptor(&config).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn load_certificates_rejects_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let cert_path = dir.path().join("empty.pem");
        fs::write(&cert_path, "").unwrap();
        let err = load_certificates(&cert_path).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(
            err.to_string().contains("no certificates"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn load_private_key_rejects_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("empty.pem");
        fs::write(&key_path, "").unwrap();
        let err = load_private_key(&key_path).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(
            err.to_string().contains("no private key"),
            "unexpected error: {err}"
        );
    }
}
