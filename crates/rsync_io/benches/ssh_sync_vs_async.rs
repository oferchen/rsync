//! crates/rsync_io/benches/ssh_sync_vs_async.rs
//!
//! Benchmark: synchronous `Read + Write` SSH-style I/O versus a tokio
//! `spawn_blocking` shim on a simulated slow link (task #1889).
//!
//! # Motivation
//!
//! The SSH transport in `rsync_io::ssh` uses blocking `Read + Write` over the
//! spawned child's stdio pipes. There is no async path today (see the
//! evaluations in #1411 and #1412). The realistic short-term comparison is not
//! a full async rewrite but a tokio "shim" that runs the existing sync code
//! under `tokio::task::spawn_blocking` from a single-threaded runtime - that is
//! what an embedder using oc-rsync inside a tokio application would do.
//!
//! # Approach
//!
//! Spinning up real `sshd` from a benchmark is not portable, so the bench uses
//! a loopback `TcpListener` as the wire. `TcpStream` exposes the same
//! `Read + Write` surface as `SshReader` / `SshWriter`, and the cost model we
//! care about (blocking syscalls, copy loop, thread context switches) is
//! identical. The "slow link" is simulated in userspace by `ThrottledRead`,
//! which sleeps `per_byte_ns` nanoseconds per byte returned. This avoids
//! depending on root-only kernel knobs like `tc qdisc netem`.
//!
//! When the `ssh` binary is available on `PATH`, an additional cell is
//! registered that spawns `ssh -V` as a smoke-check; the actual transport
//! micro-benchmark stays on the TCP loopback so results are reproducible
//! across CI runners.
//!
//! # Bench cells
//!
//! - `sync_transfer/1MB`, `sync_transfer/16MB` - blocking I/O on a dedicated
//!   writer thread, reader on the bench thread.
//! - `async_spawnblocking_transfer/1MB`, `async_spawnblocking_transfer/16MB` -
//!   identical sync I/O wrapped in `spawn_blocking` futures driven by a
//!   `current_thread` tokio runtime.
//!
//! Run with:
//!
//! ```text
//! cargo bench -p rsync_io --bench ssh_sync_vs_async
//! ```

use std::hint::black_box;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::thread;
use std::time::Duration;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use tokio::runtime::Builder;
use tokio::task;

/// Payload sizes covered by the bench. 1 MiB models a small object exchange
/// while 16 MiB stresses the steady-state copy loop.
const PAYLOAD_SIZES: &[usize] = &[1024 * 1024, 16 * 1024 * 1024];

/// I/O buffer size matching the upstream rsync default chunk (`MAX_BLOCK_SIZE`
/// is larger; 32 KiB is the typical socket read used in `io.c`).
const IO_BUF: usize = 32 * 1024;

/// Userspace slow-link parameter. 200 ns per byte caps a 32 KiB read at
/// ~6.5 ms, which is comparable to a transatlantic round trip without
/// requiring kernel cooperation. Kept low enough that the bench wall-clock
/// stays under criterion's default measurement budget.
const SLOW_LINK_NS_PER_BYTE: u64 = 200;

/// Returns `true` when the system `ssh` binary can be invoked. Used only to
/// log a skip notice for the optional ssh-spawn smoke cell; the main bench
/// cells do not depend on ssh.
fn ssh_binary_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        Command::new("ssh")
            .arg("-V")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
}

/// `Read` adapter that throttles delivery to `ns_per_byte` nanoseconds per
/// byte read. Simulates a slow link without requiring privileged kernel
/// configuration. Sleeps once per `read()` based on the number of bytes the
/// inner stream actually returned.
struct ThrottledRead<R: Read> {
    inner: R,
    ns_per_byte: u64,
}

impl<R: Read> ThrottledRead<R> {
    fn new(inner: R, ns_per_byte: u64) -> Self {
        Self { inner, ns_per_byte }
    }
}

impl<R: Read> Read for ThrottledRead<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n > 0 && self.ns_per_byte > 0 {
            let delay_ns = (n as u64).saturating_mul(self.ns_per_byte);
            thread::sleep(Duration::from_nanos(delay_ns));
        }
        Ok(n)
    }
}

/// Binds a loopback `TcpListener` on an ephemeral port and returns the
/// listener together with the connected client end. The accepted server end
/// is returned via the spawned thread's join handle.
fn loopback_pair() -> io::Result<(TcpStream, thread::JoinHandle<io::Result<TcpStream>>)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let addr = listener.local_addr()?;
    let accept = thread::spawn(move || -> io::Result<TcpStream> {
        let (stream, _) = listener.accept()?;
        Ok(stream)
    });
    let client = TcpStream::connect(addr)?;
    Ok((client, accept))
}

/// Sends `payload.len()` bytes from `writer` while `reader` drains them into
/// `sink`. Returns the number of bytes received so the bench can assert
/// completion. The reader is wrapped in `ThrottledRead` to simulate the slow
/// link without changing the writer's behaviour.
fn sync_transfer(payload: &[u8]) -> io::Result<usize> {
    let (client, accept_handle) = loopback_pair()?;
    let mut server = accept_handle
        .join()
        .map_err(|_| io::Error::other("accept thread panicked"))??;

    let payload_owned = payload.to_vec();
    let writer = thread::spawn(move || -> io::Result<()> {
        let mut w = client;
        w.write_all(&payload_owned)?;
        w.flush()?;
        // Half-close so the reader sees EOF and exits the copy loop.
        let _ = w.shutdown(Shutdown::Write);
        Ok(())
    });

    let mut throttled = ThrottledRead::new(&mut server, SLOW_LINK_NS_PER_BYTE);
    let mut buf = vec![0u8; IO_BUF];
    let mut total = 0usize;
    loop {
        let n = throttled.read(&mut buf)?;
        if n == 0 {
            break;
        }
        total += n;
        black_box(&buf[..n]);
    }
    writer
        .join()
        .map_err(|_| io::Error::other("writer thread panicked"))??;
    Ok(total)
}

/// Identical workload to [`sync_transfer`] but invoked via
/// `tokio::task::spawn_blocking` from a `current_thread` runtime. Mirrors the
/// shape of code an embedder would write to drop the existing blocking SSH
/// transport into a tokio application without rewriting it.
async fn async_spawnblocking_transfer(payload: Vec<u8>) -> io::Result<usize> {
    task::spawn_blocking(move || sync_transfer(&payload))
        .await
        .map_err(|e| io::Error::other(format!("spawn_blocking join: {e}")))?
}

/// Benchmark the sync `Read + Write` path on the throttled loopback wire.
fn bench_sync_transfer(c: &mut Criterion) {
    let mut group = c.benchmark_group("sync_transfer");
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(5));
    group.sample_size(20);
    for &size in PAYLOAD_SIZES {
        let payload = vec![0xA5u8; size];
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &payload, |b, pl| {
            b.iter(|| {
                let got = sync_transfer(pl).expect("sync transfer");
                assert_eq!(got, pl.len());
                black_box(got);
            });
        });
    }
    group.finish();
}

/// Benchmark the tokio `spawn_blocking` shim variant.
fn bench_async_spawnblocking_transfer(c: &mut Criterion) {
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build current_thread tokio runtime");

    let mut group = c.benchmark_group("async_spawnblocking_transfer");
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(5));
    group.sample_size(20);
    for &size in PAYLOAD_SIZES {
        let payload = vec![0xA5u8; size];
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &payload, |b, pl| {
            b.iter(|| {
                let got = runtime
                    .block_on(async_spawnblocking_transfer(pl.clone()))
                    .expect("async spawn_blocking transfer");
                assert_eq!(got, pl.len());
                black_box(got);
            });
        });
    }
    group.finish();
}

/// Optional smoke cell that spawns the real `ssh` binary (just `ssh -V`) to
/// confirm the system path. Skipped with a log line when ssh is absent.
fn bench_ssh_binary_smoke(c: &mut Criterion) {
    if !ssh_binary_available() {
        eprintln!(
            "ssh_sync_vs_async: ssh binary not on PATH; skipping ssh_binary_smoke cell. \
             The sync_transfer and async_spawnblocking_transfer cells do not require ssh."
        );
        return;
    }
    let mut group = c.benchmark_group("ssh_binary_smoke");
    group.warm_up_time(Duration::from_millis(200));
    group.measurement_time(Duration::from_secs(1));
    group.sample_size(10);
    group.bench_function("ssh_-V", |b| {
        b.iter(|| {
            let status = Command::new("ssh")
                .arg("-V")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .expect("spawn ssh -V");
            black_box(status);
        });
    });
    group.finish();
}

criterion_group! {
    name = ssh_sync_vs_async_benches;
    config = Criterion::default();
    targets = bench_sync_transfer, bench_async_spawnblocking_transfer, bench_ssh_binary_smoke
}
criterion_main!(ssh_sync_vs_async_benches);
