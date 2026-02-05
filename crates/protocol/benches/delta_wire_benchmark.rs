//! Benchmarks for delta encoding/decoding wire format performance.
//!
//! This benchmark suite measures critical paths in delta transfer operations:
//! - Token encoding (literal data, block matches)
//! - Token decoding
//! - DeltaOp stream encoding/decoding
//! - Whole-file delta encoding
//!
//! Run with: `cargo bench -p protocol -- delta_wire`

use std::io::Cursor;

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};

use protocol::wire::{
    DeltaOp, read_delta, read_delta_op, read_int, read_token, write_delta, write_delta_op,
    write_int, write_token_block_match, write_token_end, write_token_literal, write_token_stream,
    write_whole_file_delta,
};

// ============================================================================
// Test Data Generation
// ============================================================================

/// Generate random-ish data for benchmarking.
fn generate_data(size: usize) -> Vec<u8> {
    (0..size).map(|i| (i % 251) as u8).collect()
}

/// Generate a realistic delta operation stream.
///
/// Mix of literals and block matches typical in incremental sync.
fn generate_delta_ops(num_ops: usize, literal_size: usize) -> Vec<DeltaOp> {
    (0..num_ops)
        .map(|i| {
            if i % 3 == 0 {
                // Every third op is a literal (33% literals, 67% block matches)
                DeltaOp::Literal(generate_data(literal_size))
            } else {
                DeltaOp::Copy {
                    block_index: i as u32,
                    length: 8192,
                }
            }
        })
        .collect()
}

// ============================================================================
// Benchmark: Integer Encoding/Decoding (fundamental operation)
// ============================================================================

fn bench_int_encoding(c: &mut Criterion) {
    let mut group = c.benchmark_group("delta_int_encoding");

    // Test encoding throughput
    group.throughput(Throughput::Elements(10000));

    group.bench_function("write_int_10k", |b| {
        let mut buf = Vec::with_capacity(40000);
        b.iter(|| {
            buf.clear();
            for i in 0..10000_i32 {
                write_int(&mut buf, black_box(i)).unwrap();
            }
            black_box(&buf);
        });
    });

    group.bench_function("read_int_10k", |b| {
        // Prepare encoded data
        let mut encoded = Vec::with_capacity(40000);
        for i in 0..10000_i32 {
            write_int(&mut encoded, i).unwrap();
        }

        b.iter(|| {
            let mut cursor = Cursor::new(&encoded);
            let mut sum = 0i64;
            for _ in 0..10000 {
                sum += read_int(&mut cursor).unwrap() as i64;
            }
            black_box(sum)
        });
    });

    group.finish();
}

// ============================================================================
// Benchmark: Token Literal Encoding
// ============================================================================

fn bench_token_literal(c: &mut Criterion) {
    let mut group = c.benchmark_group("delta_token_literal");

    for size in [512, 1024, 4096, 32768, 131072] {
        let data = generate_data(size);

        group.throughput(Throughput::Bytes(size as u64));

        group.bench_with_input(BenchmarkId::new("write", size), &data, |b, data| {
            let mut buf = Vec::with_capacity(size + 16);
            b.iter(|| {
                buf.clear();
                write_token_literal(&mut buf, black_box(data)).unwrap();
                black_box(&buf);
            });
        });
    }

    group.finish();
}

// ============================================================================
// Benchmark: Token Block Match Encoding
// ============================================================================

fn bench_token_block_match(c: &mut Criterion) {
    let mut group = c.benchmark_group("delta_token_block_match");

    // Block matches are very small (4 bytes), measure throughput in elements
    group.throughput(Throughput::Elements(10000));

    group.bench_function("write_block_matches_10k", |b| {
        let mut buf = Vec::with_capacity(40000);
        b.iter(|| {
            buf.clear();
            for i in 0..10000_u32 {
                write_token_block_match(&mut buf, black_box(i)).unwrap();
            }
            black_box(&buf);
        });
    });

    group.finish();
}

// ============================================================================
// Benchmark: Whole File Delta (new file transfer)
// ============================================================================

fn bench_whole_file_delta(c: &mut Criterion) {
    let mut group = c.benchmark_group("delta_whole_file");

    for size in [4096, 65536, 1_048_576, 10_485_760] {
        let size_name = match size {
            4096 => "4KB",
            65536 => "64KB",
            1_048_576 => "1MB",
            10_485_760 => "10MB",
            _ => "unknown",
        };

        let data = generate_data(size);

        group.throughput(Throughput::Bytes(size as u64));

        group.bench_with_input(BenchmarkId::new("encode", size_name), &data, |b, data| {
            let mut buf = Vec::with_capacity(size + 1024);
            b.iter(|| {
                buf.clear();
                write_whole_file_delta(&mut buf, black_box(data)).unwrap();
                black_box(&buf);
            });
        });
    }

    group.finish();
}

// ============================================================================
// Benchmark: Token Stream Encoding (typical delta transfer)
// ============================================================================

fn bench_token_stream(c: &mut Criterion) {
    let mut group = c.benchmark_group("delta_token_stream");

    // Different scenarios
    for (name, num_ops, literal_size) in [
        ("small_delta", 10, 512),
        ("medium_delta", 100, 1024),
        ("large_delta", 1000, 4096),
    ] {
        let ops = generate_delta_ops(num_ops, literal_size);

        // Calculate total data size
        let total_bytes: usize = ops
            .iter()
            .map(|op| match op {
                DeltaOp::Literal(data) => data.len() + 4,
                DeltaOp::Copy { .. } => 4,
            })
            .sum();

        group.throughput(Throughput::Bytes(total_bytes as u64));

        group.bench_with_input(BenchmarkId::new("encode", name), &ops, |b, ops| {
            let mut buf = Vec::with_capacity(total_bytes + 1024);
            b.iter(|| {
                buf.clear();
                write_token_stream(&mut buf, black_box(ops)).unwrap();
                black_box(&buf);
            });
        });
    }

    group.finish();
}

// ============================================================================
// Benchmark: DeltaOp Encoding/Decoding
// ============================================================================

fn bench_delta_op_codec(c: &mut Criterion) {
    let mut group = c.benchmark_group("delta_op_codec");

    // Test individual op encoding
    let literal_data = generate_data(4096);
    let literal_op = DeltaOp::Literal(literal_data.clone());
    let copy_op = DeltaOp::Copy {
        block_index: 42,
        length: 8192,
    };

    group.bench_function("write_literal_4KB", |b| {
        let mut buf = Vec::with_capacity(4200);
        b.iter(|| {
            buf.clear();
            write_delta_op(&mut buf, black_box(&literal_op)).unwrap();
            black_box(&buf);
        });
    });

    group.bench_function("write_copy", |b| {
        let mut buf = Vec::with_capacity(16);
        b.iter(|| {
            buf.clear();
            write_delta_op(&mut buf, black_box(&copy_op)).unwrap();
            black_box(&buf);
        });
    });

    // Prepare encoded ops for reading benchmarks
    let mut literal_encoded = Vec::new();
    write_delta_op(&mut literal_encoded, &literal_op).unwrap();

    let mut copy_encoded = Vec::new();
    write_delta_op(&mut copy_encoded, &copy_op).unwrap();

    group.bench_function("read_literal_4KB", |b| {
        b.iter(|| {
            let mut cursor = Cursor::new(&literal_encoded);
            let op = read_delta_op(&mut cursor).unwrap();
            black_box(op)
        });
    });

    group.bench_function("read_copy", |b| {
        b.iter(|| {
            let mut cursor = Cursor::new(&copy_encoded);
            let op = read_delta_op(&mut cursor).unwrap();
            black_box(op)
        });
    });

    group.finish();
}

// ============================================================================
// Benchmark: Full Delta Roundtrip
// ============================================================================

fn bench_delta_roundtrip(c: &mut Criterion) {
    let mut group = c.benchmark_group("delta_roundtrip");

    for (name, num_ops, literal_size) in [
        ("small", 10, 512),
        ("medium", 100, 1024),
        ("large", 500, 4096),
    ] {
        let ops = generate_delta_ops(num_ops, literal_size);

        // Calculate estimated encoded size
        let total_bytes: usize = ops
            .iter()
            .map(|op| match op {
                DeltaOp::Literal(data) => data.len() + 16,
                DeltaOp::Copy { .. } => 16,
            })
            .sum();

        group.throughput(Throughput::Elements(num_ops as u64));

        // Encode
        group.bench_with_input(BenchmarkId::new("encode", name), &ops, |b, ops| {
            let mut buf = Vec::with_capacity(total_bytes);
            b.iter(|| {
                buf.clear();
                write_delta(&mut buf, black_box(ops)).unwrap();
                black_box(&buf);
            });
        });

        // Prepare encoded data for decode benchmark
        let mut encoded = Vec::with_capacity(total_bytes);
        write_delta(&mut encoded, &ops).unwrap();

        // Decode
        group.bench_with_input(BenchmarkId::new("decode", name), &encoded, |b, encoded| {
            b.iter(|| {
                let mut cursor = Cursor::new(black_box(encoded));
                let decoded = read_delta(&mut cursor).unwrap();
                black_box(decoded)
            });
        });
    }

    group.finish();
}

// ============================================================================
// Benchmark: Raw Token Reading (receiver hot path)
// ============================================================================

fn bench_token_reading(c: &mut Criterion) {
    let mut group = c.benchmark_group("delta_token_reading");

    // Prepare a stream of tokens (mixed literals and block matches)
    let num_tokens = 1000;
    let mut encoded = Vec::with_capacity(num_tokens * 100);

    for i in 0..num_tokens {
        if i % 3 == 0 {
            // Literal
            let data = generate_data(512);
            write_token_literal(&mut encoded, &data).unwrap();
        } else {
            // Block match
            write_token_block_match(&mut encoded, i as u32).unwrap();
        }
    }
    write_token_end(&mut encoded).unwrap();

    group.throughput(Throughput::Elements(num_tokens as u64));

    group.bench_function("read_mixed_tokens_1k", |b| {
        b.iter(|| {
            let mut cursor = Cursor::new(&encoded);
            let mut count = 0;
            while let Some(token) = read_token(&mut cursor).unwrap() {
                black_box(token);
                count += 1;
            }
            count
        });
    });

    group.finish();
}

// ============================================================================
// Criterion Groups
// ============================================================================

criterion_group!(
    name = delta_wire_benchmarks;
    config = Criterion::default()
        .sample_size(50)
        .measurement_time(std::time::Duration::from_secs(3));
    targets =
        bench_int_encoding,
        bench_token_literal,
        bench_token_block_match,
        bench_whole_file_delta,
        bench_token_stream,
        bench_delta_op_codec,
        bench_delta_roundtrip,
        bench_token_reading
);

criterion_main!(delta_wire_benchmarks);
