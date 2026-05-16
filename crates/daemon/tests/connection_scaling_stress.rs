//! Stress benchmark for the daemon's thread-per-connection model.
//!
//! Measures how the current `std::thread::spawn`-per-connection listener
//! scales at 100, 1000, and 10000 concurrent client connections. The goal is
//! to provide quantitative evidence about whether an async listener (tracked
//! as issues #1935 / #1367) would deliver a meaningful improvement.
//!
//! Each scenario:
//!
//! 1. Spawns the daemon with an empty module table on a pre-bound ephemeral
//!    loopback port, injecting `SignalFlags` so the test can drive a clean
//!    shutdown after the flood.
//! 2. Launches `N` client threads. Each client opens a TCP connection, reads
//!    the `@RSYNCD: <ver>` greeting, sends `@RSYNCD: 32.0\n`, then closes
//!    the socket. This exercises the accept loop, thread spawn, handshake
//!    write, and worker join paths without doing any file transfer work.
//! 3. Records wall time, peak RSS (via `getrusage(RUSAGE_SELF).ru_maxrss`),
//!    success count, and refusal counters (`ECONNREFUSED` / `EMFILE` /
//!    generic).
//! 4. Sets `shutdown = true` on the daemon's signal flags so the accept
//!    loop exits and the worker drain completes.
//!
//! Because this is a multi-second stress harness (not a microbench), the
//! tests are marked `#[ignore]` and skipped by default. To run them
//! explicitly:
//!
//! ```text
//! cargo nextest run -p daemon --test connection_scaling_stress --run-ignored only -- --test-threads=1
//! # or with the default harness:
//! cargo test  -p daemon --test connection_scaling_stress --      --ignored --test-threads=1
//! ```
//!
//! The 10k case requires `RLIMIT_NOFILE` to be at least
//! `N * 2 + FD_HEADROOM` (the test bumps the soft limit up to the hard
//! limit when possible on Unix). When the limit cannot be raised high
//! enough, the 10k scenario is skipped with a clear message rather than
//! failing.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use daemon::{DaemonConfig, run_daemon};
use platform::signal::SignalFlags;

/// Per-connection deadline. Includes connect, greeting read, and the
/// daemon's worker setup latency under heavy concurrent load.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// Headroom on top of the requested connection count for the daemon's own
/// listener + log + control file descriptors. 256 is generous.
const FD_HEADROOM: u64 = 256;

/// Snapshot of one stress run.
#[derive(Debug, Clone)]
struct StressResult {
    label: &'static str,
    target_connections: u32,
    succeeded: u32,
    refused: u32,
    emfile: u32,
    other_errors: u32,
    wall: Duration,
    peak_rss_bytes: Option<u64>,
}

impl StressResult {
    fn print(&self) {
        let rss = self
            .peak_rss_bytes
            .map(|bytes| format!("{:.2} MiB", bytes as f64 / (1024.0 * 1024.0)))
            .unwrap_or_else(|| String::from("n/a"));
        let per_conn_us = if self.succeeded > 0 {
            self.wall.as_micros() as f64 / f64::from(self.succeeded)
        } else {
            0.0
        };
        eprintln!(
            "[daemon-stress] {label}: target={target} ok={ok} refused={ref_} emfile={emfile} \
             other={other} wall={wall_ms:.1} ms ({us_per:.1} us/conn) peak_rss={rss}",
            label = self.label,
            target = self.target_connections,
            ok = self.succeeded,
            ref_ = self.refused,
            emfile = self.emfile,
            other = self.other_errors,
            wall_ms = self.wall.as_secs_f64() * 1000.0,
            us_per = per_conn_us,
            rss = rss,
        );
    }
}

/// Allocates an ephemeral loopback listener and returns its port plus the
/// listener itself (passed to the daemon via `pre_bound_listener`).
fn allocate_listener() -> (u16, TcpListener) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind ephemeral");
    let port = listener.local_addr().expect("local addr").port();
    (port, listener)
}

/// Spawns the daemon on an injected listener using injected signal flags.
///
/// Returns the join handle together with the shared signal flags. The
/// caller sets `flags.shutdown = true` once the flood finishes; the
/// accept loop notices on its next poll and drains its workers, so the
/// daemon thread exits cleanly even if some of the attempted client
/// connections were refused at the kernel level (which would otherwise
/// leave `--max-sessions` waiting forever for the missing accepts).
fn spawn_daemon(
    listener: TcpListener,
    port: u16,
) -> (
    thread::JoinHandle<Result<(), daemon::DaemonError>>,
    SignalFlags,
) {
    let flags = SignalFlags::new();
    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            "--no-detach".to_string(),
            "--port".to_string(),
            port.to_string(),
        ])
        .pre_bound_listener(listener)
        .signal_flags(flags.clone())
        .build();
    let handle = thread::spawn(move || run_daemon(config));
    (handle, flags)
}

/// One client iteration: connect, read the greeting line, send a matching
/// version reply, then drop the stream.
///
/// The daemon's session handler completes on its own once the client
/// disconnects (the read side returns EOF or the writer hits a broken
/// pipe). This is the minimum amount of protocol traffic needed to make
/// the accept-loop hand the socket off to a real worker thread.
fn run_one_client(target: SocketAddr) -> ConnectionOutcome {
    let stream = match TcpStream::connect_timeout(&target, CONNECT_TIMEOUT) {
        Ok(stream) => stream,
        Err(error) => return classify_error(&error),
    };
    if let Err(error) = stream.set_read_timeout(Some(CONNECT_TIMEOUT)) {
        return classify_error(&error);
    }
    if let Err(error) = stream.set_write_timeout(Some(CONNECT_TIMEOUT)) {
        return classify_error(&error);
    }

    let mut reader = match stream.try_clone() {
        Ok(clone) => BufReader::new(clone),
        Err(error) => return classify_error(&error),
    };

    let mut greeting = String::new();
    match reader.read_line(&mut greeting) {
        Ok(0) => return ConnectionOutcome::Other,
        Err(error) => return classify_error(&error),
        Ok(_) => {}
    }
    if !greeting.starts_with("@RSYNCD:") {
        return ConnectionOutcome::Other;
    }

    let mut writer = stream;
    if writer.write_all(b"@RSYNCD: 32.0\n").is_err() {
        // The handshake reached the daemon thread; a broken pipe at this
        // point still counts as a successful accept.
        return ConnectionOutcome::Ok;
    }
    let _ = writer.flush();

    // Drain a small amount from the socket to ensure the daemon writes
    // back before we drop. Ignore the result: success here is defined as
    // the accept + greeting round-trip, not the full handshake outcome.
    let mut scratch = [0u8; 64];
    let _ = writer.read(&mut scratch);

    ConnectionOutcome::Ok
}

#[derive(Debug, Clone, Copy)]
enum ConnectionOutcome {
    Ok,
    Refused,
    Emfile,
    Other,
}

fn classify_error(error: &std::io::Error) -> ConnectionOutcome {
    use std::io::ErrorKind;
    if error.kind() == ErrorKind::ConnectionRefused {
        return ConnectionOutcome::Refused;
    }
    // EMFILE / ENFILE surface as raw_os_error on Unix.
    #[cfg(unix)]
    if let Some(code) = error.raw_os_error()
        && (code == libc::EMFILE || code == libc::ENFILE)
    {
        return ConnectionOutcome::Emfile;
    }
    ConnectionOutcome::Other
}

/// Samples `getrusage` every 25 ms from a background thread and returns
/// the peak resident set size in bytes observed while the stop flag was
/// clear. `ru_maxrss` itself is monotonic, but sampling keeps the result
/// scoped to the stress window for unattended runs.
fn start_rss_sampler(stop: Arc<AtomicU32>) -> thread::JoinHandle<Option<u64>> {
    thread::spawn(move || {
        let mut peak: Option<u64> = current_rss_bytes();
        while stop.load(Ordering::Relaxed) == 0 {
            if let Some(now) = current_rss_bytes() {
                peak = Some(peak.map_or(now, |p| p.max(now)));
            }
            thread::sleep(Duration::from_millis(25));
        }
        if let Some(now) = current_rss_bytes() {
            peak = Some(peak.map_or(now, |p| p.max(now)));
        }
        peak
    })
}

/// Returns the peak resident set size of the current process in bytes.
///
/// Uses `getrusage(RUSAGE_SELF).ru_maxrss` because it works on every Unix
/// without taking a dependency on Mach (the `mach_task_self` / `task_info`
/// path is deprecated in `libc`) and without parsing `/proc`. On Linux
/// `ru_maxrss` is reported in kilobytes; on macOS it is reported in
/// bytes (see `man getrusage`). The value is monotonically increasing
/// for the lifetime of the process, so the sampler still reports a peak
/// across the stress run by recording the largest observed value.
#[cfg(unix)]
fn current_rss_bytes() -> Option<u64> {
    // SAFETY: `getrusage` writes a fully-owned `rusage` struct and
    // returns 0 on success.
    #[allow(unsafe_code)]
    let (rc, usage) = unsafe {
        let mut usage: libc::rusage = std::mem::zeroed();
        let rc = libc::getrusage(libc::RUSAGE_SELF, &mut usage);
        (rc, usage)
    };
    if rc != 0 {
        return None;
    }
    let raw = i64::from(usage.ru_maxrss);
    if raw <= 0 {
        return None;
    }
    #[cfg(target_os = "macos")]
    let bytes = raw as u64;
    #[cfg(not(target_os = "macos"))]
    let bytes = (raw as u64).saturating_mul(1024);
    Some(bytes)
}

#[cfg(not(unix))]
fn current_rss_bytes() -> Option<u64> {
    // Windows lacks a direct `getrusage` equivalent and the stress test
    // already gates the 10k case to `#[cfg(unix)]`. The 100 / 1k cases
    // still run; they just report `peak_rss=n/a`.
    None
}

/// Returns the current `RLIMIT_NOFILE` soft and hard limits, attempting to
/// raise the soft limit to the hard limit so the test can open many
/// sockets concurrently.
#[cfg(unix)]
fn ensure_fd_capacity(required: u64) -> Result<u64, String> {
    // SAFETY: `getrlimit` and `setrlimit` are thread-safe POSIX calls.
    // The `rlim` struct is fully owned and sized correctly.
    #[allow(unsafe_code)]
    unsafe {
        let mut rlim: libc::rlimit = std::mem::zeroed();
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut rlim) != 0 {
            return Err(format!(
                "getrlimit(RLIMIT_NOFILE) failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        let hard = rlim.rlim_max as u64;
        let soft = rlim.rlim_cur as u64;
        if required <= soft {
            return Ok(soft);
        }
        if required > hard {
            return Err(format!(
                "RLIMIT_NOFILE hard limit too low: need {required}, hard={hard}, soft={soft}"
            ));
        }
        rlim.rlim_cur = required as libc::rlim_t;
        if libc::setrlimit(libc::RLIMIT_NOFILE, &rlim) != 0 {
            return Err(format!(
                "setrlimit(RLIMIT_NOFILE, {required}) failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(required)
    }
}

#[cfg(not(unix))]
fn ensure_fd_capacity(_required: u64) -> Result<u64, String> {
    // Windows uses a separate per-process socket handle table that is not
    // governed by `RLIMIT_NOFILE`. The default ceiling is high enough
    // (>16k) for the 10k case; skip the probe and trust the OS.
    Ok(u64::MAX)
}

/// Drives a single scenario end-to-end.
fn run_scenario(label: &'static str, target_connections: u32) -> StressResult {
    let required_fds = u64::from(target_connections) * 2 + FD_HEADROOM;
    if let Err(message) = ensure_fd_capacity(required_fds) {
        eprintln!("[daemon-stress] {label}: skipping (insufficient file descriptors: {message})");
        return StressResult {
            label,
            target_connections,
            succeeded: 0,
            refused: 0,
            emfile: 0,
            other_errors: target_connections,
            wall: Duration::ZERO,
            peak_rss_bytes: None,
        };
    }

    let (port, listener) = allocate_listener();
    let target = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let (daemon_handle, daemon_flags) = spawn_daemon(listener, port);

    // Wait briefly so the daemon has time to enter its accept loop before
    // the flood starts. Without this, the first batch of connect attempts
    // races against the listener `set_nonblocking` call and reports
    // `ECONNREFUSED` purely from kernel-side backlog overflow.
    thread::sleep(Duration::from_millis(50));

    let stop_sampler = Arc::new(AtomicU32::new(0));
    let sampler = start_rss_sampler(Arc::clone(&stop_sampler));

    let succeeded = Arc::new(AtomicU32::new(0));
    let refused = Arc::new(AtomicU32::new(0));
    let emfile = Arc::new(AtomicU32::new(0));
    let other = Arc::new(AtomicU32::new(0));

    let start = Instant::now();
    let mut clients = Vec::with_capacity(target_connections as usize);
    for _ in 0..target_connections {
        let succeeded = Arc::clone(&succeeded);
        let refused = Arc::clone(&refused);
        let emfile = Arc::clone(&emfile);
        let other = Arc::clone(&other);
        clients.push(thread::spawn(move || match run_one_client(target) {
            ConnectionOutcome::Ok => {
                succeeded.fetch_add(1, Ordering::Relaxed);
            }
            ConnectionOutcome::Refused => {
                refused.fetch_add(1, Ordering::Relaxed);
            }
            ConnectionOutcome::Emfile => {
                emfile.fetch_add(1, Ordering::Relaxed);
            }
            ConnectionOutcome::Other => {
                other.fetch_add(1, Ordering::Relaxed);
            }
        }));
    }
    for client in clients {
        let _ = client.join();
    }
    let wall = start.elapsed();

    stop_sampler.store(1, Ordering::Relaxed);
    let peak_rss_bytes = sampler.join().ok().flatten();

    // Signal the daemon to stop accepting and drain. This terminates
    // even when some attempted connections were refused before reaching
    // the accept loop, which would otherwise stall a `--max-sessions`
    // teardown.
    daemon_flags.shutdown.store(true, Ordering::Relaxed);
    let _ = daemon_handle.join();

    let result = StressResult {
        label,
        target_connections,
        succeeded: succeeded.load(Ordering::Relaxed),
        refused: refused.load(Ordering::Relaxed),
        emfile: emfile.load(Ordering::Relaxed),
        other_errors: other.load(Ordering::Relaxed),
        wall,
        peak_rss_bytes,
    };
    result.print();
    result
}

#[test]
#[ignore = "stress benchmark; run with --ignored. Multi-second runtime."]
fn thread_per_connection_scaling_100() {
    let result = run_scenario("100_conn", 100);
    assert!(
        result.succeeded + result.refused + result.emfile + result.other_errors
            == result.target_connections,
        "scenario must account for every attempted connection"
    );
    assert!(
        result.succeeded > 0,
        "at least one connection should succeed at N=100"
    );
}

#[test]
#[ignore = "stress benchmark; run with --ignored. Multi-second runtime."]
fn thread_per_connection_scaling_1000() {
    let result = run_scenario("1k_conn", 1_000);
    assert!(
        result.succeeded + result.refused + result.emfile + result.other_errors
            == result.target_connections,
        "scenario must account for every attempted connection"
    );
    assert!(
        result.succeeded > 0,
        "at least one connection should succeed at N=1000"
    );
}

#[test]
#[ignore = "stress benchmark; run with --ignored. Requires high RLIMIT_NOFILE."]
#[cfg(unix)]
fn thread_per_connection_scaling_10000() {
    // The scenario internally skips (with a logged message) when the
    // hard limit is too low to honour the request; that path is the
    // expected outcome on developer laptops with the default 1024
    // soft / 2560 hard limits on macOS and 1024 / 1048576 on most
    // Linux distros.
    let result = run_scenario("10k_conn", 10_000);
    let accounted = result.succeeded + result.refused + result.emfile + result.other_errors;
    assert!(
        accounted == result.target_connections,
        "scenario must account for every attempted connection (got {accounted})"
    );
}
