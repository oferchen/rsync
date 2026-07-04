// Transfer stream setup, handshake result construction, and the
// run-server transfer-execution dispatch with per-transfer logging.
/// Transfer stream pair: separate read and write handles for the transfer engine.
///
/// For TCP connections, both sides are cloned `TcpStream` handles pointing at
/// the same socket. For stdio connections (remote-shell daemon mode), the reader
/// wraps stdin and the writer wraps stdout.
struct TransferStreams {
    read: Box<dyn Read + Send>,
    write: Box<dyn Write + Send>,
    /// Whether the write side supports TCP shutdown (false for stdio/pipes).
    supports_tcp_shutdown: bool,
    /// Stop handle for the daemon-TCP background drain thread (#503).
    ///
    /// Present only for the socket path, where `read` wraps a
    /// [`DrainingReader`] whose background thread continuously drains the
    /// peer's send buffer during the delta phase to prevent the full-duplex
    /// write-write deadlock. `None` for stdio/pipe transports, which read the
    /// socket directly and cannot wedge. The caller stops this handle after
    /// the transfer engine returns and before the goodbye drain reads the
    /// socket via another clone.
    drain_handle: Option<DrainHandle>,
    /// Pre-erasure socket clone for the tokio driver (ASY sub-rung 2).
    ///
    /// For the daemon module transfer over a real socket this carries an extra
    /// `try_clone()`d `TcpStream` (a plain dup'd fd) so the tokio receiver can
    /// wrap it as an `AsyncTransport` in sub-rung 3. It is only threaded here;
    /// this rung does not construct the wrapper or touch the socket flags, so
    /// the sync `read`/`write` clones stay byte-identical. Stdio/pipe transports
    /// keep `None` and remain on the sync path.
    #[cfg(feature = "tokio-transfer")]
    async_socket: Option<TcpStream>,
}

/// Decides whether the #503 background delta-drain thread should be armed.
///
/// The drain thread is anti-deadlock machinery for the bidirectional delta
/// phase (design doc Approach C). That phase only occurs when the client
/// actually requested a transfer, which it does by sending a non-empty argument
/// list after `@RSYNCD: OK`. When the post-`OK` argument read returns an empty
/// list the peer dropped the socket without requesting a transfer, so no delta
/// data flows and there is nothing to deadlock. Such a connection must NOT arm
/// the drain: its background thread would block reading a half-closed socket
/// clone, which on Windows never unblocks and hangs the daemon worker.
///
/// #6297: the drain is additionally gated to Unix. On Windows the drain thread
/// parks in a blocking `recv()` on the cloned socket that `SO_RCVTIMEO` does not
/// interrupt, so `stop_and_join()` never returns and every daemon connection
/// wedges at teardown (all four daemon-negotiation tests time out on the Windows
/// feature-flag jobs, on every branch commit, but pass on master). The #503
/// deadlock is a Unix-specific full-socket-buffer wedge, so on non-Unix the
/// daemon uses the raw read clone - byte-identical to master, which passes these
/// tests - and never spawns the drain thread.
#[cfg(unix)]
fn should_arm_delta_drain(client_args: &[String]) -> bool {
    !client_args.is_empty()
}

#[cfg(not(unix))]
fn should_arm_delta_drain(_client_args: &[String]) -> bool {
    false
}

/// Sets up the transfer streams for the transfer engine.
///
/// For TCP/TLS connections, configures TCP_NODELAY and clones the stream to get
/// independent read/write handles. For stdio connections (remote-shell daemon
/// mode), opens fresh stdin/stdout handles since the original pair is consumed
/// by the BufReader.
///
/// `arm_drain` gates the #503 background delta-drain thread: it is spawned only
/// when a real transfer will run (the client sent a non-empty argument list).
/// A connection whose post-`OK` argument read hit EOF - the peer dropped the
/// socket without ever requesting a transfer - carries no bidirectional delta
/// data, so it cannot hit the write-write deadlock the drain thread guards
/// against. Arming the drain for such a connection would spawn a thread that
/// blocks reading a half-closed socket clone; on Windows that thread's `recv`
/// on a `try_clone`d socket handle does not observe the peer's close and never
/// unblocks, wedging `stop_and_join()` and hanging the daemon worker. Reading
/// the socket directly on this degenerate path is byte-identical to the
/// pre-#503 behaviour and returns EOF promptly on every platform.
///
/// Returns the transfer streams on success, or sends an error and returns `None`.
fn setup_transfer_streams(
    ctx: &mut ModuleRequestContext<'_>,
    arm_drain: bool,
) -> io::Result<Option<TransferStreams>> {
    let stream = ctx.reader.get_mut();
    stream.set_nodelay(true)?;

    if stream.is_stdio() {
        // For stdio mode, the DaemonStream wraps a StdioPair (stdin + stdout).
        // The BufReader has consumed it, but the transfer engine needs separate
        // read/write handles. We open fresh stdin/stdout handles here - the
        // buffered data from the BufReader is captured in HandshakeResult.buffered
        // and chained ahead of stdin by run_server_with_handshake.
        // upstream: start_daemon(STDIN_FILENO, STDOUT_FILENO) uses the same
        // fds for both handshake and transfer.
        let stdin = io::stdin();
        let stdout = io::stdout();
        return Ok(Some(TransferStreams {
            read: Box::new(stdin),
            write: Box::new(stdout),
            supports_tcp_shutdown: false,
            // Stdio/pipe transports have independent read/write pipe buffers
            // and a peer in a separate process, so they cannot hit the
            // single-socket write-write deadlock (#503). Read the pipe
            // directly - no drain thread.
            drain_handle: None,
            // Stdio/pipe transports have no socket to hand the tokio driver.
            #[cfg(feature = "tokio-transfer")]
            async_socket: None,
        }));
    }

    let tcp = stream
        .tcp_stream()
        .expect("non-stdio stream has tcp_stream");

    let read_stream = match tcp.try_clone() {
        Ok(s) => s,
        Err(err) => {
            let payload = format!("@ERROR: failed to clone stream: {err}");
            send_error_and_exit(ctx.reader.get_mut(), ctx.limiter, ctx.messages, &payload)?;
            return Ok(None);
        }
    };

    let write_stream = match tcp.try_clone() {
        Ok(s) => s,
        Err(err) => {
            return Err(io::Error::other(format!(
                "failed to clone write stream: {err}"
            )));
        }
    };

    // ASY sub-rung 2: an extra socket clone the tokio driver can adopt as an
    // `AsyncTransport` in sub-rung 3. This is a plain `try_clone()` (a dup'd fd)
    // with no flag change - the socket stays blocking here, so the sync
    // `read_stream`/`write_stream` above are byte-identical. A clone failure is
    // non-fatal: fall back to `None` so the receiver stays on the sync path.
    #[cfg(feature = "tokio-transfer")]
    let async_socket = tcp.try_clone().ok();

    // #503: wrap the read-clone fd in a `DrainingReader` so a background thread
    // continuously drains the peer's send buffer during the delta phase. This
    // is the daemon-TCP-only anti-deadlock mechanism (design doc Approach C):
    // it keeps the peer's writes flowing so neither direction wedges on a full
    // socket buffer. The wrapper is a transparent byte pipe, so every wire byte
    // and the multiplex framing are unchanged. The `DrainHandle` is stopped by
    // the orchestrator before the goodbye drain reads the socket via another
    // clone (`ctx.reader`'s `DaemonStream`, a separate fd).
    //
    // `DrainingReader::new` arms only a bounded read timeout on this clone (NOT
    // non-blocking mode): an idle socket returns `TimedOut` and the drain loop
    // polls the stop flag instead of parking in a blocking `read()`, so
    // `DrainHandle::stop()` joins the thread promptly on every platform. It must
    // NOT use non-blocking mode: `read_stream`/`write_stream` share one open
    // file description, so a non-blocking flag would leak onto the write clone
    // and truncate the sender's writes (code 23). A read timeout leaks only
    // `SO_RCVTIMEO`, harmless to the write-only clone; the drain thread clears it
    // on exit so the goodbye clone reads a normal blocking socket.
    //
    // Armed only for a real transfer (`arm_drain`, i.e. the client sent a
    // non-empty argument list). An empty-args connection - the peer requested a
    // module then dropped the socket without sending a transfer request - has no
    // delta phase to deadlock, so it reads the socket directly and returns EOF
    // promptly (see the fn-level doc for the Windows wedge this avoids).
    if !arm_drain {
        return Ok(Some(TransferStreams {
            read: Box::new(read_stream),
            write: Box::new(write_stream),
            supports_tcp_shutdown: true,
            drain_handle: None,
            #[cfg(feature = "tokio-transfer")]
            async_socket,
        }));
    }

    let (draining_reader, drain_handle) = DrainingReader::new(read_stream);

    Ok(Some(TransferStreams {
        read: Box::new(draining_reader),
        write: Box::new(write_stream),
        supports_tcp_shutdown: true,
        drain_handle: Some(drain_handle),
        #[cfg(feature = "tokio-transfer")]
        async_socket,
    }))
}

/// Builds the handshake result for the transfer.
fn build_handshake_result(
    reader: &BufReader<DaemonStream>,
    negotiated_protocol: Option<ProtocolVersion>,
    client_args: Vec<String>,
    module: &ModuleRuntime,
) -> HandshakeResult {
    let final_protocol = negotiated_protocol.unwrap_or(ProtocolVersion::V30);
    let buffered_data = reader.buffer().to_vec();

    HandshakeResult {
        protocol: final_protocol,
        buffered: buffered_data,
        compat_exchanged: false,
        client_args: Some(client_args),
        io_timeout: module.timeout.map(|t| t.get()),
        negotiated_algorithms: None,
        compat_flags: None,
        checksum_seed: 0,
    }
}

/// Runs the daemon server body, selecting the pipeline entry per feature.
///
/// Default build (no `tokio-transfer`): calls the threaded
/// [`run_server_with_handshake`] directly - byte-for-byte the pre-ASY path.
///
/// `tokio-transfer` on: when a real socket clone is available (the daemon
/// module transfer), routes through the tokio driver
/// [`run_server_with_handshake_on`] instead. The driver `host_sync_on`-hosts the
/// **same** synchronous server body on a current-thread runtime, so every wire
/// byte, flush ordering, and goodbye handshake is identical to the direct call
/// (ASY sub-rung 2 is routing + socket plumbing only; the read chain stays
/// sync until sub-rung 3). The `async_socket` clone is dropped at the end of
/// this scope in this rung - it is threaded here so sub-rung 3 can adopt it as
/// an `AsyncTransport`. When no socket is available (stdio/pipe), stays on the
/// sync entry.
#[cfg(not(feature = "tokio-transfer"))]
fn run_daemon_transfer(
    config: ServerConfig,
    handshake: HandshakeResult,
    read_stream: &mut dyn Read,
    write_stream: &mut dyn Write,
) -> ServerResult {
    run_server_with_handshake(
        config,
        handshake,
        read_stream,
        write_stream,
        None,
        None,
        None,
    )
}

/// See the `not(tokio-transfer)` twin above. Routes the socket-backed daemon
/// receiver through the tokio driver when a socket and runtime are available.
#[cfg(feature = "tokio-transfer")]
fn run_daemon_transfer(
    config: ServerConfig,
    handshake: HandshakeResult,
    read_stream: &mut dyn Read,
    write_stream: &mut dyn Write,
    async_socket: Option<TcpStream>,
) -> ServerResult {
    match async_socket {
        // Socket-backed daemon module transfer: route through the tokio driver.
        // The driver hosts the sync server body via `host_sync_on`, so output is
        // byte-identical to the direct call. The socket clone is held for the
        // duration of the transfer so its fd stays valid, and dropped at scope
        // end (sub-rung 3 adopts it as an `AsyncTransport` instead of dropping).
        Some(socket) => with_daemon_transfer_runtime(|handle| {
            let result = run_server_with_handshake_on(
                handle,
                config,
                handshake,
                read_stream,
                write_stream,
                None,
                None,
                None,
            );
            drop(socket);
            result
        }),
        // No socket (stdio/pipe): stay on the sync entry, unchanged.
        None => run_server_with_handshake(
            config,
            handshake,
            read_stream,
            write_stream,
            None,
            None,
            None,
        ),
    }
}

/// Runs `f` with a tokio runtime handle for the daemon transfer path.
///
/// Mirrors `core::session::with_transfer_runtime`: adopts an ambient runtime
/// when one exists (the hybrid async listener dispatches workers via
/// `spawn_blocking`, so `Handle::current()` resolves inside a worker) and
/// otherwise builds a session-scoped current-thread runtime. A current-thread
/// runtime runs the future on the calling thread, so the borrowed sync
/// transports stay valid and wire ordering matches the threaded path.
#[cfg(feature = "tokio-transfer")]
fn with_daemon_transfer_runtime<R>(f: impl FnOnce(&tokio::runtime::Handle) -> R) -> R {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => f(&handle),
        Err(_) => {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build session-scoped tokio runtime");
            f(runtime.handle())
        }
    }
}

/// Executes the server transfer and logs the result.
///
/// When the module has `transfer_logging` enabled and a log sink is available,
/// a per-transfer log line is emitted using the module's configured format
/// string (or `DEFAULT_LOG_FORMAT` as fallback).
///
/// Returns the transfer exit status: `0` on success, non-zero on failure.
// The `tokio-transfer` build adds the pre-erasure socket handle as a 9th
// parameter; the allow is feature-gated so the default 8-arg build is untouched.
#[cfg_attr(feature = "tokio-transfer", allow(clippy::too_many_arguments))]
fn execute_transfer(
    ctx: &ModuleRequestContext<'_>,
    config: ServerConfig,
    handshake: HandshakeResult,
    read_stream: &mut dyn Read,
    write_stream: &mut dyn Write,
    role: ServerRole,
    final_protocol: ProtocolVersion,
    module: &ModuleRuntime,
    #[cfg(feature = "tokio-transfer")] async_socket: Option<TcpStream>,
) -> i32 {
    if let Some(log) = ctx.log_sink {
        let text = format!(
            "module '{}' from {} ({}): protocol {}, role: {:?}",
            ctx.request,
            ctx.effective_host().unwrap_or("unknown"),
            ctx.peer_ip,
            final_protocol.as_u8(),
            role
        );
        let message = rsync_info!(text).with_role(Role::Daemon);
        log_message(log, &message);
    }

    // Use standard buffered I/O for daemon socket communication.
    // io_uring SEND blocks in submit_and_wait() during bidirectional protocol
    // exchanges (NDX_DONE, stats, goodbye) when TCP backpressure occurs,
    // causing 10-second hangs. Standard I/O handles partial writes correctly,
    // matching upstream rsync's socket I/O model.
    let result = run_daemon_transfer(
        config,
        handshake,
        read_stream,
        write_stream,
        #[cfg(feature = "tokio-transfer")]
        async_socket,
    );

    match result {
        Ok(_server_stats) => {
            if let Some(log) = ctx.log_sink {
                if module.transfer_logging {
                    let operation = match role {
                        ServerRole::Generator => TransferOperation::Send,
                        ServerRole::Receiver => TransferOperation::Recv,
                    };
                    let addr_str = ctx.peer_ip.to_string();
                    let path_str = module.path.display().to_string();
                    let pid = std::process::id();
                    let now = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default();
                    let secs = now.as_secs();
                    let timestamp = format_daemon_timestamp(secs);

                    let log_ctx = LogFormatContext {
                        operation,
                        hostname: ctx.effective_host().unwrap_or("unknown"),
                        remote_addr: &addr_str,
                        module_name: ctx.request,
                        username: "",
                        filename: "",
                        file_length: 0,
                        pid,
                        module_path: &path_str,
                        timestamp: &timestamp,
                        bytes_transferred: 0,
                        bytes_checksumed: 0,
                        itemize_string: "",
                    };

                    let fmt = effective_log_format(module);
                    log_transfer(fmt, &log_ctx, log);
                }

                let text = format!(
                    "transfer to {} ({}): module={} status=success",
                    ctx.effective_host().unwrap_or("unknown"),
                    ctx.peer_ip,
                    ctx.request
                );
                let message = rsync_info!(text).with_role(Role::Daemon);
                log_message(log, &message);
            }
            0
        }
        Err(err) => {
            if let Some(log) = ctx.log_sink {
                let text = format!(
                    "transfer failed to {} ({}): module={} error={}",
                    ctx.effective_host().unwrap_or("unknown"),
                    ctx.peer_ip,
                    ctx.request,
                    err
                );
                let message = rsync_error!(1, text).with_role(Role::Daemon);
                log_message(log, &message);
            }
            1
        }
    }
}

#[cfg(test)]
mod delta_drain_gate_tests {
    //! Gating tests for the #503 delta-drain thread (`should_arm_delta_drain`).

    use super::should_arm_delta_drain;

    #[test]
    fn empty_client_args_do_not_arm_the_drain() {
        // Regression (#503, Windows CI): a peer that requested a module then
        // dropped the socket sends an empty argument list. That connection has
        // no delta phase, so it must read the socket directly rather than spawn
        // a drain thread that hangs on a half-closed socket clone on Windows.
        assert!(
            !should_arm_delta_drain(&[]),
            "empty client args means no transfer requested: the drain must stay off"
        );
    }

    #[test]
    fn non_empty_client_args_arm_the_drain() {
        // A real transfer request (non-empty argv) can enter the bidirectional
        // delta phase, so the anti-deadlock drain thread must be armed.
        let args = vec!["--server".to_owned(), "--sender".to_owned()];
        assert!(
            should_arm_delta_drain(&args),
            "a real transfer request must arm the anti-deadlock drain"
        );
    }
}
