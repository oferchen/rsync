//! TLS connector for client-side rsync:// connections.
//!
//! Wraps a `TcpStream` in a rustls TLS session, performing the client
//! handshake and returning a stream that implements `Read + Write` over
//! the encrypted channel. This eliminates the need for an external stunnel
//! wrapper when connecting to TLS-enabled rsync daemons.
//!
//! # Certificate Verification
//!
//! By default the connector trusts the Mozilla root CA bundle shipped by
//! the `webpki-roots` crate. An optional custom CA path loads additional
//! (or replacement) trust anchors from a PEM file on disk.
//!
//! # Usage
//!
//! The [`TlsConnector`] is constructed once and reused across connections.
//! Call [`TlsConnector::wrap`] to upgrade a connected `TcpStream` into a
//! TLS-protected stream.

use std::fs;
use std::io;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustls::pki_types::{CertificateDer, ServerName};
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};

/// Client-side TLS configuration for rsync:// connections.
///
/// Holds filesystem paths for optional certificate material. When
/// `ca_cert_path` is `None`, the Mozilla root CA bundle is used.
#[derive(Clone, Debug, Default)]
pub(crate) struct TlsClientConfig {
    /// Optional path to a PEM-encoded CA bundle for server certificate
    /// verification. When `None`, the built-in Mozilla root CAs are used.
    pub(crate) ca_cert_path: Option<PathBuf>,
}

/// Reusable TLS connector backed by a shared rustls `ClientConfig`.
///
/// Cloning is cheap (inner `Arc`). Build one per process or per
/// configuration and share it across connections.
#[derive(Clone, Debug)]
pub(crate) struct TlsConnector {
    config: Arc<ClientConfig>,
}

impl TlsConnector {
    /// Constructs a new connector from the given TLS client configuration.
    ///
    /// Loads root certificates from `config.ca_cert_path` when provided,
    /// otherwise falls back to the Mozilla root CA bundle. The connector
    /// uses the ring crypto provider and supports TLS 1.2 and 1.3.
    ///
    /// # Errors
    ///
    /// Returns `io::Error` if the custom CA file cannot be read or parsed,
    /// or if rustls rejects the resulting configuration.
    pub(crate) fn new(config: &TlsClientConfig) -> Result<Self, io::Error> {
        let root_store = build_root_store(config.ca_cert_path.as_deref())?;

        let client_config =
            ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
                .with_root_certificates(root_store)
                .with_no_client_auth();

        Ok(Self {
            config: Arc::new(client_config),
        })
    }

    /// Performs a TLS handshake on a connected TCP stream.
    ///
    /// The `hostname` is used for SNI (Server Name Indication) and
    /// certificate verification. On success, returns a [`TlsStream`]
    /// that implements `Read + Write` over the encrypted channel.
    ///
    /// # Errors
    ///
    /// Returns `io::Error` if the hostname is not a valid DNS name or
    /// IP address, or if the TLS handshake fails.
    pub(crate) fn wrap(&self, stream: TcpStream, hostname: &str) -> Result<TlsStream, io::Error> {
        let server_name = ServerName::try_from(hostname.to_owned())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;

        let connection = ClientConnection::new(Arc::clone(&self.config), server_name)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        Ok(TlsStream {
            inner: StreamOwned::new(connection, stream),
        })
    }
}

/// TLS-wrapped TCP stream implementing `Read + Write`.
///
/// Wraps a `rustls::StreamOwned<ClientConnection, TcpStream>` and
/// delegates all I/O through the encrypted channel. The `Debug` impl
/// shows the peer address when available.
pub(crate) struct TlsStream {
    inner: StreamOwned<ClientConnection, TcpStream>,
}

impl std::fmt::Debug for TlsStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let peer = self.inner.sock.peer_addr().ok();
        f.debug_struct("TlsStream")
            .field("peer_addr", &peer)
            .finish()
    }
}

impl io::Read for TlsStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
}

impl io::Write for TlsStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Builds a root certificate store for server verification.
///
/// When `ca_path` is `Some`, loads PEM certificates from disk and uses
/// them exclusively. When `None`, populates the store from the Mozilla
/// root CA bundle shipped by `webpki-roots`.
fn build_root_store(ca_path: Option<&Path>) -> Result<RootCertStore, io::Error> {
    let mut store = RootCertStore::empty();

    if let Some(path) = ca_path {
        let pem_data = fs::read(path)?;
        let certs: Vec<CertificateDer<'static>> =
            rustls_pemfile::certs(&mut pem_data.as_slice()).collect::<Result<Vec<_>, _>>()?;

        if certs.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("no CA certificates found in {}", path.display()),
            ));
        }

        for cert in certs {
            store
                .add(cert)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        }
    } else {
        store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    }

    Ok(store)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_tls_client_config_has_no_ca_path() {
        let config = TlsClientConfig::default();
        assert!(config.ca_cert_path.is_none());
    }

    #[test]
    fn tls_client_config_with_custom_ca_path() {
        let config = TlsClientConfig {
            ca_cert_path: Some(PathBuf::from("/etc/certs/custom-ca.pem")),
        };
        assert_eq!(
            config.ca_cert_path.as_deref(),
            Some(Path::new("/etc/certs/custom-ca.pem"))
        );
    }

    #[test]
    fn tls_connector_builds_with_default_roots() {
        let config = TlsClientConfig::default();
        let connector = TlsConnector::new(&config);
        assert!(connector.is_ok(), "expected Ok, got: {connector:?}");
    }

    #[test]
    fn tls_connector_rejects_missing_ca_file() {
        let config = TlsClientConfig {
            ca_cert_path: Some(PathBuf::from("/nonexistent/ca.pem")),
        };
        let err = TlsConnector::new(&config).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn tls_connector_rejects_empty_ca_file() {
        let dir = tempfile::tempdir().unwrap();
        let ca_path = dir.path().join("empty.pem");
        fs::write(&ca_path, "").unwrap();

        let config = TlsClientConfig {
            ca_cert_path: Some(ca_path),
        };
        let err = TlsConnector::new(&config).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(
            err.to_string().contains("no CA certificates"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn tls_connector_rejects_invalid_hostname() {
        let config = TlsClientConfig::default();
        let connector = TlsConnector::new(&config).unwrap();

        // Create a connected TcpStream via loopback
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let stream = TcpStream::connect(addr).unwrap();
        let (_server, _) = listener.accept().unwrap();

        // An empty hostname is invalid for SNI
        let err = connector.wrap(stream, "").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn tls_connector_is_clone() {
        let config = TlsClientConfig::default();
        let connector = TlsConnector::new(&config).unwrap();
        let cloned = connector.clone();
        // Both share the same Arc - verify they're usable
        assert!(Arc::ptr_eq(&connector.config, &cloned.config));
    }

    #[test]
    fn tls_stream_debug_format() {
        let config = TlsClientConfig::default();
        let connector = TlsConnector::new(&config).unwrap();

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let stream = TcpStream::connect(addr).unwrap();
        let (_server, _) = listener.accept().unwrap();

        // wrap will create the TLS connection object (handshake is lazy in
        // rustls - actual TLS frames are exchanged on first read/write)
        let tls_stream = connector.wrap(stream, "localhost").unwrap();
        let debug = format!("{tls_stream:?}");
        assert!(debug.contains("TlsStream"), "got: {debug}");
    }

    #[test]
    fn build_root_store_with_default_mozilla_roots() {
        let store = build_root_store(None).unwrap();
        // The Mozilla root bundle has > 100 CAs
        assert!(
            store.len() > 50,
            "expected many root CAs, got {}",
            store.len()
        );
    }

    #[test]
    fn build_root_store_rejects_garbage_pem() {
        let dir = tempfile::tempdir().unwrap();
        let ca_path = dir.path().join("garbage.pem");
        fs::write(&ca_path, "not a real PEM file").unwrap();

        let err = build_root_store(Some(&ca_path)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }
}
