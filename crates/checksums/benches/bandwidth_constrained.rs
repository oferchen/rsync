//! Bandwidth-constrained checksum comparison benchmarks.
//!
//! Measures how checksum algorithm choice affects wall-clock transfer time at
//! different network bandwidth limits. In bandwidth-constrained scenarios the
//! hash computation overlaps with (or is dwarfed by) network transfer time,
//! so the question is: does picking a faster hash (XXH3) over a legacy one
//! (MD5, MD4) yield measurable wall-clock improvement at real-world link speeds?
//!
//! # Benchmark groups
//!
//! ## `bandwidth_constrained` (CBP-2)
//!
//! Simulates transferring data at 1 Gbps, 100 Mbps, and 10 Mbps while hashing
//! each block with XXH3, MD4, MD5, or XXH3-128. The simulated bandwidth limit
//! is modeled as a `thread::sleep` proportional to the bytes transferred,
//! representing the minimum time the network layer would take. Wall time =
//! max(hash_time, transfer_time) per block, since hashing and transfer are
//! sequential in rsync's pipeline.
//!
//! ## `cpu_utilization` (CBP-3)
//!
//! Measures the ratio of hash computation time to total wall time (hash +
//! simulated transfer) per algorithm and bandwidth. A ratio near 1.0 means the
//! CPU is the bottleneck; near 0.0 means the network dominates. This tells
//! operators whether investing in a faster hash algorithm matters at their
//! link speed.
//!
//! ## `simd_vs_scalar_md5` (CBP-4)
//!
//! Compares SIMD-accelerated MD5 batch hashing against the scalar fallback
//! path. Uses the `SimdLevel` override to force scalar dispatch, then
//! re-detects to use the host's best SIMD backend.
//!
//! Run with: `cargo bench -p checksums --bench bandwidth_constrained`

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;
use std::time::{Duration, Instant};

use checksums::cpu_features::{SimdLevel, reset_simd_override_for_tests};
use checksums::md5_backend::Dispatcher;
use checksums::strong::{Md4, Md5, StrongDigest, Xxh3, Xxh3_128};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Block sizes covering rsync's practical range.
/// 8 KiB is the default block size; 32 KiB and 128 KiB appear for larger files.
const BLOCK_SIZE: usize = 8_192;

/// Number of blocks per benchmark iteration.
/// 1024 blocks * 8 KiB = 8 MiB total - enough to amortize measurement overhead
/// without making each iteration take too long.
const NUM_BLOCKS: usize = 1_024;

/// Simulated bandwidth tiers (bits per second).
const BANDWIDTHS_BPS: &[(u64, &str)] = &[
    (10_000_000_000, "10Gbps"),
    (1_000_000_000, "1Gbps"),
    (100_000_000, "100Mbps"),
    (10_000_000, "10Mbps"),
];

/// Batch width for SIMD vs scalar MD5 comparison.
const MD5_BATCH_WIDTH: usize = 16;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Deterministic pseudo-random buffer via xorshift64.
fn build_buffer(size: usize) -> Vec<u8> {
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut out = vec![0u8; size];
    let mut i = 0;
    while i + 8 <= size {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out[i..i + 8].copy_from_slice(&state.to_le_bytes());
        i += 8;
    }
    while i < size {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out[i] = state as u8;
        i += 1;
    }
    out
}

/// Simulated network transfer delay for `nbytes` at `bps` bits/second.
#[inline]
fn transfer_delay(nbytes: usize, bps: u64) -> Duration {
    // time = nbytes * 8 / bps  (in seconds)
    // Convert to nanos to avoid floating point:
    //   nanos = nbytes * 8 * 1_000_000_000 / bps
    let nanos = (nbytes as u128 * 8 * 1_000_000_000) / bps as u128;
    Duration::from_nanos(nanos as u64)
}

/// Spin-waits for the given duration. `thread::sleep` has millisecond
/// granularity on most OSes, which is too coarse for 10 Gbps simulation
/// (~6.5 us per 8 KiB block). Spin-waiting gives sub-microsecond accuracy.
#[inline]
fn spin_wait(dur: Duration) {
    let deadline = Instant::now() + dur;
    while Instant::now() < deadline {
        std::hint::spin_loop();
    }
}

fn configured_criterion() -> Criterion {
    Criterion::default()
        .sample_size(20)
        .warm_up_time(Duration::from_millis(500))
        .measurement_time(Duration::from_secs(3))
}

// ---------------------------------------------------------------------------
// CBP-2: Bandwidth-constrained wall-clock comparison
// ---------------------------------------------------------------------------

/// Hash all blocks with the given algorithm, interleaving a simulated network
/// transfer delay per block.
fn hash_with_bandwidth<F>(blocks: &[&[u8]], bps: u64, hash_fn: F)
where
    F: Fn(&[u8]),
{
    let delay = transfer_delay(BLOCK_SIZE, bps);
    for block in blocks {
        hash_fn(block);
        spin_wait(delay);
    }
}

fn bench_bandwidth_constrained(c: &mut Criterion) {
    let buf = build_buffer(NUM_BLOCKS * BLOCK_SIZE);
    let blocks: Vec<&[u8]> = (0..NUM_BLOCKS)
        .map(|i| &buf[i * BLOCK_SIZE..(i + 1) * BLOCK_SIZE])
        .collect();

    let total_bytes = (NUM_BLOCKS * BLOCK_SIZE) as u64;

    for &(bps, bps_label) in BANDWIDTHS_BPS {
        let mut group = c.benchmark_group(format!("bw_constrained/{bps_label}"));
        group.throughput(Throughput::Bytes(total_bytes));

        group.bench_with_input(
            BenchmarkId::new("xxh3", bps_label),
            &blocks,
            |b, blocks| {
                b.iter(|| {
                    hash_with_bandwidth(blocks, bps, |blk| {
                        black_box(Xxh3::digest(0, black_box(blk)));
                    });
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("xxh3_128", bps_label),
            &blocks,
            |b, blocks| {
                b.iter(|| {
                    hash_with_bandwidth(blocks, bps, |blk| {
                        black_box(Xxh3_128::digest(0, black_box(blk)));
                    });
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("md5", bps_label),
            &blocks,
            |b, blocks| {
                b.iter(|| {
                    hash_with_bandwidth(blocks, bps, |blk| {
                        black_box(Md5::digest(black_box(blk)));
                    });
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("md4", bps_label),
            &blocks,
            |b, blocks| {
                b.iter(|| {
                    hash_with_bandwidth(blocks, bps, |blk| {
                        black_box(Md4::digest(black_box(blk)));
                    });
                });
            },
        );

        group.finish();
    }
}

// ---------------------------------------------------------------------------
// CBP-3: CPU utilization ratio per algorithm
// ---------------------------------------------------------------------------

/// Measures hash_time / (hash_time + transfer_time) for a given algorithm and
/// bandwidth. Returns the ratio as a value in [0.0, 1.0].
fn measure_cpu_ratio<F>(blocks: &[&[u8]], bps: u64, hash_fn: F) -> f64
where
    F: Fn(&[u8]),
{
    let transfer_dur = transfer_delay(BLOCK_SIZE, bps);

    let hash_start = Instant::now();
    for block in blocks {
        hash_fn(block);
    }
    let hash_elapsed = hash_start.elapsed();

    let total_transfer = transfer_dur * blocks.len() as u32;
    let total_wall = hash_elapsed + total_transfer;

    hash_elapsed.as_nanos() as f64 / total_wall.as_nanos() as f64
}

/// Benchmarks that measure CPU utilization ratio. The ratio itself is printed
/// to stderr for reference; criterion measures the hash-only throughput for
/// each algorithm so users can cross-reference.
fn bench_cpu_utilization(c: &mut Criterion) {
    let buf = build_buffer(NUM_BLOCKS * BLOCK_SIZE);
    let blocks: Vec<&[u8]> = (0..NUM_BLOCKS)
        .map(|i| &buf[i * BLOCK_SIZE..(i + 1) * BLOCK_SIZE])
        .collect();

    let total_bytes = (NUM_BLOCKS * BLOCK_SIZE) as u64;

    // Print CPU utilization ratios for reference (one-shot measurement).
    eprintln!();
    eprintln!("--- CPU utilization ratios (hash_time / total_time) ---");
    for &(bps, bps_label) in BANDWIDTHS_BPS {
        let xxh3_ratio = measure_cpu_ratio(&blocks, bps, |blk| {
            black_box(Xxh3::digest(0, blk));
        });
        let xxh3_128_ratio = measure_cpu_ratio(&blocks, bps, |blk| {
            black_box(Xxh3_128::digest(0, blk));
        });
        let md5_ratio = measure_cpu_ratio(&blocks, bps, |blk| {
            black_box(Md5::digest(blk));
        });
        let md4_ratio = measure_cpu_ratio(&blocks, bps, |blk| {
            black_box(Md4::digest(blk));
        });

        eprintln!(
            "  {bps_label:>8}: XXH3={xxh3_ratio:.4}  XXH3-128={xxh3_128_ratio:.4}  \
             MD5={md5_ratio:.4}  MD4={md4_ratio:.4}"
        );
    }
    eprintln!("--- end ratios ---");
    eprintln!();

    // Criterion benches: hash-only throughput per algorithm (no simulated
    // transfer). This lets users compute the ratio themselves against any
    // arbitrary bandwidth by comparing hash throughput vs link throughput.
    let mut group = c.benchmark_group("cpu_utilization/hash_only");
    group.throughput(Throughput::Bytes(total_bytes));

    group.bench_function("xxh3", |b| {
        b.iter(|| {
            for block in &blocks {
                black_box(Xxh3::digest(0, black_box(block)));
            }
        });
    });

    group.bench_function("xxh3_128", |b| {
        b.iter(|| {
            for block in &blocks {
                black_box(Xxh3_128::digest(0, black_box(block)));
            }
        });
    });

    group.bench_function("md5", |b| {
        b.iter(|| {
            for block in &blocks {
                black_box(Md5::digest(black_box(block)));
            }
        });
    });

    group.bench_function("md4", |b| {
        b.iter(|| {
            for block in &blocks {
                black_box(Md4::digest(black_box(block)));
            }
        });
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// CBP-4: SIMD-accelerated MD5 vs scalar MD5
// ---------------------------------------------------------------------------

fn bench_simd_vs_scalar_md5(c: &mut Criterion) {
    let buf = build_buffer(MD5_BATCH_WIDTH * BLOCK_SIZE);
    let blocks: Vec<&[u8]> = (0..MD5_BATCH_WIDTH)
        .map(|i| &buf[i * BLOCK_SIZE..(i + 1) * BLOCK_SIZE])
        .collect();

    let total_bytes = (MD5_BATCH_WIDTH * BLOCK_SIZE) as u64;

    // Detect the host's native SIMD backend before we start overriding.
    let native_dispatcher = Dispatcher::detect();
    let native_backend = native_dispatcher.backend();

    let mut group = c.benchmark_group("simd_vs_scalar_md5");
    group.throughput(Throughput::Bytes(total_bytes));

    // --- Scalar path: force SimdLevel::None, create a fresh dispatcher ---
    group.bench_function(
        BenchmarkId::new("scalar", format!("batch{MD5_BATCH_WIDTH}")),
        |b| {
            // Force scalar for every iteration.
            reset_simd_override_for_tests(SimdLevel::None);
            let scalar_dispatcher = Dispatcher::detect();
            assert_eq!(
                scalar_dispatcher.backend(),
                checksums::md5_backend::Backend::Scalar,
                "override did not force scalar"
            );
            b.iter(|| {
                black_box(scalar_dispatcher.digest_batch(black_box(&blocks)));
            });
        },
    );

    // --- SIMD path: restore native level, create a fresh dispatcher ---
    group.bench_function(
        BenchmarkId::new(
            format!("simd_{}", native_backend.name().to_lowercase().replace(' ', "_")),
            format!("batch{MD5_BATCH_WIDTH}"),
        ),
        |b| {
            reset_simd_override_for_tests(SimdLevel::Auto);
            let simd_dispatcher = Dispatcher::detect();
            b.iter(|| {
                black_box(simd_dispatcher.digest_batch(black_box(&blocks)));
            });
        },
    );

    // --- Single-hash scalar vs SIMD for reference ---
    group.bench_function("single_scalar", |b| {
        reset_simd_override_for_tests(SimdLevel::None);
        let scalar_dispatcher = Dispatcher::detect();
        let data = &buf[..BLOCK_SIZE];
        b.iter(|| {
            black_box(scalar_dispatcher.digest(black_box(data)));
        });
    });

    group.bench_function(
        format!("single_simd_{}", native_backend.name().to_lowercase().replace(' ', "_")),
        |b| {
            reset_simd_override_for_tests(SimdLevel::Auto);
            let simd_dispatcher = Dispatcher::detect();
            let data = &buf[..BLOCK_SIZE];
            b.iter(|| {
                black_box(simd_dispatcher.digest(black_box(data)));
            });
        },
    );

    // Restore auto before exiting.
    reset_simd_override_for_tests(SimdLevel::Auto);

    group.finish();

    eprintln!();
    eprintln!(
        "--- MD5 backend: native={}, scalar forced via SimdLevel::None ---",
        native_backend.name()
    );
    eprintln!();
}

// ---------------------------------------------------------------------------
// Criterion harness
// ---------------------------------------------------------------------------

criterion_group! {
    name = benches;
    config = configured_criterion();
    targets =
        bench_bandwidth_constrained,
        bench_cpu_utilization,
        bench_simd_vs_scalar_md5,
}

criterion_main!(benches);
