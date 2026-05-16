//! crates/checksums/benches/framing_overhead_benchmark.rs
//!
//! Profiles per-block framing overhead vs raw single-shot throughput for the
//! strong checksums the delta pipeline relies on (MD4, MD5, XXH3).
//!
//! # What this measures
//!
//! The signature generator hashes a file in fixed-size blocks. Each block
//! incurs costs the raw `digest()` path does not:
//!
//! - **hasher construction / finalization** per block (state init, padding,
//!   length encoding)
//! - **seed mixing** - 4 little-endian bytes prepended (MD5 proper) or
//!   appended (MD4 legacy) per block (upstream: `checksum.c:get_checksum2()`)
//! - **batch dispatch** - signature generation accumulates up to 16 blocks and
//!   calls `md4_digest_batch` / `md5_digest_batch` to amortize lane-parallel
//!   SIMD setup (upstream-style framing happens above this batch layer)
//!
//! # Bench groups
//!
//! For each algorithm (MD4, MD5, XXH3) and total buffer size (1 MB, 16 MB,
//! 128 MB), the bench reports throughput for:
//!
//! - `raw`              - single one-shot digest over the whole buffer
//! - `framed_unseeded`  - per-block call, no seed (matches `SignatureAlgorithm::Md4`,
//!   `Md5 { Md5Seed::none() }`, `Xxh3 { seed: 0 }`)
//! - `framed_seeded`    - per-block call with seed mixing (matches
//!   `Md4Seeded` and `Md5 { Md5Seed::proper(_) }`; XXH3 is always seeded so
//!   this is equivalent to `framed_unseeded` for XXH3 - bench omitted to
//!   avoid duplicate work)
//! - `framed_batched`   - per-block via the 16-block SIMD batch helper, the
//!   path actually taken by `generate_file_signature` for unseeded MD4/MD5
//!
//! The bench varies block size across {700, 4096, 32768, 131072} - the first
//! is upstream rsync's default for small files, the others are the sizes the
//! generator picks for larger files.
//!
//! # Reading the numbers
//!
//! - **raw vs framed_unseeded** shows the cost of repeated state init +
//!   finalize per block. For MD4/MD5 with small blocks (700 B) this is
//!   substantial; for 128 KB blocks it should approach raw throughput.
//! - **framed_unseeded vs framed_seeded** isolates the seed-mix cost
//!   (one extra 4-byte `update()` per block).
//! - **framed_unseeded vs framed_batched** shows the SIMD batch payoff for
//!   the algorithms that support it (MD4/unseeded MD5). XXH3 has no batch
//!   path so its `framed_batched` group is omitted.
//!
//! Run with: `cargo bench -p checksums --bench framing_overhead_benchmark`

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;

use checksums::strong::{
    Md4, Md5, Md5Seed, StrongDigest, Xxh3, md4_digest_batch, md5_digest_batch,
};

/// Total buffer sizes to bench (1 MB, 16 MB, 128 MB).
///
/// The 128 MB size is large enough that per-block construction cost becomes
/// negligible at large block sizes but dominates at small block sizes.
const BUFFER_SIZES: &[usize] = &[1 << 20, 16 << 20, 128 << 20];

/// Block sizes the signature generator picks across the file-size range:
/// 700 is upstream's `DEFAULT_BLOCK_SIZE` (small files); 4 KiB, 32 KiB, and
/// 128 KiB cover the growth curve up to `MAX_BLOCK_SIZE_V30 = 1 << 17`.
const BLOCK_SIZES: &[usize] = &[700, 4096, 32_768, 131_072];

/// Mirrors `generate_file_signature`'s 16-block SIMD batch window.
const BATCH_SIZE: usize = 16;

/// Seed value used wherever the seeded variants are exercised. Arbitrary but
/// non-zero so the seeded code path is not short-circuited (upstream's
/// `Md4Seeded` skips the seed update when `seed == 0`).
const SEED_I32: i32 = 0x0BAD_F00Du32 as i32;
const SEED_U64: u64 = 0xDEAD_BEEF_DEAD_BEEFu64;

/// Builds a deterministic pseudo-random buffer so re-runs are comparable
/// without depending on the host RNG. Uses xorshift64 - cheap and produces
/// data the hash backends cannot specialize on.
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

/// Returns a Criterion configured to keep wall time bearable for the 128 MB
/// inputs while still giving Criterion enough samples for narrow CIs.
fn configured_criterion() -> Criterion {
    Criterion::default()
        .sample_size(10)
        .warm_up_time(std::time::Duration::from_millis(500))
        .measurement_time(std::time::Duration::from_secs(3))
}

/// Iterates `f` over each (possibly short final) block of `buf`. Used by the
/// per-block framed benches.
#[inline]
fn for_each_block<F: FnMut(&[u8])>(buf: &[u8], block_size: usize, mut f: F) {
    let mut offset = 0;
    while offset < buf.len() {
        let end = (offset + block_size).min(buf.len());
        f(&buf[offset..end]);
        offset = end;
    }
}

/// Iterates `buf` in `BATCH_SIZE` block windows, calling `f` once per window
/// with a slice-of-slices. Mirrors `generate_file_signature`'s batch loop.
#[inline]
fn for_each_batch<F: FnMut(&[&[u8]])>(buf: &[u8], block_size: usize, mut f: F) {
    let total_blocks = buf.len().div_ceil(block_size);
    let mut blocks: Vec<&[u8]> = Vec::with_capacity(BATCH_SIZE);
    let mut block_idx = 0;
    while block_idx < total_blocks {
        blocks.clear();
        let batch_end = (block_idx + BATCH_SIZE).min(total_blocks);
        for i in block_idx..batch_end {
            let start = i * block_size;
            let end = (start + block_size).min(buf.len());
            blocks.push(&buf[start..end]);
        }
        f(&blocks);
        block_idx = batch_end;
    }
}

/// MD4: raw, per-block unseeded, per-block seeded, batched.
fn bench_md4_framing(c: &mut Criterion) {
    let mut group = c.benchmark_group("md4_framing");
    for &size in BUFFER_SIZES {
        let buf = build_buffer(size);
        group.throughput(Throughput::Bytes(size as u64));

        group.bench_with_input(BenchmarkId::new("raw", size), &buf, |b, buf| {
            b.iter(|| black_box(Md4::digest(black_box(buf))));
        });

        for &block_size in BLOCK_SIZES {
            let id = format!("{size}_block_{block_size}");

            group.bench_with_input(BenchmarkId::new("framed_unseeded", &id), &buf, |b, buf| {
                b.iter(|| {
                    for_each_block(buf, block_size, |block| {
                        black_box(Md4::digest(black_box(block)));
                    });
                });
            });

            group.bench_with_input(BenchmarkId::new("framed_seeded", &id), &buf, |b, buf| {
                b.iter(|| {
                    for_each_block(buf, block_size, |block| {
                        black_box(Md4::digest_with_seed(SEED_I32, black_box(block)));
                    });
                });
            });

            group.bench_with_input(BenchmarkId::new("framed_batched", &id), &buf, |b, buf| {
                b.iter(|| {
                    for_each_batch(buf, block_size, |blocks| {
                        black_box(md4_digest_batch(black_box(blocks)));
                    });
                });
            });
        }
    }
    group.finish();
}

/// MD5: raw, per-block unseeded, per-block seeded (proper order), batched.
fn bench_md5_framing(c: &mut Criterion) {
    let mut group = c.benchmark_group("md5_framing");
    for &size in BUFFER_SIZES {
        let buf = build_buffer(size);
        group.throughput(Throughput::Bytes(size as u64));

        group.bench_with_input(BenchmarkId::new("raw", size), &buf, |b, buf| {
            b.iter(|| black_box(Md5::digest(black_box(buf))));
        });

        for &block_size in BLOCK_SIZES {
            let id = format!("{size}_block_{block_size}");

            group.bench_with_input(BenchmarkId::new("framed_unseeded", &id), &buf, |b, buf| {
                b.iter(|| {
                    for_each_block(buf, block_size, |block| {
                        black_box(Md5::digest(black_box(block)));
                    });
                });
            });

            let proper_seed = Md5Seed::proper(SEED_I32);
            group.bench_with_input(BenchmarkId::new("framed_seeded", &id), &buf, |b, buf| {
                b.iter(|| {
                    for_each_block(buf, block_size, |block| {
                        black_box(Md5::digest_with_seed(proper_seed, black_box(block)));
                    });
                });
            });

            group.bench_with_input(BenchmarkId::new("framed_batched", &id), &buf, |b, buf| {
                b.iter(|| {
                    for_each_batch(buf, block_size, |blocks| {
                        black_box(md5_digest_batch(black_box(blocks)));
                    });
                });
            });
        }
    }
    group.finish();
}

/// XXH3: raw vs per-block. XXH3 is always seeded and has no batch helper, so
/// the only framing source is the per-block hasher state cycle.
fn bench_xxh3_framing(c: &mut Criterion) {
    let mut group = c.benchmark_group("xxh3_framing");
    for &size in BUFFER_SIZES {
        let buf = build_buffer(size);
        group.throughput(Throughput::Bytes(size as u64));

        group.bench_with_input(BenchmarkId::new("raw", size), &buf, |b, buf| {
            b.iter(|| black_box(Xxh3::digest(black_box(SEED_U64), black_box(buf))));
        });

        for &block_size in BLOCK_SIZES {
            let id = format!("{size}_block_{block_size}");
            group.bench_with_input(BenchmarkId::new("framed_unseeded", &id), &buf, |b, buf| {
                b.iter(|| {
                    for_each_block(buf, block_size, |block| {
                        black_box(Xxh3::digest(0, black_box(block)));
                    });
                });
            });

            group.bench_with_input(BenchmarkId::new("framed_seeded", &id), &buf, |b, buf| {
                b.iter(|| {
                    for_each_block(buf, block_size, |block| {
                        black_box(Xxh3::digest(SEED_U64, black_box(block)));
                    });
                });
            });
        }
    }
    group.finish();
}

criterion_group! {
    name = benches;
    config = configured_criterion();
    targets = bench_md4_framing, bench_md5_framing, bench_xxh3_framing,
}

criterion_main!(benches);
