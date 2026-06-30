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
}

/// Sets up the transfer streams for the transfer engine.
///
/// For TCP/TLS connections, configures TCP_NODELAY and clones the stream to get
/// independent read/write handles. For stdio connections (remote-shell daemon
/// mode), opens fresh stdin/stdout handles since the original pair is consumed
/// by the BufReader.
///
/// Returns the transfer streams on success, or sends an error and returns `None`.
fn setup_transfer_streams(
    ctx: &mut ModuleRequestContext<'_>,
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

    Ok(Some(TransferStreams {
        read: Box::new(read_stream),
        write: Box::new(write_stream),
        supports_tcp_shutdown: true,
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
    final_protocol: ProtocolVersion,
    module: &ModuleRuntime,
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
    let result = run_server_with_handshake(
        config,
        handshake,
        read_stream,
        write_stream,
        None,
        None,
        None,
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
