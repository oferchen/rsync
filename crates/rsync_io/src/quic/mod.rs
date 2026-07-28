//! Feature-gated QUIC transport measurement spike.
//!
//! Establishes a QUIC connection (quinn + rustls with the ring provider) over
//! loopback using an in-process self-signed certificate, opens one
//! bidirectional stream, and exposes it as a blocking [`std::io::Read`] /
//! [`std::io::Write`] pair - the same shape the SSH transports present to the
//! negotiation layer.
//!
//! # Runtime confinement
//!
//! The tokio runtime is confined entirely to this module: each endpoint owns a
//! current-thread runtime and the blocking wrappers `block_on` individual
//! stream operations, mirroring the confinement philosophy of the
//! `embedded-ssh` transport. A consequence of the current-thread runtime is
//! that quinn's endpoint driver only makes progress while a `block_on` call is
//! in flight; queued packets are flushed by the next blocking operation on the
//! same endpoint. [`QuicStream::finish`] provides a deterministic delivery
//! barrier for the final write.
//!
//! The production design will evaluate driving `quinn-proto` sans-IO from a
//! plain thread instead; this spike intentionally uses the simplest working
//! path to measure build cost, dependency weight, and glue-code burden.
//!
//! # Scope
//!
//! No certificate-file loading, CLI flags, daemon integration, or config
//! surface. The acceptor generates a fresh self-signed certificate at bind
//! time and exposes its DER encoding so a test connector can pin it.

use std::io::{self, Read, Write};
use std::net::SocketAddr;
use std::sync::Arc;

use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use quinn::{Connection, Endpoint, RecvStream, SendStream};
use rustls::RootCertStore;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::runtime::{Builder, Runtime};

/// ALPN protocol identifier advertised on every QUIC connection.
pub const ALPN_RSYNC: &[u8] = b"rsync";

fn io_err(err: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> io::Error {
    io::Error::other(err)
}

/// A peer that closed the connection with application error code 0 terminated
/// the session cleanly; the blocking wrappers treat that as end-of-session
/// rather than failure, mirroring how a TCP transport treats a clean close.
fn is_clean_close(err: &quinn::ConnectionError) -> bool {
    matches!(
        err,
        quinn::ConnectionError::ApplicationClosed(close) if close.error_code == quinn::VarInt::from_u32(0)
    )
}

fn current_thread_runtime() -> io::Result<Runtime> {
    Builder::new_current_thread().enable_all().build()
}

/// Builds a rustls config pinned to TLS 1.3 with the ring provider.
///
/// Both sides construct their config through `builder_with_provider` so the
/// choice of crypto provider is explicit and immune to workspace feature
/// unification pulling in an alternative default provider.
fn ring_provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// QUIC listener: binds a UDP socket, generates a self-signed certificate,
/// and accepts one blocking stream per incoming connection.
pub struct QuicAcceptor {
    runtime: Arc<Runtime>,
    endpoint: Endpoint,
    certificate: CertificateDer<'static>,
}

impl QuicAcceptor {
    /// Binds a QUIC server endpoint on `addr` with a fresh self-signed
    /// certificate valid for `localhost`.
    pub fn bind(addr: SocketAddr) -> io::Result<Self> {
        let runtime = Arc::new(current_thread_runtime()?);
        let issued =
            rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).map_err(io_err)?;
        let certificate = issued.cert.der().clone();
        let key = PrivateKeyDer::from(PrivatePkcs8KeyDer::from(issued.signing_key.serialize_der()));

        let mut server_crypto = rustls::ServerConfig::builder_with_provider(ring_provider())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(io_err)?
            .with_no_client_auth()
            .with_single_cert(vec![certificate.clone()], key)
            .map_err(io_err)?;
        server_crypto.alpn_protocols = vec![ALPN_RSYNC.to_vec()];

        let server_config = quinn::ServerConfig::with_crypto(Arc::new(
            QuicServerConfig::try_from(server_crypto).map_err(io_err)?,
        ));

        // Endpoint creation registers the UDP socket with the runtime's
        // reactor, so it must run inside the runtime context.
        let _guard = runtime.enter();
        let endpoint = Endpoint::server(server_config, addr)?;
        drop(_guard);
        Ok(Self {
            runtime,
            endpoint,
            certificate,
        })
    }

    /// Returns the bound local address (useful with port 0).
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.endpoint.local_addr()
    }

    /// DER encoding of the self-signed certificate, for pinning by a
    /// [`QuicConnector`].
    pub fn certificate(&self) -> &CertificateDer<'static> {
        &self.certificate
    }

    /// Blocks until a client connects and opens a bidirectional stream.
    pub fn accept(&self) -> io::Result<QuicStream> {
        self.runtime.block_on(async {
            let incoming =
                self.endpoint.accept().await.ok_or_else(|| {
                    io::Error::new(io::ErrorKind::NotConnected, "endpoint closed")
                })?;
            let connection = incoming.await.map_err(io_err)?;
            let (send, recv) = connection.accept_bi().await.map_err(io_err)?;
            Ok(QuicStream {
                runtime: Arc::clone(&self.runtime),
                endpoint: self.endpoint.clone(),
                connection,
                send,
                recv,
            })
        })
    }
}

impl std::fmt::Debug for QuicAcceptor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuicAcceptor")
            .field("local_addr", &self.endpoint.local_addr())
            .finish_non_exhaustive()
    }
}

/// QUIC client: connects to an acceptor whose certificate it pins.
pub struct QuicConnector {
    runtime: Arc<Runtime>,
    endpoint: Endpoint,
}

impl QuicConnector {
    /// Creates a client endpoint that trusts exactly `server_certificate`.
    pub fn new(server_certificate: &CertificateDer<'_>) -> io::Result<Self> {
        let runtime = Arc::new(current_thread_runtime()?);
        let mut roots = RootCertStore::empty();
        roots
            .add(server_certificate.clone().into_owned())
            .map_err(io_err)?;

        let mut client_crypto = rustls::ClientConfig::builder_with_provider(ring_provider())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(io_err)?
            .with_root_certificates(roots)
            .with_no_client_auth();
        client_crypto.alpn_protocols = vec![ALPN_RSYNC.to_vec()];

        let client_config = quinn::ClientConfig::new(Arc::new(
            QuicClientConfig::try_from(client_crypto).map_err(io_err)?,
        ));

        let _guard = runtime.enter();
        let mut endpoint = Endpoint::client(SocketAddr::from(([0, 0, 0, 0], 0)))?;
        drop(_guard);
        endpoint.set_default_client_config(client_config);
        Ok(Self { runtime, endpoint })
    }

    /// Connects to `addr`, validating the peer as `server_name`, and opens
    /// one bidirectional stream.
    pub fn connect(&self, addr: SocketAddr, server_name: &str) -> io::Result<QuicStream> {
        self.runtime.block_on(async {
            let connection = self
                .endpoint
                .connect(addr, server_name)
                .map_err(io_err)?
                .await
                .map_err(io_err)?;
            let (send, recv) = connection.open_bi().await.map_err(io_err)?;
            Ok(QuicStream {
                runtime: Arc::clone(&self.runtime),
                endpoint: self.endpoint.clone(),
                connection,
                send,
                recv,
            })
        })
    }
}

impl std::fmt::Debug for QuicConnector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuicConnector")
            .field("local_addr", &self.endpoint.local_addr())
            .finish_non_exhaustive()
    }
}

/// One bidirectional QUIC stream exposed as blocking `Read` + `Write`.
///
/// Keeps the originating endpoint and connection alive for as long as the
/// stream is in use; dropping the stream releases both, closing the
/// connection once no other handles remain.
pub struct QuicStream {
    runtime: Arc<Runtime>,
    endpoint: Endpoint,
    connection: Connection,
    send: SendStream,
    recv: RecvStream,
}

impl QuicStream {
    /// Signals end-of-stream to the peer and blocks until every written byte
    /// has been acknowledged (or the peer stopped the stream).
    ///
    /// This is the delivery barrier for the final write: with a
    /// current-thread runtime nothing polls the endpoint between blocking
    /// calls, so a plain `write` may leave the tail queued locally. The peer
    /// must keep issuing blocking calls of its own until this resolves; a
    /// clean connection close from the peer (which it only sends after the
    /// application layer confirmed receipt) also counts as success, because
    /// the final transport ACK and a CONNECTION_CLOSE frame can race.
    pub fn finish(&mut self) -> io::Result<()> {
        self.send.finish().map_err(io_err)?;
        match self.runtime.block_on(self.send.stopped()) {
            Ok(_) => Ok(()),
            Err(quinn::StoppedError::ConnectionLost(err)) if is_clean_close(&err) => Ok(()),
            Err(err) => Err(io_err(err)),
        }
    }

    /// Gracefully closes the connection (application error code 0) and blocks
    /// until the endpoint has drained.
    ///
    /// Call this as the terminal operation on the side that stops polling
    /// first: with a current-thread runtime nothing drives the endpoint after
    /// the last blocking call, so without an explicit close the peer only
    /// discovers the end of the session via its idle timeout.
    pub fn close(self) {
        self.connection.close(quinn::VarInt::from_u32(0), b"");
        self.runtime.block_on(self.endpoint.wait_idle());
    }
}

impl Read for QuicStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.runtime.block_on(self.recv.read(buf)) {
            // `None` means the peer finished the stream: clean EOF.
            Ok(read) => Ok(read.unwrap_or(0)),
            Err(quinn::ReadError::ConnectionLost(err)) if is_clean_close(&err) => Ok(0),
            Err(err) => Err(io_err(err)),
        }
    }
}

impl Write for QuicStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.runtime.block_on(self.send.write(buf)).map_err(io_err)
    }

    /// No-op: quinn hands written data to the transport immediately; there is
    /// no user-space buffer to flush. Use [`QuicStream::finish`] as the
    /// delivery barrier for the final write.
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl std::fmt::Debug for QuicStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuicStream")
            .field("send", &self.send.id())
            .field("recv", &self.recv.id())
            .finish_non_exhaustive()
    }
}
