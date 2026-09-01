type WorkerResult = Result<(), (Option<SocketAddr>, io::Error)>;

/// Joins finished worker threads.
///
/// Iterates through the worker list, joining any that have completed. This
/// prevents unbounded thread handle accumulation in long-running daemons.
fn reap_finished_workers(
    workers: &mut Vec<thread::JoinHandle<WorkerResult>>,
    log_sink: Option<&SharedLogSink>,
) {
    let mut index = 0;
    while index < workers.len() {
        if workers[index].is_finished() {
            let handle = workers.remove(index);
            join_worker(handle, log_sink);
        } else {
            index += 1;
        }
    }
}

/// Waits for all remaining worker threads to complete.
fn drain_workers(
    workers: &mut Vec<thread::JoinHandle<WorkerResult>>,
    log_sink: Option<&SharedLogSink>,
) {
    while let Some(handle) = workers.pop() {
        join_worker(handle, log_sink);
    }
}

/// Joins a single worker thread and reports its outcome.
///
/// A worker runs exactly one session against one accepted socket, so every
/// outcome it can carry - an I/O failure on that socket, a rejected protocol
/// state transition, a panic that escaped `catch_unwind` - describes that one
/// connection and nothing else. None of them says anything about the listening
/// socket, whose own failures travel a different path entirely: `bind_error`
/// when a listener cannot be bound or made non-blocking, and the accept
/// engine's `poll` result. Those still propagate and still end the daemon.
///
/// The join point therefore has no fatal class to report, which is why it
/// returns nothing at all rather than a `Result` a caller might act on.
///
/// upstream: socket.c:753-765 `start_accept_loop()` runs the session in a
/// forked child that ends at `_exit(ret)`, and socket.c:676-684
/// `sigchld_handler()` reaps it with `waitpid(-1, NULL, WNOHANG)` - a NULL
/// status pointer, so the parent discards the session's outcome without ever
/// inspecting it. The `while (1)` loop at socket.c:724 has no error exit;
/// `poll` failure (:738), `accept` failure (:748) and even `fork` failure
/// (:766) each keep the loop running. Only listener setup is fatal, via
/// `exit_cleanup(RERR_SOCKETIO)` at socket.c:699 and socket.c:715.
fn join_worker(handle: thread::JoinHandle<WorkerResult>, log_sink: Option<&SharedLogSink>) {
    match handle.join() {
        Ok(Ok(())) => {}
        Ok(Err((peer, error))) => report_session_failure(peer, &error, log_sink),
        Err(payload) => {
            let description = describe_panic_payload(payload);
            let error = io::Error::other(format!(
                "worker thread panicked (unwind escaped catch_unwind): {description}"
            ));
            eprintln!("{error} [daemon={}]", env!("CARGO_PKG_VERSION"));
        }
    }
}

/// Logs a session that ended in an error, without disturbing the accept loop.
///
/// Normal connection closures (broken pipe, reset, aborted) are silent: they
/// are how a finished client leaves, not a failure. Everything else is
/// reported at error level against the peer that caused it, mirroring the
/// per-connection child's own diagnostic in upstream's fork model.
fn report_session_failure(
    peer: Option<SocketAddr>,
    error: &io::Error,
    log_sink: Option<&SharedLogSink>,
) {
    if is_connection_closed_error(error.kind()) {
        return;
    }
    let target = peer.map_or_else(|| "connection".to_owned(), |addr| addr.to_string());
    let text = format!("failed to serve legacy handshake {target}: {error}");
    match log_sink {
        Some(log) => {
            let message = rsync_error!(SOCKET_IO_EXIT_CODE, text).with_role(Role::Daemon);
            log_message(log, &message);
        }
        None => eprintln!("{text} [daemon={}]", env!("CARGO_PKG_VERSION")),
    }
}

/// Extracts a human-readable message from a panic payload.
///
/// Handles the two common payload types (`String` and `&str`) and falls back
/// to a generic description for anything else.
fn describe_panic_payload(payload: Box<dyn std::any::Any + Send>) -> String {
    match payload.downcast::<String>() {
        Ok(message) => *message,
        Err(payload) => match payload.downcast::<&str>() {
            Ok(message) => (*message).to_owned(),
            Err(_) => "unknown panic payload".to_owned(),
        },
    }
}

/// Checks if an I/O error indicates a normal connection close.
const fn is_connection_closed_error(kind: io::ErrorKind) -> bool {
    matches!(
        kind,
        io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
    )
}
