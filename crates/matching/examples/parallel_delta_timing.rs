//! Direct wall-clock timing for the parallel sender-scan delta generator.
//!
//! Bypasses criterion's adaptive iteration estimate (which explodes when one
//! variant is anomalously slow) and times single `generate` / `generate_chunked`
//! calls. Prints throughput plus token/literal counts so a chunk that stops
//! matching (literal blow-up) is immediately visible.
//!
//! Run with `RAYON_NUM_THREADS=N cargo run -p matching --release --example parallel_delta_timing`.

use std::hint::black_box;
use std::io::Cursor;
use std::num::NonZeroU8;
use std::time::Instant;

use matching::{DeltaGenerator, DeltaSignatureIndex};
use protocol::ProtocolVersion;
use signature::{
    SignatureAlgorithm, SignatureLayoutParams, calculate_signature_layout, generate_file_signature,
};

fn pseudo_random(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed | 1;
    let mut out = Vec::with_capacity(len);
    for _ in 0..len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.push((state & 0xff) as u8);
    }
    out
}

fn build_index(data: &[u8]) -> DeltaSignatureIndex {
    let params = SignatureLayoutParams::new(
        data.len() as u64,
        None,
        ProtocolVersion::NEWEST,
        NonZeroU8::new(16).unwrap(),
    );
    let layout = calculate_signature_layout(params).expect("layout");
    let signature =
        generate_file_signature(data, layout, SignatureAlgorithm::Md4).expect("signature");
    DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4).expect("index")
}

fn scattered_edits(size: usize, seed: u64, change_count: usize) -> Vec<u8> {
    let mut data = pseudo_random(size, seed);
    if change_count > 0 {
        let spacing = size / change_count;
        for i in 0..change_count {
            data[(i * spacing).min(size - 1)] ^= 0xff;
        }
    }
    data
}

/// Returns (best_seconds, tokens, literal_bytes) over 3 reps.
fn time_it(label: &str, size: usize, mut f: impl FnMut() -> (usize, u64)) {
    let mut best = f64::INFINITY;
    let mut tokens = 0usize;
    let mut literal = 0u64;
    for _ in 0..3 {
        let start = Instant::now();
        let (t, l) = black_box(f());
        let secs = start.elapsed().as_secs_f64();
        if secs < best {
            best = secs;
            tokens = t;
            literal = l;
        }
    }
    let mibs = (size as f64 / (1024.0 * 1024.0)) / best;
    println!(
        "  {label:<12} {best:8.3}s  {mibs:8.1} MiB/s  tokens={tokens:>8} literal_bytes={literal}",
    );
}

fn main() {
    const SIZE: usize = 128 * 1024 * 1024; // 128 MiB
    let basis = pseudo_random(SIZE, 0x1234_5678);
    let index = build_index(&basis);
    let source = scattered_edits(SIZE, 0x1234_5678, 32);
    let generator = DeltaGenerator::new();

    println!(
        "threads={}  size={} MiB  block_len={}",
        rayon::current_num_threads(),
        SIZE / (1024 * 1024),
        index.block_length(),
    );

    // Warm the page cache / allocator.
    let _ = generator
        .generate(Cursor::new(source.as_slice()), &index)
        .expect("warm");

    time_it("sequential", SIZE, || {
        let s = generator
            .generate(Cursor::new(source.as_slice()), &index)
            .expect("seq");
        (s.tokens().len(), s.literal_bytes())
    });

    for n in [2usize, 4, 8] {
        time_it(&format!("chunked/{n}"), SIZE, || {
            let s = generator
                .generate_chunked(source.as_slice(), &index, n)
                .expect("chunked");
            (s.tokens().len(), s.literal_bytes())
        });
    }
}
