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
/// For TCP connections, configures TCP_NODELAY and clones the stream to get
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
/// Wraps a plaintext TCP write clone into the daemon-sender's byte sink.
///
/// When `zero_copy_policy` permits SEND_ZC on Unix - which
/// [`fast_io::send_zc_policy_permits`] answers - the socket fd is handed to
/// [`fast_io::socket_writer_from_fd_zero_copy`], which returns an
/// `IORING_OP_SEND_ZC` writer when the running kernel advertises the opcode and
/// otherwise degrades to a plain fd writer. The `TcpStream` is kept alive
/// alongside the returned writer (the factory borrows the fd but does not take
/// ownership), so the fd stays valid for the transfer's lifetime.
///
/// The policy gate has two arms, not one. `Disabled` (`--no-zero-copy`) never
/// reaches the factory. `Auto` - the default every client arrives with, since
/// the oc-invented `--zero-copy` flag is deliberately never forwarded to a peer
/// argv - reaches it only in builds carrying the `iouring-send-zc` cargo
/// feature; that feature is the IUS-4 release gate which raises the socket-send
/// kernel floor from 5.6 to 6.0. A stock build therefore keeps `Auto` on the
/// unchanged `TcpStream` writer. On non-Unix there is no raw-fd path at all.
#[cfg(unix)]
fn daemon_socket_writer(
    write_stream: TcpStream,
    zero_copy_policy: fast_io::ZeroCopyPolicy,
) -> Box<dyn Write + Send> {
    use std::os::unix::io::AsRawFd;

    if !fast_io::send_zc_policy_permits(zero_copy_policy) {
        return Box::new(write_stream);
    }

    // 64 KiB matches the `MultiplexWriter` frame buffer; the factory only uses
    // it to size the fallback writer's internal buffer. The fd is borrowed, not
    // owned, so `write_stream` is moved into the sink to keep it valid.
    let fd = write_stream.as_raw_fd();
    match fast_io::socket_writer_from_fd_zero_copy(fd, 64 * 1024, zero_copy_policy) {
        Ok(zc) => Box::new(ZeroCopyTcpWriter {
            writer: zc,
            _socket: write_stream,
        }),
        // A construction failure is non-fatal: fall back to the plain
        // `TcpStream` writer so the transfer still runs with identical framing.
        Err(_) => Box::new(write_stream),
    }
}

/// Non-Unix: no raw-fd zero-copy path; keep the current `TcpStream` writer.
#[cfg(not(unix))]
fn daemon_socket_writer(
    write_stream: TcpStream,
    _zero_copy_policy: fast_io::ZeroCopyPolicy,
) -> Box<dyn Write + Send> {
    Box::new(write_stream)
}

/// Pairs the zero-copy socket writer with the `TcpStream` whose fd it borrows.
///
/// `fast_io::socket_writer_from_fd_zero_copy` does not take ownership of the fd,
/// so the `TcpStream` must outlive the writer. Holding both here ties their
/// lifetimes together: dropping this struct drops the writer first, then closes
/// the socket. Every `Write` call delegates to the inner zero-copy writer, so
/// the framed bytes are unchanged.
#[cfg(unix)]
struct ZeroCopyTcpWriter {
    writer: fast_io::IoUringOrStdSocketWriter,
    _socket: TcpStream,
}

#[cfg(unix)]
impl Write for ZeroCopyTcpWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.writer.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

/// `zero_copy_policy` opts the daemon-sender's socket write side into the
/// io_uring `IORING_OP_SEND_ZC` transport when it is
/// [`ZeroCopyPolicy::Enabled`](fast_io::ZeroCopyPolicy::Enabled) (the client
/// sent `--zero-copy`) and the write side is a plaintext TCP socket. The
/// zero-copy writer substitutes the socket write of the same framed buffer, so
/// the wire bytes are identical; only the syscall path changes. `Auto` and
/// `Disabled` - and every non-plaintext or stdio transport - keep the current
/// `TcpStream` writer unchanged, so the default transfer path is byte- and
/// behavior-identical. On non-Linux, or a build without the `io_uring` cargo
/// feature, the factory degrades to the plain fd writer; on non-Unix the raw-fd
/// path is skipped entirely and the current `TcpStream` box is used.
///
/// Returns the transfer streams on success, or sends an error and returns `None`.
/// `deadline` is the session's transfer-phase idle timeout (`io_progress.rs`),
/// present only when upstream's `check_timeout()` would check at all. When it is
/// present both halves of the socket stamp its clock - the drain reader on every
/// successful read, [`writer_marking_progress`] on every successful write - so
/// idleness is measured as upstream's `MAX(last_io_out, last_io_in)` rather than
/// from reads alone.
fn setup_transfer_streams(
    ctx: &mut ModuleRequestContext<'_>,
    arm_drain: bool,
    zero_copy_policy: fast_io::ZeroCopyPolicy,
    deadline: Option<TransferDeadline>,
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
        }));
    }

    // QUIC transport (oc extension): split the single bidirectional stream into
    // independent blocking read/write handles with `QuicStream::try_clone` - the
    // QUIC counterpart of `TcpStream::try_clone`. Reads touch only the receive
    // path and writes only the send path, so the two handles never contend
    // beyond the driver's shared lock. No #503 drain thread is needed: the QUIC
    // driver continuously pulls datagrams off the socket into a receive buffer
    // regardless of facade reads, so the single-socket write-write deadlock the
    // TCP path guards against cannot occur (the same property that lets the
    // stdio branch above skip the drain). Teardown is the stream's own graceful
    // FIN + close, not a TCP half-close, so `supports_tcp_shutdown` is false.
    #[cfg(feature = "quic")]
    if let Some(quic) = stream.quic_stream() {
        let read_half = quic.try_clone();
        let write_half = quic.try_clone();
        return Ok(Some(TransferStreams {
            read: Box::new(read_half),
            write: Box::new(write_half),
            supports_tcp_shutdown: false,
            drain_handle: None,
        }));
    }

    let tcp = stream
        .tcp_stream()
        .expect("non-stdio stream has tcp_stream");

    let read_stream = match tcp.try_clone() {
        Ok(s) => s,
        Err(err) => {
            let error = AtError::message(format!("failed to clone stream: {err}"));
            send_error(ctx.reader.get_mut(), ctx.limiter, &error)?;
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
    // No drain thread means no tick that could observe an idle socket, so the
    // deadline has no consumer on this path and the writer is left unwrapped
    // rather than stamping a clock nobody reads. Both branches that land here
    // are outside upstream's timeout concern: an empty-args connection never
    // enters the transfer phase at all, and the non-Unix daemon has never armed
    // the drain (see `should_arm_delta_drain`).
    if !arm_drain {
        return Ok(Some(TransferStreams {
            read: Box::new(read_stream),
            // Plaintext TCP write side: opt into SEND_ZC when the client sent
            // `--zero-copy`. `supports_tcp_shutdown` is true on every path that
            // reaches here, so the plaintext gate is already satisfied.
            write: daemon_socket_writer(write_stream, zero_copy_policy),
            supports_tcp_shutdown: true,
            drain_handle: None,
        }));
    }

    let writer = writer_marking_progress(
        daemon_socket_writer(write_stream, zero_copy_policy),
        deadline.as_ref(),
    );
    let (draining_reader, drain_handle) = DrainingReader::new(read_stream, deadline);

    Ok(Some(TransferStreams {
        read: Box::new(draining_reader),
        write: writer,
        supports_tcp_shutdown: true,
        drain_handle: Some(drain_handle),
    }))
}

/// Builds the handshake result for the transfer.
fn build_handshake_result(
    reader: &BufReader<DaemonStream>,
    negotiated_protocol: Option<ProtocolVersion>,
    client_args: Vec<String>,
    io_timeout: Option<NonZeroU64>,
) -> HandshakeResult {
    let final_protocol = negotiated_protocol.unwrap_or(ProtocolVersion::V30);
    let buffered_data = reader.buffer().to_vec();

    HandshakeResult {
        protocol: final_protocol,
        buffered: buffered_data,
        compat_exchanged: false,
        client_args: Some(client_args),
        // upstream: clientserver.c:1288-1289 - the reconciled minimum of the
        // client's forwarded `--timeout` and the module's `timeout` directive,
        // which is what `main.c:1295` then advertises as `MSG_IO_TIMEOUT`.
        io_timeout: io_timeout.map(NonZeroU64::get),
        negotiated_algorithms: None,
        compat_flags: None,
        checksum_seed: 0,
    }
}

/// Runs the daemon server body via the threaded
/// [`run_server_with_handshake_adopting`].
///
/// `daemon_log` carries the per-file FLOG sink for a module with
/// `transfer logging = yes`; every other hook stays `None` on the daemon path.
fn run_daemon_transfer(
    config: ServerConfig,
    handshake: HandshakeResult,
    read_stream: &mut dyn Read,
    write_stream: &mut dyn Write,
    daemon_log: Option<DaemonLog<'_>>,
) -> ServerResult {
    run_server_with_handshake_adopting(
        config,
        handshake,
        read_stream,
        write_stream,
        ServerTransferHooks {
            daemon_log,
            ..ServerTransferHooks::default()
        },
    )
}

/// Renders one per-file daemon transfer-log line into the module's `log format`.
///
/// This is the daemon's own `log_item(FLOG)` write (upstream `log.c:866-874`),
/// invoked once per processed entry by the transfer engine after the transfer,
/// in flist-index order. The `%i` string is pre-rendered with the correct
/// direction glyph by the sending/receiving context; here it only fills the
/// per-file `%f`/`%l`/`%i` fields of the module format alongside the constant
/// connection fields.
struct DaemonFileLogWriter<'a> {
    log: &'a SharedLogSink,
    fmt: String,
    operation: TransferOperation,
    hostname: String,
    remote_addr: String,
    module_name: String,
    module_path: String,
    pid: u32,
}

impl DaemonFileLog for DaemonFileLogWriter<'_> {
    fn on_entry(&mut self, name: &std::path::Path, size: u64, itemize: &str) {
        // upstream: log.c:664 `%t` renders timestring(time(NULL)) at the moment
        // the line is written; per-file lines flush right after the transfer.
        let timestamp = logging_sink::logfile::format_log_timestamp(SystemTime::now());
        let filename = name.to_string_lossy();
        let log_ctx = LogFormatContext {
            operation: self.operation,
            hostname: &self.hostname,
            remote_addr: &self.remote_addr,
            module_name: &self.module_name,
            username: "",
            filename: &filename,
            file_length: size,
            pid: self.pid,
            module_path: &self.module_path,
            timestamp: &timestamp,
            // upstream renders %b/%c from per-file byte counters; oc's per-entry
            // FLOG row carries name/length/%i only, so byte-count escapes render
            // as 0 for now. The default and %i-bearing formats do not use them.
            bytes_transferred: 0,
            bytes_checksumed: 0,
            itemize_string: itemize,
        };
        log_transfer(&self.fmt, &log_ctx, self.log);
    }
}

/// Executes the server transfer and logs the result.
///
/// When the module has `transfer_logging` enabled and a log sink is available,
/// a per-transfer log line is emitted using the module's configured format
/// string (or `DEFAULT_LOG_FORMAT` as fallback).
///
/// Returns the transfer exit status: `0` on success, non-zero on failure.
fn execute_transfer(
    ctx: &ModuleRequestContext<'_>,
    config: ServerConfig,
    handshake: HandshakeResult,
    read_stream: &mut dyn Read,
    write_stream: &mut dyn Write,
    role: ServerRole,
    module: &ModuleRuntime,
) -> i32 {
    if let Some(log) = ctx.log_sink {
        // upstream: the daemon sender announces the walk with the FLOG-only
        // `building file list` (flist.c:2248) and the daemon receiver mirrors
        // it with `receiving file list` (flist.c:2608); both land in the
        // daemon log via rwrite()'s am_daemon branch (log.c:290-303).
        let banner = match role {
            ServerRole::Generator => "building file list",
            ServerRole::Receiver => "receiving file list",
        };
        log_message(log, &rsync_info!(banner).with_role(Role::Daemon));
    }

    // upstream: clientserver.c:823-826 - a module with `transfer logging = yes`
    // makes the daemon write one `log_item(FLOG)` line per processed file. Build
    // the per-file sink so the transfer engine can render each entry into the
    // module's `log format`; `%i` renders on the daemon path whenever the format
    // carries it, since `logfile_format_has_i` is set from the module format
    // independently of the client's `-i`.
    let mut daemon_log_writer = ctx.log_sink.filter(|_| module.transfer_logging).map(|log| {
        let operation = match role {
            ServerRole::Generator => TransferOperation::Send,
            ServerRole::Receiver => TransferOperation::Recv,
        };
        DaemonFileLogWriter {
            log,
            fmt: effective_log_format(module).to_string(),
            operation,
            hostname: ctx.host_display().to_string(),
            remote_addr: ctx.peer_ip.to_string(),
            module_name: ctx.request.to_string(),
            module_path: module.path.display().to_string(),
            pid: std::process::id(),
        }
    });
    let daemon_log = daemon_log_writer.as_mut().map(|w| DaemonLog {
        format_has_i: log_format_has_i(&w.fmt),
        sink: w,
    });

    // Use standard buffered I/O for daemon socket communication.
    // io_uring SEND blocks in submit_and_wait() during bidirectional protocol
    // exchanges (NDX_DONE, stats, goodbye) when TCP backpressure occurs,
    // causing 10-second hangs. Standard I/O handles partial writes correctly,
    // matching upstream rsync's socket I/O model.
    let result = run_daemon_transfer(config, handshake, read_stream, write_stream, daemon_log);

    // Diagnostics emitted while the transfer ran go to the daemon log.
    //
    // upstream: log.c:310-327 `else if (am_daemon || logfile_name)` calls
    // `logit()` for EVERY code that reaches it, not for `FLOG` alone - `FLOG`
    // is merely the code that `return`s afterwards instead of also reaching
    // the stream switch. So a daemon that raises `FERROR` mid-session logs it
    // too, and a drain that took only `FLOG` would discard it: the message
    // would exist, be correct, and never be delivered.
    //
    // This runs after `run_daemon_transfer` returns, so anything the receiver
    // pipeline already framed to the peer through `drain_events_for_peer` is
    // gone from the buffer by now and cannot be logged twice. That ordering
    // also states the residual honestly: upstream's `rwrite()` reaches
    // `logit()` and then the `am_server` frame, feeding BOTH sinks, whereas a
    // drain removes - so on the receiver role an `FERROR` reaches the peer
    // instead of the log. Widening here can only add deliveries, never move
    // one.
    if let Some(log) = ctx.log_sink {
        for event in logging::drain_events_for_daemon_log() {
            let message = match event {
                logging::DiagnosticEvent::Info { message, .. }
                | logging::DiagnosticEvent::Debug { message, .. } => message,
                // A byte-faithful notice degrades to a lossy string on this
                // daemon-log path; the log-file sink applies the log-file
                // escape at write time. Byte-exact daemon-log fidelity is a
                // follow-up (see the notice-channel migration inventory).
                logging::DiagnosticEvent::Bytes { message, .. } => {
                    String::from_utf8_lossy(&message).into_owned()
                }
            };
            log_message(log, &rsync_info!(message).with_role(Role::Daemon));
        }
    }

    match result {
        Ok(server_stats) => {
            if let Some(log) = ctx.log_sink {
                // The per-file transfer-log lines are written by the transfer
                // engine's daemon-log hook (see `DaemonFileLogWriter`), one row
                // per processed entry in flist-index order - mirroring upstream's
                // per-file `log_item(FLOG)` (receiver.c:1273 / sender.c:461).
                // Only the totals trailer is emitted here.
                //
                // upstream: cleanup.c:222-226 - `am_daemon` always runs
                // log_exit(), whose FLOG totals trailer
                // `sent %s bytes  received %s bytes  total size %s`
                // (log.c:894-899, plain `big_num()` digits) closes every
                // daemon transfer in the log.
                let (sent, received, total_size) = match &server_stats {
                    ServerStats::Generator(stats) => {
                        (stats.bytes_sent, stats.bytes_read, stats.total_size)
                    }
                    ServerStats::Receiver(stats) => (
                        stats.bytes_sent,
                        stats.bytes_received,
                        stats.total_source_bytes,
                    ),
                };
                let text = format!(
                    "sent {sent} bytes  received {received} bytes  total size {total_size}"
                );
                log_message(log, &rsync_info!(text).with_role(Role::Daemon));
            }
            0
        }
        Err(err) => {
            if let Some(log) = ctx.log_sink {
                let text = format!(
                    "transfer failed to {} ({}): module={} error={}",
                    ctx.host_display(),
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
        // delta phase, so on Unix the anti-deadlock drain thread must be armed.
        // On non-Unix (Windows) the drain is gated off entirely - its background
        // thread cannot be reliably stopped - so even a real transfer uses the
        // raw read clone (the master path).
        let args = vec!["--server".to_owned(), "--sender".to_owned()];
        #[cfg(unix)]
        assert!(
            should_arm_delta_drain(&args),
            "a real transfer request must arm the anti-deadlock drain on Unix"
        );
        #[cfg(not(unix))]
        assert!(
            !should_arm_delta_drain(&args),
            "the drain is gated off on non-Unix even for a real transfer"
        );
    }
}
