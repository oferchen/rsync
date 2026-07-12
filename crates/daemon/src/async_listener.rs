//! Hybrid async listener skeleton: tokio accept + sync workers on a
//! dedicated OS thread per connection.
//!
//! This is the implementation slice tracked under issue #1935. It satisfies
//! the design recorded in
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
//!   next session and SIGSYS-kill its setup syscalls.
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
/// pool - which the accept loop used previously and which caps itself at 512
/// threads, queueing excess tasks - a raw thread-per-connection has no ceiling,
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
            // per-thread seccomp filter (LSM-SECCOMP) that is a one-way latch:
            // once armed, the thread traps every unlisted syscall for the rest
            // of its life. tokio's blocking pool reuses threads across tasks,
            // so a filter armed by one session would SIGSYS-kill the *next*
            // session's pre-seccomp setup syscalls (capability drop `capget`,
            // socket `FIONBIO` ioctl) - a flaky whole-process kill. A fresh
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

    use super::*;
    use std::io::Read;
    use std::net::{IpAddr, Ipv4Addr, TcpStream as StdTcpStream};
    use std::sync::atomic::AtomicUsize;
    use std::thread;
    use std::time::Duration;

    /// Reserves an ephemeral port by binding then dropping a std listener,
    /// returning the address. The window between drop and rebind is short
    /// enough that loopback test timing rarely loses the port.
    fn reserve_port() -> SocketAddr {
        let any = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
        let probe = std::net::TcpListener::bind(any).expect("probe bind");
        let addr = probe.local_addr().expect("probe addr");
        drop(probe);
        addr
    }

    #[test]
    fn binds_accepts_and_dispatches_worker() {
        let runtime = tokio::runtime::Runtime::new().expect("build runtime");
        let local_addr = reserve_port();

        let invocations = Arc::new(AtomicUsize::new(0));
        let shutdown = Arc::new(AtomicBool::new(false));

        let invocations_for_worker = Arc::clone(&invocations);
        let worker: SyncWorker = Arc::new(move |mut stream: TcpStream, _peer| {
            invocations_for_worker.fetch_add(1, Ordering::SeqCst);
            // Drain so the client sees an orderly close.
            let mut sink = Vec::new();
            let _ = stream.read_to_end(&mut sink);
            Ok(())
        });

        let shutdown_for_loop = Arc::clone(&shutdown);
        let worker_for_loop = Arc::clone(&worker);
        let server = thread::spawn(move || {
            runtime.block_on(async move {
                accept_loop(
                    local_addr,
                    DEFAULT_MAX_INFLIGHT_WORKERS,
                    shutdown_for_loop,
                    worker_for_loop,
                )
                .await
            })
        });

        // Brief settle while the loop reaches the first await.
        thread::sleep(Duration::from_millis(50));

        for _ in 0..3 {
            let _ = StdTcpStream::connect(local_addr).expect("connect");
        }

        // Allow the per-connection worker threads to drain.
        thread::sleep(Duration::from_millis(200));

        shutdown.store(true, Ordering::Release);
        let result = server.join().expect("server thread");
        assert!(result.is_ok(), "accept loop error: {result:?}");
        assert!(invocations.load(Ordering::SeqCst) >= 1);
    }

    #[test]
    fn worker_thread_cap_bounds_concurrency() {
        // With a cap of 2, at most two per-connection workers may run at once
        // even when more connections arrive together. Without the cap (raw
        // thread-per-connection) all four would overlap.
        let runtime = tokio::runtime::Runtime::new().expect("build runtime");
        let local_addr = reserve_port();

        let concurrent = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let total = Arc::new(AtomicUsize::new(0));
        let shutdown = Arc::new(AtomicBool::new(false));

        let concurrent_w = Arc::clone(&concurrent);
        let peak_w = Arc::clone(&peak);
        let total_w = Arc::clone(&total);
        let worker: SyncWorker = Arc::new(move |mut stream: TcpStream, _peer| {
            let now = concurrent_w.fetch_add(1, Ordering::SeqCst) + 1;
            peak_w.fetch_max(now, Ordering::SeqCst);
            // Hold the worker slot long enough that concurrent connections
            // would overlap absent the cap.
            thread::sleep(Duration::from_millis(150));
            let mut sink = Vec::new();
            let _ = stream.read_to_end(&mut sink);
            concurrent_w.fetch_sub(1, Ordering::SeqCst);
            total_w.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });

        let shutdown_for_loop = Arc::clone(&shutdown);
        let worker_for_loop = Arc::clone(&worker);
        let server = thread::spawn(move || {
            runtime.block_on(async move {
                accept_loop(local_addr, 2, shutdown_for_loop, worker_for_loop).await
            })
        });

        // Brief settle while the loop reaches the first await.
        thread::sleep(Duration::from_millis(50));

        // Fire four connections close together so workers would overlap.
        let mut clients = Vec::new();
        for _ in 0..4 {
            clients.push(StdTcpStream::connect(local_addr).expect("connect"));
        }
        thread::sleep(Duration::from_millis(50));
        drop(clients);

        // Allow all four to drain through the 2-permit gate.
        thread::sleep(Duration::from_millis(900));

        shutdown.store(true, Ordering::Release);
        let result = server.join().expect("server thread");
        assert!(result.is_ok(), "accept loop error: {result:?}");

        assert_eq!(
            total.load(Ordering::SeqCst),
            4,
            "all connections should drain"
        );
        let observed = peak.load(Ordering::SeqCst);
        assert!(observed >= 1, "at least one worker ran");
        assert!(
            observed <= 2,
            "cap should bound concurrency to 2, saw {observed}"
        );
    }

    #[test]
    fn run_hybrid_listener_shuts_down_promptly() {
        let bind_addr = reserve_port();
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker: SyncWorker = Arc::new(|_, _| Ok(()));

        let shutdown_for_thread = Arc::clone(&shutdown);
        let worker_for_thread = Arc::clone(&worker);
        let handle = thread::spawn(move || {
            run_hybrid_listener(
                bind_addr,
                1,
                DEFAULT_MAX_INFLIGHT_WORKERS,
                shutdown_for_thread,
                worker_for_thread,
            )
        });

        thread::sleep(Duration::from_millis(150));
        shutdown.store(true, Ordering::Release);

        let result = handle.join().expect("listener thread");
        assert!(result.is_ok(), "listener error: {result:?}");
    }
}
