//! SZC.c: `IORING_OP_SEND_ZC` vs plain `IORING_OP_SEND` at 100K-file
//! high-IOPS scale.
//!
//! Complements the IUS-3 bench (`ius_3_send_zc_vs_send.rs`) which measured
//! SEND_ZC on four chunk-shape workloads (16 KiB-1 MiB). This bench targets
//! the many-small-file regime: 100,000 files of 1-4 KiB each, simulating a
//! large source tree or package cache where per-file overhead dominates
//! throughput. The goal is to quantify whether SEND_ZC's two-CQE lifecycle
//! (transfer + notification drain) adds measurable per-op latency at high
//! IOPS vs the single-CQE plain SEND path.
//!
//! # Topology
//!
//! Both bench groups drive a single TCP loopback pair (`127.0.0.1`) and
//! submit one SQE per simulated file. Each file is represented by a
//! pre-generated payload slice from a contiguous buffer; the send loop
//! iterates all 100K slices sequentially. The loopback peer drains on a
//! background thread.
//!
//! # Workloads
//!
//! | Label | Files | Size per file | Total data |
//! |-------|-------|---------------|------------|
//! | `100k_1KiB` | 100,000 | 1,024 bytes | ~97.7 MiB |
//! | `100k_4KiB` | 100,000 | 4,096 bytes | ~390.6 MiB |
//! | `100k_mixed` | 100,000 | 1-4 KiB (LCG) | ~244 MiB |
//!
//! # Feature gating
//!
//! - The `send_plain` group needs only the `io_uring` feature (default on).
//! - The `send_zc` group requires the `iouring-send-zc` cargo feature and
//!   runtime probe success. Without the feature the group is cfg-compiled
//!   out; with the feature but no kernel support, the group skips cleanly.
//!
//! # When to run
//!
//! ```sh
//! OC_RSYNC_BENCH_SZC_C=1 \
//!   cargo bench -p fast_io --bench szcc_send_zc_100k_iops
//!
//! # full bench with both groups (Linux 6.0+ required for send_zc rows)
//! OC_RSYNC_BENCH_SZC_C=1 \
//!   cargo bench -p fast_io --features iouring-send-zc \
//!     --bench szcc_send_zc_100k_iops
//! ```
//!
//! # What the numbers inform
//!
//! - `send_zc / send_plain >= 0.95` at 100K files: SEND_ZC per-op overhead
//!   is negligible at high IOPS; no regression risk from the two-CQE drain.
//! - `send_zc / send_plain < 0.90` at 100K files: the notification CQE
//!   drain adds measurable latency per file; SEND_ZC dispatch threshold
//!   should remain above the small-file regime.

#[cfg(target_os = "linux")]
use std::env;
#[cfg(target_os = "linux")]
use std::io::Read;
#[cfg(target_os = "linux")]
use std::net::{TcpListener, TcpStream};
#[cfg(target_os = "linux")]
use std::os::unix::io::AsRawFd;
#[cfg(target_os = "linux")]
use std::thread;
#[cfg(target_os = "linux")]
use std::time::Duration;

#[cfg(target_os = "linux")]
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
#[cfg(target_os = "linux")]
use io_uring::{IoUring, opcode, types};

/// Env-var gate: set to `1` to run the bench.
#[cfg(target_os = "linux")]
const ENABLE_ENV: &str = "OC_RSYNC_BENCH_SZC_C";

/// SQ depth. Kept shallow for one-SQE-at-a-time lockstep measurement.
#[cfg(target_os = "linux")]
const SQ_ENTRIES: u32 = 16;

/// Fill byte for payload buffers.
#[cfg(target_os = "linux")]
const FILL_BYTE: u8 = 0xa5;

/// Loopback bind address with kernel-assigned port.
#[cfg(target_os = "linux")]
const LOOPBACK_BIND: &str = "127.0.0.1:0";

/// File count for the high-IOPS workload.
#[cfg(target_os = "linux")]
const FILE_COUNT: usize = 100_000;

/// LCG seed for deterministic mixed-size generation.
#[cfg(target_os = "linux")]
const MIXED_LCG_SEED: u64 = 0x5acc_2026_0605_u64;

/// Minimum file size for the mixed workload (1 KiB).
#[cfg(target_os = "linux")]
const MIXED_MIN: usize = 1024;

/// Maximum file size for the mixed workload (4 KiB).
#[cfg(target_os = "linux")]
const MIXED_MAX: usize = 4096;

/// Returns a deterministic size in `[MIXED_MIN, MIXED_MAX]` for file `i`.
#[cfg(target_os = "linux")]
fn mixed_file_size(i: usize) -> usize {
    let mut state = MIXED_LCG_SEED.wrapping_add(i as u64);
    state = state
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    let span = (MIXED_MAX - MIXED_MIN) as u64;
    MIXED_MIN + ((state >> 33) % (span + 1)) as usize
}

/// Workload descriptor.
#[cfg(target_os = "linux")]
#[derive(Copy, Clone)]
struct Workload {
    label: &'static str,
    /// Fixed per-file size, or `0` for the mixed workload.
    file_bytes: usize,
}

#[cfg(target_os = "linux")]
const WORKLOAD_1K: Workload = Workload {
    label: "100k_1KiB",
    file_bytes: 1024,
};

#[cfg(target_os = "linux")]
const WORKLOAD_4K: Workload = Workload {
    label: "100k_4KiB",
    file_bytes: 4096,
};

#[cfg(target_os = "linux")]
const WORKLOAD_MIXED: Workload = Workload {
    label: "100k_mixed_1KiB_to_4KiB",
    file_bytes: 0,
};

#[cfg(target_os = "linux")]
const WORKLOADS: &[Workload] = &[WORKLOAD_1K, WORKLOAD_4K, WORKLOAD_MIXED];

/// Returns the byte size for file `i` within a workload.
#[cfg(target_os = "linux")]
fn file_size_for(workload: &Workload, i: usize) -> usize {
    if workload.file_bytes == 0 {
        mixed_file_size(i)
    } else {
        workload.file_bytes
    }
}

/// Computes total bytes across all `FILE_COUNT` files for a workload.
#[cfg(target_os = "linux")]
fn workload_total_bytes(workload: &Workload) -> u64 {
    (0..FILE_COUNT)
        .map(|i| file_size_for(workload, i) as u64)
        .sum()
}

/// Probes whether io_uring is usable on this host.
#[cfg(target_os = "linux")]
fn io_uring_usable() -> bool {
    IoUring::new(SQ_ENTRIES).is_ok()
}

/// Returns `true` when the bench should run.
#[cfg(target_os = "linux")]
fn bench_enabled() -> bool {
    match env::var(ENABLE_ENV) {
        Ok(v) if v == "1" => io_uring_usable(),
        _ => false,
    }
}

/// Loopback TCP pair with a background drain thread.
#[cfg(target_os = "linux")]
struct Loopback {
    sender: TcpStream,
    drain: Option<thread::JoinHandle<()>>,
}

#[cfg(target_os = "linux")]
impl Loopback {
    fn new() -> std::io::Result<Self> {
        let listener = TcpListener::bind(LOOPBACK_BIND)?;
        let addr = listener.local_addr()?;
        let drain = thread::spawn(move || {
            let (mut peer, _) = match listener.accept() {
                Ok(p) => p,
                Err(_) => return,
            };
            let _ = peer.set_read_timeout(Some(Duration::from_secs(60)));
            let mut sink = [0u8; 64 * 1024];
            loop {
                match peer.read(&mut sink) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        });
        let sender = TcpStream::connect(addr)?;
        Ok(Self {
            sender,
            drain: Some(drain),
        })
    }

    fn fd(&self) -> std::os::unix::io::RawFd {
        self.sender.as_raw_fd()
    }
}

#[cfg(target_os = "linux")]
impl Drop for Loopback {
    fn drop(&mut self) {
        let _ = self.sender.shutdown(std::net::Shutdown::Both);
        if let Some(handle) = self.drain.take() {
            let _ = handle.join();
        }
    }
}

/// Submits one `IORING_OP_SEND` and drains its single CQE.
#[cfg(target_os = "linux")]
fn submit_send_one(
    ring: &mut IoUring,
    fd: std::os::unix::io::RawFd,
    buf: &[u8],
) -> std::io::Result<usize> {
    let entry = opcode::Send::new(types::Fd(fd), buf.as_ptr(), buf.len() as u32)
        .build()
        .user_data(0);
    // SAFETY: `buf` is borrowed for the full lifetime of this function.
    // The kernel reads the pointer only between push and the CQE arrival,
    // fully contained by `submit_and_wait`.
    unsafe {
        ring.submission()
            .push(&entry)
            .map_err(|_| std::io::Error::other("submission queue full"))?;
    }
    ring.submit_and_wait(1)?;
    let cqe = ring
        .completion()
        .next()
        .ok_or_else(|| std::io::Error::other("no completion"))?;
    let result = cqe.result();
    if result < 0 {
        return Err(std::io::Error::from_raw_os_error(-result));
    }
    Ok(result as usize)
}

/// Sends all `FILE_COUNT` simulated files over plain `IORING_OP_SEND`.
#[cfg(target_os = "linux")]
fn run_send_plain(
    ring: &mut IoUring,
    fd: std::os::unix::io::RawFd,
    buf: &[u8],
    workload: &Workload,
) -> std::io::Result<()> {
    for i in 0..FILE_COUNT {
        let size = file_size_for(workload, i);
        let chunk = &buf[..size];
        let n = submit_send_one(ring, fd, chunk)?;
        if n != chunk.len() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "short SEND on loopback",
            ));
        }
    }
    Ok(())
}

/// Bench group: plain `IORING_OP_SEND` at 100K-file IOPS scale.
#[cfg(target_os = "linux")]
fn bench_send_plain(c: &mut Criterion) {
    if !bench_enabled() {
        eprintln!(
            "Skipping szcc_send_zc_100k_iops::send_plain: set {ENABLE_ENV}=1 on a Linux 5.6+ \
             host with io_uring_setup(2) reachable to enable."
        );
        return;
    }

    let mut group = c.benchmark_group("szcc_send_plain_100k");
    group.sample_size(10);

    let payload = vec![FILL_BYTE; MIXED_MAX];

    for workload in WORKLOADS {
        group.throughput(Throughput::Bytes(workload_total_bytes(workload)));
        group.bench_with_input(
            BenchmarkId::from_parameter(workload.label),
            workload,
            |b, workload| {
                b.iter_with_setup(
                    || {
                        let ring = IoUring::new(SQ_ENTRIES).expect("ring");
                        let loopback = Loopback::new().expect("loopback");
                        (ring, loopback)
                    },
                    |(mut ring, loopback)| {
                        let fd = loopback.fd();
                        run_send_plain(&mut ring, fd, &payload, workload).expect("send_plain");
                        drop(loopback);
                    },
                );
            },
        );
    }

    group.finish();
}

/// Bench group: `IORING_OP_SEND_ZC` at 100K-file IOPS scale.
///
/// Only compiled when `iouring-send-zc` is enabled. Skips at runtime when
/// the kernel does not advertise the opcode.
#[cfg(all(target_os = "linux", feature = "iouring-send-zc"))]
fn bench_send_zc(c: &mut Criterion) {
    if !bench_enabled() {
        eprintln!(
            "Skipping szcc_send_zc_100k_iops::send_zc: set {ENABLE_ENV}=1 on a Linux 6.0+ \
             host with IORING_OP_SEND_ZC support to enable."
        );
        return;
    }
    if !fast_io::io_uring::send_zc_supported() {
        eprintln!(
            "Skipping szcc_send_zc_100k_iops::send_zc: kernel does not advertise \
             IORING_OP_SEND_ZC (requires Linux 6.0+)."
        );
        return;
    }

    let mut group = c.benchmark_group("szcc_send_zc_100k");
    group.sample_size(10);

    let payload = vec![FILL_BYTE; MIXED_MAX];

    for workload in WORKLOADS {
        group.throughput(Throughput::Bytes(workload_total_bytes(workload)));
        group.bench_with_input(
            BenchmarkId::from_parameter(workload.label),
            workload,
            |b, workload| {
                b.iter_with_setup(
                    || {
                        let ring = IoUring::new(SQ_ENTRIES).expect("ring");
                        let loopback = Loopback::new().expect("loopback");
                        (ring, loopback)
                    },
                    |(mut ring, loopback)| {
                        let fd = loopback.fd();
                        for i in 0..FILE_COUNT {
                            let size = file_size_for(workload, i);
                            let chunk = &payload[..size];
                            let n = fast_io::io_uring::try_send_zc(&mut ring, fd, chunk, 0)
                                .expect("send_zc");
                            assert_eq!(n, chunk.len(), "short SEND_ZC on loopback");
                        }
                        drop(loopback);
                    },
                );
            },
        );
    }

    group.finish();
}

#[cfg(all(target_os = "linux", feature = "iouring-send-zc"))]
criterion_group!(szcc_send_zc_100k_iops, bench_send_plain, bench_send_zc);

#[cfg(all(target_os = "linux", not(feature = "iouring-send-zc")))]
criterion_group!(szcc_send_zc_100k_iops, bench_send_plain);

#[cfg(target_os = "linux")]
criterion_main!(szcc_send_zc_100k_iops);

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!(
        "szcc_send_zc_100k_iops: skipped (Linux-only bench; IORING_OP_SEND / IORING_OP_SEND_ZC \
         require io_uring)"
    );
}
