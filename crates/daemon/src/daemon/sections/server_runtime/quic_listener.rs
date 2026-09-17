// QUIC/UDP listener binding (oc extension, feature `quic`, Unix-only daemon).
//
// This is the datagram sibling of the TCP `bind_listeners_per_family` pipeline
// in `listener.rs`. It reuses the identical `resolve_bind_addresses` policy so
// the QUIC listener binds the same ordered, dual-stack set of addresses as the
// TCP one, and mirrors the per-family tolerance: a dual-stack startup that
// loses one family (e.g. an IPv6-degraded CI runner) still comes up on the
// survivor, and only a total bind failure is fatal.
//
// Scope: bind and hold the acceptors. Driving `accept()` -> `QuicStream` ->
// session is the next QUIC task (#55); `serve_connections` keeps the returned
// acceptors alive for it. See docs/design/quic-transport-policy.md.

use rsync_io::quic::{QuicAcceptor, QuicServerIdentity};

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
/// live [`QuicAcceptor`] is deferred to [`materialize_and_serve_quic`] because
/// `QuicAcceptor::from_socket` spawns the connection's I/O driver thread, and
/// `become_daemon`'s `fork(2)` would orphan a driver spawned before it (only
/// the calling thread survives a fork). Binding the fd early and materializing
/// the acceptor in the post-fork process keeps the privileged bind early and
/// the driver alive in the serving process.
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

/// Materializes a live [`QuicAcceptor`] per pre-bound UDP socket and hands each
/// to a serve thread, running in the daemon's final post-fork identity.
///
/// Called after `become_daemon` and the daemon-level privilege drop so the QUIC
/// I/O driver `QuicAcceptor::from_socket` spawns lives in the serving process,
/// not a pre-fork parent whose threads the `fork(2)` discards. A socket whose
/// acceptor cannot be built (e.g. a bad operator certificate) is logged and
/// skipped rather than taking the whole daemon down - the TCP listener keeps
/// serving. `context` carries the same runtime state the TCP path's
/// [`ConnectionContext`] does, so the served session is transport-agnostic.
fn materialize_and_serve_quic(
    sockets: Vec<std::net::UdpSocket>,
    identity: &QuicIdentity,
    context: &ConnectionContext,
    log_sink: Option<&SharedLogSink>,
) {
    let server_identity = quic_server_identity(identity);
    for socket in sockets {
        let local = socket.local_addr().ok();
        // Server-speaks-first: the daemon writes the `@RSYNCD:` greeting before
        // the client sends anything, so the acceptor opens the bidirectional
        // stream. The peer connects with `QuicConnector::connect_server_first`.
        match QuicAcceptor::from_socket_server_first(socket, &server_identity) {
            Ok(acceptor) => {
                if let (Some(log), Some(addr)) = (log_sink, local) {
                    let text = format!("QUIC listener serving on {addr}");
                    let message = rsync_info!(text).with_role(Role::Daemon);
                    log_message(log, &message);
                }
                let context = context.clone();
                thread::spawn(move || serve_quic_acceptor(acceptor, context));
            }
            Err(error) => {
                if let Some(log) = log_sink {
                    let addr = local
                        .map(|a| a.to_string())
                        .unwrap_or_else(|| "<unknown>".to_owned());
                    let text = format!("failed to start QUIC listener on {addr}: {error}");
                    let message =
                        rsync_error!(FEATURE_UNAVAILABLE_EXIT_CODE, text).with_role(Role::Daemon);
                    log_message(log, &message);
                }
            }
        }
    }
}

/// The single owner of the QUIC accept -> `@RSYNCD` session handoff.
///
/// Blocks on the acceptor's one incoming connection, wraps the accepted
/// [`QuicStream`](rsync_io::quic::QuicStream) in a [`DaemonStream::Quic`], and
/// runs the SAME [`ConnectionContext::serve_session`] core the TCP accept path
/// uses. The greeting/module/auth/transfer flow is therefore identical across
/// transports - there is no parallel QUIC session runner.
///
/// Thread-backed, never forked. A `QuicStream` is driven by an in-process
/// background thread, and `fork(2)` copies only the calling thread, so a forked
/// child would inherit a dead driver and deadlock on the first read. The TCP
/// path forks per connection for `chroot`/cwd isolation (upstream
/// `clientserver.c` `start_accept_loop`); QUIC (oc extension, default off) runs
/// the session in-thread instead, matching the non-unix thread-backed session
/// model. The acceptor is one-shot at the transport layer (`rsync_io` refuses a
/// second connection per endpoint), so this serves exactly one session.
fn serve_quic_acceptor(acceptor: QuicAcceptor, context: ConnectionContext) {
    match acceptor.accept() {
        Ok(stream) => {
            // The real client address, recorded by the QUIC driver when the
            // connection was established, so `hosts allow`/`hosts deny` and the
            // session log evaluate against the true peer rather than a
            // fabricated localhost. The fallback cannot be reached for an
            // accepted stream (the stream exists only after the connection).
            let peer_addr = stream
                .peer_addr()
                .unwrap_or_else(|| SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0));
            let _ = context.serve_session(DaemonStream::quic(stream), peer_addr);
        }
        Err(error) => {
            if let Some(log) = context.log_sink.as_ref() {
                let text = format!("QUIC accept failed: {error}");
                let message = rsync_error!(SOCKET_IO_EXIT_CODE, text).with_role(Role::Daemon);
                log_message(log, &message);
            }
        }
    }
}

