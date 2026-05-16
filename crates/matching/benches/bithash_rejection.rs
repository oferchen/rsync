//! Bithash prefilter rejection-rate benchmark (#2063).
//!
//! Measures the rejection rate of the bithash prefilter described in
//! `docs/design/zsync-bithash.md` section 7. The bithash is a one-sided
//! filter sized to roughly 8x the rsum-bucket count, so its design target
//! is a rejection rate of `~0.875` on uniform-random misses.
//!
//! Run with:
//! ```
//! cargo bench -p matching --features bench-internal --bench bithash_rejection
//! ```
//!
//! # What this benchmark reports
//!
//! Two complementary quantities, both written to stdout before the
//! Criterion timing tables:
//!
//! 1. **Static utilization** of the bithash after the basis is indexed.
//!    Reported as the fraction of set bits in the bit array; rejection
//!    on uniform-random probes converges to `1.0 - utilization`.
//! 2. **Observed rejection rate** when scanning a *target* file across
//!    the basis at one-byte-aligned offsets. Two target classes drive
//!    the matrix:
//!    - `target = basis` (sequential 100% match): rejection rate is
//!      close to 0 because most rolling-hash positions land on an
//!      indexed block.
//!    - `target = random` (100% miss): rejection rate is dominated by
//!      the bithash and converges to the design 7/8 bound at large
//!      block counts.
//!
//! # Methodology notes
//!
//! - Basis sizes: 1 MiB, 16 MiB, 128 MiB. Default block size.
//! - Probes are computed via [`RollingDigest::from_bytes`] over a
//!   contiguous slice of the target at the current cursor; this matches
//!   the rolling-hash advance pattern in
//!   `crates/matching/src/generator.rs` without re-running the full
//!   delta loop.
//! - The "post-tag" qualifier in the design doc means probes that pass
//!   the `tag_table` gate. The bench reports rejection both unconditionally
//!   and post-tag, using the [`DeltaSignatureIndex::tag_admits`] and
//!   [`DeltaSignatureIndex::bithash_admits`] accessors exposed under
//!   `bench-internal`.

#![cfg(feature = "bench-internal")]

use std::hint::black_box;
use std::num::NonZeroU8;
use std::sync::OnceLock;

use checksums::RollingDigest;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

use matching::DeltaSignatureIndex;
use protocol::ProtocolVersion;
use signature::{
    SignatureAlgorithm, SignatureLayoutParams, calculate_signature_layout, generate_file_signature,
};

const BASIS_SIZES: &[(usize, &str)] = &[
    (1 << 20, "1MiB"),
    (16 << 20, "16MiB"),
    (128 << 20, "128MiB"),
];

/// Deterministic pseudo-random byte sequence keyed by `seed`.
///
/// Uses a 64-bit LCG so the bench is reproducible without a `rand`
/// dependency. The mixer is the same SplitMix64 step used in the wider
/// workspace test helpers.
fn pseudo_random_bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut state = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut out = vec![0u8; len];
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

/// Deterministic byte sequence keyed by a small mixer so the basis is
/// not all-zero (which would make the bithash trivial).
fn structured_basis(seed: u64, len: usize) -> Vec<u8> {
    // Mix two streams so the basis has visible structure (the low byte
    // moves slowly, the high byte fast) without being purely random.
    let mut out = vec![0u8; len];
    let mut a = seed.wrapping_add(0xDEAD_BEEF);
    for (i, byte) in out.iter_mut().enumerate() {
        a = a.wrapping_mul(0x5DEE_CE66D).wrapping_add(0xB);
        *byte = ((a >> 16) ^ (i as u64)) as u8;
    }
    out
}

fn build_index(data: &[u8]) -> DeltaSignatureIndex {
    let params = SignatureLayoutParams::new(
        data.len() as u64,
        None,
        ProtocolVersion::NEWEST,
        NonZeroU8::new(16).expect("non-zero"),
    );
    let layout = calculate_signature_layout(params).expect("layout");
    let signature =
        generate_file_signature(data, layout, SignatureAlgorithm::Md4).expect("signature");
    DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4).expect("index")
}

/// Cached basis + index per size, so each Criterion sample reuses the
/// same expensive setup. Static utilization is computed once and reported
/// the first time a size is touched.
fn fixture_for(size: usize, label: &str) -> &'static Fixture {
    static CACHE: OnceLock<Vec<(usize, Fixture)>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| {
        BASIS_SIZES
            .iter()
            .map(|(sz, lbl)| (*sz, Fixture::new(*sz, lbl)))
            .collect()
    });
    &cache
        .iter()
        .find(|(sz, _)| *sz == size)
        .unwrap_or_else(|| panic!("missing fixture for {label}"))
        .1
}

struct Fixture {
    basis: Vec<u8>,
    target_random: Vec<u8>,
    index: DeltaSignatureIndex,
    block_len: usize,
    utilization: f64,
}

impl Fixture {
    fn new(size: usize, label: &str) -> Self {
        let basis = structured_basis(0xA5A5_A5A5_A5A5_A5A5, size);
        let target_random = pseudo_random_bytes(0x1234_5678_DEAD_BEEF, size);
        let index = build_index(&basis);
        let block_len = index.block_length();
        let block_count = index.block_count();
        let utilization = index.bithash_utilization();
        eprintln!(
            "bithash[{label}]: blocks={block_count} block_len={block_len} \
             utilization={utilization:.4} predicted_rejection_uniform={pred:.4}",
            pred = 1.0 - utilization,
        );
        Self {
            basis,
            target_random,
            index,
            block_len,
            utilization,
        }
    }
}

/// Walks `target` at single-byte strides counting tag-admit, bithash-admit,
/// and final probe outcomes. Returns `(probes, tag_admits, bithash_admits,
/// full_matches)`.
///
/// `stride` lets the caller subsample large basis sizes so the bench
/// stays under Criterion's measurement budget; the rejection ratios are
/// stride-invariant under uniform sampling.
fn count_admits(
    target: &[u8],
    index: &DeltaSignatureIndex,
    block_len: usize,
    stride: usize,
) -> (u64, u64, u64, u64) {
    if target.len() < block_len {
        return (0, 0, 0, 0);
    }
    let mut probes = 0u64;
    let mut tag_admits = 0u64;
    let mut bithash_admits = 0u64;
    let mut full_matches = 0u64;
    let last = target.len() - block_len;
    let mut pos = 0usize;
    while pos <= last {
        let window = &target[pos..pos + block_len];
        let digest = RollingDigest::from_bytes(window);
        probes += 1;
        if index.tag_admits(digest.sum1()) {
            tag_admits += 1;
            if index.bithash_admits(digest.value()) {
                bithash_admits += 1;
                if index.find_match_bytes(digest, window).is_some() {
                    full_matches += 1;
                }
            }
        }
        pos += stride;
    }
    (probes, tag_admits, bithash_admits, full_matches)
}

fn bench_static_rejection(c: &mut Criterion) {
    let mut group = c.benchmark_group("bithash_static");

    for (size, label) in BASIS_SIZES {
        let fixture = fixture_for(*size, label);
        group.bench_with_input(
            BenchmarkId::new("utilization_probe", label),
            &fixture.utilization,
            |b, util| {
                b.iter(|| black_box(*util));
            },
        );
    }

    group.finish();
}

fn bench_observed_rejection(c: &mut Criterion) {
    let mut group = c.benchmark_group("bithash_observed");
    group.sample_size(10);

    for (size, label) in BASIS_SIZES {
        let fixture = fixture_for(*size, label);
        // Stride caps the per-iteration work for the 128 MiB case so the
        // bench stays inside the default Criterion measurement budget.
        let stride = match *size {
            n if n <= 1 << 20 => 1,
            n if n <= 16 << 20 => 16,
            _ => 256,
        };

        // 100% match path: target == basis. The rolling hash hits an
        // indexed block at every offset, so rejection is near zero.
        group.throughput(Throughput::Bytes(
            fixture.basis.len() as u64 / stride as u64,
        ));
        group.bench_with_input(
            BenchmarkId::new("match_basis_equals_target", label),
            &(fixture, stride),
            |b, (fx, stride)| {
                b.iter(|| {
                    let (probes, tag, bit, full) =
                        count_admits(&fx.basis, &fx.index, fx.block_len, *stride);
                    black_box((probes, tag, bit, full));
                });
            },
        );

        // 100% miss path: target is uncorrelated random data. Most
        // probes hit the tag table by chance (~1/2 for 16-bit s1) but
        // the bithash should reject ~7/8 of the survivors.
        group.bench_with_input(
            BenchmarkId::new("miss_random_target", label),
            &(fixture, stride),
            |b, (fx, stride)| {
                b.iter(|| {
                    let (probes, tag, bit, full) =
                        count_admits(&fx.target_random, &fx.index, fx.block_len, *stride);
                    black_box((probes, tag, bit, full));
                });
            },
        );
    }

    group.finish();
}

/// Reports observed rejection rate as a one-shot stderr emission so the
/// bench output captures concrete numbers without relying on Criterion's
/// throughput tables. Runs once per basis size before the Criterion
/// measurement loop.
fn report_rejection_summary() {
    eprintln!(
        "bithash rejection summary (size,target,probes,tag_admits,bithash_admits,full_matches,\
         tag_admit_rate,post_tag_bithash_reject_rate)"
    );
    for (size, label) in BASIS_SIZES {
        let fixture = fixture_for(*size, label);
        let stride = match *size {
            n if n <= 1 << 20 => 1,
            n if n <= 16 << 20 => 16,
            _ => 256,
        };
        for (kind, data) in [
            ("basis", fixture.basis.as_slice()),
            ("random", fixture.target_random.as_slice()),
        ] {
            let (probes, tag, bit, full) =
                count_admits(data, &fixture.index, fixture.block_len, stride);
            let tag_rate = if probes == 0 {
                0.0
            } else {
                tag as f64 / probes as f64
            };
            let post_tag_reject = if tag == 0 {
                0.0
            } else {
                1.0 - (bit as f64 / tag as f64)
            };
            eprintln!(
                "bithash[{label},{kind}] probes={probes} tag={tag} bithash={bit} full={full} \
                 tag_admit_rate={tag_rate:.4} post_tag_reject={post_tag_reject:.4}"
            );
        }
    }
}

/// Wraps the Criterion entry point so the rejection summary lands
/// before the timing tables. `report_rejection_summary` is idempotent
/// because the fixtures are cached in a `OnceLock`.
fn bench_rejection_with_summary(c: &mut Criterion) {
    report_rejection_summary();
    bench_static_rejection(c);
    bench_observed_rejection(c);
}

criterion_group!(
    name = benches;
    config = Criterion::default()
        .sample_size(10)
        .measurement_time(std::time::Duration::from_secs(5));
    targets = bench_rejection_with_summary
);

criterion_main!(benches);
