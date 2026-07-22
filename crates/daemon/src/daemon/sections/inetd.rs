// Inetd/connect-program stdin detection for standalone daemon mode.
//
// upstream: clientserver.c:1546-1560 - `daemon_main()` checks
// `is_a_socket(STDIN_FILENO)` before entering the TCP accept loop. When stdin
// is a socket (inetd invocation or `RSYNC_CONNECT_PROG` pipe), the daemon
// serves a single session over stdin/stdout instead of binding a TCP listener.
//
// upstream: socket.c:500-518 - `is_a_socket(fd)` calls
// `getsockopt(fd, SOL_SOCKET, SO_TYPE, ...)` and returns 1 on success.

/// Checks whether stdin is a socket (inetd/connect-program invocation).
///
/// Returns `true` when the process was spawned by inetd, xinetd, systemd
/// socket activation, or `RSYNC_CONNECT_PROG` with a socketpair - all of
/// which set stdin to an `AF_UNIX` or `AF_INET` socket. The check uses
/// `getsockopt(SO_TYPE)` via `socket2::SockRef`, matching upstream rsync's
/// `is_a_socket()` in `socket.c:500`.
///
/// On non-Unix platforms this always returns `false` since inetd-style
/// invocation does not apply.
///
/// upstream: socket.c:500-518 - `is_a_socket(fd)`.
#[cfg(unix)]
fn is_stdin_socket() -> bool {
    // socket2::SockRef::from() on Unix takes &impl AsFd. std::io::Stdin
    // implements AsFd, so this is entirely safe (no unsafe block needed).
    // If the fd is not a socket, SockRef::r#type() returns Err because
    // getsockopt(SO_TYPE) fails with ENOTSOCK.
    let stdin = io::stdin();
    let sock = socket2::SockRef::from(&stdin);
    sock.r#type().is_ok()
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
        bandwidth_burst,
        log_file,
        reverse_lookup,
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

    let connection_limiter: Option<Arc<ConnectionLimiter>> = None;
    let modules: Vec<ModuleRuntime> = modules
        .into_iter()
        .map(|definition| ModuleRuntime::new(definition, connection_limiter.clone()))
        .collect();

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
    // upstream: clientserver.c:1559 - start_daemon(STDIN_FILENO, STDIN_FILENO)
    // passes the same fd for both read and write. We use separate stdin/stdout
    // handles since Rust's std::io separates them.
    let stdin = io::stdin();
    let stdout = io::stdout();
    let pair = crate::daemon_stream::StdioPair::new(Box::new(stdin), Box::new(stdout));
    let stream = DaemonStream::stdio(pair);

    // upstream: start_daemon() with inherited fds uses 127.0.0.1:0 as the
    // synthetic peer address since there is no TCP socket to query for a real
    // peer address.
    let peer_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);

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
        log_connection(log, peer_host.as_deref(), peer_addr);
    }

    handle_legacy_session(
        stream,
        peer_addr,
        LegacySessionParams {
            modules: &modules,
            motd_lines: &motd_lines,
            daemon_limit: bandwidth_limit,
            daemon_burst: bandwidth_burst,
            log_sink,
            peer_host,
            reverse_lookup,
        },
    )
    .map_err(|error| {
        DaemonError::new(
            SOCKET_IO_EXIT_CODE,
            rsync_error!(
                SOCKET_IO_EXIT_CODE,
                format!("inetd daemon session failed: {error}")
            )
            .with_role(Role::Daemon),
        )
    })
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

    /// Verifies that `getsockopt(SO_TYPE)` succeeds on a real socket fd and
    /// fails on a regular file fd - the two branches of `is_stdin_socket()`.
    #[cfg(unix)]
    #[test]
    fn socket_detection_distinguishes_socket_from_file() {
        use std::os::unix::net::UnixStream;

        // A Unix socketpair fd must be detected as a socket.
        let (sock_a, _sock_b) = UnixStream::pair().expect("socketpair");
        let sock_ref = socket2::SockRef::from(&sock_a);
        assert!(
            sock_ref.r#type().is_ok(),
            "getsockopt(SO_TYPE) should succeed on a socket fd"
        );

        // A regular file fd must not be detected as a socket.
        let devnull = std::fs::File::open("/dev/null").expect("/dev/null");
        let devnull_ref = socket2::SockRef::from(&devnull);
        assert!(
            devnull_ref.r#type().is_err(),
            "getsockopt(SO_TYPE) should fail on a regular file fd"
        );
    }
}
