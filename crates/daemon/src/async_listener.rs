//! Hybrid async listener skeleton: tokio accept + sync workers via
//! [`tokio::task::spawn_blocking`].
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
//!   `std::net::TcpStream` (blocking mode) and handed to a closure running
//!   on the blocking pool via [`tokio::task::spawn_blocking`].
//! - The blocking closure is the caller-supplied sync worker; this module
//!   does not embed any knowledge of the daemon session state machine.
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

/// Sync per-connection worker invoked on the blocking pool.
///
/// The worker owns the converted blocking `TcpStream` for the lifetime of
/// the connection. Returning an `io::Result` lets the caller surface
/// transport errors without panicking; panics in the worker are caught by
/// the tokio join handle and logged by the dispatcher.
pub type SyncWorker = Arc<dyn Fn(TcpStream, SocketAddr) -> io::Result<()> + Send + Sync + 'static>;

/// Builds and runs the hybrid async listener.
///
/// `bind_addr` is bound via `tokio::net::TcpListener::bind`. Each accepted
/// stream is dispatched to `worker` through [`tokio::task::spawn_blocking`]
/// so the existing synchronous transfer machinery runs unchanged on the
/// blocking pool.
///
/// `worker_threads` caps the size of the tokio multi-thread runtime. The
/// hybrid model places the synchronous worker on the blocking pool, so the
/// worker count only governs the accept loop and per-connection async
/// dispatcher; a small bounded value is sufficient.
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

    runtime.block_on(async move { accept_loop(bind_addr, shutdown, worker).await })
}

async fn accept_loop(
    bind_addr: SocketAddr,
    shutdown: Arc<AtomicBool>,
    worker: SyncWorker,
) -> io::Result<()> {
    let listener = TokioTcpListener::bind(bind_addr).await?;

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

            let join = tokio::task::spawn_blocking(move || worker(std_stream, peer_addr)).await;
            match join {
                Ok(Ok(())) => {}
                Ok(Err(error)) => log_worker_error(peer_addr, &error),
                Err(join_error) => {
                    let error = io::Error::other(format!("worker join error: {join_error}"));
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
                accept_loop(local_addr, shutdown_for_loop, worker_for_loop).await
            })
        });

        // Brief settle while the loop reaches the first await.
        thread::sleep(Duration::from_millis(50));

        for _ in 0..3 {
            let _ = StdTcpStream::connect(local_addr).expect("connect");
        }

        // Allow the spawn_blocking dispatch to drain.
        thread::sleep(Duration::from_millis(200));

        shutdown.store(true, Ordering::Release);
        let result = server.join().expect("server thread");
        assert!(result.is_ok(), "accept loop error: {result:?}");
        assert!(invocations.load(Ordering::SeqCst) >= 1);
    }

    #[test]
    fn run_hybrid_listener_shuts_down_promptly() {
        let bind_addr = reserve_port();
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker: SyncWorker = Arc::new(|_, _| Ok(()));

        let shutdown_for_thread = Arc::clone(&shutdown);
        let worker_for_thread = Arc::clone(&worker);
        let handle = thread::spawn(move || {
            run_hybrid_listener(bind_addr, 1, shutdown_for_thread, worker_for_thread)
        });

        thread::sleep(Duration::from_millis(150));
        shutdown.store(true, Ordering::Release);

        let result = handle.join().expect("listener thread");
        assert!(result.is_ok(), "listener error: {result:?}");
    }
}
