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

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use bytes::Bytes;
use quinn_proto::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use quinn_proto::{ClientConfig, ConnectionError, Endpoint, EndpointConfig, ServerConfig, VarInt};
use rustls::RootCertStore;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

use driver::{Role, spawn_io};

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
    /// Any other connection loss.
    Error(String),
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
            other => Self::Error(other.to_string()),
        }
    }
}

/// State shared between the blocking facade and the driver thread.
#[derive(Default)]
struct State {
    /// Handshake finished and the bidirectional stream exists.
    stream_ready: bool,
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
            Some(Terminal::Error(msg)) => return Err(io_err(msg.clone())),
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
    pub fn bind(addr: SocketAddr) -> io::Result<Self> {
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

        let server_config = ServerConfig::with_crypto(Arc::new(
            QuicServerConfig::try_from(server_crypto).map_err(io_err)?,
        ));

        let socket = UdpSocket::bind(addr)?;
        let local = socket.local_addr()?;
        // allow_mtud = false: a std UdpSocket cannot set the don't-fragment
        // bit, so MTU discovery probes could be silently fragmented.
        let endpoint = Endpoint::new(
            Arc::new(EndpointConfig::default()),
            Some(Arc::new(server_config)),
            false,
            None,
        );
        let io = spawn_io(socket, endpoint, Role::Server, None)?;
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

/// QUIC client: connects to an acceptor whose certificate it pins.
pub struct QuicConnector {
    config: ClientConfig,
}

impl QuicConnector {
    /// Creates a client configuration that trusts exactly
    /// `server_certificate`.
    pub fn new(server_certificate: &CertificateDer<'_>) -> io::Result<Self> {
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

        let config = ClientConfig::new(Arc::new(
            QuicClientConfig::try_from(client_crypto).map_err(io_err)?,
        ));
        Ok(Self { config })
    }

    /// Connects to `addr`, validating the peer as `server_name`, and opens
    /// one bidirectional stream.
    pub fn connect(&self, addr: SocketAddr, server_name: &str) -> io::Result<QuicStream> {
        let bind_ip: IpAddr = match addr {
            SocketAddr::V4(_) => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            SocketAddr::V6(_) => IpAddr::V6(Ipv6Addr::UNSPECIFIED),
        };
        let socket = UdpSocket::bind(SocketAddr::new(bind_ip, 0))?;
        let mut endpoint = Endpoint::new(Arc::new(EndpointConfig::default()), None, false, None);
        let (handle, conn) = endpoint
            .connect(Instant::now(), self.config.clone(), addr, server_name)
            .map_err(io_err)?;
        let io = spawn_io(socket, endpoint, Role::Client, Some((handle, conn)))?;
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
                Some(Terminal::Error(msg)) => return Err(io_err(msg.clone())),
                None => {}
            }
            if st.drained {
                return Err(io_err("driver exited before the stream finished"));
            }
            st = shared.wait(st);
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
                Some(Terminal::Error(msg)) => return Err(io_err(msg.clone())),
                None => {}
            }
            if st.drained {
                return Err(io_err("driver exited mid-stream"));
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
                let msg = match terminal {
                    Terminal::Clean => "connection closed".to_owned(),
                    Terminal::Error(msg) => msg.clone(),
                };
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, msg));
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
                return Err(io_err("driver exited mid-stream"));
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
