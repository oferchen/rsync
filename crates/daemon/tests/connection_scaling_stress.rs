//! Stress benchmark for the daemon's thread-per-connection model.
//!
//! Measures how the current `std::thread::spawn`-per-connection listener
//! scales at 100, 1000, 5000, and 10000 concurrent client connections. The
//! goal is to provide quantitative evidence about whether an async listener
//! (tracked as issues #1935 / #1367) would deliver a meaningful improvement.
//!
//! Each scenario:
//!
//! 1. Spawns the daemon with an empty module table on a pre-bound ephemeral
//!    loopback port, injecting `SignalFlags` so the test can drive a clean
//!    shutdown after the flood.
//! 2. Launches `N` client threads (in batches of `CLIENT_BATCH_SIZE` for
//!    higher connection counts to avoid client-side thread exhaustion).
//!    Each client opens a TCP connection, reads the `@RSYNCD: <ver>`
//!    greeting, sends `@RSYNCD: 32.0\n`, then closes the socket. This
//!    exercises the accept loop, thread spawn, handshake write, and worker
//!    join paths without doing any file transfer work.
//! 3. Records wall time, peak RSS (via `getrusage(RUSAGE_SELF).ru_maxrss`),
//!    baseline RSS (before the flood), per-connection latencies with
//!    percentile breakdown (p50/p95/p99), success count, and refusal
//!    counters (`ECONNREFUSED` / `EMFILE` / generic).
//! 4. Computes per-connection RSS overhead and thread-stack memory
//!    estimates based on the platform's default stack size.
//! 5. Sets `shutdown = true` on the daemon's signal flags so the accept
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
//! The 5k and 10k cases require `RLIMIT_NOFILE` to be at least
//! `N * 2 + FD_HEADROOM` (the test bumps the soft limit up to the hard
//! limit when possible on Unix). When the limit cannot be raised high
//! enough, those scenarios are skipped with a clear message rather than
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
/// Maximum number of client threads launched concurrently per batch.
/// Higher connection counts are split into batches of this size to avoid
/// exhausting the client-side thread pool (each client thread holds a
/// socket fd plus stack memory). Using 2000 keeps the client-side fd
/// pressure manageable while still producing a realistic burst.
const CLIENT_BATCH_SIZE: u32 = 2_000;

/// Default thread stack size in bytes. `std::thread::spawn` uses the
/// platform default: 8 MiB on Linux, 512 KiB on macOS, 1 MiB on Windows.
/// Used to estimate total thread-stack memory at a given connection count.
#[cfg(target_os = "linux")]
const DEFAULT_THREAD_STACK_BYTES: u64 = 8 * 1024 * 1024;
#[cfg(target_os = "macos")]
const DEFAULT_THREAD_STACK_BYTES: u64 = 512 * 1024;
#[cfg(windows)]
const DEFAULT_THREAD_STACK_BYTES: u64 = 1024 * 1024;
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
const DEFAULT_THREAD_STACK_BYTES: u64 = 8 * 1024 * 1024;

/// Latency percentiles computed from per-connection timing samples.
#[derive(Debug, Clone, Default)]
struct LatencyPercentiles {
    p50_us: u64,
    p95_us: u64,
    p99_us: u64,
    max_us: u64,
    min_us: u64,
    sample_count: usize,
}

impl LatencyPercentiles {
    /// Computes percentiles from a mutable slice of microsecond latencies.
    /// The slice is sorted in place. Returns `None` if the slice is empty.
    fn from_samples(samples: &mut [u64]) -> Option<Self> {
        if samples.is_empty() {
            return None;
        }
        samples.sort_unstable();
        let n = samples.len();
        Some(Self {
            p50_us: samples[n / 2],
            p95_us: samples[(n as f64 * 0.95) as usize],
            p99_us: samples[(n as f64 * 0.99) as usize],
            max_us: samples[n - 1],
            min_us: samples[0],
            sample_count: n,
        })
    }

    fn print(&self) {
        eprintln!(
            "  latency: p50={p50} us  p95={p95} us  p99={p99} us  \
             min={min} us  max={max} us  (n={n})",
            p50 = self.p50_us,
            p95 = self.p95_us,
            p99 = self.p99_us,
            min = self.min_us,
            max = self.max_us,
            n = self.sample_count,
        );
    }
}

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
    baseline_rss_bytes: Option<u64>,
    peak_rss_bytes: Option<u64>,
    latency: Option<LatencyPercentiles>,
}

impl StressResult {
    fn print(&self) {
        let peak_rss = self
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
            rss = peak_rss,
        );
        self.print_rss_overhead();
        self.print_thread_stack_estimate();
        if let Some(ref latency) = self.latency {
            latency.print();
        }
    }

    /// Reports per-connection RSS overhead derived from the delta between
    /// baseline and peak RSS divided by the number of successful connections.
    fn print_rss_overhead(&self) {
        let (Some(baseline), Some(peak)) = (self.baseline_rss_bytes, self.peak_rss_bytes) else {
            return;
        };
        let delta = peak.saturating_sub(baseline);
        if self.succeeded == 0 {
            return;
        }
        let per_conn_bytes = delta as f64 / f64::from(self.succeeded);
        let per_conn_kib = per_conn_bytes / 1024.0;
        eprintln!(
            "  rss overhead: baseline={baseline:.2} MiB  peak={peak:.2} MiB  \
             delta={delta:.2} MiB  per_conn={per_conn:.1} KiB",
            baseline = baseline as f64 / (1024.0 * 1024.0),
            peak = peak as f64 / (1024.0 * 1024.0),
            delta = delta as f64 / (1024.0 * 1024.0),
            per_conn = per_conn_kib,
        );
    }

    /// Estimates total thread-stack reservation based on the platform's
    /// default stack size and the number of successful connections. The
    /// daemon spawns one OS thread per accepted connection, each reserving
    /// the default stack (8 MiB on Linux, 512 KiB on macOS, 1 MiB on
    /// Windows).
    fn print_thread_stack_estimate(&self) {
        if self.succeeded == 0 {
            return;
        }
        let total_stack = u64::from(self.succeeded) * DEFAULT_THREAD_STACK_BYTES;
        let stack_mib = total_stack as f64 / (1024.0 * 1024.0);
        let stack_gib = total_stack as f64 / (1024.0 * 1024.0 * 1024.0);
        eprintln!(
            "  thread stacks: {conns} threads x {stack_kib} KiB/stack = \
             {total_mib:.1} MiB ({total_gib:.2} GiB) reserved",
            conns = self.succeeded,
            stack_kib = DEFAULT_THREAD_STACK_BYTES / 1024,
            total_mib = stack_mib,
            total_gib = stack_gib,
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
///
/// Returns the outcome and, on success, the round-trip latency in
/// microseconds (from connect attempt to greeting receipt).
fn run_one_client(target: SocketAddr) -> (ConnectionOutcome, Option<u64>) {
    let t0 = Instant::now();
    let stream = match TcpStream::connect_timeout(&target, CONNECT_TIMEOUT) {
        Ok(stream) => stream,
        Err(error) => return (classify_error(&error), None),
    };
    if let Err(error) = stream.set_read_timeout(Some(CONNECT_TIMEOUT)) {
        return (classify_error(&error), None);
    }
    if let Err(error) = stream.set_write_timeout(Some(CONNECT_TIMEOUT)) {
        return (classify_error(&error), None);
    }

    let mut reader = match stream.try_clone() {
        Ok(clone) => BufReader::new(clone),
        Err(error) => return (classify_error(&error), None),
    };

    let mut greeting = String::new();
    match reader.read_line(&mut greeting) {
        Ok(0) => return (ConnectionOutcome::Other, None),
        Err(error) => return (classify_error(&error), None),
        Ok(_) => {}
    }
    if !greeting.starts_with("@RSYNCD:") {
        return (ConnectionOutcome::Other, None);
    }

    let latency_us = t0.elapsed().as_micros() as u64;

    let mut writer = stream;
    if writer.write_all(b"@RSYNCD: 32.0\n").is_err() {
        // The handshake reached the daemon thread; a broken pipe at this
        // point still counts as a successful accept.
        return (ConnectionOutcome::Ok, Some(latency_us));
    }
    let _ = writer.flush();

    // Drain a small amount from the socket to ensure the daemon writes
    // back before we drop. Ignore the result: success here is defined as
    // the accept + greeting round-trip, not the full handshake outcome.
    let mut scratch = [0u8; 64];
    let _ = writer.read(&mut scratch);

    (ConnectionOutcome::Ok, Some(latency_us))
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
    // EMFILE / ENFILE surface as raw_os_error on Unix. On Windows the
    // equivalents are WSAEMFILE (10024) for socket() exhaustion and
    // ERROR_TOO_MANY_OPEN_FILES (4) for general handle exhaustion.
    if let Some(code) = error.raw_os_error() {
        #[cfg(unix)]
        if code == libc::EMFILE || code == libc::ENFILE {
            return ConnectionOutcome::Emfile;
        }
        #[cfg(windows)]
        if code == 10024 || code == 4 {
            return ConnectionOutcome::Emfile;
        }
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
    let raw = usage.ru_maxrss;
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
///
/// For large connection counts (above `CLIENT_BATCH_SIZE`), clients are
/// launched in batches to prevent the client side from exhausting its own
/// thread and fd resources before the daemon has processed earlier waves.
/// Each batch runs concurrently; batches are joined before the next wave
/// starts.
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
            baseline_rss_bytes: None,
            peak_rss_bytes: None,
            latency: None,
        };
    }

    // Capture baseline RSS before spawning the daemon.
    let baseline_rss_bytes = current_rss_bytes();

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

    // Collect per-connection latencies from successful handshakes.
    let latencies = Arc::new(std::sync::Mutex::new(Vec::with_capacity(
        target_connections as usize,
    )));

    let start = Instant::now();
    let mut remaining = target_connections;
    while remaining > 0 {
        let batch_size = remaining.min(CLIENT_BATCH_SIZE);
        remaining -= batch_size;

        let mut clients = Vec::with_capacity(batch_size as usize);
        for _ in 0..batch_size {
            let succeeded = Arc::clone(&succeeded);
            let refused = Arc::clone(&refused);
            let emfile = Arc::clone(&emfile);
            let other = Arc::clone(&other);
            let latencies = Arc::clone(&latencies);
            clients.push(thread::spawn(move || {
                let (outcome, latency_us) = run_one_client(target);
                match outcome {
                    ConnectionOutcome::Ok => {
                        succeeded.fetch_add(1, Ordering::Relaxed);
                        if let Some(us) = latency_us {
                            latencies.lock().expect("latency lock").push(us);
                        }
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
                }
            }));
        }
        for client in clients {
            let _ = client.join();
        }
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

    let mut latency_samples = latencies.lock().expect("latency lock").clone();
    let latency = LatencyPercentiles::from_samples(&mut latency_samples);

    let result = StressResult {
        label,
        target_connections,
        succeeded: succeeded.load(Ordering::Relaxed),
        refused: refused.load(Ordering::Relaxed),
        emfile: emfile.load(Ordering::Relaxed),
        other_errors: other.load(Ordering::Relaxed),
        wall,
        baseline_rss_bytes,
        peak_rss_bytes,
        latency,
    };
    result.print();
    result
}

#[test]
#[ignore = "stress benchmark; run with --ignored. Multi-second runtime."]
fn thread_per_connection_scaling_100() {
    let result = run_scenario("100_conn", 100);
    assert_all_accounted(&result);
    assert!(
        result.succeeded > 0,
        "at least one connection should succeed at N=100"
    );
}

#[test]
#[ignore = "stress benchmark; run with --ignored. Multi-second runtime."]
fn thread_per_connection_scaling_1000() {
    let result = run_scenario("1k_conn", 1_000);
    assert_all_accounted(&result);
    assert!(
        result.succeeded > 0,
        "at least one connection should succeed at N=1000"
    );
}

#[test]
#[ignore = "stress benchmark; run with --ignored. Requires high RLIMIT_NOFILE."]
#[cfg(unix)]
fn thread_per_connection_scaling_5000() {
    let result = run_scenario("5k_conn", 5_000);
    assert_all_accounted(&result);
    // At 5K we expect most connections to succeed if the fd limit is high
    // enough. When the test was skipped internally (fd limit too low), all
    // connections are classified as other_errors.
    if result.peak_rss_bytes.is_some() {
        assert!(
            result.succeeded > 0,
            "at least one connection should succeed at N=5000 when fd limits allow"
        );
    }
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
    assert_all_accounted(&result);
}

/// Runs the full scaling ladder (1K, 5K, 10K) in a single test and prints
/// a comparative summary table. Useful for a single-invocation report that
/// shows per-connection overhead scaling across all tiers.
#[test]
#[ignore = "full scaling ladder; run with --ignored. Multi-second runtime."]
#[cfg(unix)]
fn thread_per_connection_scaling_ladder() {
    let r_1k = run_scenario("ladder_1k", 1_000);
    let r_5k = run_scenario("ladder_5k", 5_000);
    let r_10k = run_scenario("ladder_10k", 10_000);

    eprintln!();
    eprintln!("[daemon-stress] === Connection Scaling Summary ===");
    eprintln!(
        "{:<12} {:>8} {:>8} {:>10} {:>10} {:>12} {:>12}",
        "tier", "target", "ok", "wall_ms", "p99_us", "peak_MiB", "stack_MiB"
    );
    for r in [&r_1k, &r_5k, &r_10k] {
        let p99 = r
            .latency
            .as_ref()
            .map(|l| l.p99_us.to_string())
            .unwrap_or_else(|| "n/a".to_string());
        let peak = r
            .peak_rss_bytes
            .map(|b| format!("{:.1}", b as f64 / (1024.0 * 1024.0)))
            .unwrap_or_else(|| "n/a".to_string());
        let stack = if r.succeeded > 0 {
            let total = u64::from(r.succeeded) * DEFAULT_THREAD_STACK_BYTES;
            format!("{:.1}", total as f64 / (1024.0 * 1024.0))
        } else {
            "n/a".to_string()
        };
        eprintln!(
            "{:<12} {:>8} {:>8} {:>10.1} {:>10} {:>12} {:>12}",
            r.label,
            r.target_connections,
            r.succeeded,
            r.wall.as_secs_f64() * 1000.0,
            p99,
            peak,
            stack,
        );
    }
    eprintln!("[daemon-stress] === End Summary ===");
    eprintln!();

    // Validate accounting for all tiers that ran.
    for r in [&r_1k, &r_5k, &r_10k] {
        assert_all_accounted(r);
    }
}

/// Profiles thread-stack overhead at the 10K tier. Computes the ratio of
/// estimated stack reservation to observed peak RSS, which indicates how
/// much of the process's memory footprint is attributable to thread stacks
/// versus heap allocations. On Linux (8 MiB default stack), 10K threads
/// reserve ~80 GiB of virtual address space; actual RSS depends on how
/// many stack pages each thread touches.
#[test]
#[ignore = "thread-stack profiling; run with --ignored. Requires high RLIMIT_NOFILE."]
#[cfg(unix)]
fn thread_stack_overhead_10k() {
    let result = run_scenario("stack_10k", 10_000);
    assert_all_accounted(&result);

    if result.succeeded > 0 {
        let total_stack_reserved = u64::from(result.succeeded) * DEFAULT_THREAD_STACK_BYTES;
        let stack_reserved_mib = total_stack_reserved as f64 / (1024.0 * 1024.0);
        let stack_reserved_gib = total_stack_reserved as f64 / (1024.0 * 1024.0 * 1024.0);
        eprintln!();
        eprintln!("[daemon-stress] === Thread-Stack Overhead Profile (10K) ===");
        eprintln!("  threads spawned:        {}", result.succeeded);
        eprintln!(
            "  per-thread stack:       {} KiB",
            DEFAULT_THREAD_STACK_BYTES / 1024
        );
        eprintln!(
            "  total stack reserved:   {stack_reserved_mib:.1} MiB ({stack_reserved_gib:.2} GiB)"
        );

        if let (Some(baseline), Some(peak)) = (result.baseline_rss_bytes, result.peak_rss_bytes) {
            let rss_delta = peak.saturating_sub(baseline);
            let rss_delta_mib = rss_delta as f64 / (1024.0 * 1024.0);
            let per_thread_rss = rss_delta as f64 / f64::from(result.succeeded);
            let per_thread_rss_kib = per_thread_rss / 1024.0;

            // Touched stack pages: the portion of the reserved stack that
            // the OS has actually faulted in (RSS delta / thread count).
            // This is typically 8-64 KiB per thread for the daemon's
            // greeting handler, far below the full 8 MiB reservation.
            let touched_pct = if DEFAULT_THREAD_STACK_BYTES > 0 {
                (per_thread_rss / DEFAULT_THREAD_STACK_BYTES as f64) * 100.0
            } else {
                0.0
            };

            eprintln!("  RSS delta (peak-base):  {rss_delta_mib:.2} MiB");
            eprintln!(
                "  per-thread RSS:         {per_thread_rss_kib:.1} KiB \
                 ({touched_pct:.2}% of reserved stack)"
            );
            eprintln!(
                "  stack utilization:      each thread touches ~{touched_pct:.1}% \
                 of its {stack_kib} KiB reservation",
                stack_kib = DEFAULT_THREAD_STACK_BYTES / 1024,
            );
        } else {
            eprintln!("  RSS data:               n/a (platform does not support getrusage)");
        }

        if let Some(ref latency) = result.latency {
            eprintln!(
                "  handshake latency p99:  {} us ({:.2} ms)",
                latency.p99_us,
                latency.p99_us as f64 / 1000.0,
            );
        }
        eprintln!("[daemon-stress] === End Profile ===");
        eprintln!();
    }
}

/// Asserts that every attempted connection is accounted for in one of the
/// outcome buckets.
fn assert_all_accounted(result: &StressResult) {
    let accounted = result.succeeded + result.refused + result.emfile + result.other_errors;
    assert!(
        accounted == result.target_connections,
        "scenario must account for every attempted connection \
         (target={target}, accounted={accounted})",
        target = result.target_connections,
    );
}
