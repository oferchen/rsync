/// Applies a socket-options string (`--sockopts`, else `socket options`) to a
/// daemon socket.
///
/// upstream: socket.c:606-610 hands the listener socket to
/// set_socket_options(), the one parser the client's `--sockopts` also uses:
/// exact option names, `name=value` read with atoi(), and every problem - an
/// unknown name, a value on an `OPT_ON` option, a failed setsockopt(2) - is
/// reported and skipped, never fatal.
fn apply_daemon_socket_options(
    socket: &socket2::Socket,
    options: &str,
    log_sink: Option<&SharedLogSink>,
) {
    core::client::apply_socket_options_reporting(socket, OsStr::new(options), &mut |text| {
        warn_socket_option(log_sink, text);
    });
}

/// Unconditionally enables `SO_KEEPALIVE` on a freshly accepted client stream.
///
/// upstream: clientserver.c:1396 - daemon unconditionally enables SO_KEEPALIVE
/// on the accepted client socket via `set_socket_options(f_in, "SO_KEEPALIVE")`
/// in `start_daemon()`, before the protocol handshake and independent of the
/// per-module `socket options` config (which is a separate concern applied via
/// `lp_socket_options()`). Without it, idle daemon connections can be silently
/// dropped by NAT/firewall timeouts. Best-effort: a failed `setsockopt(2)`
/// warns and the session still proceeds, mirroring upstream's warn-and-continue
/// in socket.c:738-741.
fn enable_accepted_stream_keepalive(stream: &TcpStream, log_sink: Option<&SharedLogSink>) {
    if let Err(error) = socket2::SockRef::from(stream).set_keepalive(true) {
        warn_socket_option(
            log_sink,
            format!("failed to set socket option SO_KEEPALIVE: {error}"),
        );
    }
}
/// Emits a non-fatal socket-option warning through the daemon log sink.
///
/// upstream: socket.c:set_socket_options() reports parse and `setsockopt(2)`
/// problems via `rprintf(FERROR, ...)` / `rsyserr(FERROR, ...)` and continues.
/// The daemon routes those through its log sink at warning level.
fn warn_socket_option(log_sink: Option<&SharedLogSink>, text: String) {
    if let Some(log) = log_sink {
        let message = rsync_warning!(text).with_role(Role::Daemon);
        log_message(log, &message);
    }
}
