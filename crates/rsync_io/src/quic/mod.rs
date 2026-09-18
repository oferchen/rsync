//! Feature-gated sans-IO QUIC transport: `quinn-proto` driven by a dedicated
//! I/O thread.
//!
//! Establishes a QUIC connection over a UDP socket with an in-process
//! self-signed certificate, opens one bidirectional stream, and exposes it as
//! a blocking [`std::io::Read`] / [`std::io::Write`] pair - the same shape
//! the SSH transports present to the negotiation layer. No async runtime: a
//! plain thread owns the UDP socket and the
//! `quinn_proto::Endpoint`/`Connection` state machines. Datagrams are fed via
//! `Endpoint::handle`, timers via `Connection::poll_timeout`/`handle_timeout`,
//! and outgoing packets via `Connection::poll_transmit`.
//!
//! This model won the concurrency-model decision over a confined
//! current-thread tokio runtime around `quinn`; see
//! `docs/design/quic-transport-concurrency-model.md` for the measurements and
//! rationale.
//!
//! # Concurrency primitive
//!
//! The blocking facade and the I/O thread exchange bytes through a
//! condvar-guarded buffer pair (one `Mutex<State>` + one `Condvar`), not an
//! mpsc channel: stream reads and writes need backpressure in both directions
//! plus shared flags (EOF, FIN acknowledgement, terminal errors), and a single
//! guarded struct expresses all of that directly where channels would still
//! need a mutex beside them.
//!
//! Because the I/O thread blocks in `recv_from` (with a timeout derived from
//! the quinn timer), the facade wakes it by sending a one-byte datagram from a
//! dedicated loopback "waker" socket to the endpoint's own port - the UDP
//! analogue of the self-pipe trick. Datagrams from the waker's address are
//! discarded before they reach the QUIC state machine. A 100 ms sleep cap
//! bounds the impact of a hypothetically lost wake datagram.
//!
//! # Teardown
//!
//! The driver thread makes progress regardless of what the facade is doing,
//! so the final transport ACK goes out promptly by construction - there is no
//! equivalent of the confined runtime's "nothing polls between blocking
//! calls" hazard. [`QuicStream::finish`] still blocks until the peer
//! acknowledged (or stopped) the send stream, and [`QuicStream::close`]
//! blocks until the connection has drained and the driver thread exited.
//!
//! # Scope
//!
//! No certificate-file loading, CLI flags, daemon integration, or config
//! surface. The acceptor generates a fresh self-signed certificate at bind
//! time and exposes its DER encoding so a connector can pin it.

mod driver;
mod error;
mod trust;
mod tuning;

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub use bandwidth::{BandwidthLimiter, ThrottlingWriter};

use bytes::Bytes;
use quinn_proto::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use quinn_proto::{ClientConfig, Endpoint, EndpointConfig, ServerConfig};
pub use rustls::RootCertStore;
pub use rustls::client::danger::ServerCertVerifier;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

use driver::{Role, spawn_io};
use tuning::build_transport_config;
pub use tuning::{CongestionAlgorithm, QuicTransportTuning};

pub use error::{
    TransportFault, connect_fault, connection_fault, driver_gone, io_fault, stream_reset,
};
pub use quinn_proto::{ConnectError, ConnectionError, TransportError, TransportErrorCode, VarInt};

pub use trust::{
    ClientAuth, Fingerprint, KnownHostsFile, KnownHostsStore, TofuVerifier, TrustPolicy,
    default_known_hosts_path, load_cert_chain_and_key, load_private_ca, private_ca_file, resolve,
    system_roots, tofu, tofu_file,
};

/// ALPN protocol identifier advertised on every QUIC connection.
pub const ALPN_RSYNC: &[u8] = b"rsync";

/// Facade-to-driver send buffer capacity; writers block once it is full.
const SEND_CAP: usize = 4 * 1024 * 1024;
/// Driver-to-facade receive high-water mark; the driver stops draining the
/// QUIC stream (letting flow control throttle the peer) until the facade
/// consumes down to half of this.
const RECV_HIGH_WATER: usize = 4 * 1024 * 1024;
/// Upper bound on one blocking `recv_from`; bounds recovery from a lost wake
/// datagram without relying on it for correctness.
const MAX_SLEEP: Duration = Duration::from_millis(100);
/// Largest possible UDP datagram payload.
const DATAGRAM_BUF: usize = 65535;

fn io_err(err: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> io::Error {
    io::Error::other(err)
}

fn ring_provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn loopback_of(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::LOCALHOST),
    }
}

/// How the connection ended, recorded once by the driver.
#[derive(Clone, Debug)]
enum Terminal {
    /// Application close with error code 0 (or our own local close): the
    /// session ended cleanly, mirroring how a TCP transport treats a clean
    /// close.
    Clean,
    /// Any other connection loss, classified so the surfaced `io::Error`
    /// carries the `ErrorKind`/tag `core::ExitCode::from_io_error` maps to the
    /// parity exit code (see [`error`]).
    Error(error::TransportFault),
}

impl Terminal {
    fn from_loss(reason: &ConnectionError) -> Self {
        match reason {
            ConnectionError::ApplicationClosed(close)
                if close.error_code == VarInt::from_u32(0) =>
            {
                Self::Clean
            }
            ConnectionError::LocallyClosed => Self::Clean,
            other => Self::Error(error::connection_fault(other)),
        }
    }
}

/// State shared between the blocking facade and the driver thread.
#[derive(Default)]
struct State {
    /// Handshake finished and the bidirectional stream exists.
    stream_ready: bool,
    /// Remote address of the established connection, recorded by the driver
    /// once the connection exists. On a server endpoint this is the connecting
    /// client's address; the daemon uses it as the session peer for logging
    /// and `hosts allow`/`hosts deny` evaluation.
    peer: Option<SocketAddr>,
    /// Ordered stream data awaiting facade reads.
    recv: VecDeque<Bytes>,
    recv_len: usize,
    /// Peer sent FIN; EOF once `recv` drains.
    recv_fin: bool,
    /// Driver stopped draining at the high-water mark; the facade re-signals
    /// once it has consumed below half.
    recv_paused: bool,
    /// Facade bytes awaiting submission to the QUIC send stream.
    send: VecDeque<u8>,
    /// Facade requested FIN after the buffered bytes.
    send_fin: bool,
    /// FIN fully acknowledged by the peer (or the stream was stopped).
    send_finished: bool,
    /// Peer stopped reading our stream; further writes fail.
    send_stopped: bool,
    /// Facade requested a graceful connection close.
    close_requested: bool,
    /// Last facade handle dropped; close and exit.
    shutdown: bool,
    /// How the connection ended, if it has.
    terminal: Option<Terminal>,
    /// Driver thread has exited (connection drained or socket failure).
    drained: bool,
    /// Facade work is queued for the driver.
    pending: bool,
    /// Driver is blocked in `recv_from` and needs a wake datagram.
    sleeping: bool,
}

struct Shared {
    state: Mutex<State>,
    cond: Condvar,
}

impl Shared {
    fn new() -> Self {
        Self {
            state: Mutex::new(State::default()),
            cond: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn wait<'a>(&self, guard: MutexGuard<'a, State>) -> MutexGuard<'a, State> {
        self.cond
            .wait(guard)
            .unwrap_or_else(PoisonError::into_inner)
    }
}

/// Handle shared by the facade types; owns the waker socket and the driver
/// thread handle. The driver itself only holds `Arc<Shared>`, so dropping the
/// last facade handle runs `Drop for Io` and tells the driver to exit.
struct Io {
    shared: Arc<Shared>,
    wake: UdpSocket,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Io {
    /// Marks facade work pending and wakes the driver if it is asleep.
    fn signal(&self, st: &mut State) {
        st.pending = true;
        if st.sleeping {
            let _ = self.wake.send(&[0]);
        }
    }

    fn join(&self) {
        let handle = self
            .thread
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(handle) = handle {
            let _ = handle.join();
        }
    }
}

impl Drop for Io {
    fn drop(&mut self) {
        let mut st = self.shared.lock();
        st.shutdown = true;
        self.signal(&mut st);
    }
}

/// Blocks until the bidirectional stream exists (or the connection failed).
fn wait_stream(io: &Arc<Io>) -> io::Result<QuicStream> {
    let mut st = io.shared.lock();
    loop {
        if st.stream_ready {
            return Ok(QuicStream { io: Arc::clone(io) });
        }
        match &st.terminal {
            Some(Terminal::Clean) => {
                return Err(io::Error::new(
                    io::ErrorKind::NotConnected,
                    "connection closed before a stream was established",
                ));
            }
            Some(Terminal::Error(fault)) => return Err(fault.to_io_error()),
            None => {}
        }
        if st.drained {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "QUIC driver exited before a stream was established",
            ));
        }
        st = io.shared.wait(st);
    }
}

/// The TLS identity a [`QuicAcceptor`] presents to connecting clients.
///
/// Mirrors the daemon's Decision A (docs/design/quic-transport-policy.md): an
/// operator either supplies a certificate/key pair on disk, or the listener
/// mints a fresh in-memory self-signed certificate at bind time that is never
/// persisted and rotates on every restart.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum QuicServerIdentity {
    /// Generate a fresh self-signed certificate valid for `localhost` in
    /// memory at bind time. Nothing is written to disk and the identity
    /// changes on every bind.
    Ephemeral,
    /// Load the PEM-encoded certificate chain and private key from the given
    /// paths. The chain's first certificate is the leaf presented for pinning.
    PemFiles {
        /// Path to the PEM certificate chain (leaf first).
        cert: PathBuf,
        /// Path to the PEM private key (PKCS#8, PKCS#1, or SEC1).
        key: PathBuf,
    },
}

impl QuicServerIdentity {
    /// Materializes the leaf certificate, the full chain, and the private key
    /// for this identity.
    ///
    /// For [`QuicServerIdentity::Ephemeral`] this generates a self-signed
    /// certificate via `rcgen`. For [`QuicServerIdentity::PemFiles`] it reads
    /// and parses the operator-supplied PEM files, surfacing an
    /// [`io::Error`] when a file is missing, unreadable, or contains no
    /// certificate.
    fn materialize(
        &self,
    ) -> io::Result<(
        CertificateDer<'static>,
        Vec<CertificateDer<'static>>,
        PrivateKeyDer<'static>,
    )> {
        match self {
            Self::Ephemeral => {
                let issued = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()])
                    .map_err(io_err)?;
                let certificate = issued.cert.der().clone();
                let key = PrivateKeyDer::from(PrivatePkcs8KeyDer::from(
                    issued.signing_key.serialize_der(),
                ));
                Ok((certificate.clone(), vec![certificate], key))
            }
            Self::PemFiles { cert, key } => {
                // Shared loader (one owner): the client's mutual-TLS cert
                // (`trust::ClientAuth`) parses identical chain/key shapes.
                let (chain, key) = trust::load_cert_chain_and_key(cert, key)?;
                let leaf = chain
                    .first()
                    .ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("no certificate found in {}", cert.display()),
                        )
                    })?
                    .clone();
                Ok((leaf, chain, key))
            }
        }
    }
}

/// QUIC listener: binds a UDP socket, generates a self-signed certificate,
/// and accepts one blocking stream for its single incoming connection.
pub struct QuicAcceptor {
    io: Arc<Io>,
    certificate: CertificateDer<'static>,
    local: SocketAddr,
}

impl QuicAcceptor {
    /// Binds a QUIC server endpoint on `addr` with a fresh self-signed
    /// certificate valid for `localhost`.
    ///
    /// Convenience wrapper over [`QuicAcceptor::from_socket`] that binds a
    /// plain [`UdpSocket`] to `addr` and presents an ephemeral in-memory
    /// certificate. Callers that need dual-stack isolation
    /// (`IPV6_V6ONLY`) or an operator-supplied certificate build the socket
    /// themselves and call [`QuicAcceptor::from_socket`].
    pub fn bind(addr: SocketAddr) -> io::Result<Self> {
        let socket = UdpSocket::bind(addr)?;
        Self::from_socket(socket, &QuicServerIdentity::Ephemeral)
    }

    /// Wraps an already-bound `socket` in a QUIC server endpoint presenting
    /// the certificate selected by `identity`.
    ///
    /// The caller owns the UDP socket, so it also owns every socket option
    /// that must be set before `bind(2)` - notably `IPV6_V6ONLY`, which the
    /// daemon sets on its IPv6 listener so a dual-stack QUIC bind does not
    /// collide with the paired IPv4 socket (mirrors the TCP listener's
    /// `set_only_v6(true)`). This crate deliberately keeps that policy in the
    /// caller so it stays identical to the TCP path.
    ///
    /// The accepted stream is client-opened: the peer opens the single
    /// bidirectional stream and speaks first. For the server-speaks-first rsync
    /// daemon (which writes the `@RSYNCD:` greeting before the client sends
    /// anything) use [`QuicAcceptor::from_socket_server_first`] instead.
    pub fn from_socket(socket: UdpSocket, identity: &QuicServerIdentity) -> io::Result<Self> {
        Self::from_socket_with(socket, identity, false, None)
    }

    /// Like [`QuicAcceptor::from_socket`], but the server OPENS the single
    /// bidirectional stream and speaks first.
    ///
    /// This is the rsync daemon's mode: the daemon writes the `@RSYNCD:`
    /// greeting before the client sends anything, and a QUIC stream is invisible
    /// to its peer until a frame is sent on it, so the speaker must be the
    /// opener or both sides deadlock. The peer must connect with
    /// [`QuicConnector::connect_server_first`].
    pub fn from_socket_server_first(
        socket: UdpSocket,
        identity: &QuicServerIdentity,
    ) -> io::Result<Self> {
        Self::from_socket_with(socket, identity, true, None)
    }

    /// Like [`QuicAcceptor::from_socket_server_first`], but the daemon requires
    /// and verifies a client certificate (mutual TLS).
    ///
    /// `client_ca` is the trust-anchor store (loaded from the daemon's
    /// `quic client ca file` directive via
    /// [`load_private_ca`]) against which each connecting client's certificate
    /// chain is verified. A client that presents no certificate, or one not
    /// anchored by `client_ca`, is rejected in the TLS handshake before any
    /// `@RSYNCD:` byte is exchanged. This is the daemon-side mirror of the
    /// client's `--quic-ca` server verification; the presented server identity
    /// (`identity`) is unchanged from the plain path.
    pub fn from_socket_server_first_mutual(
        socket: UdpSocket,
        identity: &QuicServerIdentity,
        client_ca: RootCertStore,
    ) -> io::Result<Self> {
        Self::from_socket_with(socket, identity, true, Some(client_ca))
    }

    fn from_socket_with(
        socket: UdpSocket,
        identity: &QuicServerIdentity,
        opens_stream: bool,
        client_ca: Option<RootCertStore>,
    ) -> io::Result<Self> {
        let (certificate, chain, key) = identity.materialize()?;

        let versions = rustls::ServerConfig::builder_with_provider(ring_provider())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(io_err)?;
        // Default off: no `client_ca` means `with_no_client_auth`, byte-identical
        // to the pre-mutual-TLS path. A configured client CA installs a
        // `WebPkiClientVerifier` that requires and verifies a client certificate
        // against those roots (the daemon-side mirror of `--quic-ca`).
        let mut server_crypto = match client_ca {
            None => versions
                .with_no_client_auth()
                .with_single_cert(chain, key)
                .map_err(io_err)?,
            Some(roots) => {
                let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
                    Arc::new(roots),
                    ring_provider(),
                )
                .build()
                .map_err(io_err)?;
                versions
                    .with_client_cert_verifier(verifier)
                    .with_single_cert(chain, key)
                    .map_err(io_err)?
            }
        };
        server_crypto.alpn_protocols = vec![ALPN_RSYNC.to_vec()];

        let mut server_config = ServerConfig::with_crypto(Arc::new(
            QuicServerConfig::try_from(server_crypto).map_err(io_err)?,
        ));
        // The daemon's own endpoint takes env/default tuning; client `--quic-*`
        // flags configure the client endpoint, never the server's.
        server_config.transport_config(build_transport_config(QuicTransportTuning::default())?);

        let local = socket.local_addr()?;
        // allow_mtud = false: a std UdpSocket cannot set the don't-fragment
        // bit, so MTU discovery probes could be silently fragmented.
        let endpoint = Endpoint::new(
            Arc::new(EndpointConfig::default()),
            Some(Arc::new(server_config)),
            false,
            None,
        );
        let io = spawn_io(socket, endpoint, Role::Server, opens_stream, None)?;
        Ok(Self {
            io,
            certificate,
            local,
        })
    }

    /// Returns the bound local address (useful with port 0).
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        Ok(self.local)
    }

    /// DER encoding of the self-signed certificate, for pinning by a
    /// [`QuicConnector`].
    pub fn certificate(&self) -> &CertificateDer<'static> {
        &self.certificate
    }

    /// Blocks until a client connects and opens a bidirectional stream.
    pub fn accept(&self) -> io::Result<QuicStream> {
        wait_stream(&self.io)
    }
}

impl std::fmt::Debug for QuicAcceptor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuicAcceptor")
            .field("local_addr", &self.local)
            .finish_non_exhaustive()
    }
}

/// How a [`QuicConnector`] decides whether to trust the server's certificate.
///
/// The connector's connect path is identical for every variant - the trust
/// source only shapes the rustls [`rustls::ClientConfig`] the handshake runs
/// against. This is the seam the client-verification ladder (policy B) plugs
/// into: `--quic-ca` supplies [`QuicTrust::Roots`] built from a private CA
/// bundle, system-root verification supplies [`QuicTrust::Roots`] built from
/// the platform store, and TOFU `quic_known_hosts` supplies a
/// [`QuicTrust::Verifier`] that pins the server's SPKI. Tests and the
/// exact-pin escape hatch use [`QuicTrust::Pinned`].
///
/// See `docs/design/quic-transport-policy.md` (Decision B) and
/// `docs/design/quic-transport-integration.md` (Phase 3).
pub enum QuicTrust {
    /// Trust exactly one certificate, verifying the chain against it as the
    /// sole trust anchor. The zero-dependency escape hatch and the shape the
    /// loopback tests use.
    Pinned(CertificateDer<'static>),
    /// Verify the server's certificate chain against a root store - the
    /// platform trust store (system-roots default) or a private CA bundle
    /// supplied via `--quic-ca`.
    Roots(RootCertStore),
    /// Delegate the trust decision to a custom rustls verifier, e.g. the TOFU
    /// SPKI-pinning verifier that backs `quic_known_hosts`.
    Verifier(Arc<dyn ServerCertVerifier>),
}

impl std::fmt::Debug for QuicTrust {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pinned(_) => f.write_str("QuicTrust::Pinned(..)"),
            Self::Roots(_) => f.write_str("QuicTrust::Roots(..)"),
            Self::Verifier(_) => f.write_str("QuicTrust::Verifier(..)"),
        }
    }
}

/// QUIC client: connects to an acceptor over a trust source it is configured
/// with (an exact pin, a root store, or a custom verifier - see [`QuicTrust`]).
pub struct QuicConnector {
    config: ClientConfig,
}

impl QuicConnector {
    /// Creates a client configuration that trusts exactly
    /// `server_certificate`. Convenience wrapper over
    /// [`QuicConnector::with_trust`] with [`QuicTrust::Pinned`], preserved as
    /// the exact-pin path used by the loopback tests.
    pub fn new(server_certificate: &CertificateDer<'_>) -> io::Result<Self> {
        Self::pinned(server_certificate.clone().into_owned())
    }

    /// Creates a connector that trusts exactly `server_certificate`.
    pub fn pinned(server_certificate: CertificateDer<'static>) -> io::Result<Self> {
        Self::with_trust(QuicTrust::Pinned(server_certificate))
    }

    /// Creates a connector whose trust decision is supplied by `trust`, taking
    /// congestion control and flow-control windows from the environment and
    /// built-in defaults. Convenience wrapper over
    /// [`QuicConnector::with_trust_tuned`] with default tuning.
    pub fn with_trust(trust: QuicTrust) -> io::Result<Self> {
        Self::with_trust_tuned(trust, QuicTransportTuning::default())
    }

    /// Creates a connector whose trust decision is supplied by `trust` and
    /// whose transport tuning (congestion controller, flow-control windows) is
    /// resolved from `tuning` (CLI `--quic-cc`/`--quic-window`), falling back to
    /// the environment and defaults inside [`build_transport_config`].
    ///
    /// Every trust variant produces a TLS 1.3, ALPN-`rsync` client config
    /// through the one shared builder below; only the verifier stage differs.
    /// This is the constructor the CA/system-root/TOFU resolution feeds.
    pub fn with_trust_tuned(trust: QuicTrust, tuning: QuicTransportTuning) -> io::Result<Self> {
        Self::with_trust_tuned_client_auth(trust, tuning, None)
    }

    /// Like [`QuicConnector::with_trust_tuned`], but the client presents
    /// `client_auth` (a certificate chain + private key) in the QUIC handshake
    /// for mutual TLS.
    ///
    /// `client_auth` is `Some` only when both `--quic-cert` and `--quic-key`
    /// were supplied; `None` reproduces today's behaviour exactly - the client
    /// presents no certificate (rustls `with_no_client_auth`). The server-trust
    /// decision (`trust`) and transport tuning are unaffected: mutual TLS adds
    /// the client's own identity without changing how the client verifies the
    /// daemon. The certificate is installed via rustls `with_client_auth_cert`.
    pub fn with_trust_tuned_client_auth(
        trust: QuicTrust,
        tuning: QuicTransportTuning,
        client_auth: Option<ClientAuth>,
    ) -> io::Result<Self> {
        let builder = rustls::ClientConfig::builder_with_provider(ring_provider())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(io_err)?;
        let builder = match trust {
            QuicTrust::Pinned(certificate) => {
                let mut roots = RootCertStore::empty();
                roots.add(certificate).map_err(io_err)?;
                builder.with_root_certificates(roots)
            }
            QuicTrust::Roots(roots) => builder.with_root_certificates(roots),
            QuicTrust::Verifier(verifier) => builder
                .dangerous()
                .with_custom_certificate_verifier(verifier),
        };
        // Default off: no `client_auth` keeps `with_no_client_auth`, so the
        // handshake is byte-identical to the pre-mutual-TLS client. A supplied
        // certificate is presented via `with_client_auth_cert`.
        let mut client_crypto = match client_auth {
            None => builder.with_no_client_auth(),
            Some(auth) => builder
                .with_client_auth_cert(auth.chain, auth.key)
                .map_err(io_err)?,
        };
        client_crypto.alpn_protocols = vec![ALPN_RSYNC.to_vec()];

        let mut config = ClientConfig::new(Arc::new(
            QuicClientConfig::try_from(client_crypto).map_err(io_err)?,
        ));
        config.transport_config(build_transport_config(tuning)?);
        Ok(Self { config })
    }

    /// Connects to `addr`, validating the peer as `server_name`, and opens the
    /// single bidirectional stream (the client speaks first).
    ///
    /// For the server-speaks-first rsync daemon - where the client must instead
    /// accept the stream the daemon opens and read its `@RSYNCD:` greeting
    /// first - use [`QuicConnector::connect_server_first`].
    pub fn connect(&self, addr: SocketAddr, server_name: &str) -> io::Result<QuicStream> {
        self.connect_with(addr, server_name, true)
    }

    /// Like [`QuicConnector::connect`], but the client ACCEPTS the single
    /// bidirectional stream the server opens and lets the server speak first.
    ///
    /// This is the mode for connecting to the rsync daemon, whose acceptor is
    /// built with [`QuicAcceptor::from_socket_server_first`]; the daemon writes
    /// the `@RSYNCD:` greeting before the client sends anything.
    pub fn connect_server_first(
        &self,
        addr: SocketAddr,
        server_name: &str,
    ) -> io::Result<QuicStream> {
        self.connect_with(addr, server_name, false)
    }

    fn connect_with(
        &self,
        addr: SocketAddr,
        server_name: &str,
        opens_stream: bool,
    ) -> io::Result<QuicStream> {
        let bind_ip: IpAddr = match addr {
            SocketAddr::V4(_) => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            SocketAddr::V6(_) => IpAddr::V6(Ipv6Addr::UNSPECIFIED),
        };
        let socket = UdpSocket::bind(SocketAddr::new(bind_ip, 0))?;
        let mut endpoint = Endpoint::new(Arc::new(EndpointConfig::default()), None, false, None);
        let (handle, conn) = endpoint
            .connect(Instant::now(), self.config.clone(), addr, server_name)
            .map_err(|e| error::connect_fault(&e))?;
        let io = spawn_io(
            socket,
            endpoint,
            Role::Client,
            opens_stream,
            Some((handle, conn)),
        )?;
        wait_stream(&io)
    }
}

impl std::fmt::Debug for QuicConnector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuicConnector").finish_non_exhaustive()
    }
}

/// One bidirectional QUIC stream exposed as blocking `Read` + `Write`.
///
/// Keeps the driver thread (and thus the endpoint and connection) alive while
/// in use; dropping every handle signals the driver to close the connection
/// and exit.
pub struct QuicStream {
    io: Arc<Io>,
}

impl QuicStream {
    /// Signals end-of-stream to the peer and blocks until every written byte
    /// has been acknowledged (or the peer stopped the stream).
    ///
    /// The driver thread keeps transmitting and retransmitting while this
    /// blocks, so acknowledgement arrives without the peer having to issue
    /// blocking calls of its own. A clean connection close from the peer also
    /// counts as success, because the final ACK and a CONNECTION_CLOSE frame
    /// can race.
    pub fn finish(&mut self) -> io::Result<()> {
        let shared = &self.io.shared;
        let mut st = shared.lock();
        st.send_fin = true;
        self.io.signal(&mut st);
        loop {
            if st.send_finished {
                return Ok(());
            }
            match &st.terminal {
                Some(Terminal::Clean) => return Ok(()),
                Some(Terminal::Error(fault)) => return Err(fault.to_io_error()),
                None => {}
            }
            if st.drained {
                return Err(error::driver_gone(
                    "driver exited before the stream finished",
                ));
            }
            st = shared.wait(st);
        }
    }

    /// Returns a second handle to the same bidirectional stream.
    ///
    /// Reads touch only the receive path and writes only the send path, so an
    /// independent read handle and write handle over one clone pair never
    /// contend beyond the shared lock. This is what lets a caller split the
    /// stream into a blocking [`Read`] half and a blocking [`Write`] half - the
    /// shape the daemon `@RSYNCD` handshake needs - without cloning a socket.
    #[must_use]
    pub fn try_clone(&self) -> QuicStream {
        QuicStream {
            io: Arc::clone(&self.io),
        }
    }

    /// Returns the remote peer's address for this connection.
    ///
    /// On a stream accepted by a [`QuicAcceptor`] this is the connecting
    /// client's address, recorded by the driver when the connection was
    /// established. Returns `None` only if the connection ended before the
    /// address was recorded, which cannot happen for a stream returned by
    /// [`QuicAcceptor::accept`] (the stream exists only after the connection
    /// does).
    #[must_use]
    pub fn peer_addr(&self) -> Option<SocketAddr> {
        self.io.shared.lock().peer
    }

    /// Wraps this stream's write half in the shared bandwidth-pacing decorator
    /// so `--bwlimit` throttles QUIC egress exactly as it throttles the TCP/SSH
    /// sender socket.
    ///
    /// This routes QUIC writes through [`ThrottlingWriter`] - the same
    /// [`BandwidthLimiter`]-driven decorator the TCP/SSH sender installs at the
    /// bottom of its writer stack - rather than re-implementing the clamp and
    /// pace loop on the stream. Pass `Some(limiter)` on the sender role and
    /// `None` on the receiver (or `--bwlimit=0`), where the wrapper is a
    /// zero-overhead passthrough. Because [`QuicStream`] is a byte pipe, the
    /// decorator paces the exact wire bytes handed to the driver, matching
    /// upstream's `sleep_for_bwlimit` on the raw socket write (`io.c:846,861`).
    ///
    /// The returned writer owns this handle; obtain the read half first via
    /// [`QuicStream::try_clone`] when a split read/write pair is needed.
    #[must_use]
    pub fn throttled_writer(
        self,
        limiter: Option<BandwidthLimiter>,
    ) -> ThrottlingWriter<QuicStream> {
        ThrottlingWriter::new(self, limiter)
    }

    /// Builds a teardown guard for this connection.
    ///
    /// Dropping the guard finishes the send stream (flushing every buffered
    /// byte and its FIN, then blocking until the peer acknowledges it or the
    /// connection ends) and then closes the connection gracefully. This is the
    /// QUIC analogue of a TCP socket flushing its send buffer as it closes:
    /// hold the guard for the lifetime of a transfer and let it drop last, so
    /// no trailing bytes are truncated by an abrupt close.
    #[must_use]
    pub fn shutdown_guard(&self) -> QuicShutdown {
        QuicShutdown {
            io: Arc::clone(&self.io),
        }
    }

    /// Gracefully closes the connection (application error code 0) and blocks
    /// until it has drained and the driver thread exited.
    pub fn close(self) {
        {
            let shared = &self.io.shared;
            let mut st = shared.lock();
            st.close_requested = true;
            self.io.signal(&mut st);
            while !st.drained {
                st = shared.wait(st);
            }
        }
        self.io.join();
    }
}

impl Read for QuicStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let shared = &self.io.shared;
        let mut st = shared.lock();
        loop {
            if !st.recv.is_empty() {
                let mut copied = 0;
                while copied < buf.len() {
                    let Some(mut chunk) = st.recv.pop_front() else {
                        break;
                    };
                    let take = chunk.len().min(buf.len() - copied);
                    buf[copied..copied + take].copy_from_slice(&chunk[..take]);
                    copied += take;
                    st.recv_len -= take;
                    if take < chunk.len() {
                        let rest = chunk.split_off(take);
                        st.recv.push_front(rest);
                        break;
                    }
                }
                if st.recv_paused && st.recv_len < RECV_HIGH_WATER / 2 {
                    st.recv_paused = false;
                    self.io.signal(&mut st);
                }
                return Ok(copied);
            }
            if st.recv_fin {
                return Ok(0);
            }
            match &st.terminal {
                // Clean close is end-of-session, mirroring a TCP clean close.
                Some(Terminal::Clean) => return Ok(0),
                Some(Terminal::Error(fault)) => return Err(fault.to_io_error()),
                None => {}
            }
            if st.drained {
                return Err(error::driver_gone("driver exited mid-stream"));
            }
            st = shared.wait(st);
        }
    }
}

impl Write for QuicStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let shared = &self.io.shared;
        let mut st = shared.lock();
        loop {
            if let Some(terminal) = &st.terminal {
                return Err(match terminal {
                    // A clean peer close mid-write is a broken pipe, as on TCP.
                    Terminal::Clean => {
                        io::Error::new(io::ErrorKind::BrokenPipe, "connection closed")
                    }
                    // A faulted close keeps its classification (timeout stays a
                    // timeout, a protocol failure stays a protocol failure)
                    // instead of collapsing every write failure to BrokenPipe.
                    Terminal::Error(fault) => fault.to_io_error(),
                });
            }
            if st.send_stopped {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "stream stopped by peer",
                ));
            }
            if st.send_fin {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "write after finish",
                ));
            }
            let space = SEND_CAP - st.send.len();
            if space > 0 {
                let take = space.min(buf.len());
                st.send.extend(buf[..take].iter().copied());
                self.io.signal(&mut st);
                return Ok(take);
            }
            if st.drained {
                return Err(error::driver_gone("driver exited mid-stream"));
            }
            st = shared.wait(st);
        }
    }

    /// No-op: bytes are handed to the driver as they are written; use
    /// [`QuicStream::finish`] as the delivery barrier for the final write.
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl std::fmt::Debug for QuicStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuicStream").finish_non_exhaustive()
    }
}

/// Connection teardown guard produced by [`QuicStream::shutdown_guard`].
///
/// On drop it flushes the send stream and FIN, blocks until the peer has
/// acknowledged them (or the connection ended), then closes the connection
/// gracefully and joins the driver thread. Keeping teardown in one guard - held
/// alongside independent read/write handles cloned from the same stream - gives
/// those handles simple `Drop`s (a reference-count decrement) while still
/// guaranteeing the last written bytes reach the peer before the close, exactly
/// as a TCP socket flushes its send buffer on close.
pub struct QuicShutdown {
    io: Arc<Io>,
}

impl Drop for QuicShutdown {
    fn drop(&mut self) {
        let shared = &self.io.shared;
        // Flush + FIN, then wait for the delivery barrier: every queued byte is
        // drained to the send stream and acknowledged, or the connection ended.
        {
            let mut st = shared.lock();
            st.send_fin = true;
            self.io.signal(&mut st);
            while !st.send_finished && st.terminal.is_none() && !st.drained {
                st = shared.wait(st);
            }
        }
        // Graceful close (application error code 0); wait until it has drained.
        {
            let mut st = shared.lock();
            st.close_requested = true;
            self.io.signal(&mut st);
            while !st.drained {
                st = shared.wait(st);
            }
        }
        self.io.join();
    }
}

impl std::fmt::Debug for QuicShutdown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuicShutdown").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use std::thread;

    use rustls::DigitallySignedStruct;
    use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified};
    use rustls::pki_types::{ServerName, UnixTime};
    use rustls::{Error as TlsError, SignatureScheme};

    use super::*;

    /// Test-only verifier that accepts any certificate identity while still
    /// checking handshake signatures against the crypto provider. It stands in
    /// for the shape a real TOFU/SPKI-pinning verifier (QUIC-5c/5d) will take:
    /// the trust *decision* is custom, the TLS mechanics are unchanged. Its
    /// existence proves [`QuicConnector`] can drive an arbitrary
    /// [`ServerCertVerifier`] through the same `connect` path.
    #[derive(Debug)]
    struct AcceptAnyCert {
        provider: Arc<rustls::crypto::CryptoProvider>,
    }

    impl ServerCertVerifier for AcceptAnyCert {
        fn verify_server_cert(
            &self,
            _end_entity: &CertificateDer<'_>,
            _intermediates: &[CertificateDer<'_>],
            _server_name: &ServerName<'_>,
            _ocsp_response: &[u8],
            _now: UnixTime,
        ) -> Result<ServerCertVerified, TlsError> {
            Ok(ServerCertVerified::assertion())
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

    /// A [`QuicTrust::Roots`] store built from the server certificate yields a
    /// working connector without touching `connect`. Encodes WHY the trust
    /// source is pluggable: `--quic-ca` and system-roots verification (policy
    /// B) both arrive as a `RootCertStore`, and the connector must accept one
    /// without any change to the pin-based path the loopback tests exercise.
    #[test]
    fn roots_trust_source_builds_connector() {
        let acceptor =
            QuicAcceptor::bind("127.0.0.1:0".parse().expect("addr")).expect("bind acceptor");
        let mut roots = RootCertStore::empty();
        roots
            .add(acceptor.certificate().clone().into_owned())
            .expect("add cert");
        QuicConnector::with_trust(QuicTrust::Roots(roots)).expect("build roots connector");
    }

    /// The ALPN identifier advertised on every QUIC connection is exactly
    /// `rsync`. This is a wire-format constant, not an internal name: a QUIC
    /// rsync peer completes the handshake only when the client advertises this
    /// exact protocol id, so a rename would silently break interop (the connect
    /// path maps an ALPN mismatch to `RERR_STARTCLIENT`). TLS 1.3 and this ALPN
    /// are mandatory under QUIC; both loopback round-trip tests succeed
    /// precisely because the shared client/acceptor config advertises `rsync`
    /// over TLS 1.3 (both built with `.with_protocol_versions(&[&TLS13])`).
    #[test]
    fn alpn_identifier_is_the_rsync_wire_constant() {
        assert_eq!(ALPN_RSYNC, b"rsync");
    }

    /// A custom [`ServerCertVerifier`] drives a full round trip over the same
    /// `connect`/`QuicStream` path as the pinned form. Encodes WHY the trust
    /// source is pluggable: the future TOFU verifier is a `dyn
    /// ServerCertVerifier`, and swapping the trust decision must not perturb
    /// the connect path or the byte pipe below it.
    #[test]
    fn custom_verifier_trust_source_round_trips() {
        let acceptor =
            QuicAcceptor::bind("127.0.0.1:0".parse().expect("addr")).expect("bind acceptor");
        let addr = acceptor.local_addr().expect("local addr");

        let server = thread::spawn(move || {
            let mut stream = acceptor.accept().expect("accept");
            let mut buf = [0u8; 4];
            stream.read_exact(&mut buf).expect("read");
            assert_eq!(&buf, b"ping");
            stream.write_all(b"pong").expect("write");
            stream.finish().expect("finish");
        });

        let verifier = Arc::new(AcceptAnyCert {
            provider: ring_provider(),
        });
        let connector = QuicConnector::with_trust(QuicTrust::Verifier(verifier))
            .expect("build verifier connector");
        let mut stream = connector.connect(addr, "localhost").expect("connect");
        stream.write_all(b"ping").expect("write");
        stream.finish().expect("finish");
        let mut reply = [0u8; 4];
        stream.read_exact(&mut reply).expect("read reply");
        assert_eq!(&reply, b"pong");
        stream.close();

        server.join().expect("server thread");
    }

    /// Drives one `ping`/`pong` round trip against `acceptor`, pinning the
    /// client to `cert`. Proves the acceptor's presented certificate is the
    /// one under test and the byte pipe works end to end.
    fn round_trip_pinned(acceptor: QuicAcceptor, cert: CertificateDer<'static>) {
        let addr = acceptor.local_addr().expect("local addr");
        let server = thread::spawn(move || {
            let mut stream = acceptor.accept().expect("accept");
            let mut buf = [0u8; 4];
            stream.read_exact(&mut buf).expect("read");
            assert_eq!(&buf, b"ping");
            stream.write_all(b"pong").expect("write");
            stream.finish().expect("finish");
        });

        let connector = QuicConnector::new(&cert).expect("build connector");
        let mut stream = connector.connect(addr, "localhost").expect("connect");
        stream.write_all(b"ping").expect("write");
        stream.finish().expect("finish");
        let mut reply = [0u8; 4];
        stream.read_exact(&mut reply).expect("read reply");
        assert_eq!(&reply, b"pong");
        stream.close();
        server.join().expect("server thread");
    }

    /// `from_socket` with an ephemeral identity binds a caller-owned UDP socket
    /// and serves a round trip - the identity-parameterized path is
    /// behaviorally identical to the historical `bind` helper.
    #[test]
    fn from_socket_ephemeral_binds_and_round_trips() {
        let socket = UdpSocket::bind("127.0.0.1:0").expect("udp bind");
        let acceptor = QuicAcceptor::from_socket(socket, &QuicServerIdentity::Ephemeral)
            .expect("bind ephemeral");
        let cert = acceptor.certificate().clone().into_owned();
        round_trip_pinned(acceptor, cert);
    }

    /// Wraps DER bytes in a PEM block. The workspace builds `rcgen` without its
    /// `pem` feature, so the test encodes the block itself: base64 (64-column
    /// wrapped) between the standard `-----BEGIN/END <label>-----` markers.
    fn der_to_pem(label: &str, der: &[u8]) -> String {
        use base64::Engine as _;
        let body = base64::engine::general_purpose::STANDARD.encode(der);
        let mut out = format!("-----BEGIN {label}-----\n");
        for chunk in body.as_bytes().chunks(64) {
            out.push_str(std::str::from_utf8(chunk).expect("base64 is ascii"));
            out.push('\n');
        }
        out.push_str(&format!("-----END {label}-----\n"));
        out
    }

    /// `from_socket` with a `PemFiles` identity presents the operator-supplied
    /// certificate verbatim: the listener's leaf cert equals the one on disk
    /// and a client pinned to it completes a round trip. Encodes WHY Files
    /// identity matters - the daemon `quic cert file` / `quic key file`
    /// directives must yield a stable, on-disk identity, not a fresh ephemeral
    /// one.
    #[test]
    fn from_socket_pem_files_presents_configured_cert() {
        let issued = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()])
            .expect("generate cert");
        let dir = tempfile::tempdir().expect("tempdir");
        let cert_path = dir.path().join("cert.pem");
        let key_path = dir.path().join("key.pem");
        std::fs::write(
            &cert_path,
            der_to_pem("CERTIFICATE", issued.cert.der().as_ref()),
        )
        .expect("write cert pem");
        std::fs::write(
            &key_path,
            der_to_pem("PRIVATE KEY", &issued.signing_key.serialize_der()),
        )
        .expect("write key pem");
        let expected = issued.cert.der().clone();

        let socket = UdpSocket::bind("127.0.0.1:0").expect("udp bind");
        let acceptor = QuicAcceptor::from_socket(
            socket,
            &QuicServerIdentity::PemFiles {
                cert: cert_path,
                key: key_path,
            },
        )
        .expect("bind pem files");
        assert_eq!(
            acceptor.certificate(),
            &expected,
            "listener must present the configured leaf certificate"
        );
        round_trip_pinned(acceptor, expected);
    }

    /// A `PemFiles` identity pointing at a missing certificate surfaces the
    /// file error instead of silently falling back to an ephemeral identity.
    #[test]
    fn from_socket_pem_files_missing_cert_errors() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = UdpSocket::bind("127.0.0.1:0").expect("udp bind");
        let err = QuicAcceptor::from_socket(
            socket,
            &QuicServerIdentity::PemFiles {
                cert: dir.path().join("absent-cert.pem"),
                key: dir.path().join("absent-key.pem"),
            },
        )
        .expect_err("a missing certificate file must fail the bind");
        let _ = err;
    }

    /// Builds a CA and a client leaf certificate signed by it (SAN
    /// `oc-rsync-client`, `clientAuth` EKU), returning `(ca_pem, cert_pem,
    /// key_pem)`. The leaf/key are what a client presents via `--quic-cert` /
    /// `--quic-key`; the CA PEM is what the daemon's `quic client ca file`
    /// loads to anchor it. Each call produces an independent CA, so two calls
    /// model a trusted vs an untrusted issuer.
    fn ca_and_client_identity() -> (String, String, String) {
        use rcgen::{
            BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
            KeyUsagePurpose,
        };

        let ca_key = KeyPair::generate().expect("ca key");
        let mut ca_params =
            CertificateParams::new(vec!["oc-rsync client test ca".to_owned()]).expect("ca params");
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params.key_usages = vec![
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::CrlSign,
        ];
        let ca_cert = ca_params.self_signed(&ca_key).expect("self-sign ca");
        let ca_pem = der_to_pem("CERTIFICATE", ca_cert.der().as_ref());
        let issuer = Issuer::new(ca_params, ca_key);

        let leaf_key = KeyPair::generate().expect("leaf key");
        let mut leaf_params =
            CertificateParams::new(vec!["oc-rsync-client".to_owned()]).expect("leaf params");
        leaf_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
        let leaf = leaf_params
            .signed_by(&leaf_key, &issuer)
            .expect("sign leaf");
        let cert_pem = der_to_pem("CERTIFICATE", leaf.der().as_ref());
        let key_pem = der_to_pem("PRIVATE KEY", &leaf_key.serialize_der());
        (ca_pem, cert_pem, key_pem)
    }

    /// Writes the CA/cert/key PEMs into `dir` and returns their paths as
    /// `(ca, cert, key)`.
    fn write_pems(
        dir: &std::path::Path,
        ca: &str,
        cert: &str,
        key: &str,
    ) -> (PathBuf, PathBuf, PathBuf) {
        let ca_path = dir.join("client-ca.pem");
        let cert_path = dir.join("client-cert.pem");
        let key_path = dir.join("client-key.pem");
        std::fs::write(&ca_path, ca).expect("write ca");
        std::fs::write(&cert_path, cert).expect("write cert");
        std::fs::write(&key_path, key).expect("write key");
        (ca_path, cert_path, key_path)
    }

    /// Binds a mutual-TLS acceptor requiring a client cert anchored by
    /// `client_ca_roots`, spawns a server thread that accepts one connection and
    /// sends `b"ok"`, and returns `(addr, pinned_server_cert, join_handle)`. The
    /// server tolerates a rejected handshake (`accept` returning `Err`) so the
    /// refusal tests do not panic in the server thread.
    fn spawn_mutual_server(
        client_ca_roots: RootCertStore,
    ) -> (SocketAddr, CertificateDer<'static>, thread::JoinHandle<()>) {
        let socket = UdpSocket::bind("127.0.0.1:0").expect("udp bind");
        let acceptor = QuicAcceptor::from_socket_server_first_mutual(
            socket,
            &QuicServerIdentity::Ephemeral,
            client_ca_roots,
        )
        .expect("bind mutual acceptor");
        let addr = acceptor.local_addr().expect("local addr");
        let server_cert = acceptor.certificate().clone().into_owned();
        let handle = thread::spawn(move || {
            if let Ok(mut stream) = acceptor.accept() {
                let _ = stream.write_all(b"ok");
                let _ = stream.finish();
            }
        });
        (addr, server_cert, handle)
    }

    /// Mutual TLS happy path: a client presenting a certificate anchored by the
    /// daemon's configured client CA completes the handshake and the byte pipe
    /// works. Encodes WHY mutual TLS matters: the daemon requires a client
    /// certificate, and one signed by the trusted CA is exactly what must be
    /// accepted.
    #[test]
    fn mutual_tls_accepts_client_signed_by_configured_ca() {
        let (ca_pem, cert_pem, key_pem) = ca_and_client_identity();
        let dir = tempfile::tempdir().expect("tempdir");
        let (ca_path, cert_path, key_path) = write_pems(dir.path(), &ca_pem, &cert_pem, &key_pem);

        let client_ca = load_private_ca(&ca_path).expect("load client ca");
        let (addr, server_cert, server) = spawn_mutual_server(client_ca);

        let client_auth = ClientAuth::from_pem_files(&cert_path, &key_path).expect("client auth");
        let connector = QuicConnector::with_trust_tuned_client_auth(
            QuicTrust::Pinned(server_cert),
            QuicTransportTuning::default(),
            Some(client_auth),
        )
        .expect("connector with client cert");
        let mut stream = connector
            .connect_server_first(addr, "localhost")
            .expect("a client cert anchored by the configured CA must be accepted");
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).expect("read greeting");
        assert_eq!(
            &buf, b"ok",
            "the mutual-TLS session must carry stream bytes"
        );
        stream.close();
        server.join().expect("server thread");
    }

    /// A daemon requiring client auth refuses a client that presents no
    /// certificate. Encodes WHY: the security property of mutual TLS is that an
    /// anonymous client cannot connect; a verifier that allowed unauthenticated
    /// clients would pass the happy path yet fail here.
    #[test]
    fn mutual_tls_rejects_client_presenting_no_cert() {
        let (ca_pem, _cert_pem, _key_pem) = ca_and_client_identity();
        let dir = tempfile::tempdir().expect("tempdir");
        let ca_path = dir.path().join("client-ca.pem");
        std::fs::write(&ca_path, &ca_pem).expect("write ca");

        let client_ca = load_private_ca(&ca_path).expect("load client ca");
        let (addr, server_cert, server) = spawn_mutual_server(client_ca);

        // No client auth: the default connect path, which presents no cert.
        let connector = QuicConnector::with_trust_tuned_client_auth(
            QuicTrust::Pinned(server_cert),
            QuicTransportTuning::default(),
            None,
        )
        .expect("connector without client cert");
        connector
            .connect_server_first(addr, "localhost")
            .expect_err("a client presenting no certificate must be refused");
        server.join().expect("server thread");
    }

    /// A daemon requiring client auth refuses a client whose certificate is
    /// signed by an untrusted CA. This is the discriminating test: it fails if
    /// the client verifier is replaced with an accept-all one, because such a
    /// verifier would accept the off-CA certificate. Encodes WHY: `quic client
    /// ca file` is only meaningful if a cert it does NOT anchor is rejected.
    #[test]
    fn mutual_tls_rejects_client_signed_by_untrusted_ca() {
        let (trusted_ca_pem, _tc, _tk) = ca_and_client_identity();
        let (_untrusted_ca_pem, rogue_cert_pem, rogue_key_pem) = ca_and_client_identity();
        let dir = tempfile::tempdir().expect("tempdir");
        let ca_path = dir.path().join("trusted-ca.pem");
        let cert_path = dir.path().join("rogue-cert.pem");
        let key_path = dir.path().join("rogue-key.pem");
        std::fs::write(&ca_path, &trusted_ca_pem).expect("write ca");
        std::fs::write(&cert_path, &rogue_cert_pem).expect("write cert");
        std::fs::write(&key_path, &rogue_key_pem).expect("write key");

        let client_ca = load_private_ca(&ca_path).expect("load client ca");
        let (addr, server_cert, server) = spawn_mutual_server(client_ca);

        let rogue_auth = ClientAuth::from_pem_files(&cert_path, &key_path).expect("rogue auth");
        let connector = QuicConnector::with_trust_tuned_client_auth(
            QuicTrust::Pinned(server_cert),
            QuicTransportTuning::default(),
            Some(rogue_auth),
        )
        .expect("connector with rogue client cert");
        connector
            .connect_server_first(addr, "localhost")
            .expect_err("a client cert signed by an untrusted CA must be refused");
        server.join().expect("server thread");
    }

    /// Default off: a non-mutual acceptor (no `quic client ca file`) accepts a
    /// client that presents no certificate, exactly as before mutual TLS
    /// existed. Encodes WHY: enabling the feature must not change the plain QUIC
    /// path - with neither the daemon directive nor a client cert, the handshake
    /// is byte-identical to today's.
    #[test]
    fn plain_acceptor_unchanged_without_client_auth() {
        let socket = UdpSocket::bind("127.0.0.1:0").expect("udp bind");
        let acceptor =
            QuicAcceptor::from_socket_server_first(socket, &QuicServerIdentity::Ephemeral)
                .expect("bind plain acceptor");
        let addr = acceptor.local_addr().expect("local addr");
        let server_cert = acceptor.certificate().clone().into_owned();
        let server = thread::spawn(move || {
            let mut stream = acceptor.accept().expect("accept");
            stream.write_all(b"ok").expect("write");
            stream.finish().expect("finish");
        });

        let connector = QuicConnector::with_trust_tuned_client_auth(
            QuicTrust::Pinned(server_cert),
            QuicTransportTuning::default(),
            None,
        )
        .expect("plain connector");
        let mut stream = connector
            .connect_server_first(addr, "localhost")
            .expect("the plain path must accept a client without a certificate");
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).expect("read greeting");
        assert_eq!(&buf, b"ok");
        stream.close();
        server.join().expect("server thread");
    }
}
