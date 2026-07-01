//! Parallel vs sequential file-signature generation benchmark.
//!
//! Validates the SMP Adler32 + strong-checksum signature path that the
//! network-receiver basis-signature step gates behind `--checksum-threads`.
//! The windowed generator ([`generate_file_signature_windowed`]) fans the
//! per-block rolling+strong checksums across the rayon pool; the sequential
//! generator ([`generate_file_signature`]) hashes them on one core. Both emit
//! byte-identical [`FileSignature`] output - this bench measures only the
//! wall-clock/throughput difference on multi-core hardware.
//!
//! # Sizes
//!
//! - **64 KiB** - below `PARALLEL_THRESHOLD_BYTES` (256 KiB). The production
//!   path stays sequential here, so the windowed generator must NOT regress
//!   meaningfully relative to sequential at this size.
//! - **256 MiB** - many blocks, well above threshold. This is where SMP-across
//!   -blocks should show a real multi-core speedup.
//!
//! Run with `cargo bench -p signature`.

use std::hint::black_box;
use std::io::Cursor;
use std::num::NonZeroU8;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

use protocol::ProtocolVersion;
use signature::parallel::generate_file_signature_windowed;
use signature::{
    SignatureAlgorithm, SignatureLayoutParams, calculate_signature_layout, generate_file_signature,
};

/// Deterministic, non-trivial bytes so each block hashes distinctly.
fn build_buffer(size: usize) -> Vec<u8> {
    (0..size).map(|i| (i % 251) as u8).collect()
}

/// The two representative sizes: a below-threshold small file that must not
/// regress, and a large many-block file where SMP should pay off.
const SIZES: &[(&str, usize)] = &[("64KiB", 64 * 1024), ("256MiB", 256 * 1024 * 1024)];

fn bench_signature(c: &mut Criterion) {
    // MD5 is a representative CPU-intensive strong checksum. XXH3 is much
    // cheaper per block; MD5 better reflects the single-core hot path the
    // parallelization targets.
    let algorithm = SignatureAlgorithm::Md5 {
        seed_config: checksums::strong::Md5Seed::none(),
    };

    let mut group = c.benchmark_group("file_signature");
    for &(label, size) in SIZES {
        let data = build_buffer(size);
        let layout = calculate_signature_layout(SignatureLayoutParams::new(
            size as u64,
            None,
            ProtocolVersion::NEWEST,
            NonZeroU8::new(16).expect("16 is non-zero"),
        ))
        .expect("layout");

        group.throughput(Throughput::Bytes(size as u64));

        group.bench_with_input(BenchmarkId::new("sequential", label), &data, |b, data| {
            b.iter(|| {
                let sig = generate_file_signature(Cursor::new(data.as_slice()), layout, algorithm)
                    .expect("sequential signature");
                black_box(sig);
            });
        });

        group.bench_with_input(BenchmarkId::new("windowed", label), &data, |b, data| {
            b.iter(|| {
                let sig = generate_file_signature_windowed(
                    Cursor::new(data.as_slice()),
                    layout,
                    algorithm,
                )
                .expect("windowed signature");
                black_box(sig);
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_signature);
criterion_main!(benches);
