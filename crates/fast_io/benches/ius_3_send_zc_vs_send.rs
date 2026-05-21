//! IUS-3: `IORING_OP_SEND_ZC` vs plain `IORING_OP_SEND` socket-send bench.
//!
//! See `docs/design/ius-3-send-zc-bench-design-2026-05-21.md` for the bench
//! design, kernel matrix, workload matrix, and decision criteria that feed
//! IUS-4 (the default-on / opt-in decision for the `iouring-send-zc` cargo
//! feature). The kernel-compat matrix this bench operates against is
//! enumerated in `docs/audits/ius-2-send-zc-kernel-compat-matrix.md` (PR
//! #4664, merged).
//!
//! # Topology
//!
//! Both bench groups drive a single TCP loopback pair (`127.0.0.1`) and
//! submit identical payloads through one freshly-built `IoUring`. The plain
//! `send_plain` group uses `IORING_OP_SEND`; the `send_zc` group dispatches
//! through [`fast_io::io_uring::try_send_zc`] which submits
//! `IORING_OP_SEND_ZC` and drains both CQEs (transfer + notification) before
//! returning, matching the production buffer-lifetime contract documented at
//! `crates/fast_io/src/io_uring/send_zc.rs:130`.
//!
//! Loopback isolates the syscall/CPU cost of the two primitives without
//! depending on a NIC, switch, or peer host. The followup numbers-capture PR
//! adds a `tc qdisc`-based bandwidth-shape wrapper script
//! (`scripts/bench-ius-3-tc-setup.sh`) that applies `netem` to the loopback
//! interface for the 1 Gbps and 10 Gbps shapes; the harness itself is
//! unchanged. tc-based bandwidth shapes are therefore out of scope for the
//! scaffold; followup PR adds the tc setup script.
//!
//! # Feature gating
//!
//! - The `send_plain` group needs only the `io_uring` feature (default on).
//! - The `send_zc` group additionally requires the `iouring-send-zc` cargo
//!   feature and runtime probe success
//!   ([`fast_io::io_uring::send_zc_supported`]). Without the feature the
//!   group is `cfg`-compiled out; with the feature but no kernel support,
//!   the group skips cleanly with a stderr notice rather than panicking.
//!
//! # When to run
//!
//! ```sh
//! # default build (send_plain only): never useful, opt out via env gate
//! OC_RSYNC_BENCH_IUS_3=1 \
//!   cargo bench -p fast_io --bench ius_3_send_zc_vs_send
//!
//! # full bench with both groups (Linux 6.0+ required for send_zc rows)
//! OC_RSYNC_BENCH_IUS_3=1 \
//!   cargo bench -p fast_io --features iouring-send-zc \
//!     --bench ius_3_send_zc_vs_send
//! ```
//!
//! The env-var gate matches the convention in other `fast_io` benches
//! (`iouring_per_file_vs_shared`, `nvme_data_path`) so a default
//! `cargo bench -p fast_io` does not pick up the IUS-3 work by accident.
//!
//! # What the numbers inform
//!
//! Decision criteria (see design doc section 7): SEND_ZC is promoted to
//! `ZeroCopyPolicy::Auto` (IUS-4) only when
//!
//! - `send_zc_throughput / send_throughput >= 1.1` on at least 3 of 4
//!   workload shapes for kernels >= 6.0, **and**
//! - user+system CPU% reduction >= 10% on at least 2 workload shapes for
//!   kernels >= 6.0.
//!
//! Only the throughput half is reported by this criterion harness; the
//! CPU%, syscall-count, and `copy_to_user`-bytes metrics are layered on
//! out-of-band by the numbers-capture followup PR (criterion does not
//! expose per-iteration sys CPU and does not own the strace pid).

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

/// Env-var gate: set to `1` to actually run the bench; otherwise the
/// harness prints a skip line and exits 0 so default `cargo bench -p
/// fast_io` is cheap. Matches the convention in `iouring_per_file_vs_shared`.
#[cfg(target_os = "linux")]
const ENABLE_ENV: &str = "OC_RSYNC_BENCH_IUS_3";

/// Submission-queue depth. SEND_ZC posts two CQEs per submission so we
/// keep the SQ shallow enough to drain in lockstep without queue
/// pressure obscuring the per-call cost.
#[cfg(target_os = "linux")]
const SQ_ENTRIES: u32 = 16;

/// Sentinel byte used to fill bench payloads. The bench measures the
/// send-side primitive only, so the receiver discards the bytes without
/// verifying them; the actual byte value is therefore arbitrary.
#[cfg(target_os = "linux")]
const FILL_BYTE: u8 = 0xa5;

/// Loopback bind address. Port `0` lets the kernel assign a free port so
/// concurrent bench runs do not collide.
#[cfg(target_os = "linux")]
const LOOPBACK_BIND: &str = "127.0.0.1:0";

/// LCG state for the deterministic `mixed_chunks` workload. Same seed
/// every bench invocation so the size distribution is reproducible and
/// the SEND vs SEND_ZC delta is not buried in seed jitter.
#[cfg(target_os = "linux")]
const MIXED_LCG_SEED: u64 = 0xb1ef_2026_0521_u64;

/// Workload spec: a label, the per-call payload size in bytes, and the
/// call count per criterion iter. The four shapes match the matrix in
/// the IUS-3 design doc section 4 (`small_chunks`, `medium_chunks`,
/// `large_chunks`, `mixed`).
#[cfg(target_os = "linux")]
#[derive(Copy, Clone)]
struct Workload {
    label: &'static str,
    chunk_bytes: usize,
    calls: usize,
}

#[cfg(target_os = "linux")]
const SMALL_CHUNKS: Workload = Workload {
    label: "small_chunks_16KiB_x_10000",
    chunk_bytes: 16 * 1024,
    calls: 10_000,
};

#[cfg(target_os = "linux")]
const MEDIUM_CHUNKS: Workload = Workload {
    label: "medium_chunks_256KiB_x_1000",
    chunk_bytes: 256 * 1024,
    calls: 1_000,
};

#[cfg(target_os = "linux")]
const LARGE_CHUNKS: Workload = Workload {
    label: "large_chunks_1MiB_x_100",
    chunk_bytes: 1024 * 1024,
    calls: 100,
};

/// Marker for the mixed workload: chunk size is per-call random in
/// `[MIXED_MIN, MIXED_MAX]`; `chunk_bytes` here is the upper bound used
/// to size the payload buffer.
#[cfg(target_os = "linux")]
const MIXED_CHUNKS: Workload = Workload {
    label: "mixed_chunks_4KiB_to_1MiB_x_1000",
    chunk_bytes: 1024 * 1024,
    calls: 1_000,
};

#[cfg(target_os = "linux")]
const MIXED_MIN: usize = 4 * 1024;
#[cfg(target_os = "linux")]
const MIXED_MAX: usize = 1024 * 1024;

/// Returns the deterministic size for the `i`-th mixed-workload chunk.
///
/// Knuth LCG seeded with [`MIXED_LCG_SEED`]; the modulus picks a value
/// in `[MIXED_MIN, MIXED_MAX]`. Same `i` always returns the same size so
/// the SEND and SEND_ZC bench groups are byte-identical workloads.
#[cfg(target_os = "linux")]
fn mixed_chunk_size(i: usize) -> usize {
    let mut state = MIXED_LCG_SEED.wrapping_add(i as u64);
    state = state
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    let span = (MIXED_MAX - MIXED_MIN) as u64;
    MIXED_MIN + ((state >> 33) % (span + 1)) as usize
}

/// All four workloads in iteration order. The criterion bench groups
/// loop over this slice so `send_plain` and `send_zc` produce
/// directly-comparable rows in the criterion report.
#[cfg(target_os = "linux")]
const WORKLOADS: &[Workload] = &[SMALL_CHUNKS, MEDIUM_CHUNKS, LARGE_CHUNKS, MIXED_CHUNKS];

/// Probes `io_uring_setup(2)` with a small ring so an unsupported host
/// (locked-down container, kernel < 5.6, seccomp filter) gives a clean
/// skip rather than a panic mid-iter.
#[cfg(target_os = "linux")]
fn io_uring_usable() -> bool {
    IoUring::new(SQ_ENTRIES).is_ok()
}

/// Returns `true` when the bench should actually run. False when either
/// the env var is unset or the kernel rejects io_uring.
#[cfg(target_os = "linux")]
fn bench_enabled() -> bool {
    match env::var(ENABLE_ENV) {
        Ok(v) if v == "1" => io_uring_usable(),
        _ => false,
    }
}

/// Loopback peer: holds the listener, the sender-side `TcpStream` (the
/// bench writes to this), and the drain thread that reads until the
/// sender closes. Drop order is sender-then-thread so the thread
/// observes EOF cleanly and joins without timing out.
#[cfg(target_os = "linux")]
struct Loopback {
    sender: TcpStream,
    drain: Option<thread::JoinHandle<()>>,
}

#[cfg(target_os = "linux")]
impl Loopback {
    /// Binds a fresh loopback pair, spawns a drain thread, and returns
    /// the sender stream + join handle. The drain thread reads
    /// indefinitely (until the sender closes) so the bench never blocks
    /// on a back-pressured socket.
    fn new() -> std::io::Result<Self> {
        let listener = TcpListener::bind(LOOPBACK_BIND)?;
        let addr = listener.local_addr()?;
        let drain = thread::spawn(move || {
            let (mut peer, _) = match listener.accept() {
                Ok(p) => p,
                Err(_) => return,
            };
            // 30 s timeout so a misbehaving bench iter cannot hang the
            // drain thread indefinitely. Loopback receives should
            // satisfy in microseconds.
            let _ = peer.set_read_timeout(Some(Duration::from_secs(30)));
            let mut sink = [0u8; 64 * 1024];
            loop {
                match peer.read(&mut sink) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
        });
        let sender = TcpStream::connect(addr)?;
        Ok(Self {
            sender,
            drain: Some(drain),
        })
    }

    /// Raw fd for io_uring SQE construction.
    fn fd(&self) -> std::os::unix::io::RawFd {
        self.sender.as_raw_fd()
    }
}

#[cfg(target_os = "linux")]
impl Drop for Loopback {
    fn drop(&mut self) {
        // Closing the sender lets the drain thread observe EOF on the
        // peer fd; without this step the thread blocks in `read` until
        // the read timeout fires.
        let _ = self.sender.shutdown(std::net::Shutdown::Both);
        if let Some(handle) = self.drain.take() {
            let _ = handle.join();
        }
    }
}

/// Submits one `IORING_OP_SEND` SQE for `buf` and drains the single CQE.
///
/// Mirrors the production batched-SEND path in
/// `crates/fast_io/src/io_uring/batching.rs:272` (`submit_send_batch`)
/// reduced to one call so the bench measures the per-submission cost
/// without batching artifacts. Short sends are reported as an error so
/// the bench skips that iter rather than looping internally - the
/// loopback peer has 30 s of read timeout, so short sends would indicate
/// a real failure rather than back-pressure.
#[cfg(target_os = "linux")]
fn submit_send_one(
    ring: &mut IoUring,
    fd: std::os::unix::io::RawFd,
    buf: &[u8],
) -> std::io::Result<usize> {
    let entry = opcode::Send::new(types::Fd(fd), buf.as_ptr(), buf.len() as u32)
        .build()
        .user_data(0);
    // SAFETY: `buf` is borrowed for the full lifetime of this call; the
    // kernel reads the pointer only between push and the matching CQE
    // arrival, which is fully contained by `submit_and_wait` below.
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

/// Sums the byte counts that one workload would dispatch in a single
/// criterion iter. Used as the `Throughput::Bytes` hint so criterion
/// reports MiB/s in addition to ns/iter.
#[cfg(target_os = "linux")]
fn workload_bytes(workload: &Workload) -> u64 {
    if workload.label == MIXED_CHUNKS.label {
        (0..workload.calls).map(mixed_chunk_size).sum::<usize>() as u64
    } else {
        (workload.chunk_bytes * workload.calls) as u64
    }
}

/// Runs `workload` against the plain SEND primitive: pre-fills one
/// max-size buffer (reused across the inner loop) and submits one
/// `IORING_OP_SEND` per call. Reports any send error to the caller so
/// criterion drops the iter rather than reporting noise.
#[cfg(target_os = "linux")]
fn run_send_plain(
    ring: &mut IoUring,
    fd: std::os::unix::io::RawFd,
    buf: &[u8],
    workload: &Workload,
) -> std::io::Result<()> {
    for i in 0..workload.calls {
        let chunk = if workload.label == MIXED_CHUNKS.label {
            &buf[..mixed_chunk_size(i)]
        } else {
            &buf[..workload.chunk_bytes]
        };
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

/// Bench group: plain `IORING_OP_SEND` against a fresh ring + loopback
/// pair per criterion iter. The ring and loopback are reconstructed
/// inside `iter_with_setup` so per-iter cost reflects the production
/// "one-shot writer" lifecycle; the long-lived shared-ring case is
/// covered by `iouring_per_file_vs_shared.rs` for the file-write path
/// and is out of scope for IUS-3.
#[cfg(target_os = "linux")]
fn bench_send_plain(c: &mut Criterion) {
    if !bench_enabled() {
        eprintln!(
            "Skipping ius_3_send_zc_vs_send::send_plain: set {ENABLE_ENV}=1 on a Linux 5.6+ \
             host with io_uring_setup(2) reachable to enable."
        );
        return;
    }

    let mut group = c.benchmark_group("ius_3_send_plain");
    group.sample_size(10);

    let payload = vec![FILL_BYTE; MIXED_MAX];

    for workload in WORKLOADS {
        group.throughput(Throughput::Bytes(workload_bytes(workload)));
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

/// Bench group: `IORING_OP_SEND_ZC` via [`fast_io::io_uring::try_send_zc`].
///
/// Only compiled when the `iouring-send-zc` cargo feature is enabled.
/// Runtime probe ([`fast_io::io_uring::send_zc_supported`]) gates the
/// per-iter dispatch; on kernels < 6.0 the group skips cleanly so the
/// build still succeeds.
#[cfg(all(target_os = "linux", feature = "iouring-send-zc"))]
fn bench_send_zc(c: &mut Criterion) {
    if !bench_enabled() {
        eprintln!(
            "Skipping ius_3_send_zc_vs_send::send_zc: set {ENABLE_ENV}=1 on a Linux 6.0+ host \
             with IORING_OP_SEND_ZC support to enable."
        );
        return;
    }
    if !fast_io::io_uring::send_zc_supported() {
        eprintln!(
            "Skipping ius_3_send_zc_vs_send::send_zc: kernel does not advertise \
             IORING_OP_SEND_ZC (requires Linux 6.0+; see \
             docs/audits/ius-2-send-zc-kernel-compat-matrix.md)."
        );
        return;
    }

    let mut group = c.benchmark_group("ius_3_send_zc");
    group.sample_size(10);

    let payload = vec![FILL_BYTE; MIXED_MAX];

    for workload in WORKLOADS {
        group.throughput(Throughput::Bytes(workload_bytes(workload)));
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
                        for i in 0..workload.calls {
                            let chunk = if workload.label == MIXED_CHUNKS.label {
                                &payload[..mixed_chunk_size(i)]
                            } else {
                                &payload[..workload.chunk_bytes]
                            };
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
criterion_group!(ius_3_send_zc_vs_send, bench_send_plain, bench_send_zc);

#[cfg(all(target_os = "linux", not(feature = "iouring-send-zc")))]
criterion_group!(ius_3_send_zc_vs_send, bench_send_plain);

#[cfg(target_os = "linux")]
criterion_main!(ius_3_send_zc_vs_send);

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!(
        "ius_3_send_zc_vs_send: skipped (Linux-only bench; IORING_OP_SEND / IORING_OP_SEND_ZC \
         require io_uring)"
    );
}
