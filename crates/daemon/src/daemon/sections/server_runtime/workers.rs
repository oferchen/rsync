/// What a thread-backed session hands back to the parent.
///
/// Unix sessions run in a forked child, which reports its own failure and
/// exits, so it has no in-process result to return.
#[cfg(not(unix))]
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
    backing: SessionBacking,
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
    fn try_reap(mut self) -> Result<SessionOutcome, Self> {
        match self.backing.try_collect() {
            Some(outcome) => Ok(outcome),
            None => Err(self),
        }
    }

    /// Waits for the session to end, however long that takes, and reports how.
    fn wait(mut self) -> SessionOutcome {
        self.backing.collect()
    }
}

/// What actually runs one session, and how the parent observes its end.
///
/// The backing is a **platform** decision, not a runtime one: a Unix daemon
/// forks a child per connection, mirroring upstream, while Windows - which has
/// no `fork` - keeps serving sessions on threads.
///
/// The two can never coexist in one process, which is why this is a `cfg`
/// split rather than an enum with two variants. `session_fork::fork_session`
/// requires a single-threaded parent, and the connection workers are the only
/// other threads the daemon runs; a child forked while a thread-backed session
/// was still live could deadlock on the allocator or on the log sink's mutex,
/// holding a lock no thread exists in the child to release.
///
/// upstream: `socket.c:753-765` `start_accept_loop()` forks per accepted
/// connection and keeps only the pid.
#[cfg(unix)]
struct SessionBacking {
    /// Pid of the child serving this connection.
    child_pid: i32,
}

#[cfg(not(unix))]
struct SessionBacking {
    /// Taken when the session is collected, so the handle is joined once.
    handle: Option<thread::JoinHandle<WorkerResult>>,
}

#[cfg(unix)]
impl SessionBacking {
    /// Collects the child if it has already ended, without blocking.
    fn try_collect(&mut self) -> Option<SessionOutcome> {
        match platform::session_fork::try_reap(self.child_pid) {
            Ok(Some(end)) => Some(SessionOutcome::from_child_end(end)),
            Ok(None) => None,
            Err(error) => Some(self.unreapable(&error)),
        }
    }

    /// Blocks until the child ends.
    fn collect(&mut self) -> SessionOutcome {
        match platform::session_fork::wait_for_child(self.child_pid) {
            Ok(end) => SessionOutcome::from_child_end(end),
            Err(error) => self.unreapable(&error),
        }
    }

    /// Reports a child whose status could not be collected at all.
    ///
    /// This is not a session outcome - it means the parent lost track of the
    /// child - so it is reported as an abnormal end rather than swallowed.
    fn unreapable(&self, error: &io::Error) -> SessionOutcome {
        SessionOutcome::Died(format!(
            "cannot reap session child {}: {error}",
            self.child_pid
        ))
    }
}

#[cfg(not(unix))]
impl SessionBacking {
    fn try_collect(&mut self) -> Option<SessionOutcome> {
        let handle = self.handle.take()?;
        if handle.is_finished() {
            // `is_finished` already reported the thread has ended, so this
            // join returns without blocking.
            Some(join_backing(handle))
        } else {
            self.handle = Some(handle);
            None
        }
    }

    fn collect(&mut self) -> SessionOutcome {
        self.handle.take().map_or_else(
            || SessionOutcome::Died("session backing was already collected".to_owned()),
            join_backing,
        )
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
    ///
    /// Only a thread backing can carry the error itself. A forked child owns
    /// the same log sink and reports its own diagnostic before exiting, so on
    /// Unix a failure arrives as [`SessionOutcome::EndedWithStatus`] instead -
    /// re-reporting it in the parent would print the same line twice.
    #[cfg_attr(unix, expect(dead_code, reason = "thread backing only; see SessionBacking"))]
    Failed(Option<SocketAddr>, io::Error),
    /// The session's own process exited non-zero, having already said why.
    EndedWithStatus(i32),
    /// The session died abnormally: a panic that escaped `catch_unwind` under
    /// a thread backing, or a fatal signal under a forked one.
    Died(String),
}

#[cfg(unix)]
impl SessionOutcome {
    /// Classifies how a forked session child ended.
    ///
    /// A signal is the one class the child cannot report for itself, so it is
    /// the one the parent must describe.
    fn from_child_end(end: platform::session_fork::ChildEnd) -> Self {
        match end {
            platform::session_fork::ChildEnd::Exited(0) => Self::Ok,
            platform::session_fork::ChildEnd::Exited(code) => Self::EndedWithStatus(code),
            platform::session_fork::ChildEnd::Signalled(signal) => {
                Self::Died(format!("session child killed by signal {signal}"))
            }
        }
    }
}

/// Collects a finished thread's outcome.
///
/// The only site that knows the backing is a thread. A fork-backed worker
/// resolves its own `waitpid` status into the same [`SessionOutcome`] via
/// [`SessionOutcome::from_child_end`].
#[cfg(not(unix))]
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
        // The session already reported why it failed, on the same log sink.
        // Upstream discards this status entirely (a NULL `waitpid` status);
        // recording it at debug keeps the fact available without repeating
        // the child's own diagnostic.
        SessionOutcome::EndedWithStatus(status) => {
            logging::debug_log!(Exit, 1, "session ended with status {status}");
        }
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
///
/// Reachable on every platform: a forked child's panic never crosses the
/// process boundary as a payload, but the child still runs
/// [`ConnectionContext::serve_session`]'s own `catch_unwind`, which describes
/// the panic in the child before it exits.
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
