// Inetd stdin detection for standalone daemon mode.
//
// upstream: clientserver.c:1746-1757 - `daemon_main()` checks
// `is_inetd_socket(STDIN_FILENO)` before entering the TCP accept loop. When
// stdin is a connected IP stream (inetd invocation, or `RSYNC_CONNECT_PROG`,
// whose `sock_exec()` hands the program a loopback TCP pair), the daemon
// serves a single session over stdin/stdout instead of binding a TCP listener.

/// Checks whether stdin is an inetd connection.
///
/// True only for a `SOCK_STREAM` socket whose peer is `AF_INET` or
/// `AF_INET6`. A launcher that gives the process a local socket for its own
/// I/O - an `AF_UNIX` socketpair, as an ADB shell without a PTY does - must
/// not select inetd mode: that daemon is meant to listen, and serving the
/// launcher's socket instead leaves the port unbound and logs the session as
/// `connect from UNKNOWN`. `socket2::SockAddr::as_socket()` yields an address
/// only for the two IP families, which is exactly upstream's family test.
///
/// On non-Unix platforms this always returns `false` since inetd-style
/// invocation does not apply.
///
/// upstream: clientserver.c:1725-1744 - `is_inetd_socket()`.
#[cfg(unix)]
fn is_stdin_socket() -> bool {
    is_inetd_socket(&io::stdin())
}

/// The descriptor test behind [`is_stdin_socket`], for any descriptor.
#[cfg(unix)]
fn is_inetd_socket(fd: &impl std::os::fd::AsFd) -> bool {
    // socket2::SockRef::from() on Unix takes &impl AsFd, so this is entirely
    // safe (no unsafe block needed). A non-socket fails both
    // getsockopt(SO_TYPE) and getpeername with ENOTSOCK.
    let sock = socket2::SockRef::from(fd);
    sock.r#type()
        .is_ok_and(|kind| kind == socket2::Type::STREAM)
        && sock
            .peer_addr()
            .is_ok_and(|peer| peer.as_socket().is_some())
}

#[cfg(not(unix))]
fn is_stdin_socket() -> bool {
    false
}

/// Serves a single daemon session over stdin/stdout for inetd-style invocations.
///
/// This is the inetd equivalent of the TCP accept loop: the daemon reads and
/// writes the `@RSYNCD:` protocol over the inherited stdin/stdout file
/// descriptors, then exits. No TCP binding, signal registration, or
/// daemonization occurs.
///
/// upstream: clientserver.c:1548-1559 - when `is_a_socket(STDIN_FILENO)` is
/// true, `daemon_main()` redirects stdout/stderr to `/dev/null` and calls
/// `start_daemon(STDIN_FILENO, STDIN_FILENO)`.
fn serve_inetd_session(options: RuntimeOptions) -> Result<(), DaemonError> {
    let brand = options.brand;

    let RuntimeOptions {
        modules,
        motd_lines,
        bandwidth_limit,
        log_file,
        reverse_lookup,
        lock_file,
        daemon_timeout,
        ..
    } = options;

    let log_sink = if let Some(path) = log_file {
        Some(open_log_sink(&path, brand)?)
    } else {
        None
    };

    // Inetd path serves one session in this process and exits, but the
    // hardening is still cheap insurance: PR_SET_NO_NEW_PRIVS prevents
    // any later setuid exec (e.g. a pre-xfer-exec hook configured on the
    // requested module) from regaining privileges, and the LSM audit line
    // makes the active kernel defenses visible to operators inspecting
    // inetd-style logs.
    apply_startup_hardening(log_sink.as_ref());

    // The lock file matters MORE here than on the accept loop, not less. This
    // process serves one session and exits, so there is no in-process counter
    // spanning concurrent inetd children - the `fcntl` byte-range lock is the
    // only mechanism that can enforce `max connections` across them.
    let (modules, _connection_limiter) = build_module_runtimes_with_lock_file(modules, lock_file)?;

    // LSM-CAP.5: verify required Linux capabilities are present before serving
    // the inetd session. Mirrors the standalone path so per-module
    // `uid = root` modules fail loud at startup instead of producing a
    // confusing per-file `chown failed` mid-transfer. No-op on non-Linux.
    if let Err(reason) = preflight_required_capabilities(&modules) {
        return Err(DaemonError::new(
            FEATURE_UNAVAILABLE_EXIT_CODE,
            rsync_error!(
                FEATURE_UNAVAILABLE_EXIT_CODE,
                format!("oc-rsyncd: error: {reason}")
            )
            .with_role(Role::Daemon),
        ));
    }

    // Build a DaemonStream::Stdio from process stdin/stdout.
    // upstream: clientserver.c:1759 - start_daemon(STDIN_FILENO, STDIN_FILENO)
    // passes the same fd for both read and write. We use separate stdin/stdout
    // handles since Rust's std::io separates them.
    let stdin = io::stdin();
    let stdout = io::stdout();
    let pair = crate::daemon_stream::StdioPair::new(Box::new(stdin), Box::new(stdout));
    let stream = DaemonStream::stdio(pair);

    // upstream: clientserver.c:1759 - `start_daemon(STDIN_FILENO, STDIN_FILENO)`.
    // Under inetd, fd 0 IS the connected socket, so `client_addr()` skips the
    // environment arm entirely and `client_sockaddr()` reads the real peer via
    // `getpeername` (clientname.c:37-45).
    //
    // This site previously fabricated `127.0.0.1` on the stated premise that
    // "there is no TCP socket to query" - which is simply untrue here, and is
    // why the hole survived review. Every `hosts allow` / `hosts deny` rule was
    // evaluated against a synthetic localhost instead of the real client.
    let peer_addr = match crate::daemon::peer_address::inherited_socket_peer_addr() {
        Ok(addr) => addr,
        // upstream: clientname.c:41-45 - `getpeername` failure is fatal
        // (`exit_cleanup(RERR_SOCKETIO)`), never a fallback address. Serving a
        // session whose peer cannot be named would evaluate the ACL against
        // nothing.
        Err(err) => {
            let code = ExitCode::SocketIo;
            return Err(DaemonError::with_code(
                code,
                rsync_error!(
                    code.as_i32(),
                    format!("getpeername on the inherited stdin socket failed: {err}")
                )
                .with_role(Role::Daemon),
            ));
        }
    };

    // upstream: clientname.c `client_name` forward-confirms the reverse-DNS
    // name unconditionally, so this pre-module log/registry name is confirmed
    // too. Per-module `forward lookup` still governs the access-control match
    // in `module_peer_hostname`.
    let peer_host = if reverse_lookup {
        resolve_peer_hostname(peer_addr.ip(), true)
    } else {
        None
    };

    if let Some(log) = log_sink.as_ref() {
        log_connection(
            log,
            peer_host_display(peer_host.as_deref(), reverse_lookup),
            peer_addr,
        );
    }

    let outcome = handle_legacy_session(
        stream,
        peer_addr,
        LegacySessionParams {
            modules: &modules,
            motd_lines: &motd_lines,
            daemon_limit: bandwidth_limit,
            log_sink,
            peer_host,
            reverse_lookup,
            daemon_timeout,
        },
    );

    single_session_exit(outcome, "inetd daemon session failed")
}

#[cfg(test)]
mod inetd_tests {
    use super::*;

    /// Verifies that `is_stdin_socket()` returns `false` when run from a normal
    /// terminal or test harness (stdin is a pipe or pty, not a socket).
    #[test]
    fn stdin_is_not_socket_in_test_harness() {
        assert!(!is_stdin_socket());
    }

    /// A connected TCP stream is what inetd hands the daemon, so it must
    /// select the single-session path.
    #[cfg(unix)]
    #[test]
    fn connected_tcp_stream_is_an_inetd_socket() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let client =
            std::net::TcpStream::connect(listener.local_addr().expect("addr")).expect("connect");
        let (server, _) = listener.accept().expect("accept");
        assert!(is_inetd_socket(&server));
        assert!(is_inetd_socket(&client));
    }

    /// A local socket on stdin (an ADB shell without a PTY) is the launcher's
    /// own I/O channel, not a client connection. Treating it as inetd leaves
    /// the daemon's port unbound and logs `connect from UNKNOWN`.
    /// upstream: clientserver.c:1740-1743 accepts only AF_INET/AF_INET6.
    #[cfg(unix)]
    #[test]
    fn unix_socketpair_is_not_an_inetd_socket() {
        let (ours, _peer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        assert!(!is_inetd_socket(&ours));
    }

    /// An IP peer alone is not enough: inetd supplies a stream, and a
    /// connected datagram socket must not be served as one.
    /// upstream: clientserver.c:1735 `type != SOCK_STREAM`.
    #[cfg(unix)]
    #[test]
    fn connected_udp_socket_is_not_an_inetd_socket() {
        let a = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind a");
        let b = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind b");
        a.connect(b.local_addr().expect("addr")).expect("connect");
        assert!(!is_inetd_socket(&a));
    }

    #[cfg(unix)]
    #[test]
    fn regular_file_is_not_an_inetd_socket() {
        let devnull = std::fs::File::open("/dev/null").expect("/dev/null");
        assert!(!is_inetd_socket(&devnull));
    }
}
