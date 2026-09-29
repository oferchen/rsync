// QUIC/UDP listener binding (oc extension, feature `quic`, Unix-only daemon).
//
// This is the datagram sibling of the TCP `bind_listeners_per_family` pipeline
// in `listener.rs`. It reuses the identical `resolve_bind_addresses` policy so
// the QUIC listener binds the same ordered, dual-stack set of addresses as the
// TCP one, and mirrors the per-family tolerance: a dual-stack startup that
// loses one family (e.g. an IPv6-degraded CI runner) still comes up on the
// survivor, and only a total bind failure is fatal.
//
// Sessions never run here. The bound sockets move into a forked QUIC front
// process that terminates QUIC and relays each stream over a Unix socket to
// the daemon parent, which serves it exactly like an accepted TCP connection:
// through the accept loop's admission and a forked per-session child. See
// docs/design/quic-transport-policy.md.

use rsync_io::quic::{QuicAcceptor, QuicServerIdentity, QuicServerSetup, QuicStream};
use std::os::unix::net::UnixStream;

/// Maps the daemon's resolved [`QuicIdentity`] onto the transport-layer
/// [`QuicServerIdentity`] the acceptor consumes.
///
/// The two types live in different layers on purpose: [`QuicIdentity`] is the
/// daemon's config-time decision (the operator-supplied cert/key paths), while
/// [`QuicServerIdentity`] is what `rsync_io` needs to materialize the
/// certificate at bind time. Only the operator-supplied [`QuicServerIdentity::PemFiles`]
/// form is produced here: QUIC has no ephemeral fallback (a request without a
/// configured cert fails loudly at startup, see `serve_connections`).
fn quic_server_identity(identity: &QuicIdentity) -> QuicServerIdentity {
    QuicServerIdentity::PemFiles {
        cert: identity.cert.clone(),
        key: identity.key.clone(),
    }
}

/// Builds one bound UDP socket for the QUIC listener at `addr`.
///
/// Uses `socket2` so the IPv6 socket can set `IPV6_V6ONLY`, exactly as the TCP
/// listener does in `bind_with_backlog`: in dual-stack mode the daemon binds a
/// separate `[::]` and `0.0.0.0` socket, and without v6-only isolation the
/// IPv6 wildcard would also claim IPv4 traffic and the paired IPv4 bind would
/// fail with `EADDRINUSE`. `SO_REUSEADDR` mirrors the TCP path so a restart can
/// rebind without waiting out the previous socket.
fn bind_quic_socket(addr: SocketAddr) -> io::Result<std::net::UdpSocket> {
    let domain = if addr.is_ipv4() {
        socket2::Domain::IPV4
    } else {
        socket2::Domain::IPV6
    };
    let socket = socket2::Socket::new(domain, socket2::Type::DGRAM, Some(socket2::Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    if addr.is_ipv6() {
        socket.set_only_v6(true)?;
    }
    socket.bind(&addr.into())?;
    Ok(socket.into())
}

/// Binds one QUIC/UDP socket per entry in `bind_addresses`, tolerating
/// per-family failures while at least one family binds.
///
/// The datagram counterpart of [`bind_listeners_per_family`]: it walks the same
/// `resolve_bind_addresses` list in the same order, warns via
/// [`warn_per_family_bind_failure`] when a family fails in dual-stack mode, and
/// only returns `Err` when every family failed (callers map that to a
/// `DaemonError`). `quic_port` is the resolved `effective_quic_port()`.
///
/// This binds only the raw UDP sockets - the privileged step, which must run
/// while `CAP_NET_BIND_SERVICE` is still held because `effective_quic_port()`
/// defaults to the daemon port (873, privileged). Turning each socket into a
/// live [`QuicAcceptor`] happens in the QUIC front process
/// ([`start_quic_front`]), because an acceptor spawns I/O driver threads and
/// the daemon parent must stay single-threaded to fork its session children.
fn bind_quic_sockets_per_family(
    bind_addresses: &[IpAddr],
    quic_port: u16,
    log_sink: Option<&SharedLogSink>,
) -> Result<Vec<std::net::UdpSocket>, io::Error> {
    let dual_stack = bind_addresses.len() > 1;
    let mut sockets = Vec::with_capacity(bind_addresses.len());
    let mut last_error: Option<io::Error> = None;

    for addr in bind_addresses {
        let requested_addr = SocketAddr::new(*addr, quic_port);
        match bind_quic_socket(requested_addr) {
            Ok(socket) => sockets.push(socket),
            Err(error) => {
                if dual_stack {
                    warn_per_family_bind_failure(log_sink, requested_addr, &error);
                    last_error = Some(error);
                    continue;
                }
                return Err(error);
            }
        }
    }

    if sockets.is_empty() {
        let error = last_error.unwrap_or_else(|| {
            io::Error::new(
                io::ErrorKind::AddrNotAvailable,
                "no addresses available to bind the QUIC listener",
            )
        });
        return Err(error);
    }

    Ok(sockets)
}

/// The daemon parent's handle on its QUIC front process.
///
/// The front process owns the UDP sockets and terminates QUIC. Each accepted
/// stream arrives on `channel` as one end of a Unix socket pair, relayed to
/// the QUIC stream by the front process; the parent serves it like an accepted
/// TCP connection.
struct QuicFront {
    pid: i32,
    /// Taken on drop, so the channel closes before the front process is
    /// reaped.
    channel: Option<platform::fd_pass::FdChannel>,
}

/// Forks the QUIC front process and hands it the bound UDP sockets.
///
/// Must run while the daemon parent is single-threaded: it forks. The TLS
/// identity (certificate, key and client CA) is loaded here, before the fork,
/// so the front process can give up filesystem access before it starts any
/// thread. `listener_fds` are the parent's TCP listeners, which the front
/// process closes at once.
///
/// Returns `None` (after logging why) when the QUIC listener cannot start; the
/// TCP listeners keep serving either way.
fn start_quic_front(
    sockets: Vec<std::net::UdpSocket>,
    identity: &QuicIdentity,
    listener_fds: &[std::os::fd::RawFd],
    log_sink: Option<&SharedLogSink>,
) -> Option<QuicFront> {
    let setup = match load_quic_setup(identity) {
        Ok(setup) => setup,
        Err(text) => {
            log_quic_front_error(log_sink, text);
            return None;
        }
    };
    let (parent_end, front_end) = match platform::fd_pass::fd_channel_pair() {
        Ok(pair) => pair,
        Err(error) => {
            log_quic_front_error(
                log_sink,
                format!("failed to create the QUIC front channel: {error}"),
            );
            return None;
        }
    };
    let parent_pid = std::process::id();
    match platform::session_fork::fork_session() {
        Ok(platform::session_fork::ForkSide::Child) => {
            drop(parent_end);
            platform::session_fork::close_inherited_listeners(listener_fds);
            let code = run_quic_front(sockets, &setup, front_end, parent_pid, log_sink);
            platform::session_fork::exit_child(code);
        }
        Ok(platform::session_fork::ForkSide::Parent { child_pid }) => {
            // The UDP sockets now belong to the front process alone.
            drop(sockets);
            drop(front_end);
            Some(QuicFront {
                pid: child_pid,
                channel: Some(parent_end),
            })
        }
        Err(error) => {
            log_quic_front_error(
                log_sink,
                format!("could not create the QUIC front process: {error}"),
            );
            None
        }
    }
}

/// Loads the operator's certificate, key and (for mutual TLS) client CA into a
/// reusable acceptor setup.
///
/// Mutual TLS (oc extension, default off): when `quic client ca file` is set,
/// every QUIC client must present a certificate anchored by that bundle.
/// Unset, no client certificate is requested and the handshake is unchanged.
fn load_quic_setup(identity: &QuicIdentity) -> Result<QuicServerSetup, String> {
    let client_ca = match identity.client_ca.as_deref() {
        None => None,
        Some(path) => Some(rsync_io::quic::load_private_ca(path).map_err(|error| {
            format!(
                "failed to load QUIC client-auth CA {}: {error}",
                path.display()
            )
        })?),
    };
    // Server-speaks-first: the daemon writes the `@RSYNCD:` greeting before
    // the client sends anything, so the server opens each stream. The peer
    // connects with `QuicConnector::connect_server_first`.
    QuicServerSetup::server_first(&quic_server_identity(identity), client_ca)
        .map_err(|error| format!("failed to load the QUIC server identity: {error}"))
}

fn log_quic_front_error(log_sink: Option<&SharedLogSink>, text: String) {
    match log_sink {
        Some(log) => {
            let message = rsync_error!(FEATURE_UNAVAILABLE_EXIT_CODE, text).with_role(Role::Daemon);
            log_message(log, &message);
        }
        None => eprintln!("{text} [daemon={}]", env!("CARGO_PKG_VERSION")),
    }
}

/// The QUIC front process: sandbox itself, then accept and relay.
///
/// Runs in a child the daemon parent forked while single-threaded. Before the
/// first thread exists it drops to an unprivileged identity, arranges to die
/// with the parent, and (on Linux) removes all filesystem access, because it
/// is the one daemon process that parses attacker-controlled QUIC and TLS
/// before any `hosts allow` or module check. It never forks and never serves a
/// session: every accepted stream goes to the parent over `channel`.
///
/// Returns the exit status. The main thread blocks on `channel`, so the process
/// ends when the parent closes it (a daemon shutdown or the parent's death).
fn run_quic_front(
    sockets: Vec<std::net::UdpSocket>,
    setup: &QuicServerSetup,
    channel: platform::fd_pass::FdChannel,
    parent_pid: u32,
    log_sink: Option<&SharedLogSink>,
) -> i32 {
    if let Err(error) = drop_quic_front_privileges() {
        log_quic_front_error(
            log_sink,
            format!("QUIC front process could not drop privileges: {error}"),
        );
        return FEATURE_UNAVAILABLE_EXIT_CODE;
    }
    if platform::session_fork::die_with_parent(parent_pid).is_err() {
        return 0;
    }
    if let fast_io::landlock::LandlockOutcome::Error(error) =
        fast_io::landlock::deny_all_filesystem_access()
    {
        log_quic_front_error(
            log_sink,
            format!("QUIC front process could not remove its filesystem access: {error}"),
        );
        return FEATURE_UNAVAILABLE_EXIT_CODE;
    }

    let channel = Arc::new(channel);
    for socket in sockets {
        let local = socket.local_addr().ok();
        match QuicAcceptor::from_setup(socket, setup) {
            Ok(acceptor) => {
                if let (Some(log), Some(addr)) = (log_sink, local) {
                    let text = format!("QUIC listener serving on {addr}");
                    log_message(log, &rsync_info!(text).with_role(Role::Daemon));
                }
                let channel = Arc::clone(&channel);
                let log = log_sink.cloned();
                thread::spawn(move || accept_and_hand_off(acceptor, &channel, log.as_ref()));
            }
            Err(error) => {
                let addr = local.map_or_else(|| "<unknown>".to_owned(), |a| a.to_string());
                log_quic_front_error(
                    log_sink,
                    format!("failed to start QUIC listener on {addr}: {error}"),
                );
            }
        }
    }

    // Nothing is ever sent this way; the receive returns when the parent's
    // end closes.
    let mut scratch = [0u8; 1];
    let _ = channel.recv(&mut scratch);
    0
}

/// Drops a root front process to `nobody`. A front process that is not root
/// (a non-root daemon, or one already dropped to the global `uid`/`gid`) keeps
/// its identity.
fn drop_quic_front_privileges() -> io::Result<()> {
    if !platform::privilege::is_effective_root() {
        return Ok(());
    }
    let uid = resolve_nobody_uid()?;
    let gid = resolve_nobody_gid()?;
    platform::privilege::drop_privileges(Some(uid), &[gid])
}

/// Accepts QUIC connections forever, handing each to the daemon parent.
///
/// A hand-off that fails because the parent is gone ends the front process.
fn accept_and_hand_off(
    acceptor: QuicAcceptor,
    channel: &platform::fd_pass::FdChannel,
    log_sink: Option<&SharedLogSink>,
) {
    let Ok(local) = acceptor.local_addr() else {
        return;
    };
    loop {
        match acceptor.accept() {
            Ok(stream) => {
                let Some(peer) = stream.peer_addr() else {
                    continue;
                };
                let record = platform::fd_pass::RelayRecord {
                    peer,
                    local,
                    client_identity: None,
                };
                if hand_off_quic_stream(stream, &record, channel).is_err() {
                    platform::session_fork::exit_child(0);
                }
            }
            Err(error) => {
                if let Some(log) = log_sink {
                    let text = format!("QUIC accept failed: {error}");
                    let message = rsync_error!(SOCKET_IO_EXIT_CODE, text).with_role(Role::Daemon);
                    log_message(log, &message);
                }
                return;
            }
        }
    }
}

/// Sends the parent one end of a fresh socket pair plus the connection's
/// addresses, and relays the QUIC stream through the other end.
///
/// The address is the QUIC connection's real remote address, so the parent's
/// `hosts allow`/`hosts deny` and session log judge the true client.
fn hand_off_quic_stream(
    stream: QuicStream,
    record: &platform::fd_pass::RelayRecord,
    channel: &platform::fd_pass::FdChannel,
) -> io::Result<()> {
    use std::os::fd::AsFd;

    let (ours, theirs) = UnixStream::pair()?;
    channel.send(&record.encode()?, theirs.as_fd())?;
    drop(theirs);
    thread::spawn(move || relay_quic_stream(stream, ours));
    Ok(())
}

/// Bytes moved per read in each relay direction.
const RELAY_CHUNK: usize = 256 * 1024;

/// Copies bytes both ways between a QUIC stream and the session's Unix socket
/// until both directions end.
///
/// A peer FIN becomes `shutdown(SHUT_WR)` on the socket, and the session's EOF
/// becomes a QUIC FIN. When the QUIC side resets or is lost, the session reads
/// EOF, and the socket closes completely once its writes fail too.
fn relay_quic_stream(stream: QuicStream, socket: UnixStream) {
    use std::net::Shutdown;

    let Ok(socket_reader) = socket.try_clone() else {
        return;
    };
    let quic_reader = stream.try_clone();
    // Only the write half is shut, whatever ended the copy: the session still
    // reads the peer's EOF, and shutting the read half too would discard the
    // session's final bytes before the outbound copy has sent them. A reset
    // stream closes the relay once the outbound copy fails as well.
    let inbound = thread::spawn(move || {
        let mut socket = socket;
        let _ = copy_chunks(quic_reader, &mut socket);
        let _ = socket.shutdown(Shutdown::Write);
    });
    let mut quic_writer = stream;
    if copy_chunks(socket_reader, &mut quic_writer).is_ok() {
        let _ = quic_writer.finish();
    }
    let _ = inbound.join();
}

/// Copies `reader` to `writer` until EOF, through one bounded buffer.
fn copy_chunks<R: Read, W: Write>(mut reader: R, writer: &mut W) -> io::Result<()> {
    let mut buf = vec![0u8; RELAY_CHUNK];
    loop {
        let read = match reader.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => n,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        writer.write_all(&buf[..read])?;
    }
}

impl QuicFront {
    /// Receives the next connection the front process handed over.
    ///
    /// Called when the channel polled readable. [`RelayEvent::Closed`] means
    /// the front process is gone (or sent something no front process sends)
    /// and the channel must not be polled again.
    fn receive(&self, log_sink: Option<&SharedLogSink>) -> RelayEvent {
        let mut record = [0u8; platform::fd_pass::MAX_RECORD];
        let Some(channel) = self.channel.as_ref() else {
            return RelayEvent::Closed;
        };
        match channel.recv(&mut record) {
            Ok(Some((len, fd))) => match platform::fd_pass::RelayRecord::decode(&record[..len]) {
                Ok(relayed) => RelayEvent::Connection(UnixStream::from(fd), relayed.peer),
                Err(_) => {
                    log_quic_front_error(
                        log_sink,
                        "QUIC front process sent a malformed connection record".to_owned(),
                    );
                    RelayEvent::Closed
                }
            },
            Ok(None) => RelayEvent::Closed,
            Err(error) => {
                log_quic_front_error(log_sink, format!("QUIC front channel failed: {error}"));
                RelayEvent::Closed
            }
        }
    }

    /// The channel descriptor the accept engine polls, and every forked
    /// session child must close.
    fn fd(&self) -> Option<std::os::fd::BorrowedFd<'_>> {
        use std::os::fd::AsFd;
        self.channel.as_ref().map(AsFd::as_fd)
    }
}

impl Drop for QuicFront {
    /// Closes the channel, which ends the front process, then reaps it.
    ///
    /// Only the daemon parent drops a `QuicFront`: forked session children
    /// leave through `exit_child` and never run destructors.
    fn drop(&mut self) {
        drop(self.channel.take());
        let _ = platform::session_fork::wait_for_child(self.pid);
    }
}

/// What one receive from the QUIC front channel produced.
enum RelayEvent {
    /// A relayed QUIC connection and its client's address.
    Connection(UnixStream, SocketAddr),
    /// The front process is gone; stop polling its channel.
    Closed,
}
