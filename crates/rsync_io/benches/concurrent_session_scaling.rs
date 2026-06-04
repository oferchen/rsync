//! Concurrent session scaling benchmark for the `spawn_blocking` bridge.
//!
//! Measures the throughput and latency characteristics of N concurrent
//! sessions passing through the `tokio::task::spawn_blocking` bridge -
//! the same hybrid async/sync architecture used by the daemon's
//! `async_listener` to hand off accepted connections to synchronous
//! transfer workers.
//!
//! # Motivation
//!
//! The embedded SSH transport (russh) is async, but the rsync protocol
//! engine is synchronous. The production bridge runs each session's sync
//! transfer code inside a `spawn_blocking` task. Tokio's blocking pool
//! grows on demand up to a default of 512 threads, but thread creation
//! latency and OS scheduling overhead cause observable throughput
//! degradation at high concurrency. This harness quantifies the
//! saturation curve so the async-native migration path (RUSSH-9..15) can
//! set a concrete improvement target.
//!
//! # Bench cells
//!
//! Four concurrency levels are tested: 64, 128, 256, and 512 sessions.
//! Each session performs a fixed-size loopback transfer through the
//! `spawn_blocking` bridge, simulating the workload of a daemon serving
//! that many concurrent rsync connections.
//!
//! Metrics collected per bench iteration:
//! - **Aggregate throughput**: total bytes / wall time across all sessions
//! - **Tail latency**: p50, p95, p99 of per-session completion time
//! - **Thread saturation**: peak blocking thread count vs configured max
//!
//! # Approach
//!
//! Real SSH is not portable for benchmarks (no `sshd` on CI), so each
//! session uses a loopback TCP pair. The sync worker reads a fixed payload
//! from its TCP stream, simulating the receive side of a transfer. The
//! overhead measured is purely the `spawn_blocking` dispatch, thread pool
//! contention, and OS scheduling - which is exactly the ceiling this bench
//! is designed to quantify.
//!
//! Run with:
//!
//! ```text
//! cargo bench -p rsync_io --bench concurrent_session_scaling
//! ```

use std::hint::black_box;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use tokio::runtime::Builder;

/// Payload each session transfers. 64 KiB is large enough to exercise the
/// copy loop without dominating the benchmark wall clock at high N.
const SESSION_PAYLOAD: usize = 64 * 1024;

/// Read buffer size matching the daemon's typical I/O chunk.
const IO_BUF: usize = 32 * 1024;

/// Concurrency levels tested. Covers the range from comfortable (64) to
/// the tokio default `max_blocking_threads` limit (512).
const CONCURRENCY_LEVELS: &[usize] = &[64, 128, 256, 512];

/// Per-session latency record collected during a benchmark iteration.
#[derive(Clone, Copy)]
struct SessionLatency {
    duration: Duration,
}

/// Aggregate metrics from one benchmark iteration at a given concurrency.
struct IterationMetrics {
    /// Wall-clock duration for the entire iteration (all sessions).
    wall_time: Duration,
    /// Per-session latencies, sorted ascending.
    latencies: Vec<SessionLatency>,
    /// Peak observed thread count on the blocking pool.
    peak_threads: usize,
}

impl IterationMetrics {
    /// Returns the latency at the given percentile (0.0 - 1.0).
    fn percentile(&self, p: f64) -> Duration {
        if self.latencies.is_empty() {
            return Duration::ZERO;
        }
        let idx = ((self.latencies.len() as f64 * p).ceil() as usize)
            .saturating_sub(1)
            .min(self.latencies.len() - 1);
        self.latencies[idx].duration
    }
}

/// Runs a single benchmark iteration: launches `concurrency` sessions in
/// parallel through the `spawn_blocking` bridge and collects metrics.
///
/// The architecture mirrors `crates/daemon/src/async_listener.rs`:
/// - A tokio multi-thread runtime hosts the dispatcher.
/// - Each session is dispatched via `spawn_blocking`.
/// - The sync worker drains a TCP loopback payload.
fn run_concurrent_sessions(concurrency: usize) -> io::Result<IterationMetrics> {
    let thread_counter = Arc::new(AtomicUsize::new(0));
    let peak_counter = Arc::new(AtomicUsize::new(0));

    // Bind a listener per session. Collecting all addresses up front avoids
    // racing between bind and connect.
    let mut listeners = Vec::with_capacity(concurrency);
    let mut addrs = Vec::with_capacity(concurrency);
    for _ in 0..concurrency {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let addr = listener.local_addr()?;
        addrs.push(addr);
        listeners.push(listener);
    }

    // Pre-generate the payload once.
    let payload = vec![0xA5u8; SESSION_PAYLOAD];

    // Spawn sender threads that connect and push the payload. These run
    // outside the tokio runtime to model external clients.
    let sender_handles: Vec<_> = addrs
        .iter()
        .map(|&addr| {
            let payload = payload.clone();
            thread::spawn(move || -> io::Result<()> {
                let mut stream = TcpStream::connect(addr)?;
                stream.write_all(&payload)?;
                stream.flush()?;
                let _ = stream.shutdown(Shutdown::Write);
                Ok(())
            })
        })
        .collect();

    // Accept all connections before entering the timed section. This
    // isolates the benchmark from TCP handshake variability.
    let mut accepted: Vec<TcpStream> = Vec::with_capacity(concurrency);
    for listener in &listeners {
        let (stream, _) = listener.accept()?;
        accepted.push(stream);
    }
    drop(listeners);

    // Build the tokio runtime that drives the spawn_blocking dispatch.
    // Use a small worker count (2) to model the production daemon where
    // async workers are few and blocking workers are many.
    let runtime = Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(concurrency.max(4))
        .enable_all()
        .thread_name("bench-async")
        .build()
        .map_err(|e| io::Error::other(format!("runtime build: {e}")))?;

    let wall_start = Instant::now();

    // Dispatch all sessions through spawn_blocking, collecting join
    // handles for per-session latency measurement.
    let handles: Vec<_> = accepted
        .into_iter()
        .map(|stream| {
            let tc = Arc::clone(&thread_counter);
            let pc = Arc::clone(&peak_counter);
            runtime.spawn(async move {
                tokio::task::spawn_blocking(move || -> io::Result<SessionLatency> {
                    // Track thread pool occupancy.
                    let current = tc.fetch_add(1, Ordering::SeqCst) + 1;
                    pc.fetch_max(current, Ordering::SeqCst);

                    let session_start = Instant::now();

                    // Sync worker: drain the payload from the TCP stream.
                    let mut stream = stream;
                    let mut buf = vec![0u8; IO_BUF];
                    let mut total = 0usize;
                    loop {
                        let n = stream.read(&mut buf)?;
                        if n == 0 {
                            break;
                        }
                        total += n;
                        black_box(&buf[..n]);
                    }

                    tc.fetch_sub(1, Ordering::SeqCst);

                    assert_eq!(
                        total, SESSION_PAYLOAD,
                        "session received {total} bytes, expected {SESSION_PAYLOAD}"
                    );

                    Ok(SessionLatency {
                        duration: session_start.elapsed(),
                    })
                })
                .await
            })
        })
        .collect();

    // Collect all session results.
    let results: Vec<SessionLatency> = runtime.block_on(async {
        let mut latencies = Vec::with_capacity(handles.len());
        for handle in handles {
            let result = handle
                .await
                .map_err(|e| io::Error::other(format!("tokio join: {e}")))?
                .map_err(|e| io::Error::other(format!("spawn_blocking join: {e}")))?;
            latencies.push(result?);
        }
        Ok::<_, io::Error>(latencies)
    })?;

    let wall_time = wall_start.elapsed();

    // Join sender threads (best-effort; errors are non-fatal for bench).
    for h in sender_handles {
        let _ = h.join();
    }

    let mut latencies = results;
    latencies.sort_by_key(|l| l.duration);

    let peak = peak_counter.load(Ordering::SeqCst);

    Ok(IterationMetrics {
        wall_time,
        latencies,
        peak_threads: peak,
    })
}

/// Criterion benchmark: aggregate throughput at each concurrency level.
///
/// Measures the total bytes transferred per second across all concurrent
/// sessions. A sub-linear scaling curve indicates `spawn_blocking` pool
/// saturation.
fn bench_concurrent_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("concurrent_session_throughput");
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(10);

    for &n in CONCURRENCY_LEVELS {
        let total_bytes = (n * SESSION_PAYLOAD) as u64;
        group.throughput(Throughput::Bytes(total_bytes));
        group.bench_with_input(
            BenchmarkId::new("sessions", n),
            &n,
            |b, &concurrency| {
                b.iter(|| {
                    let metrics =
                        run_concurrent_sessions(concurrency).expect("concurrent sessions");
                    black_box(&metrics.wall_time);
                });
            },
        );
    }
    group.finish();
}

/// Criterion benchmark: per-session tail latency at each concurrency.
///
/// Reports the p99 session completion time. This surfaces the scheduling
/// delay that the last sessions experience when the blocking pool is
/// fully occupied and new tasks queue behind thread creation.
fn bench_concurrent_tail_latency(c: &mut Criterion) {
    let mut group = c.benchmark_group("concurrent_session_p99_latency");
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(10);

    for &n in CONCURRENCY_LEVELS {
        group.bench_with_input(
            BenchmarkId::new("sessions", n),
            &n,
            |b, &concurrency| {
                b.iter_custom(|iters| {
                    let mut total = Duration::ZERO;
                    for _ in 0..iters {
                        let metrics =
                            run_concurrent_sessions(concurrency).expect("concurrent sessions");
                        total += metrics.percentile(0.99);
                    }
                    total
                });
            },
        );
    }
    group.finish();
}

/// Criterion benchmark: thread pool saturation detection.
///
/// Measures the wall-clock time as a proxy, and logs the peak thread
/// count observed during each iteration. When peak threads equals the
/// `max_blocking_threads` setting, the pool is fully saturated and
/// additional sessions queue behind thread creation or worker completion.
fn bench_thread_saturation(c: &mut Criterion) {
    let mut group = c.benchmark_group("concurrent_session_saturation");
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(10);

    for &n in CONCURRENCY_LEVELS {
        group.bench_with_input(
            BenchmarkId::new("sessions", n),
            &n,
            |b, &concurrency| {
                b.iter_custom(|iters| {
                    let mut total_wall = Duration::ZERO;
                    for _ in 0..iters {
                        let metrics =
                            run_concurrent_sessions(concurrency).expect("concurrent sessions");
                        // Log saturation data for manual analysis.
                        eprintln!(
                            "  [N={concurrency}] wall={:.1}ms p50={:.1}ms p95={:.1}ms \
                             p99={:.1}ms peak_threads={}",
                            metrics.wall_time.as_secs_f64() * 1000.0,
                            metrics.percentile(0.50).as_secs_f64() * 1000.0,
                            metrics.percentile(0.95).as_secs_f64() * 1000.0,
                            metrics.percentile(0.99).as_secs_f64() * 1000.0,
                            metrics.peak_threads,
                        );
                        total_wall += metrics.wall_time;
                    }
                    total_wall
                });
            },
        );
    }
    group.finish();
}

criterion_group! {
    name = concurrent_session_benches;
    config = Criterion::default();
    targets =
        bench_concurrent_throughput,
        bench_concurrent_tail_latency,
        bench_thread_saturation
}
criterion_main!(concurrent_session_benches);
