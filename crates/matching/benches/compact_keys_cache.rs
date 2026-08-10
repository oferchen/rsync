//! Compact-lookup cache-behaviour benchmark (#2073).
//!
//! Profiles the cache behaviour of [`DeltaSignatureIndex`]'s compact
//! bucket lookup at index sizes that straddle the L1, L2, LLC, and
//! main-memory tiers of modern CPUs. Under the ZSO-4 compact-key design
//! the bucket array is keyed on `rsum >> 16` and capped at `2^16` slots,
//! so the hot table stays at most 256 KiB regardless of basis size.
//! The numbers this bench produces feed back into #2072 / #2073 by
//! quantifying how much the cap actually buys at each cache tier.
//!
//! # Running
//!
//! Plain Criterion timing:
//! ```text
//! cargo bench -p matching --features bench-internal --bench compact_keys_cache
//! ```
//!
//! With Linux `perf` counters wrapped around the binary (preferred for
//! hard cache-miss numbers):
//! ```text
//! perf stat -e cache-misses,cache-references,LLC-loads,LLC-load-misses \
//!     -- cargo bench -p matching --features bench-internal \
//!        --bench compact_keys_cache -- --profile-time 10
//! ```
//!
//! With `valgrind --tool=cachegrind` for a deterministic
//! cache-miss simulation independent of the host's actual cache sizes:
//! ```text
//! valgrind --tool=cachegrind \
//!     --LL=33554432,16,64 --D1=32768,8,64 --I1=32768,8,64 \
//!     target/release/deps/compact_keys_cache-<hash> --profile-time 10
//! ```
//!
//! Replace `--LL=33554432,16,64` with the LLC size, associativity, and
//! line size for the target microarchitecture. The bench reports the
//! lookup-table footprint in bytes on stderr before the timing tables so
//! the cachegrind operator can pick LL sizes that bracket the workload.
//!
//! # Methodology
//!
//! Four logical index sizes. The bucket-array footprint plateaus at the
//! `2^16` cap; the chain-entry footprint scales with `n_entries`:
//!
//! | label  | n_entries | bucket footprint | total footprint |
//! |--------|-----------|------------------|-----------------|
//! | `l1`   | 1024      | 16 KiB           | ~28 KiB         |
//! | `l2`   | 16 384    | 256 KiB          | ~448 KiB        |
//! | `llc`  | 1 048 576 | 256 KiB          | ~12 MiB         |
//! | `ram`  | 8 388 608 | 256 KiB          | ~96 MiB         |
//!
//! For each size, two access patterns:
//!
//! - `sequential`: probe rsums sampled in the order the basis blocks were
//!   indexed. This is the best-case prefetch-friendly traversal and
//!   approximates a delta-generation workload over an unmodified basis.
//! - `scattered`: probe rsums sampled via a deterministic xorshift
//!   permutation. The probe stream defeats the hardware prefetcher and
//!   approximates lookup cost when a target shares few aligned matches
//!   with its basis.
//!
//! The bench focuses on the *lookup table* alone via the
//! `bench-internal` accessor [`DeltaSignatureIndex::lookup_probe`], which
//! skips the tag-table fast-path, the bithash prefilter, and the
//! strong-checksum verify. Isolating the lookup means cachegrind/perf
//! numbers attribute misses to the table layout rather than the
//! surrounding signature-bytes streaming work.
//!
//! # Interpreting results
//!
//! Under the ZSO-4 compact-key layout the bucket array is bounded at
//! 256 KiB (`2^16 * 8` bytes), so the *bucket fetch* itself stays inside
//! L2 even for the `ram` size. The chain backing store grows with the
//! entry count and is the working set that pushes through the cache
//! hierarchy:
//!
//! - `l1` and `l2` sizes should be effectively cache-resident; sequential
//!   and scattered throughput should be near-identical, with cache-miss
//!   rates near zero.
//! - `llc` and `ram` sizes should show a widening gap: scattered probes
//!   stall on LLC misses while sequential probes still benefit from the
//!   chain backing store's monotonic insertion order.
//!
//! Action thresholds for #2072:
//!
//! - **Favourable** (justify packing): scattered-probe cache-miss rate at
//!   the `llc` or `ram` size is >= 2x the sequential rate, and total
//!   probes-per-second on the scattered case is bottlenecked by memory
//!   bandwidth. Further chain-entry packing should approximately halve
//!   those miss rates and recover most of the throughput gap.
//! - **Unfavourable** (defer #2072): scattered and sequential cache-miss
//!   rates stay within ~20% across all sizes, or the `ram` case is
//!   bottlenecked by something other than memory traffic (strong-checksum
//!   verify, allocator, etc). Further packing buys little if the cost is
//!   not actually in the chain fetch.
//!
//! # See also
//!
//! - `crates/matching/src/index/compact_lookup.rs` for the current layout.
//! - `docs/design/zsync-prune.md` and `docs/design/zsync-bithash.md` for
//!   sibling bench harnesses (#2063, #2067, #2071) that established the
//!   `bench-internal` pattern this bench follows.

#![cfg(feature = "bench-internal")]

use std::hint::black_box;
use std::num::{NonZeroU8, NonZeroU32};
use std::sync::OnceLock;
use std::time::Duration;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

use matching::DeltaSignatureIndex;
use protocol::ProtocolVersion;
use signature::{
    SignatureAlgorithm, SignatureLayoutParams, calculate_signature_layout, generate_file_signature,
};

/// Fixed block length forces a one-to-one mapping between basis size and
/// indexed block count. The value is deliberately small so the largest
/// configured size keeps the basis allocation in the hundreds-of-MiB range
/// rather than the multi-GiB range.
const FIXED_BLOCK_LENGTH: u32 = 64;

/// Each lookup probe drives this many cached rsum keys per Criterion
/// iteration. Decoupling probe-count from index size lets the per-iteration
/// cost stay roughly constant across the L1 / L2 / LLC / RAM tiers.
const PROBES_PER_ITER: usize = 8192;

/// Maximum entry count actually built when [`MATCHING_BENCH_MAX_ENTRIES`]
/// is unset. Caps RAM use so the bench runs on a 16 GiB workstation.
const DEFAULT_MAX_ENTRIES: usize = 1 << 20;

/// Environment toggle that lets a benchmarking host opt in to the
/// 256-MiB-table `ram` configuration, which needs ~512 MiB of basis bytes
/// during index build.
const MAX_ENTRIES_ENV: &str = "MATCHING_BENCH_MAX_ENTRIES";

const SIZES: &[(&str, usize)] = &[
    ("l1", 1 << 10),  // 1024 entries -> ~32 KiB lookup table
    ("l2", 1 << 14),  // 16384 entries -> ~512 KiB lookup table
    ("llc", 1 << 20), // 1 Mi entries -> ~32 MiB lookup table
    ("ram", 1 << 23), // 8 Mi entries -> ~256 MiB lookup table (opt-in)
];

/// Returns the maximum entry count the bench will allocate.
///
/// Honours the [`MATCHING_BENCH_MAX_ENTRIES`] environment variable so
/// hosts with enough RAM can opt in to the `ram` tier without editing
/// the bench source.
fn max_entries_cap() -> usize {
    std::env::var(MAX_ENTRIES_ENV)
        .ok()
        .and_then(|raw| raw.parse::<usize>().ok())
        .unwrap_or(DEFAULT_MAX_ENTRIES)
}

/// Deterministic xorshift64* byte sequence keyed by `seed`.
///
/// Produces the same byte stream on every run so cachegrind diffs and
/// perf re-runs are directly comparable. No `rand` dependency.
fn xorshift_bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut state = if seed == 0 {
        0x9E37_79B9_7F4A_7C15
    } else {
        seed
    };
    let mut out = vec![0u8; len];
    for chunk in out.chunks_mut(8) {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let bytes = state.wrapping_mul(0x2545_F491_4F6C_DD1D).to_le_bytes();
        chunk.copy_from_slice(&bytes[..chunk.len()]);
    }
    out
}

/// Builds a [`DeltaSignatureIndex`] over `data` with a forced block length
/// so the indexed block count tracks `data.len() / FIXED_BLOCK_LENGTH`.
fn build_index(data: &[u8]) -> DeltaSignatureIndex {
    let params = SignatureLayoutParams::new(
        data.len() as u64,
        NonZeroU32::new(FIXED_BLOCK_LENGTH),
        ProtocolVersion::NEWEST,
        NonZeroU8::new(16).expect("non-zero"),
    );
    let layout = calculate_signature_layout(params).expect("layout");
    let signature =
        generate_file_signature(data, layout, SignatureAlgorithm::Md4).expect("signature");
    DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4).expect("index")
}

/// Fixture holding the built index plus the two probe key streams.
struct Fixture {
    label: &'static str,
    requested_entries: usize,
    block_count: usize,
    lookup_capacity: usize,
    lookup_bytes: usize,
    sequential_keys: Vec<(u16, u16)>,
    scattered_keys: Vec<(u16, u16)>,
    index: DeltaSignatureIndex,
}

impl Fixture {
    fn new(label: &'static str, requested_entries: usize) -> Self {
        let basis_bytes = requested_entries
            .checked_mul(FIXED_BLOCK_LENGTH as usize)
            .expect("basis size fits in usize");
        // Deterministic seed-per-label so the basis is reproducible.
        let seed = label_seed(label);
        let basis = xorshift_bytes(seed, basis_bytes);
        let index = build_index(&basis);
        // The basis bytes are no longer needed after index build; drop
        // them explicitly so the fixture's resident-set is bounded by
        // the lookup table footprint rather than the source data.
        drop(basis);

        let block_count = index.block_count();
        let lookup_capacity = index.lookup_capacity();
        let lookup_bytes = index.lookup_bytes();

        // Sequential keys: walk the basis blocks in index order.
        let mut sequential_keys = Vec::with_capacity(PROBES_PER_ITER);
        for i in 0..PROBES_PER_ITER {
            let block_idx = i % block_count;
            let digest = index.block(block_idx).rolling();
            sequential_keys.push((digest.sum1(), digest.sum2()));
        }

        // Scattered keys: walk the basis blocks via a deterministic
        // xorshift permutation. The stride is large and prime relative
        // to `block_count` so consecutive probes land in different
        // cache lines of the lookup table for the larger sizes.
        let mut scattered_keys = Vec::with_capacity(PROBES_PER_ITER);
        let mut scatter_state = seed.wrapping_add(0xDEAD_BEEF_CAFE_BABE);
        for _ in 0..PROBES_PER_ITER {
            scatter_state ^= scatter_state << 13;
            scatter_state ^= scatter_state >> 7;
            scatter_state ^= scatter_state << 17;
            let block_idx = (scatter_state as usize) % block_count;
            let digest = index.block(block_idx).rolling();
            scattered_keys.push((digest.sum1(), digest.sum2()));
        }

        Self {
            label,
            requested_entries,
            block_count,
            lookup_capacity,
            lookup_bytes,
            sequential_keys,
            scattered_keys,
            index,
        }
    }
}

/// Mixes a label string into a u64 seed without pulling in `std::hash`.
fn label_seed(label: &str) -> u64 {
    let mut h: u64 = 0xCBF2_9CE4_8422_2325;
    for byte in label.as_bytes() {
        h ^= u64::from(*byte);
        h = h.wrapping_mul(0x100_0000_01B3);
    }
    h
}

/// Returns the configured fixtures, capped to the host's RAM budget.
fn fixtures() -> &'static [Fixture] {
    static CACHE: OnceLock<Vec<Fixture>> = OnceLock::new();
    CACHE.get_or_init(|| {
        let cap = max_entries_cap();
        let mut out = Vec::new();
        for (label, n) in SIZES {
            if *n > cap {
                eprintln!(
                    "compact_keys_cache[{label}]: skipped (requested={n} > cap={cap}); set \
                     {MAX_ENTRIES_ENV}={n} to opt in",
                );
                continue;
            }
            out.push(Fixture::new(label, *n));
        }
        out
    })
}

/// One-shot stderr emission with the lookup-table footprint per fixture.
/// Lets a cachegrind operator pick `--LL` parameters that bracket the
/// scan, and gives perf consumers a baseline for cache-miss attribution.
fn report_footprint() {
    eprintln!(
        "compact_keys_cache footprint (label,requested_entries,block_count,lookup_capacity,\
         lookup_bytes,probes_per_iter)"
    );
    for fx in fixtures() {
        eprintln!(
            "compact_keys_cache[{label}] entries={req} blocks={bc} slots={cap} bytes={bytes} \
             probes={probes}",
            label = fx.label,
            req = fx.requested_entries,
            bc = fx.block_count,
            cap = fx.lookup_capacity,
            bytes = fx.lookup_bytes,
            probes = PROBES_PER_ITER,
        );
    }
}

/// Drives `PROBES_PER_ITER` lookup probes from the prebuilt key stream.
///
/// Each probe walks the compact-key bucket chain via the bench-only
/// [`DeltaSignatureIndex::lookup_probe`] accessor, which returns the
/// number of yielded basis-block indices. The chain is addressed by the
/// upper-half discriminator (`rsum >> 16`) per the ZSO-4 compact-key
/// design; `black_box` on the running total prevents the optimizer from
/// hoisting the loop out of the timed section.
#[inline(never)]
fn drive_probes(index: &DeltaSignatureIndex, keys: &[(u16, u16)]) -> usize {
    let mut total = 0usize;
    for (sum1, sum2) in keys {
        total = total.wrapping_add(index.lookup_probe(*sum1, *sum2));
    }
    total
}

fn bench_lookup_cache(c: &mut Criterion) {
    let mut group = c.benchmark_group("compact_keys_cache");
    // Reuse the small-sample-size convention from the sibling benches so
    // the LLC/RAM iterations stay inside Criterion's measurement budget.
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(5));

    for fx in fixtures() {
        group.throughput(Throughput::Elements(PROBES_PER_ITER as u64));

        group.bench_with_input(BenchmarkId::new("sequential", fx.label), fx, |b, fx| {
            b.iter(|| {
                let total = drive_probes(&fx.index, black_box(&fx.sequential_keys));
                black_box(total)
            });
        });

        group.bench_with_input(BenchmarkId::new("scattered", fx.label), fx, |b, fx| {
            b.iter(|| {
                let total = drive_probes(&fx.index, black_box(&fx.scattered_keys));
                black_box(total)
            });
        });
    }

    group.finish();
}

fn bench_entry(c: &mut Criterion) {
    report_footprint();
    bench_lookup_cache(c);
}

criterion_group!(
    name = benches;
    config = Criterion::default()
        .sample_size(10)
        .measurement_time(Duration::from_secs(5));
    targets = bench_entry
);

criterion_main!(benches);
