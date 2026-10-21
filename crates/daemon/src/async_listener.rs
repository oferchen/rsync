//! Hybrid async listener skeleton: tokio accept + sync workers on a
//! dedicated OS thread per connection.
//!
//! It follows the design recorded in
//! `docs/design/daemon-tokio-async-listener-impl.md` and the runtime choice
//! captured in `docs/design/daemon-async-runtime-choice.md`: keep the
//! synchronous per-connection worker untouched while replacing the
//! `std::thread::spawn` accept primitive with a tokio multi-thread runtime
//! driving `tokio::net::TcpListener::accept`.
//!
//! # Status
//!
//! Skeleton, gated behind the opt-in `async-daemon` cargo feature. Default
//! builds remain tokio-free and continue to use the existing
//! `serve_connections` thread-per-connection loop. Production rollout
//! requires the trigger conditions documented in the runtime-choice ADR
//! (sustained >1k concurrent connections, blocking-pool starvation
//! measurements, two release cycles of green async-daemon CI).
//!
//! # Hybrid model
//!
//! - `tokio::runtime::Builder::new_multi_thread()` owns the accept loop.
//! - Each accepted `tokio::net::TcpStream` is converted back to
//!   `std::net::TcpStream` (blocking mode) and handed to the caller-supplied
//!   sync worker running on a dedicated OS thread (one per connection).
//! - A dedicated thread - rather than tokio's reused `spawn_blocking` pool -
//!   is required because the worker arms a per-thread seccomp filter that must
//!   die with the connection; a reused thread would leak the filter into the
//!   next session and fail its pre-seccomp setup syscalls with EPERM.
//! - The worker is the caller-supplied sync closure; this module does not
//!   embed any knowledge of the daemon session state machine.
//!
//! # Cross-platform
//!
//! Tokio supports Linux, macOS, and Windows. Stream conversion via
//! `into_std()` plus `set_nonblocking(false)` is identical across the three
//! targets; no `#[cfg]` gates are required inside this module.

use std::io;
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::net::TcpListener as TokioTcpListener;
use tokio::runtime::Builder;

/// Sync per-connection worker invoked on a dedicated OS thread.
///
/// The worker owns the converted blocking `TcpStream` for the lifetime of
/// the connection. Returning an `io::Result` lets the caller surface
/// transport errors without panicking. Each connection runs on its own
/// thread so a per-thread seccomp filter armed by the worker cannot leak
/// into a later session.
pub type SyncWorker = Arc<dyn Fn(TcpStream, SocketAddr) -> io::Result<()> + Send + Sync + 'static>;

/// Default upper bound on concurrent per-connection worker threads.
///
/// Each connection runs its sync worker on a dedicated OS thread (required for
/// the seccomp-filter lifetime invariant). Unlike tokio's `spawn_blocking`
/// pool - which caps itself at 512 threads, queueing excess tasks - a raw
/// thread-per-connection has no ceiling,
/// so a connect flood (e.g. slowloris) would spawn unbounded blocked-on-read
/// threads. This semaphore restores that backpressure: at the cap the accept
/// task parks until an in-flight worker completes, matching tokio's historical
/// blocking-pool default.
///
/// This is only the flood-protection floor used when the operator does not
/// configure a higher `max connections`. Callers derive the effective cap so
/// it never binds below a configured connection limit (see
/// [`run_hybrid_listener`]); otherwise a daemon with `max connections = 1000`
/// would be silently throttled to 512 concurrent sessions.
pub const DEFAULT_MAX_INFLIGHT_WORKERS: usize = 512;

/// Builds and runs the hybrid async listener.
///
/// `bind_addr` is bound via `tokio::net::TcpListener::bind`. Each accepted
/// stream is dispatched to `worker` on a dedicated OS thread so the existing
/// synchronous transfer machinery runs unchanged and any per-thread seccomp
/// filter it arms dies with the connection.
///
/// `worker_threads` caps the size of the tokio multi-thread runtime. The
/// hybrid model runs each synchronous worker on its own thread, so the
/// worker count only governs the accept loop and per-connection async
/// dispatcher; a small bounded value is sufficient.
///
/// `max_inflight` bounds the number of concurrent per-connection worker
/// threads. Callers pass a value derived from the operator's `max connections`
/// so the accept loop never throttles below the configured limit; a value of
/// `0` is clamped up to `1`. See [`DEFAULT_MAX_INFLIGHT_WORKERS`].
///
/// `shutdown` is polled between accepts. Setting it from another thread
/// drains the loop and returns `Ok(())`. The listener does not install
/// signal handlers; integration sites wire `SIGTERM`/`Ctrl-C` to this flag.
///
/// # Errors
///
/// Returns the underlying `io::Error` if the runtime cannot be built or the
/// listener cannot bind. Per-connection errors are reported through the
/// worker's own `io::Result` and never abort the accept loop.
pub fn run_hybrid_listener(
    bind_addr: SocketAddr,
    worker_threads: usize,
    max_inflight: usize,
    shutdown: Arc<AtomicBool>,
    worker: SyncWorker,
) -> io::Result<()> {
    let worker_threads = worker_threads.max(1);
    let runtime = Builder::new_multi_thread()
        .worker_threads(worker_threads)
        .enable_io()
        .enable_time()
        .thread_name("oc-rsyncd-async")
        .build()?;

    runtime.block_on(async move { accept_loop(bind_addr, max_inflight, shutdown, worker).await })
}

async fn accept_loop(
    bind_addr: SocketAddr,
    max_inflight: usize,
    shutdown: Arc<AtomicBool>,
    worker: SyncWorker,
) -> io::Result<()> {
    let listener = TokioTcpListener::bind(bind_addr).await?;
    let permits = Arc::new(tokio::sync::Semaphore::new(max_inflight.max(1)));
    accept_until_shutdown(listener, permits, shutdown, worker).await
}

/// Accepts on `listener` until `shutdown` is set, dispatching each connection
/// to `worker` on its own thread once one of `permits` is free.
async fn accept_until_shutdown(
    listener: TokioTcpListener,
    permits: Arc<tokio::sync::Semaphore>,
    shutdown: Arc<AtomicBool>,
    worker: SyncWorker,
) -> io::Result<()> {
    loop {
        if shutdown.load(Ordering::Acquire) {
            return Ok(());
        }

        // `accept().await` parks on the kernel event source; poll the
        // shutdown flag at a coarse interval so a stalled accept does not
        // delay graceful shutdown indefinitely.
        let accept =
            tokio::time::timeout(std::time::Duration::from_millis(250), listener.accept()).await;

        let (stream, peer_addr) = match accept {
            Ok(Ok(pair)) => pair,
            Ok(Err(error)) if error.kind() == io::ErrorKind::Interrupted => continue,
            Ok(Err(error)) => return Err(error),
            Err(_) => continue,
        };

        let worker = Arc::clone(&worker);
        let permits = Arc::clone(&permits);
        tokio::spawn(async move {
            // Convert the tokio stream back to a blocking std stream so the
            // existing sync worker can use `read`/`write` directly. The
            // `into_std()` + `set_nonblocking(false)` pair is the canonical
            // bridge documented for the hybrid pattern.
            let std_stream = match stream.into_std() {
                Ok(std_stream) => std_stream,
                Err(error) => {
                    log_dispatch_error(peer_addr, &error);
                    return;
                }
            };
            if let Err(error) = std_stream.set_nonblocking(false) {
                log_dispatch_error(peer_addr, &error);
                return;
            }

            // Bound the number of concurrent worker threads to `max_inflight`
            // (see `DEFAULT_MAX_INFLIGHT_WORKERS`). Acquiring here - after the cheap accept
            // but before the expensive `thread::spawn` - parks this dispatch
            // task when at capacity so a connect flood cannot spawn unbounded
            // blocked-on-read threads. The permit is released when this task
            // ends, i.e. once the worker thread has signalled completion.
            let _permit = match permits.acquire_owned().await {
                Ok(permit) => permit,
                // The semaphore is never closed in normal operation; treat a
                // closed semaphore as shutdown and drop the connection.
                Err(_) => return,
            };

            // Run the synchronous worker on a dedicated OS thread rather than
            // tokio's shared `spawn_blocking` pool. The worker installs a
            // per-thread seccomp filter that is a one-way latch:
            // once armed, the thread denies every unlisted syscall for the rest
            // of its life. tokio's blocking pool reuses threads across tasks,
            // so a filter armed by one session would fail the *next* session's
            // pre-seccomp setup syscalls (capability drop `capget`, socket
            // `FIONBIO` ioctl) with EPERM - a flaky broken session. A fresh
            // thread per connection keeps the design invariant documented in
            // `engage_seccomp_sandbox`: the filter dies with the disposable
            // worker thread and never leaks into another session.
            let (done_tx, done_rx) = tokio::sync::oneshot::channel();
            let spawn = std::thread::Builder::new()
                .name(String::from("oc-rsyncd-worker"))
                .spawn(move || {
                    let _ = done_tx.send(worker(std_stream, peer_addr));
                });
            if let Err(error) = spawn {
                log_dispatch_error(peer_addr, &error);
                return;
            }
            match done_rx.await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => log_worker_error(peer_addr, &error),
                Err(_recv) => {
                    let error = io::Error::other("worker thread terminated without result");
                    log_worker_error(peer_addr, &error);
                }
            }
        });
    }
}

fn log_dispatch_error(peer_addr: SocketAddr, error: &io::Error) {
    eprintln!(
        "async-daemon: failed to dispatch worker for {peer_addr}: {error} [daemon={}]",
        env!("CARGO_PKG_VERSION")
    );
}

fn log_worker_error(peer_addr: SocketAddr, error: &io::Error) {
    eprintln!(
        "async-daemon: worker for {peer_addr} returned error: {error} [daemon={}]",
        env!("CARGO_PKG_VERSION")
    );
}

#[cfg(test)]
mod tests {
    //! Note: tests build a runtime explicitly with
    //! `tokio::runtime::Runtime::new()` rather than using `#[tokio::test]`
    //! because the `tokio::test` macro expands `core::future::Future`, which
    //! collides with the local `core` crate that shadows the standard
    //! library.
    //!
    //! Each test binds its listener on port 0 before the loop starts, so a
    //! client connect queues in the backlog instead of racing the loop's bind,
    //! and every wait is a blocking receive on an event the worker sends.

    use super::*;
    use std::io::Read;
    use std::net::{IpAddr, Ipv4Addr, TcpListener as StdTcpListener, TcpStream as StdTcpStream};
    use std::sync::atomic::AtomicUsize;
    use std::sync::{Mutex, mpsc};
    use std::thread;
    use tokio::sync::Semaphore;

    fn loopback_any_port() -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)
    }

    /// Runs `accept_until_shutdown` on its own thread over a listener bound before it starts.
    fn spawn_server(
        permits: Arc<Semaphore>,
        shutdown: Arc<AtomicBool>,
        worker: SyncWorker,
    ) -> (SocketAddr, thread::JoinHandle<io::Result<()>>) {
        let listener = StdTcpListener::bind(loopback_any_port()).expect("bind");
        let addr = listener.local_addr().expect("local addr");
        listener.set_nonblocking(true).expect("nonblocking");
        let server = thread::spawn(move || {
            let runtime = tokio::runtime::Runtime::new().expect("build runtime");
            runtime.block_on(async move {
                accept_until_shutdown(
                    TokioTcpListener::from_std(listener)?,
                    permits,
                    shutdown,
                    worker,
                )
                .await
            })
        });
        (addr, server)
    }

    /// Drains `stream` to EOF, so a worker ends when its client hangs up.
    fn drain(mut stream: TcpStream) {
        let mut sink = Vec::new();
        let _ = stream.read_to_end(&mut sink);
    }

    #[test]
    fn binds_accepts_and_dispatches_worker() {
        const CLIENTS: usize = 3;
        let (done_tx, done_rx) = mpsc::channel();
        let done_tx = Mutex::new(done_tx);
        let worker: SyncWorker = Arc::new(move |stream: TcpStream, _peer| {
            drain(stream);
            done_tx
                .lock()
                .expect("done sender")
                .send(())
                .expect("report");
            Ok(())
        });
        let shutdown = Arc::new(AtomicBool::new(false));
        let permits = Arc::new(Semaphore::new(DEFAULT_MAX_INFLIGHT_WORKERS));
        let (addr, server) = spawn_server(permits, Arc::clone(&shutdown), worker);

        for _ in 0..CLIENTS {
            drop(StdTcpStream::connect(addr).expect("connect"));
        }
        for _ in 0..CLIENTS {
            done_rx.recv().expect("every connection reaches a worker");
        }

        shutdown.store(true, Ordering::Release);
        let result = server.join().expect("server thread");
        assert!(result.is_ok(), "accept loop error: {result:?}");
    }

    #[test]
    fn worker_thread_cap_bounds_concurrency() {
        // With a cap of 2, the first two workers hold both permits while they
        // wait to be released, so the other two connections stay parked on the
        // semaphore. Without the cap all four would run at once.
        const CAP: usize = 2;
        const CLIENTS: usize = 4;
        let concurrent = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let (done_tx, done_rx) = mpsc::channel();
        let started_tx = Mutex::new(started_tx);
        let release_rx = Mutex::new(release_rx);
        let done_tx = Mutex::new(done_tx);
        let (concurrent_w, peak_w) = (Arc::clone(&concurrent), Arc::clone(&peak));
        let worker: SyncWorker = Arc::new(move |stream: TcpStream, _peer| {
            let now = concurrent_w.fetch_add(1, Ordering::SeqCst) + 1;
            peak_w.fetch_max(now, Ordering::SeqCst);
            started_tx
                .lock()
                .expect("started sender")
                .send(())
                .expect("report");
            release_rx
                .lock()
                .expect("release receiver")
                .recv()
                .expect("release");
            drain(stream);
            concurrent_w.fetch_sub(1, Ordering::SeqCst);
            done_tx
                .lock()
                .expect("done sender")
                .send(())
                .expect("report");
            Ok(())
        });
        let shutdown = Arc::new(AtomicBool::new(false));
        let permits = Arc::new(Semaphore::new(CAP));
        let (addr, server) = spawn_server(Arc::clone(&permits), Arc::clone(&shutdown), worker);

        let clients: Vec<_> = (0..CLIENTS)
            .map(|_| StdTcpStream::connect(addr).expect("connect"))
            .collect();
        for _ in 0..CAP {
            started_rx.recv().expect("a worker starts");
        }
        assert_eq!(
            permits.available_permits(),
            0,
            "the running workers must hold every permit"
        );
        assert_eq!(peak.load(Ordering::SeqCst), CAP);

        drop(clients);
        for _ in 0..CLIENTS {
            release_tx.send(()).expect("release a worker");
        }
        for _ in 0..CLIENTS {
            done_rx.recv().expect("every connection drains");
        }

        shutdown.store(true, Ordering::Release);
        let result = server.join().expect("server thread");
        assert!(result.is_ok(), "accept loop error: {result:?}");
        let observed = peak.load(Ordering::SeqCst);
        assert!(
            observed <= CAP,
            "cap should bound concurrency to {CAP}, saw {observed}"
        );
    }

    #[test]
    fn run_hybrid_listener_shuts_down_promptly() {
        let shutdown = Arc::new(AtomicBool::new(true));
        let worker: SyncWorker = Arc::new(|_, _| Ok(()));
        let result = run_hybrid_listener(
            loopback_any_port(),
            1,
            DEFAULT_MAX_INFLIGHT_WORKERS,
            shutdown,
            worker,
        );
        assert!(result.is_ok(), "listener error: {result:?}");
    }
}
