/// Strategy for sourcing accepted client connections in the daemon accept loop.
///
/// Selected once at accept-loop entry from the available readiness mechanism.
/// Each implementation hides *how* the next connection becomes ready - one
/// `poll(2)` over every listener fd, or a per-platform readiness queue - behind
/// a uniform [`poll`] interface. The accept loop body (signal handling,
/// capacity refusal, worker spawn) is therefore identical regardless of
/// listener count or platform.
///
/// This is the seam the per-platform readiness engines (io_uring multishot
/// `IORING_OP_ACCEPT`, kqueue `EVFILT_READ`, IOCP `AcceptEx`) plug into without
/// touching the shared loop body.
///
/// upstream: socket.c `start_accept_loop()` runs a single `select(2)` over all
/// listener descriptors; the engine abstraction preserves that "one loop over
/// N listeners" shape while letting the readiness mechanism vary.
///
/// [`poll`]: AcceptEngine::poll
trait AcceptEngine {
    /// Polls for the next accepted connection.
    ///
    /// Blocks for at most one internal poll interval (bounding signal-flag
    /// inspection latency) before yielding control. The returned [`TcpStream`]
    /// is always in blocking mode, matching upstream's synchronous per-session
    /// I/O model.
    fn poll(&mut self) -> Result<AcceptOutcome, DaemonError>;

    /// Stops the engine, releasing its readiness resources. Idempotent.
    fn shutdown(&mut self);
}

/// Result of polling an [`AcceptEngine`].
enum AcceptOutcome {
    /// A client connection was accepted (stream already set to blocking).
    Connection(TcpStream, SocketAddr),
    /// No connection was ready within the poll interval. The engine has
    /// already waited the appropriate amount, so the caller must re-check
    /// signal flags and poll again without adding its own sleep.
    Idle,
}

/// Signal-check cadence for the portable engines' readiness wait, in
/// milliseconds.
///
/// Bounds the `poll(2)` park so shutdown/reload/graceful-exit flags are
/// re-inspected at the same 50ms interval the previous `WouldBlock` sleep
/// provided - shutdown latency is unchanged, only the idle mechanism differs.
const READINESS_WAIT_MILLIS: u16 = 50;

/// Parks until at least one listener has a pending connection or
/// `timeout_millis` elapses, reporting which listeners became readable.
///
/// Returns the indices of the ready listeners, or an empty vector on timeout or
/// `EINTR` (the caller re-checks signal flags and parks again). Parking on
/// readiness rather than spinning on a non-blocking `accept` means `accept` is
/// invoked roughly once per connection instead of once per wakeup.
///
/// One `poll(2)` covers every listener, so an N-family daemon parks in a single
/// kernel wait rather than one wait per family. That is what lets the accept
/// path stay single-threaded; see [`PollAcceptEngine`].
///
/// upstream: socket.c `start_accept_loop()` parks in `select(2)` over all
/// listener fds before calling `accept(2)`.
#[cfg(unix)]
fn poll_ready(
    listeners: &[(TcpListener, SocketAddr)],
    timeout_millis: u16,
) -> io::Result<Vec<usize>> {
    use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
    use std::os::fd::AsFd;

    let mut fds: Vec<PollFd<'_>> = listeners
        .iter()
        .map(|(listener, _)| PollFd::new(listener.as_fd(), PollFlags::POLLIN))
        .collect();
    match poll(&mut fds, PollTimeout::from(timeout_millis)) {
        Ok(0) => Ok(Vec::new()),
        Ok(_) => Ok(fds
            .iter()
            .enumerate()
            .filter(|(_, fd)| {
                // POLLERR/POLLHUP also warrant an accept attempt: the error is
                // reported by accept(2) itself, which the transient-failure arm
                // then logs. Ignoring them would spin on a permanently ready fd.
                fd.revents().is_some_and(|events| {
                    events.intersects(PollFlags::POLLIN | PollFlags::POLLERR | PollFlags::POLLHUP)
                })
            })
            .map(|(index, _)| index)
            .collect()),
        Err(nix::errno::Errno::EINTR) => Ok(Vec::new()),
        Err(errno) => Err(io::Error::from(errno)),
    }
}

/// Portable fallback for targets without `poll(2)`: sleep the signal-check
/// interval, then report every listener ready so the caller falls back to a
/// non-blocking `accept` probe - the pre-readiness idle shape, kept only where
/// no readiness primitive is available (the daemon accept path is Unix-only in
/// practice).
#[cfg(not(unix))]
fn poll_ready(
    listeners: &[(TcpListener, SocketAddr)],
    timeout_millis: u16,
) -> io::Result<Vec<usize>> {
    thread::sleep(Duration::from_millis(u64::from(timeout_millis)));
    Ok((0..listeners.len()).collect())
}

/// Accept engine for any number of bound listeners: one `poll(2)` over every
/// listener fd, then one `accept` from a ready listener.
///
/// # Single-threaded by construction
///
/// Every listener is polled from the caller's thread, so the accept path holds
/// no acceptor threads however many address families are bound. That is a
/// correctness precondition, not a tidiness preference: the per-connection
/// state a daemon session applies includes process-wide syscalls - `chroot(2)`
/// and `setuid(2)` above all - which can only be isolated per connection by
/// forking the session, and [`platform::session_fork`] may only be called from
/// a single-threaded accept path. A child forked from a multithreaded parent
/// can deadlock on an allocator lock held by a thread that does not exist in
/// the child, so an engine that kept one acceptor thread per family would make
/// that fork illegal on exactly the dual-stack topology daemons default to.
///
/// upstream: socket.c `start_accept_loop()` - a single `select(2)` over all
/// listener fds in one process, with no per-family threads.
///
/// # One connection per poll (admission correctness)
///
/// Exactly one accepted connection is returned per [`poll`](AcceptEngine::poll).
/// This is load-bearing for `--max-connections` accounting: the shared accept
/// loop reaps finished workers (dropping their `ConnectionGuard`s) once per
/// iteration, *before* it polls. An engine that drained several ready
/// connections in one poll would hand them to the admission path back-to-back
/// with no intervening reap, so guards from just-completed transfers would
/// accumulate and spuriously trip the cap under rapid sequential load.
///
/// # Fairness across families
///
/// The scan of ready listeners starts one past the family served last, so a
/// continuously busy family cannot starve its sibling. The previous
/// thread-per-listener engine got this from the OS scheduler; a single-threaded
/// poll has to rotate explicitly.
///
/// # Accept errors are transient
///
/// upstream: socket.c:593 `if (fd < 0) continue;` - the accept loop ignores
/// every `accept(2)` failure and keeps serving. A transient per-connection
/// error (ECONNABORTED when a client resets between handshake and accept, or
/// EMFILE/ENFILE under a burst) must never tear the daemon down, and a failure
/// on one family must never stop the others being polled.
struct PollAcceptEngine {
    listeners: Vec<(TcpListener, SocketAddr)>,
    /// Index to begin the next ready-listener scan at, rotated after each
    /// accept so no family can monopolise the loop.
    next_start: usize,
    log_sink: Option<SharedLogSink>,
}

impl PollAcceptEngine {
    fn new(
        listeners: Vec<TcpListener>,
        bound_addresses: &[SocketAddr],
        log_sink: Option<SharedLogSink>,
    ) -> Result<Self, DaemonError> {
        let mut paired = Vec::with_capacity(listeners.len());
        for (listener, local_addr) in listeners.into_iter().zip(bound_addresses.iter().copied()) {
            // Non-blocking so a spurious readiness (the pending connection was
            // reset before accept could take it) returns WouldBlock instead of
            // parking the single accept thread.
            listener
                .set_nonblocking(true)
                .map_err(|error| bind_error(local_addr, error))?;
            paired.push((listener, local_addr));
        }
        Ok(Self {
            listeners: paired,
            next_start: 0,
            log_sink,
        })
    }

    /// Takes one connection from the listener at `index`, mapping every
    /// `accept(2)` failure to [`AcceptOutcome::Idle`] so the loop re-checks
    /// signal flags and retries.
    fn accept_from(&mut self, index: usize) -> AcceptOutcome {
        let (listener, local_addr) = &self.listeners[index];
        let local_addr = *local_addr;
        match listener.accept() {
            Ok((tcp_stream, raw_peer_addr)) => {
                if let Err(error) = tcp_stream.set_nonblocking(false) {
                    if let Some(log) = self.log_sink.as_ref() {
                        let text = format!("failed to set accepted socket to blocking: {error}");
                        let message = rsync_warning!(text).with_role(Role::Daemon);
                        log_message(log, &message);
                    }
                    return AcceptOutcome::Idle;
                }
                AcceptOutcome::Connection(tcp_stream, raw_peer_addr)
            }
            // Spurious readiness, or an interrupted accept: no sleep, the next
            // poll parks in the readiness wait again.
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => AcceptOutcome::Idle,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => AcceptOutcome::Idle,
            Err(error) => {
                warn_transient_accept_failure(self.log_sink.as_ref(), local_addr, &error);
                thread::sleep(Duration::from_millis(50));
                AcceptOutcome::Idle
            }
        }
    }
}

impl AcceptEngine for PollAcceptEngine {
    fn poll(&mut self) -> Result<AcceptOutcome, DaemonError> {
        let ready = match poll_ready(&self.listeners, READINESS_WAIT_MILLIS) {
            Ok(ready) => ready,
            Err(error) => {
                // A poll(2) failure over valid listener fds is as transient as
                // an accept(2) failure: log, back off so a persistent error
                // cannot hot-spin, and keep serving.
                if let Some((_, local_addr)) = self.listeners.first() {
                    warn_transient_accept_failure(self.log_sink.as_ref(), *local_addr, &error);
                }
                thread::sleep(Duration::from_millis(50));
                return Ok(AcceptOutcome::Idle);
            }
        };
        if ready.is_empty() {
            // Timeout or EINTR: the wait itself parked, so the caller adds no
            // sleep of its own before re-checking signal flags.
            return Ok(AcceptOutcome::Idle);
        }

        let count = self.listeners.len();
        for offset in 0..count {
            let index = (self.next_start + offset) % count;
            if !ready.contains(&index) {
                continue;
            }
            self.next_start = (index + 1) % count;
            return Ok(self.accept_from(index));
        }
        Ok(AcceptOutcome::Idle)
    }

    fn shutdown(&mut self) {}
}

/// macOS `kqueue` accept engine: one `EVFILT_READ` registration per listener,
/// readiness-driven `accept` that yields exactly one connection per poll.
///
/// Replaces the portable engines' per-listener `poll(2)` park with a single
/// `kevent(2)` wait over all listener fds. On a quiet daemon the thread parks
/// in the kernel until a connection arrives or the 100ms signal-check timeout
/// elapses, bounding shutdown-flag inspection to the same cadence the
/// dual-stack engine uses.
///
/// # One connection per poll (admission correctness)
///
/// The engine returns **exactly one** accepted connection per
/// [`poll`](AcceptEngine::poll), matching the single-listener engine's
/// one-at-a-time contract. This is load-bearing for `--max-connections`
/// accounting: the shared accept loop reaps finished worker threads (dropping
/// their [`ConnectionGuard`]s) only once per loop iteration, *before* it polls.
/// An engine that drained and buffered several ready connections in one poll
/// would hand them to the admission path back-to-back with no intervening reap,
/// so guards from just-completed sequential transfers - still held while their
/// worker threads finish teardown after the client has disconnected - would
/// accumulate and spuriously trip the cap under rapid sequential load. Yielding
/// one per poll routes every connection through the loop body's reap cadence,
/// keeping the process-global counter in lockstep exactly as the portable
/// engines do.
///
/// # Level-triggered readiness
///
/// Listeners are registered `EVFILT_READ` **without** `EV_CLEAR`
/// (level-triggered) via [`submit_read_level`]. Because only one connection is
/// taken per wake, a backlog that queues several connections must re-fire on the
/// next `wait`; an edge-triggered (`EV_CLEAR`) registration would consume the
/// edge after the first accept and strand the remainder until a *new* connection
/// arrived. Level-triggered readiness re-surfaces the pending backlog on every
/// poll, so no queued connection is ever lost.
///
/// Admission (`--max-connections`) and the N-listener fan-out are otherwise
/// unchanged: this engine only sources accepted streams; the shared loop body
/// still gates every returned connection through the process-global admission
/// counter.
///
/// Selected by [`build_accept_engine`] on macOS with a graceful fallback to the
/// portable engines if `kqueue(2)` setup fails, so a kqueue error never breaks
/// connection service.
///
/// [`submit_read_level`]: fast_io::KqueueLoop::submit_read_level
#[cfg(all(target_os = "macos", feature = "macos-kqueue"))]
struct KqueueAcceptEngine {
    /// Registered listeners keyed by their `EVFILT_READ` user-data index.
    listeners: Vec<(TcpListener, SocketAddr)>,
    /// kqueue event surface; dropped (closing its fd) on [`Self::shutdown`].
    kq: fast_io::KqueueLoop,
    log_sink: Option<SharedLogSink>,
}

#[cfg(all(target_os = "macos", feature = "macos-kqueue"))]
impl KqueueAcceptEngine {
    /// Signal-check cadence for the `kevent(2)` wait, matching the dual-stack
    /// engine's `recv_timeout` interval so shutdown latency is identical.
    const WAIT_TIMEOUT: Duration = Duration::from_millis(100);

    /// Builds the engine, registering a level-triggered `EVFILT_READ` event per
    /// listener.
    ///
    /// Returns an `io::Error` (not a [`DaemonError`]) so the caller can fall
    /// back to the portable engines on any kqueue setup failure without
    /// aborting daemon startup.
    fn new(
        listeners: Vec<TcpListener>,
        bound_addresses: &[SocketAddr],
        log_sink: Option<SharedLogSink>,
    ) -> io::Result<Self> {
        use std::os::unix::io::AsRawFd;

        let kq = fast_io::KqueueLoop::new()?;
        let mut registered: Vec<(TcpListener, SocketAddr)> = Vec::with_capacity(listeners.len());
        for (index, (listener, local_addr)) in listeners
            .into_iter()
            .zip(bound_addresses.iter().copied())
            .enumerate()
        {
            // Non-blocking so the single accept below returns WouldBlock (rather
            // than blocking) if the readiness was spurious or already consumed.
            listener.set_nonblocking(true)?;
            // Level-triggered: a pending backlog re-fires on the next wait, so
            // taking one connection per poll never strands queued connections.
            kq.submit_read_level(listener.as_raw_fd(), index as u64)?;
            registered.push((listener, local_addr));
        }
        Ok(Self {
            listeners: registered,
            kq,
            log_sink,
        })
    }

    /// Accepts exactly one connection from a ready listener.
    ///
    /// Returns the accepted (blocking) stream on success, `Ok(None)` if the
    /// readiness was spurious / already consumed (`WouldBlock`) or the accepted
    /// socket could not be reset to blocking, or the fatal accept error paired
    /// with the listener's local address. Accepted streams are reset to blocking
    /// because BSD kernels propagate the listener's `O_NONBLOCK` to the accepted
    /// socket, which would otherwise break the synchronous handshake reader.
    fn accept_one(
        &self,
        index: usize,
    ) -> Result<Option<(TcpStream, SocketAddr)>, (SocketAddr, io::Error)> {
        let (listener, local_addr) = &self.listeners[index];
        let local_addr = *local_addr;
        loop {
            match listener.accept() {
                Ok((stream, peer_addr)) => {
                    if let Err(error) = stream.set_nonblocking(false) {
                        if let Some(log) = self.log_sink.as_ref() {
                            let text =
                                format!("failed to set accepted socket to blocking: {error}");
                            let message = rsync_warning!(text).with_role(Role::Daemon);
                            log_message(log, &message);
                        }
                        return Ok(None);
                    }
                    return Ok(Some((stream, peer_addr)));
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(None),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err((local_addr, error)),
            }
        }
    }
}

#[cfg(all(target_os = "macos", feature = "macos-kqueue"))]
impl AcceptEngine for KqueueAcceptEngine {
    fn poll(&mut self) -> Result<AcceptOutcome, DaemonError> {
        let events = match self.kq.wait(Some(Self::WAIT_TIMEOUT)) {
            Ok(events) => events,
            // EINTR is folded into an empty result by KqueueLoop::wait; any
            // other kevent failure means the readiness surface is unusable.
            // Surface it against the first listener so the loop body exits
            // rather than spinning on a broken kqueue.
            Err(error) => return Err(accept_error(self.listeners[0].1, error)),
        };
        if events.is_empty() {
            // Timeout with no readiness: let the caller re-check signal flags.
            return Ok(AcceptOutcome::Idle);
        }

        // Take exactly one connection this poll so every admission is preceded
        // by the loop body's worker-reap step. Level-triggered readiness re-
        // fires the remaining backlog on the next poll, so nothing is stranded.
        let mut fatal: Option<(SocketAddr, io::Error)> = None;
        for event in events {
            let index = event.user_data as usize;
            if index >= self.listeners.len() {
                continue;
            }
            match self.accept_one(index) {
                Ok(Some((stream, peer_addr))) => {
                    return Ok(AcceptOutcome::Connection(stream, peer_addr));
                }
                Ok(None) => continue,
                Err(err) => {
                    fatal.get_or_insert(err);
                }
            }
        }

        if let Some((local_addr, error)) = fatal {
            return Err(accept_error(local_addr, error));
        }
        Ok(AcceptOutcome::Idle)
    }

    fn shutdown(&mut self) {
        // The KqueueLoop closes its fd on drop; there are no acceptor threads to
        // join. Clearing the listeners drops their fds too, matching the
        // portable engines' teardown. Idempotent: a second call finds it empty.
        self.listeners.clear();
    }
}

/// Attempts to build the macOS kqueue accept engine.
///
/// Returns `Ok(Some(engine))` on success, `Ok(None)` if kqueue setup fails so
/// the caller falls back to the portable engines, threading `listeners` back out
/// unchanged on failure. Any kqueue error is non-fatal: connection service must
/// continue through the blocking engine.
#[cfg(all(target_os = "macos", feature = "macos-kqueue"))]
fn try_build_kqueue_engine(
    listeners: Vec<TcpListener>,
    bound_addresses: &[SocketAddr],
    state: &AcceptLoopState<'_>,
) -> Result<Box<dyn AcceptEngine>, Vec<TcpListener>> {
    // Clone the listeners up front so a mid-registration failure can hand the
    // originals back to the fallback path untouched.
    let mut clones: Vec<TcpListener> = Vec::with_capacity(listeners.len());
    for listener in &listeners {
        match listener.try_clone() {
            Ok(clone) => clones.push(clone),
            Err(_) => return Err(listeners),
        }
    }
    match KqueueAcceptEngine::new(clones, bound_addresses, state.log_sink.clone()) {
        Ok(engine) => Ok(Box::new(engine)),
        Err(error) => {
            if let Some(log) = state.log_sink.as_ref() {
                let text =
                    format!("kqueue accept engine unavailable, using blocking accept: {error}");
                let message = rsync_info!(text).with_role(Role::Daemon);
                log_message(log, &message);
            }
            Err(listeners)
        }
    }
}

/// Builds the accept engine for the bound listener topology.
///
/// On macOS a `KqueueAcceptEngine` is tried first, falling back to
/// [`PollAcceptEngine`] if `kqueue(2)` setup fails. Every other topology - one
/// listener or several - uses [`PollAcceptEngine`], which polls them all from
/// the calling thread. The choice is made once here and never re-evaluated
/// inside the loop.
///
/// Both engines are single-threaded, so the accept path spawns no threads of
/// its own on any platform or listener count. [`PollAcceptEngine`] documents
/// why that is a correctness precondition rather than a preference.
fn build_accept_engine(
    listeners: Vec<TcpListener>,
    bound_addresses: &[SocketAddr],
    state: &AcceptLoopState<'_>,
) -> Result<Box<dyn AcceptEngine>, DaemonError> {
    #[cfg(all(target_os = "macos", feature = "macos-kqueue"))]
    let listeners = match try_build_kqueue_engine(listeners, bound_addresses, state) {
        Ok(engine) => return Ok(engine),
        Err(listeners) => listeners,
    };

    let engine = PollAcceptEngine::new(listeners, bound_addresses, state.log_sink.clone())?;
    Ok(Box::new(engine))
}

/// Drives the daemon accept loop over an [`AcceptEngine`].
///
/// The loop body is identical regardless of engine: check signal flags, poll
/// for the next connection, and dispatch it through the shared admission and
/// worker-spawn path. Polling cadence and readiness mechanism are entirely the
/// engine's concern.
fn run_accept_loop(
    engine: &mut dyn AcceptEngine,
    state: &mut AcceptLoopState<'_>,
) -> Result<(), DaemonError> {
    loop {
        if let Some(true) = check_signals_and_maintain(state) {
            break;
        }

        match engine.poll()? {
            AcceptOutcome::Connection(tcp_stream, raw_peer_addr) => {
                if handle_accepted_connection(tcp_stream, raw_peer_addr, state) {
                    break;
                }
            }
            AcceptOutcome::Idle => continue,
        }
    }

    engine.shutdown();
    Ok(())
}
