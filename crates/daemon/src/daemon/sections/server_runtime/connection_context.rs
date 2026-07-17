/// Shareable per-connection runtime context.
///
/// Groups every piece of daemon-wide state a single connection needs to run
/// its full session (module table, MOTD, log sink, bandwidth limits, client
/// socket options, and the reverse-lookup / PROXY-protocol toggles). All
/// fields are owned or `Arc`-shared so the context can be cloned cheaply and
/// handed to a worker on either the synchronous thread-per-connection accept
/// loop or the tokio `spawn_blocking` async accept path.
///
/// The context is built once and shared; both accept paths run the same
/// [`ConnectionContext::serve_session`] core so the per-connection wire
/// behaviour (PROXY header parsing, greeting, module select, auth, transfer)
/// is byte-identical regardless of which accept engine sourced the socket.
#[derive(Clone)]
struct ConnectionContext {
    modules: Arc<Vec<ModuleRuntime>>,
    motd_lines: Arc<Vec<String>>,
    log_sink: Option<SharedLogSink>,
    // Read only by the async accept path's `serve_one_connection`; the sync
    // accept loop applies client socket options in `handle_accepted_connection`
    // before wrapping the stream, so this field is unused in default builds.
    #[cfg_attr(not(feature = "async-daemon"), allow(dead_code))]
    client_socket_options: Arc<Vec<SocketOption>>,
    bandwidth_limit: Option<NonZeroU64>,
    bandwidth_burst: Option<NonZeroU64>,
    reverse_lookup: bool,
    proxy_protocol: bool,
}

impl ConnectionContext {
    /// Builds the shared per-connection context from the resolved runtime
    /// state assembled by the accept-loop startup sequence.
    #[allow(clippy::too_many_arguments)]
    fn new(
        modules: Arc<Vec<ModuleRuntime>>,
        motd_lines: Arc<Vec<String>>,
        log_sink: Option<SharedLogSink>,
        client_socket_options: Arc<Vec<SocketOption>>,
        bandwidth_limit: Option<NonZeroU64>,
        bandwidth_burst: Option<NonZeroU64>,
        reverse_lookup: bool,
        proxy_protocol: bool,
    ) -> Self {
        Self {
            modules,
            motd_lines,
            log_sink,
            client_socket_options,
            bandwidth_limit,
            bandwidth_burst,
            reverse_lookup,
            proxy_protocol,
        }
    }

    /// Runs the full legacy `@RSYNCD:` session for one already-wrapped
    /// connection under `catch_unwind` panic isolation.
    ///
    /// This is the shared session core used by both the synchronous
    /// `spawn_connection_worker` and the async accept path. The caller is
    /// responsible for having applied the accepted-stream socket tuning and
    /// client socket options before wrapping the raw socket in `stream`.
    ///
    /// `raw_peer_addr` is the un-normalized accept-time address; the function
    /// normalizes it exactly as the sync worker does. A panic escaping the
    /// session is caught, logged, and swallowed so the daemon stays alive,
    /// matching upstream's fork-per-connection crash isolation.
    ///
    /// upstream: clientserver.c - `start_daemon()` forks a child per
    /// connection which runs `rsync_module()`.
    fn serve_session(
        &self,
        stream: DaemonStream,
        raw_peer_addr: SocketAddr,
    ) -> io::Result<()> {
        let peer_addr = normalize_peer_address(raw_peer_addr);
        let log_for_worker = self.log_sink.clone();

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            handle_session(
                stream,
                peer_addr,
                SessionParams {
                    modules: self.modules.as_slice(),
                    motd_lines: self.motd_lines.as_slice(),
                    daemon_limit: self.bandwidth_limit,
                    daemon_burst: self.bandwidth_burst,
                    log_sink: log_for_worker.clone(),
                    reverse_lookup: self.reverse_lookup,
                    proxy_protocol: self.proxy_protocol,
                },
            )
        }));

        match result {
            Ok(inner) => inner,
            Err(payload) => {
                let description = describe_panic_payload(payload);
                if let Some(log) = log_for_worker.as_ref() {
                    let text =
                        format!("connection handler for {peer_addr} panicked: {description}");
                    let message =
                        rsync_error!(SOCKET_IO_EXIT_CODE, text).with_role(Role::Daemon);
                    log_message(log, &message);
                }
                Ok(())
            }
        }
    }

    /// Serves a single accepted connection from a raw `TcpStream`.
    ///
    /// Applies the accepted-stream socket tuning and client socket options,
    /// wraps the socket in a [`DaemonStream`], and delegates to
    /// [`ConnectionContext::serve_session`]. This is the entry point used by
    /// the async accept path, which receives raw sockets from the tokio
    /// listener and has already enforced admission control before dispatch.
    #[cfg(feature = "async-daemon")]
    fn serve_one_connection(
        &self,
        tcp_stream: TcpStream,
        raw_peer_addr: SocketAddr,
    ) -> io::Result<()> {
        apply_accepted_stream_tcp_notsent_lowat(&tcp_stream);
        // upstream: clientserver.c:1396 - daemon unconditionally enables
        // SO_KEEPALIVE on the accepted client socket, independent of the
        // per-module `socket options` config applied below.
        enable_accepted_stream_keepalive(&tcp_stream, self.log_sink.as_ref());
        let stream = DaemonStream::plain(tcp_stream);
        apply_client_options(&stream, &self.client_socket_options, self.log_sink.as_ref());
        self.serve_session(stream, raw_peer_addr)
    }
}
