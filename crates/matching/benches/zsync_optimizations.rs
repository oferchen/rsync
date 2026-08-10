//! Bench harness for the zsync-inspired matching-engine optimizations (#2514).
//!
//! Provides a single command to measure the speedup of the matching pipeline
//! built on top of [`DeltaSignatureIndex`]. The harness is deliberately
//! decoupled from the per-optimization bench cells already in the workspace
//! (`bithash_rejection.rs`, `seq_match_redundant.rs`, `prune_duplicate_heavy.rs`,
//! `compact_keys_cache.rs`, `zsync_medium_dataset.rs`) so a release engineer
//! can produce a baseline vs optimized comparison in one invocation:
//!
//! ```
//! cargo bench -p matching --bench zsync_optimizations
//! ```
//!
//! # Workloads
//!
//! Three target-side workloads are crossed with the typical rsync block-size
//! ladder (1024, 4096, 16384 bytes):
//!
//! - **A - sequential**: basis is 16 MiB of seeded random bytes, source is
//!   identical. Every block matches at index `K -> K + 1`. This is the
//!   best case for the ZSO-2 seq-match lookahead (`extend_run`).
//! - **B - pure miss**: basis is 16 MiB random, source is a different 16 MiB
//!   random stream with no shared blocks. Best case for the ZSO-1 bithash
//!   prefilter, which must reject ~7/8 of probes before the hash lookup.
//! - **C - interleaved**: basis is 16 MiB, source is the same basis with
//!   random 64-byte insertions every 4 KiB. Roughly 50/50 hit/miss mix that
//!   matches a common real-world editing pattern.
//!
//! # Reported metrics
//!
//! Two derived counter snapshots are emitted to stderr before the Criterion
//! timing tables so they survive CI capture without requiring an opt-in
//! feature flag:
//!
//! - **matches**: full block matches observed across a one-byte rolling
//!   scan of the target, counted via [`DeltaSignatureIndex::find_match_bytes`].
//!   Confirmed hits advance the cursor by one block, matching the production
//!   match-then-skip behaviour in `generator.rs`.
//! - **hash_lookups**: total probe attempts. Each `find_match_bytes` call
//!   enters the tag-table fast path, so this is also the upper bound on
//!   compact-lookup chain walks. The zsync optimizations (bithash, compact
//!   key, prune) all aim to lower the *cost* of a lookup, not the count;
//!   `matches / hash_lookups` is therefore the productivity ratio the
//!   harness reports.
//!
//! Criterion measures the production [`generate_delta`] path end-to-end
//! per (workload, block_size) pair. Dividing the absolute counts emitted
//! by the warmup phase by the Criterion mean time yields matches/sec and
//! hash_lookups/sec for direct baseline-vs-optimized comparison.
//!
//! # Reproducibility
//!
//! All byte streams come from a seeded [`SmallRng`]. Fixed seeds keep runs
//! comparable across hosts and across the baseline / optimized variants
//! that other agents land in `crates/matching/src/index/`.

#![deny(unsafe_code)]

use std::hint::black_box;
use std::num::{NonZeroU8, NonZeroU32};
use std::sync::OnceLock;

use checksums::RollingDigest;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};

use matching::{DeltaSignatureIndex, generate_delta};
use protocol::ProtocolVersion;
use signature::{
    SignatureAlgorithm, SignatureLayoutParams, calculate_signature_layout, generate_file_signature,
};

/// Target corpus size: 16 MiB matches the workload spec for ZSO-6 and keeps
/// each Criterion iteration well inside the default measurement budget.
const CORPUS_SIZE: usize = 16 * 1024 * 1024;

/// Block sizes (in bytes) exercised by the harness. The middle value (4096)
/// brackets typical rsync defaults at this corpus size; the outer values
/// pressure the matcher with very small and moderately large blocks.
const BLOCK_SIZES: &[u32] = &[1024, 4096, 16384];

/// Fixed seed for the basis byte stream.
const BASIS_SEED: u64 = 0xA5A5_5A5A_C0FF_EE00;

/// Fixed seed for the "pure miss" source byte stream.
const MISS_SEED: u64 = 0x9E37_79B9_7F4A_7C15;

/// Fixed seed used to pick insertion offsets and payloads for workload C.
const INTERLEAVE_SEED: u64 = 0xDEAD_BEEF_CAFE_F00D;

/// Workload C inserts `INSERTION_LEN` random bytes every `INSERTION_STRIDE`
/// bytes of basis, producing a ~50/50 hit/miss mix at 4096-byte blocks.
const INSERTION_STRIDE: usize = 4096;
const INSERTION_LEN: usize = 64;

/// Generates `len` bytes of pseudo-random data from a seeded `SmallRng`.
///
/// `SmallRng::seed_from_u64` is part of `rand`'s reproducible RNG surface,
/// so two runs with the same seed produce byte-identical output regardless
/// of host architecture.
fn seeded_bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut rng = SmallRng::seed_from_u64(seed);
    let mut out = vec![0u8; len];
    rng.fill_bytes(&mut out);
    out
}

/// Builds the signature layout used by all bench cells, parameterized by
/// explicit block size so the matcher hot path is the dominant cost.
fn layout_for(len: u64, block_size: u32) -> signature::SignatureLayout {
    let params = SignatureLayoutParams::new(
        len,
        NonZeroU32::new(block_size),
        ProtocolVersion::NEWEST,
        NonZeroU8::new(16).expect("strong length is non-zero"),
    );
    calculate_signature_layout(params).expect("signature layout")
}

/// Builds a [`DeltaSignatureIndex`] over `basis` at the requested block size
/// using MD4 as the strong checksum, matching the wire-default algorithm.
fn build_index(basis: &[u8], block_size: u32) -> DeltaSignatureIndex {
    let layout = layout_for(basis.len() as u64, block_size);
    let signature =
        generate_file_signature(basis, layout, SignatureAlgorithm::Md4).expect("signature");
    DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4)
        .expect("index from signature")
}

/// Constructs the workload C source: basis interleaved with random
/// insertions every `INSERTION_STRIDE` bytes.
fn interleaved_source(basis: &[u8]) -> Vec<u8> {
    let mut rng = SmallRng::seed_from_u64(INTERLEAVE_SEED);
    let mut payload = vec![0u8; INSERTION_LEN];
    let estimated_insertions = basis.len() / INSERTION_STRIDE + 1;
    let mut out = Vec::with_capacity(basis.len() + estimated_insertions * INSERTION_LEN);
    let mut cursor = 0usize;
    while cursor < basis.len() {
        let end = (cursor + INSERTION_STRIDE).min(basis.len());
        out.extend_from_slice(&basis[cursor..end]);
        if end < basis.len() {
            rng.fill_bytes(&mut payload);
            out.extend_from_slice(&payload);
        }
        cursor = end;
    }
    out
}

/// Workload-specific source byte stream.
fn build_source(workload: Workload, basis: &[u8]) -> Vec<u8> {
    match workload {
        Workload::Sequential => basis.to_vec(),
        Workload::PureMiss => seeded_bytes(MISS_SEED, basis.len()),
        Workload::Interleaved => interleaved_source(basis),
    }
}

/// One of the three target workloads enumerated in the module docs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Workload {
    Sequential,
    PureMiss,
    Interleaved,
}

impl Workload {
    /// Stable short label used in Criterion bench IDs and stderr summaries.
    const fn label(self) -> &'static str {
        match self {
            Workload::Sequential => "sequential",
            Workload::PureMiss => "pure_miss",
            Workload::Interleaved => "interleaved",
        }
    }
}

const WORKLOADS: &[Workload] = &[
    Workload::Sequential,
    Workload::PureMiss,
    Workload::Interleaved,
];

/// Cached fixture per (workload, block_size). Built once and reused so the
/// 16 MiB allocations and signature builds do not dominate iteration noise.
struct Fixture {
    workload: Workload,
    block_size: u32,
    source: Vec<u8>,
    index: DeltaSignatureIndex,
}

impl Fixture {
    fn new(workload: Workload, block_size: u32) -> Self {
        let basis = seeded_bytes(BASIS_SEED, CORPUS_SIZE);
        let source = build_source(workload, &basis);
        let index = build_index(&basis, block_size);
        Self {
            workload,
            block_size,
            source,
            index,
        }
    }
}

fn fixtures() -> &'static Vec<Fixture> {
    static CACHE: OnceLock<Vec<Fixture>> = OnceLock::new();
    CACHE.get_or_init(|| {
        let mut out = Vec::with_capacity(WORKLOADS.len() * BLOCK_SIZES.len());
        for &workload in WORKLOADS {
            for &block_size in BLOCK_SIZES {
                out.push(Fixture::new(workload, block_size));
            }
        }
        out
    })
}

fn fixture(workload: Workload, block_size: u32) -> &'static Fixture {
    fixtures()
        .iter()
        .find(|fx| fx.workload == workload && fx.block_size == block_size)
        .expect("fixture for workload + block_size")
}

/// Counter snapshot reported for each workload before the Criterion timing
/// tables. The probe loop mirrors the matcher's per-byte rolling scan, but
/// stops short of the full delta-script construction so the counts are
/// stable across baseline and optimized index implementations.
#[derive(Clone, Copy, Debug, Default)]
struct MatchCounts {
    /// Total `find_match_bytes` probes issued. Each probe enters the
    /// production tag-table fast path, so this is also the upper bound
    /// on compact-lookup chain walks.
    hash_lookups: u64,
    /// Confirmed full matches. Subset of `hash_lookups`.
    matches: u64,
}

/// Walks `source` at byte strides counting probes and confirmed matches.
///
/// On a confirmed match the cursor advances by a full block, matching the
/// production match-then-skip behaviour in `generator.rs`. On a miss the
/// cursor advances one byte, mirroring the rolling-hash slide. The total
/// probe count is therefore deterministic for a given (basis, source,
/// block_size) triple and stable across optimized index variants.
fn count_matches(source: &[u8], index: &DeltaSignatureIndex) -> MatchCounts {
    let block_len = index.block_length();
    let mut counts = MatchCounts::default();
    if source.len() < block_len || block_len == 0 {
        return counts;
    }
    let last = source.len() - block_len;
    let mut pos = 0usize;
    while pos <= last {
        let window = &source[pos..pos + block_len];
        let digest = RollingDigest::from_bytes(window);
        counts.hash_lookups += 1;
        if index.find_match_bytes(digest, window).is_some() {
            counts.matches += 1;
            pos += block_len;
        } else {
            pos += 1;
        }
    }
    counts
}

/// Pretty-prints a per-workload counter snapshot to stderr so CI capture
/// preserves the absolute numbers alongside Criterion's relative timings.
fn report_counts(fx: &Fixture) {
    let counts = count_matches(&fx.source, &fx.index);
    let label = fx.workload.label();
    let bs = fx.block_size;
    let hash_lookups = counts.hash_lookups;
    let matches = counts.matches;
    let match_rate = if hash_lookups == 0 {
        0.0
    } else {
        matches as f64 / hash_lookups as f64
    };
    eprintln!(
        "zsync_opt[{label},bs={bs}] hash_lookups={hash_lookups} \
         matches={matches} match_rate={match_rate:.6}"
    );
}

/// Pre-walks every fixture once to populate the counter snapshot. The
/// fixtures themselves are built lazily inside [`fixture`]; this helper
/// just forces that work before the Criterion measurement loop and emits
/// the counter table.
fn warmup_and_report() {
    eprintln!(
        "zsync_optimizations harness: corpus={CORPUS_SIZE} bytes block_sizes={BLOCK_SIZES:?} \
         workloads={:?}",
        WORKLOADS.iter().map(|w| w.label()).collect::<Vec<_>>(),
    );
    eprintln!(
        "header: workload,block_size,hash_lookups,matches,match_rate \
         (probes counted at one-byte strides; matches advance by block_length)"
    );
    for fx in fixtures() {
        report_counts(fx);
    }
}

/// Criterion cell: end-to-end matcher throughput. Each iteration runs the
/// production `generate_delta` over the workload source against a pre-built
/// index. Throughput is reported in source bytes per second so the absolute
/// rate stays comparable across block sizes and workloads.
fn bench_matching_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("zsync_optimizations/match_throughput");

    for &workload in WORKLOADS {
        for &block_size in BLOCK_SIZES {
            let fx = fixture(workload, block_size);
            group.throughput(Throughput::Bytes(fx.source.len() as u64));
            let id = BenchmarkId::new(
                workload.label(),
                format!("bs{block_size}_{}MiB", CORPUS_SIZE / (1024 * 1024)),
            );
            group.bench_with_input(id, fx, |b, fx| {
                b.iter(|| {
                    let script = generate_delta(black_box(fx.source.as_slice()), &fx.index);
                    black_box(script)
                });
            });
        }
    }

    group.finish();
}

/// Entry-point wrapper that emits the counter summary once, before the
/// first Criterion sample, then delegates to the timing cell. The
/// fixtures are cached in a `OnceLock`, so the warmup is idempotent.
fn bench_with_summary(c: &mut Criterion) {
    static REPORTED: OnceLock<()> = OnceLock::new();
    REPORTED.get_or_init(warmup_and_report);
    bench_matching_throughput(c);
}

criterion_group!(
    name = benches;
    config = Criterion::default()
        .sample_size(10)
        .measurement_time(std::time::Duration::from_secs(5));
    targets = bench_with_summary
);

criterion_main!(benches);
