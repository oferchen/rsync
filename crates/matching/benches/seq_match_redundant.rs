//! Sequential-match throughput benchmark on highly redundant basis (#2067).
//!
//! Exercises the `want_i` adjacent-block hint path
//! (`crates/matching/src/index/mod.rs::check_block_match_slices` plus the
//! generator's match loop) on corpora where consecutive basis blocks are
//! the most likely next match. The benchmark plan binding in
//! `docs/design/zsync-seq-match.md` calls for log-file-style and
//! tar-archive-style corpora, plus a random control.
//!
//! Run with:
//! ```
//! cargo bench -p matching --features bench-internal --bench seq_match_redundant
//! ```
//!
//! # Corpora
//!
//! - `log_like`: 16 MiB of repeated newline-terminated log lines with a
//!   monotonic counter, mirroring an append-only log fixture.
//! - `tar_like`: 16 MiB of a small "record" block tiled with subtle
//!   per-record header deltas. Approximates a tar archive of nearly
//!   identical files without depending on the `tar` crate.
//! - `random_control`: 16 MiB of uncorrelated random bytes. Confirms
//!   the seq-match shortcut adds negligible overhead when it cannot fire.
//!
//! # What is measured
//!
//! For each corpus the bench reports:
//! - **Match throughput** of [`generate_delta`] when target = basis
//!   plus a handful of small mutations. This is the canonical seq-match
//!   hot path: long match streaks separated by short literal runs.
//! - **Token counts** (copy / literal) emitted as a one-shot stderr
//!   summary, so the seq-match win is observable in the bench output
//!   even without wired counters.

#![cfg(feature = "bench-internal")]

use std::hint::black_box;
use std::io::Cursor;
use std::num::NonZeroU8;
use std::sync::OnceLock;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

use matching::{DeltaScript, DeltaSignatureIndex, DeltaToken, generate_delta};
use protocol::ProtocolVersion;
use signature::{
    SignatureAlgorithm, SignatureLayoutParams, calculate_signature_layout, generate_file_signature,
};

const BASIS_SIZE: usize = 16 << 20;

/// Builds a log-file-style basis: repeated newline-terminated records
/// with a monotonic counter so the bytes are not literally identical
/// but the byte sequence is highly compressible.
fn log_like_basis(size: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(size + 128);
    let mut counter: u64 = 0;
    while out.len() < size {
        let secs = counter % 60;
        let millis = counter % 1000;
        let request_id = counter.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let line = format!(
            "2026-05-16T00:00:{secs:02}.{millis:03}Z INFO \
             request_id={request_id:016x} status=200 path=/api/items/{counter}\n",
        );
        out.extend_from_slice(line.as_bytes());
        counter = counter.wrapping_add(1);
    }
    out.truncate(size);
    out
}

/// Builds a tar-archive-style basis by tiling a 4 KiB record with a
/// per-record header delta. Approximates an archive of many nearly
/// identical small files without pulling in a tar crate.
fn tar_like_basis(size: usize) -> Vec<u8> {
    const RECORD: usize = 4096;
    let mut record_template = vec![0u8; RECORD];
    // Fill the body with a fixed pattern so consecutive records have
    // identical interiors but distinct headers.
    for (i, byte) in record_template.iter_mut().enumerate().skip(64) {
        *byte = ((i.wrapping_mul(31337)) ^ 0xAB) as u8;
    }
    let mut out = Vec::with_capacity(size + RECORD);
    let mut record_index: u64 = 0;
    while out.len() < size {
        let header = format!("file{record_index:010}.bin\0size=0000004096\0mtime=1700000000\0");
        let mut record = record_template.clone();
        let header_bytes = header.as_bytes();
        let copy_len = header_bytes.len().min(RECORD);
        record[..copy_len].copy_from_slice(&header_bytes[..copy_len]);
        out.extend_from_slice(&record);
        record_index = record_index.wrapping_add(1);
    }
    out.truncate(size);
    out
}

/// Builds an uncorrelated random basis as a regression control.
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

/// Produces a target that is mostly identical to `basis` with a few
/// small mutations at evenly spaced offsets. This is the canonical
/// "append-only with patches" workload that exercises long seq-match
/// streaks broken by short literal runs.
fn mutate_basis(basis: &[u8], num_patches: usize, patch_size: usize) -> Vec<u8> {
    let mut out = basis.to_vec();
    if num_patches == 0 || basis.is_empty() {
        return out;
    }
    let spacing = basis.len() / (num_patches + 1);
    for k in 0..num_patches {
        let start = (k + 1) * spacing;
        let end = (start + patch_size).min(out.len());
        if start >= end {
            continue;
        }
        for byte in &mut out[start..end] {
            *byte = byte.wrapping_add(0x5A);
        }
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
        // 8 patches of 256 bytes each: roughly 0.012% perturbation,
        // matching the "append-only with small edits" workload class.
        let target = mutate_basis(&basis, 8, 256);
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
            Fixture::new("log_like", log_like_basis(BASIS_SIZE)),
            Fixture::new("tar_like", tar_like_basis(BASIS_SIZE)),
            Fixture::new(
                "random_control",
                random_basis(0xC0FF_EE00_DEAD_BEEF, BASIS_SIZE),
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

fn report_token_summary() {
    eprintln!(
        "seq_match summary (corpus,basis_bytes,target_bytes,copy_tokens,literal_tokens,\
         literal_bytes,copy_bytes_estimated)"
    );
    for fx in fixtures() {
        let script = generate_delta(Cursor::new(&fx.target), &fx.index).expect("generate");
        let (copies, literals, literal_bytes, copy_bytes) = count_tokens(&script);
        eprintln!(
            "seq_match[{label}] basis={bsz} target={tsz} copy={cp} lit={lt} lit_bytes={lb} \
             copy_bytes={cb}",
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

fn bench_match_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("seq_match_throughput");
    group.sample_size(10);

    for fx in fixtures() {
        group.throughput(Throughput::Bytes(fx.target.len() as u64));
        group.bench_with_input(BenchmarkId::new("generate_delta", fx.label), fx, |b, fx| {
            b.iter(|| {
                let script =
                    generate_delta(Cursor::new(black_box(&fx.target)), &fx.index).expect("script");
                black_box(script)
            });
        });
    }

    group.finish();
}

fn bench_seq_match_entry(c: &mut Criterion) {
    report_token_summary();
    bench_match_throughput(c);
}

criterion_group!(
    name = benches;
    config = Criterion::default()
        .sample_size(10)
        .measurement_time(std::time::Duration::from_secs(5));
    targets = bench_seq_match_entry
);

criterion_main!(benches);
