type WorkerResult = Result<(), (Option<SocketAddr>, io::Error)>;
/// One in-flight connection, owned by the accept loop.
///
/// Pairs the session's backing handle with the `max connections` slot that
/// connection occupies. The slot guard lives here, in the parent, rather than
/// inside the session, so the slot's lifetime is decided by the accept loop's
/// reap rather than by the session's own exit.
///
/// That placement is what lets the backing change from a thread to a forked
/// child without the cap quietly stopping being enforced: a child inherits a
/// COPY of everything the session owns, so a guard moved into the session
/// would be released by the child's copy and never by the parent, leaving the
/// parent's count raised for the life of the daemon.
///
/// upstream: socket.c:753-765 `start_accept_loop()` keeps the forked child's
/// pid and reaps it later; the connection slot belongs to the parent, not to
/// the session running inside it.
struct SessionWorker {
    handle: thread::JoinHandle<WorkerResult>,
    /// The `max connections` slot, released when the accept loop reaps this
    /// worker. Held for its `Drop`, never read.
    _slot: ConnectionGuard,
}
impl SessionWorker {
    /// Reaps this worker if its session has ended, yielding how it ended;
    /// hands the worker back untouched if the session is still running.
    ///
    /// Observing that a session ended and collecting its outcome are ONE
    /// step, not two. A thread backing could split them into an
    /// `is_finished()` predicate plus a later join, but a forked child cannot:
    /// `waitpid(pid, .., WNOHANG)` reports the child's exit AND consumes its
    /// status in the same call, so a predicate would discard the outcome it
    /// had just collected. Keeping them together is what lets the backing
    /// change without every caller changing with it.
    ///
    /// Consuming `self` is the other half of the contract: reaping a worker
    /// drops its `max connections` slot guard, and that drop is the only
    /// thing that releases the slot (see [`SessionWorker`]).
    fn try_reap(self) -> Result<SessionOutcome, Self> {
        if self.handle.is_finished() {
            // `is_finished` already reported the thread has ended, so this
            // join returns without blocking.
            Ok(join_backing(self.handle))
        } else {
            Err(self)
        }
    }

    /// Waits for the session to end, however long that takes, and reports how.
    fn wait(self) -> SessionOutcome {
        join_backing(self.handle)
    }
}

/// How one session ended, independent of what backed it.
///
/// The accept loop needs to report a session's fate without knowing whether a
/// thread or a forked child ran it, so the three ways a session can end are
/// named here rather than left as a `thread::Result`, whose shape is a
/// property of the thread backing alone.
///
/// upstream: socket.c:676-684 `sigchld_handler()` reaps with
/// `waitpid(-1, NULL, WNOHANG)` - a NULL status pointer, so upstream's parent
/// discards the session's fate entirely. oc reports it instead; see
/// [`report_worker_outcome`] for why that is never fatal to the loop.
enum SessionOutcome {
    /// The session returned normally.
    Ok,
    /// The session failed, against this peer where one is known.
    Failed(Option<SocketAddr>, io::Error),
    /// The session died abnormally: a panic that escaped `catch_unwind`
    /// today, or a fatal signal once a forked child backs it.
    Died(String),
}

/// Collects a finished thread's outcome.
///
/// The only site that knows the backing is a thread. A fork-backed worker
/// resolves its own `waitpid` status into the same [`SessionOutcome`].
fn join_backing(handle: thread::JoinHandle<WorkerResult>) -> SessionOutcome {
    match handle.join() {
        Ok(Ok(())) => SessionOutcome::Ok,
        Ok(Err((peer, error))) => SessionOutcome::Failed(peer, error),
        Err(payload) => SessionOutcome::Died(format!(
            "worker thread panicked (unwind escaped catch_unwind): {}",
            describe_panic_payload(payload)
        )),
    }
}

/// Joins finished worker threads.
///
/// Iterates through the worker list, joining any that have completed. This
/// prevents unbounded thread handle accumulation in long-running daemons.
fn reap_finished_workers(workers: &mut Vec<SessionWorker>, log_sink: Option<&SharedLogSink>) {
    let mut still_running = Vec::with_capacity(workers.len());
    for worker in workers.drain(..) {
        match worker.try_reap() {
            // The reaped worker is dropped here, and with it the slot guard -
            // the only thing that releases the `max connections` slot now
            // that the guard is parent-owned.
            Ok(outcome) => report_worker_outcome(outcome, log_sink),
            Err(worker) => still_running.push(worker),
        }
    }
    *workers = still_running;
}

/// Waits for all remaining worker threads to complete.
fn drain_workers(workers: &mut Vec<SessionWorker>, log_sink: Option<&SharedLogSink>) {
    while let Some(worker) = workers.pop() {
        report_worker_outcome(worker.wait(), log_sink);
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
fn report_worker_outcome(outcome: SessionOutcome, log_sink: Option<&SharedLogSink>) {
    match outcome {
        SessionOutcome::Ok => {}
        SessionOutcome::Failed(peer, error) => report_session_failure(peer, &error, log_sink),
        SessionOutcome::Died(description) => {
            eprintln!("{description} [daemon={}]", env!("CARGO_PKG_VERSION"));
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
