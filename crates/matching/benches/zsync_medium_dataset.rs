//! Match-index throughput on a medium (100 MiB / 5% modified) corpus (#2081).
//!
//! Tracks the zsync-style match-index path through the same rolling-hash +
//! strong-checksum pipeline production uses (`DeltaSignatureIndex` +
//! `DeltaGenerator::generate`). Three cells are reported:
//!
//! 1. `signature_build` - cost of walking the 100 MiB basis once to emit
//!    the rolling + strong block signatures the index is built from.
//! 2. `target_match_scan` - cost of scanning the 100 MiB target stream
//!    against a pre-built index, exercising the rolling-hash probe and
//!    strong-checksum verification on every candidate.
//! 3. `full_delta_round` - the combined signature-build + target-scan path,
//!    matching the end-to-end work a generator pays per file.
//!
//! # Corpus
//!
//! - Basis: 100 MiB deterministic content from a seeded splitmix64 PRNG.
//! - Target: basis with ~5% of bytes flipped at deterministic offsets.
//!   We pick the in-place flip variant rather than insertion / shift to
//!   keep block alignment stable so the match-index path itself is the
//!   dominant cost - this is what zsync's matched-block bitmap and the
//!   rolling probe were tuned against, and it matches the test corpora
//!   already used by `prune_duplicate_heavy.rs` and
//!   `bithash_rejection.rs`. The flip count is `0.05 * size` bytes; with
//!   ~6400 blocks per 100 MiB at the default block length, an even spread
//!   leaves a substantial fraction of blocks fully clean and exercises
//!   both the match-hit and false-alarm code paths.
//!
//! # Sample size
//!
//! Criterion sample size is held to 10 with a 5 s measurement window;
//! each iteration touches 100 MiB so the wall-clock budget per cell stays
//! well under the 5-minute-per-iteration ceiling on commodity hardware.
//!
//! Run with:
//! ```
//! cargo bench -p matching --bench zsync_medium_dataset
//! ```

use std::hint::black_box;
use std::num::NonZeroU8;
use std::sync::OnceLock;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};

use matching::{DeltaSignatureIndex, generate_delta};
use protocol::ProtocolVersion;
use signature::{
    SignatureAlgorithm, SignatureLayoutParams, calculate_signature_layout, generate_file_signature,
};

/// Corpus size: 100 MiB. Large enough to push the rolling-hash and
/// strong-checksum verification paths through L2/LLC, small enough that
/// each Criterion iteration completes well under five minutes.
const CORPUS_SIZE: usize = 100 * 1024 * 1024;

/// Fraction of basis bytes flipped to form the target.
const MODIFY_FRACTION: f64 = 0.05;

/// Deterministic seed for the basis PRNG.
const BASIS_SEED: u64 = 0xA5A5_5A5A_C0FF_EE00;

/// Deterministic seed for the modification PRNG.
const MODIFY_SEED: u64 = 0x9E37_79B9_7F4A_7C15;

/// Generates a deterministic pseudo-random byte stream of the requested
/// size from a 64-bit seed. Uses splitmix64 to keep the bench self-
/// contained: no `rand` dev-dependency, and the output is stable across
/// platforms.
fn deterministic_bytes(seed: u64, size: usize) -> Vec<u8> {
    let mut state = seed;
    let mut out = vec![0u8; size];
    for chunk in out.chunks_mut(8) {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        let bytes = z.to_le_bytes();
        chunk.copy_from_slice(&bytes[..chunk.len()]);
    }
    out
}

/// Flips `fraction` of the bytes in `data` at deterministic offsets.
/// In-place flip variant: byte at offset `p` becomes `data[p] ^ 0xFF`.
fn flip_in_place(data: &mut [u8], seed: u64, fraction: f64) {
    let count = (data.len() as f64 * fraction) as usize;
    if count == 0 || data.is_empty() {
        return;
    }
    let mut state = seed;
    for _ in 0..count {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        let pos = (z as usize) % data.len();
        data[pos] ^= 0xFF;
    }
}

/// Builds the default signature layout used by oc-rsync for a file of the
/// given length. Mirrors the helper used by the existing
/// `delta_matching_benchmark.rs` bench.
fn build_layout(len: u64) -> signature::SignatureLayout {
    let params = SignatureLayoutParams::new(
        len,
        None,
        ProtocolVersion::NEWEST,
        NonZeroU8::new(16).expect("non-zero"),
    );
    calculate_signature_layout(params).expect("layout")
}

/// Builds an index from a basis slice using MD4 as the strong checksum.
fn build_index(basis: &[u8]) -> DeltaSignatureIndex {
    let layout = build_layout(basis.len() as u64);
    let signature =
        generate_file_signature(basis, layout, SignatureAlgorithm::Md4).expect("signature");
    DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4).expect("index")
}

/// Shared corpus: built once and reused across all bench cells so the
/// 100 MiB allocation does not dominate per-iteration noise.
struct Corpus {
    basis: Vec<u8>,
    target: Vec<u8>,
    index: DeltaSignatureIndex,
}

fn corpus() -> &'static Corpus {
    static CACHE: OnceLock<Corpus> = OnceLock::new();
    CACHE.get_or_init(|| {
        let basis = deterministic_bytes(BASIS_SEED, CORPUS_SIZE);
        let mut target = basis.clone();
        flip_in_place(&mut target, MODIFY_SEED, MODIFY_FRACTION);
        let index = build_index(&basis);
        Corpus {
            basis,
            target,
            index,
        }
    })
}

/// Cell 1: cost of building basis signatures (rolling + strong over the
/// 100 MiB basis).
fn bench_signature_build(c: &mut Criterion) {
    let mut group = c.benchmark_group("zsync_medium/signature_build");
    let corpus = corpus();
    group.throughput(Throughput::Bytes(corpus.basis.len() as u64));
    group.bench_function("100MiB_md4", |b| {
        b.iter(|| {
            let layout = build_layout(corpus.basis.len() as u64);
            let sig = generate_file_signature(
                black_box(corpus.basis.as_slice()),
                layout,
                SignatureAlgorithm::Md4,
            );
            black_box(sig)
        });
    });
    group.finish();
}

/// Cell 2: cost of scanning the target against a pre-built match index.
/// This isolates the rolling-hash + strong-checksum match throughput from
/// the basis signature build.
fn bench_target_match_scan(c: &mut Criterion) {
    let mut group = c.benchmark_group("zsync_medium/target_match_scan");
    let corpus = corpus();
    group.throughput(Throughput::Bytes(corpus.target.len() as u64));
    group.bench_function("100MiB_5pct_flip", |b| {
        b.iter(|| {
            let script = generate_delta(black_box(corpus.target.as_slice()), &corpus.index);
            black_box(script)
        });
    });
    group.finish();
}

/// Cell 3: full delta-emission round - build the basis index from scratch
/// and immediately scan the target through it. This is the work a fresh
/// per-file transfer pays end to end.
fn bench_full_delta_round(c: &mut Criterion) {
    let mut group = c.benchmark_group("zsync_medium/full_delta_round");
    let corpus = corpus();
    // Throughput counts the target bytes that turn into copy / literal
    // tokens; the basis pass is amortised into the same wall-clock budget.
    group.throughput(Throughput::Bytes(corpus.target.len() as u64));
    group.bench_function("100MiB_5pct_flip", |b| {
        b.iter(|| {
            let index = build_index(black_box(corpus.basis.as_slice()));
            let script = generate_delta(black_box(corpus.target.as_slice()), &index);
            black_box(script)
        });
    });
    group.finish();
}

criterion_group!(
    name = benches;
    config = Criterion::default()
        .sample_size(10)
        .measurement_time(std::time::Duration::from_secs(5));
    targets =
        bench_signature_build,
        bench_target_match_scan,
        bench_full_delta_round
);

criterion_main!(benches);
