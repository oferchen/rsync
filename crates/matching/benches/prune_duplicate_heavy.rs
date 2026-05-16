//! Prune-step throughput benchmark on duplicate-heavy basis (#2071).
//!
//! Exercises the matched-block pruning bitmap described in
//! `docs/design/zsync-prune.md` on a VM-disk-image-style corpus where a
//! large fraction of basis blocks are duplicate zero-filled blocks. With
//! prune enabled, an emitted COPY for a zero block sets a bit so later
//! probes against the same zero pattern skip the duplicate sibling.
//!
//! Run with:
//! ```
//! cargo bench -p matching --features bench-internal --bench prune_duplicate_heavy
//! ```
//!
//! # Corpora
//!
//! - `zero_heavy_16MiB`: 16 MiB basis with ~70% zero-block extents,
//!   approximating a sparse VM disk image. The remaining 30% is
//!   structured non-zero data so the matcher is exercised beyond the
//!   zero-block fast path.
//! - `repeated_blocks_16MiB`: 16 MiB basis with a 4 KiB pattern tiled
//!   `N` times, modelling the parent design's "synthetic 50% duplicate
//!   density" corpus class scaled to a bench-friendly size.
//! - `no_duplicates_16MiB`: 16 MiB of uncorrelated random bytes as the
//!   parent design's zero-regression control.
//!
//! # What is measured
//!
//! For each corpus the bench runs [`DeltaGenerator::generate`] with the
//! pruning bitmap enabled (production default) and disabled (via the
//! bench-only [`DeltaGenerator::with_prune_matched`] toggle exposed
//! under `bench-internal`). The Criterion output reports wall-clock
//! match throughput for both configurations side by side, and a one-shot
//! stderr summary names the copy / literal byte split for each
//! configuration so the wire-equality of the output is observable.

#![cfg(feature = "bench-internal")]

use std::hint::black_box;
use std::io::Cursor;
use std::num::NonZeroU8;
use std::sync::OnceLock;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

use matching::{DeltaGenerator, DeltaScript, DeltaSignatureIndex, DeltaToken};
use protocol::ProtocolVersion;
use signature::{
    SignatureAlgorithm, SignatureLayoutParams, calculate_signature_layout, generate_file_signature,
};

const BASIS_SIZE: usize = 16 << 20;

/// Builds a VM-disk-image-style basis: roughly `zero_fraction` of the
/// bytes are zero-filled extents, the rest is structured non-zero data.
/// The zero extents are large enough (multiple block lengths) that
/// consecutive basis blocks share identical content and therefore exercise
/// the duplicate-sibling code path the prune step targets.
fn zero_heavy_basis(seed: u64, size: usize, zero_fraction: f64) -> Vec<u8> {
    let zero_bytes = (size as f64 * zero_fraction) as usize;
    let mut out = vec![0u8; size];
    // Fill the tail with structured non-zero data.
    let mut state = seed.wrapping_add(0xA5A5_A5A5_A5A5_A5A5);
    for byte in out.iter_mut().skip(zero_bytes) {
        state = state.wrapping_mul(0x5DEE_CE66D).wrapping_add(0xB);
        // Bias the byte away from zero so the prune step is the dominant
        // duplicate source rather than incidental zeros in the tail.
        let b = ((state >> 16) | 1) as u8;
        *byte = b;
    }
    out
}

/// Builds a basis with a single 4 KiB pattern tiled across the file,
/// producing maximum duplicate density.
fn repeated_blocks_basis(size: usize) -> Vec<u8> {
    const PATTERN: usize = 4096;
    let mut pattern = vec![0u8; PATTERN];
    for (i, byte) in pattern.iter_mut().enumerate() {
        *byte = ((i.wrapping_mul(2654435761)) ^ 0xC3) as u8;
    }
    let mut out = Vec::with_capacity(size + PATTERN);
    while out.len() < size {
        out.extend_from_slice(&pattern);
    }
    out.truncate(size);
    out
}

/// Builds an uncorrelated random basis as the zero-regression control.
fn random_basis(seed: u64, size: usize) -> Vec<u8> {
    let mut state = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
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

struct Fixture {
    label: &'static str,
    basis: Vec<u8>,
    target: Vec<u8>,
    index: DeltaSignatureIndex,
}

impl Fixture {
    fn new(label: &'static str, basis: Vec<u8>) -> Self {
        // Target = basis (same content). Duplicate-heavy match where every
        // probe at a duplicate position has many candidate siblings.
        let target = basis.clone();
        let index = build_index(&basis);
        Self {
            label,
            basis,
            target,
            index,
        }
    }
}

fn fixtures() -> &'static [Fixture] {
    static CACHE: OnceLock<Vec<Fixture>> = OnceLock::new();
    CACHE.get_or_init(|| {
        vec![
            Fixture::new(
                "zero_heavy_16MiB",
                zero_heavy_basis(0xDEAD_BEEF_CAFE_BABE, BASIS_SIZE, 0.7),
            ),
            Fixture::new("repeated_blocks_16MiB", repeated_blocks_basis(BASIS_SIZE)),
            Fixture::new(
                "no_duplicates_16MiB",
                random_basis(0xC0FF_EE00_FACE_F00D, BASIS_SIZE),
            ),
        ]
    })
}

fn count_tokens(script: &DeltaScript) -> (u64, u64, u64, u64) {
    let mut copies = 0u64;
    let mut copy_bytes = 0u64;
    let mut literals = 0u64;
    let mut literal_bytes = 0u64;
    for token in script.tokens() {
        match token {
            DeltaToken::Copy { len, .. } => {
                copies += 1;
                copy_bytes += *len as u64;
            }
            DeltaToken::Literal(bytes) => {
                literals += 1;
                literal_bytes += bytes.len() as u64;
            }
        }
    }
    (copies, literals, literal_bytes, copy_bytes)
}

fn run(fx: &Fixture, prune: bool) -> DeltaScript {
    DeltaGenerator::new()
        .with_prune_matched(prune)
        .generate(Cursor::new(&fx.target), &fx.index)
        .expect("generate")
}

fn report_summary() {
    eprintln!(
        "prune summary (corpus,prune,basis_bytes,target_bytes,copy_tokens,literal_tokens,\
         literal_bytes,copy_bytes)"
    );
    for fx in fixtures() {
        for prune in [false, true] {
            let script = run(fx, prune);
            let (copies, literals, literal_bytes, copy_bytes) = count_tokens(&script);
            eprintln!(
                "prune[{label},prune={prune}] basis={bsz} target={tsz} copy={cp} lit={lt} \
                 lit_bytes={lb} copy_bytes={cb}",
                label = fx.label,
                bsz = fx.basis.len(),
                tsz = fx.target.len(),
                cp = copies,
                lt = literals,
                lb = literal_bytes,
                cb = copy_bytes,
            );
        }
    }
}

fn bench_prune_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("prune_throughput");
    group.sample_size(10);

    for fx in fixtures() {
        group.throughput(Throughput::Bytes(fx.target.len() as u64));

        group.bench_with_input(BenchmarkId::new("no_prune", fx.label), fx, |b, fx| {
            b.iter(|| {
                let script = run(fx, false);
                black_box(script)
            });
        });

        group.bench_with_input(BenchmarkId::new("prune", fx.label), fx, |b, fx| {
            b.iter(|| {
                let script = run(fx, true);
                black_box(script)
            });
        });
    }

    group.finish();
}

fn bench_prune_entry(c: &mut Criterion) {
    report_summary();
    bench_prune_throughput(c);
}

criterion_group!(
    name = benches;
    config = Criterion::default()
        .sample_size(10)
        .measurement_time(std::time::Duration::from_secs(5));
    targets = bench_prune_entry
);

criterion_main!(benches);
