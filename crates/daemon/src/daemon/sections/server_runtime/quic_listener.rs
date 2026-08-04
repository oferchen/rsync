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
/// The two enums live in different layers on purpose: [`QuicIdentity`] is the
/// daemon's config-time decision (which directives the operator set), while
/// [`QuicServerIdentity`] is what `rsync_io` needs to materialize a
/// certificate at bind time.
fn quic_server_identity(identity: &QuicIdentity) -> QuicServerIdentity {
    match identity {
        QuicIdentity::Ephemeral => QuicServerIdentity::Ephemeral,
        QuicIdentity::Files { cert, key } => QuicServerIdentity::PemFiles {
            cert: cert.clone(),
            key: key.clone(),
        },
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

/// Binds one QUIC/UDP acceptor per entry in `bind_addresses`, tolerating
/// per-family failures while at least one family binds.
///
/// The datagram counterpart of [`bind_listeners_per_family`]: it walks the same
/// `resolve_bind_addresses` list in the same order, warns via
/// [`warn_per_family_bind_failure`] when a family fails in dual-stack mode, and
/// only returns `Err` when every family failed (callers map that to a
/// `DaemonError`). `quic_port` is the resolved `effective_quic_port()`.
///
/// The returned acceptors are bound but idle; the accept loop that turns each
/// into a session lands under QUIC task #55.
fn bind_quic_listeners_per_family(
    bind_addresses: &[IpAddr],
    quic_port: u16,
    identity: &QuicIdentity,
    log_sink: Option<&SharedLogSink>,
) -> Result<Vec<QuicAcceptor>, io::Error> {
    let server_identity = quic_server_identity(identity);
    let dual_stack = bind_addresses.len() > 1;
    let mut acceptors = Vec::with_capacity(bind_addresses.len());
    let mut last_error: Option<io::Error> = None;

    for addr in bind_addresses {
        let requested_addr = SocketAddr::new(*addr, quic_port);
        let result = bind_quic_socket(requested_addr)
            .and_then(|socket| QuicAcceptor::from_socket(socket, &server_identity));
        match result {
            Ok(acceptor) => acceptors.push(acceptor),
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

    if acceptors.is_empty() {
        let error = last_error.unwrap_or_else(|| {
            io::Error::new(
                io::ErrorKind::AddrNotAvailable,
                "no addresses available to bind the QUIC listener",
            )
        });
        return Err(error);
    }

    Ok(acceptors)
}
