//! Benchmarks for wire protocol encoding/decoding throughput.
//!
//! This benchmark suite measures performance of fundamental wire protocol primitives:
//! - Varint encode/decode (single values and batches)
//! - File entry encode/decode via FileListWriter/FileListReader
//! - Multiplex frame encode/decode (MessageHeader)
//! - Delta token encode/decode (DeltaOp)
//!
//! Run with: `cargo bench -p protocol -- wire_protocol`

use std::io::Cursor;
use std::path::PathBuf;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;

use protocol::flist::{FileEntry, FileListReader, FileListWriter};
use protocol::wire::{
    DeltaOp, read_delta_op, write_delta_op, write_token_block_match, write_token_end,
    write_token_literal,
};
use protocol::{
    MessageCode, MessageHeader, ProtocolVersion, decode_varint, encode_varint_to_vec, read_varint,
    read_varlong, recv_msg, send_msg, write_varint, write_varlong,
};

// ---------------------------------------------------------------------------
// Varint encode/decode
// ---------------------------------------------------------------------------

fn bench_varint_encode_single(c: &mut Criterion) {
    let mut group = c.benchmark_group("wire_varint_encode_single");

    let values: &[(&str, i32)] = &[
        ("tiny_42", 42),
        ("1byte_max_127", 127),
        ("2byte_1000", 1000),
        ("3byte_100000", 100_000),
        ("5byte_max", i32::MAX),
        ("negative", -1),
    ];

    for &(name, value) in values {
        group.bench_function(name, |b| {
            let mut buf = Vec::with_capacity(8);
            b.iter(|| {
                buf.clear();
                write_varint(&mut buf, black_box(value)).unwrap();
                black_box(&buf);
            });
        });
    }

    group.finish();
}

fn bench_varint_decode_single(c: &mut Criterion) {
    let mut group = c.benchmark_group("wire_varint_decode_single");

    let values: &[(&str, i32)] = &[
        ("tiny_42", 42),
        ("1byte_max_127", 127),
        ("2byte_1000", 1000),
        ("3byte_100000", 100_000),
        ("5byte_max", i32::MAX),
    ];

    for &(name, value) in values {
        let mut encoded = Vec::new();
        encode_varint_to_vec(value, &mut encoded);

        group.bench_function(name, |b| {
            b.iter(|| {
                let (decoded, _rest) = decode_varint(black_box(&encoded)).unwrap();
                black_box(decoded)
            });
        });
    }

    group.finish();
}

fn bench_varint_encode_batch(c: &mut Criterion) {
    let mut group = c.benchmark_group("wire_varint_encode_batch");
    group.throughput(Throughput::Elements(1000));

    // Mix of value sizes typical in protocol exchanges
    let values: Vec<i32> = (0..1000)
        .map(|i| match i % 4 {
            0 => i % 128,           // 1-byte values
            1 => 128 + i * 7,       // 2-byte values
            2 => 100_000 + i * 100, // 3-byte values
            _ => i32::MAX - i,      // 5-byte values
        })
        .collect();

    group.bench_function("write_1k_mixed", |b| {
        let mut buf = Vec::with_capacity(5000);
        b.iter(|| {
            buf.clear();
            for &v in black_box(&values) {
                write_varint(&mut buf, v).unwrap();
            }
            black_box(&buf);
        });
    });

    group.finish();
}

fn bench_varint_decode_batch(c: &mut Criterion) {
    let mut group = c.benchmark_group("wire_varint_decode_batch");
    group.throughput(Throughput::Elements(1000));

    let values: Vec<i32> = (0..1000)
        .map(|i| match i % 4 {
            0 => i % 128,
            1 => 128 + i * 7,
            2 => 100_000 + i * 100,
            _ => i32::MAX - i,
        })
        .collect();

    let mut encoded = Vec::with_capacity(5000);
    for &v in &values {
        write_varint(&mut encoded, v).unwrap();
    }

    group.bench_function("read_1k_mixed", |b| {
        b.iter(|| {
            let mut cursor = Cursor::new(black_box(&encoded));
            let mut sum = 0i64;
            for _ in 0..1000 {
                sum += read_varint(&mut cursor).unwrap() as i64;
            }
            black_box(sum)
        });
    });

    group.finish();
}

fn bench_varlong_roundtrip(c: &mut Criterion) {
    let mut group = c.benchmark_group("wire_varlong_roundtrip");
    group.throughput(Throughput::Elements(1000));

    // File sizes and timestamps - typical varlong payloads
    let values: Vec<i64> = (0..1000)
        .map(|i| match i % 3 {
            0 => i as i64 * 1024,                // small file sizes
            1 => (i as i64 + 1) * 1_048_576,     // MB-range file sizes
            _ => 1_700_000_000 + i as i64 * 100, // timestamps
        })
        .collect();

    group.bench_function("write_1k", |b| {
        let mut buf = Vec::with_capacity(8000);
        b.iter(|| {
            buf.clear();
            for &v in black_box(&values) {
                write_varlong(&mut buf, v, 3).unwrap();
            }
            black_box(&buf);
        });
    });

    let mut encoded = Vec::with_capacity(8000);
    for &v in &values {
        write_varlong(&mut encoded, v, 3).unwrap();
    }

    group.bench_function("read_1k", |b| {
        b.iter(|| {
            let mut cursor = Cursor::new(black_box(&encoded));
            let mut sum = 0i64;
            for _ in 0..1000 {
                sum = sum.wrapping_add(read_varlong(&mut cursor, 3).unwrap());
            }
            black_box(sum)
        });
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// File entry encode/decode
// ---------------------------------------------------------------------------

/// Generate a realistic file entry for benchmarking.
fn make_file_entry(index: usize) -> FileEntry {
    let depth = (index % 5) + 1;
    let path: PathBuf = (0..depth)
        .map(|d| format!("dir_{}", (index + d) % 100))
        .collect::<PathBuf>()
        .join(format!("file_{index}.txt"));
    FileEntry::new_file(path, (index * 1024) as u64, 0o644)
}

fn bench_file_entry_encode_single(c: &mut Criterion) {
    let mut group = c.benchmark_group("wire_file_entry_encode_single");

    let entry = make_file_entry(42);

    group.bench_function("single_entry", |b| {
        let mut buf = Vec::with_capacity(256);
        b.iter(|| {
            buf.clear();
            let mut writer = FileListWriter::new(ProtocolVersion::NEWEST);
            writer.write_entry(&mut buf, black_box(&entry)).unwrap();
            black_box(&buf);
        });
    });

    group.finish();
}

fn bench_file_entry_decode_single(c: &mut Criterion) {
    let mut group = c.benchmark_group("wire_file_entry_decode_single");

    let entry = make_file_entry(42);
    let mut encoded = Vec::with_capacity(256);
    {
        let mut writer = FileListWriter::new(ProtocolVersion::NEWEST);
        writer.write_entry(&mut encoded, &entry).unwrap();
        writer.write_end(&mut encoded, None).unwrap();
    }

    group.bench_function("single_entry", |b| {
        b.iter(|| {
            let mut cursor = Cursor::new(black_box(&encoded));
            let mut reader = FileListReader::new(ProtocolVersion::NEWEST);
            let decoded = reader.read_entry(&mut cursor).unwrap();
            black_box(decoded)
        });
    });

    group.finish();
}

fn bench_file_entry_encode_batch(c: &mut Criterion) {
    let mut group = c.benchmark_group("wire_file_entry_encode_batch");

    for count in [100, 1000] {
        let entries: Vec<FileEntry> = (0..count).map(make_file_entry).collect();

        group.throughput(Throughput::Elements(count as u64));

        group.bench_with_input(
            BenchmarkId::new("entries", count),
            &entries,
            |b, entries| {
                let mut buf = Vec::with_capacity(count * 100);
                b.iter(|| {
                    buf.clear();
                    let mut writer = FileListWriter::new(ProtocolVersion::NEWEST);
                    for entry in entries {
                        writer.write_entry(&mut buf, black_box(entry)).unwrap();
                    }
                    writer.write_end(&mut buf, None).unwrap();
                    black_box(&buf);
                });
            },
        );
    }

    group.finish();
}

fn bench_file_entry_decode_batch(c: &mut Criterion) {
    let mut group = c.benchmark_group("wire_file_entry_decode_batch");

    for count in [100, 1000] {
        let entries: Vec<FileEntry> = (0..count).map(make_file_entry).collect();
        let mut encoded = Vec::with_capacity(count * 100);
        {
            let mut writer = FileListWriter::new(ProtocolVersion::NEWEST);
            for entry in &entries {
                writer.write_entry(&mut encoded, entry).unwrap();
            }
            writer.write_end(&mut encoded, None).unwrap();
        }

        group.throughput(Throughput::Elements(count as u64));

        group.bench_with_input(
            BenchmarkId::new("entries", count),
            &encoded,
            |b, encoded| {
                b.iter(|| {
                    let mut cursor = Cursor::new(black_box(encoded));
                    let mut reader = FileListReader::new(ProtocolVersion::NEWEST);
                    let mut decoded = Vec::with_capacity(count);
                    while let Some(entry) = reader.read_entry(&mut cursor).unwrap() {
                        decoded.push(entry);
                    }
                    black_box(decoded)
                });
            },
        );
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// Multiplex frame encode/decode (MessageHeader)
// ---------------------------------------------------------------------------

fn bench_message_header_encode(c: &mut Criterion) {
    let mut group = c.benchmark_group("wire_mplex_header_encode");

    let codes = [
        ("data", MessageCode::Data),
        ("info", MessageCode::Info),
        ("error", MessageCode::Error),
    ];

    for (name, code) in codes {
        let header = MessageHeader::new(code, 4096).unwrap();

        group.bench_function(name, |b| {
            b.iter(|| {
                let encoded = black_box(header).encode();
                black_box(encoded)
            });
        });
    }

    group.finish();
}

fn bench_message_header_decode(c: &mut Criterion) {
    let mut group = c.benchmark_group("wire_mplex_header_decode");

    let codes = [
        ("data", MessageCode::Data),
        ("info", MessageCode::Info),
        ("error", MessageCode::Error),
    ];

    for (name, code) in codes {
        let header = MessageHeader::new(code, 4096).unwrap();
        let encoded = header.encode();

        group.bench_function(name, |b| {
            b.iter(|| {
                let decoded = MessageHeader::decode(black_box(&encoded)).unwrap();
                black_box(decoded)
            });
        });
    }

    group.finish();
}

fn bench_message_header_roundtrip_batch(c: &mut Criterion) {
    let mut group = c.benchmark_group("wire_mplex_header_roundtrip_batch");
    group.throughput(Throughput::Elements(1000));

    // Prepare 1000 headers with varying codes and payload lengths
    let headers: Vec<MessageHeader> = (0..1000)
        .map(|i| {
            let code = match i % 5 {
                0 => MessageCode::Data,
                1 => MessageCode::Info,
                2 => MessageCode::Error,
                3 => MessageCode::Warning,
                _ => MessageCode::Redo,
            };
            let payload_len = ((i * 997) % 16384) as u32;
            MessageHeader::new(code, payload_len).unwrap()
        })
        .collect();

    group.bench_function("encode_1k", |b| {
        b.iter(|| {
            let mut total = 0u32;
            for &header in black_box(&headers) {
                let bytes = header.encode();
                total = total.wrapping_add(u32::from_le_bytes(bytes));
            }
            black_box(total)
        });
    });

    let encoded: Vec<[u8; 4]> = headers.iter().map(|h| h.encode()).collect();

    group.bench_function("decode_1k", |b| {
        b.iter(|| {
            let mut total = 0u32;
            for bytes in black_box(&encoded) {
                let header = MessageHeader::decode(bytes).unwrap();
                total = total.wrapping_add(header.payload_len());
            }
            black_box(total)
        });
    });

    group.finish();
}

fn bench_mplex_frame_roundtrip(c: &mut Criterion) {
    let mut group = c.benchmark_group("wire_mplex_frame_roundtrip");

    for size in [64, 1024, 8192] {
        let payload: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();

        group.throughput(Throughput::Bytes(size as u64));

        group.bench_with_input(
            BenchmarkId::new("send_recv", size),
            &payload,
            |b, payload| {
                let mut buf = Vec::with_capacity(size + 16);
                b.iter(|| {
                    buf.clear();
                    send_msg(&mut buf, MessageCode::Data, black_box(payload)).unwrap();
                    let mut cursor = Cursor::new(&buf);
                    let frame = recv_msg(&mut cursor).unwrap();
                    black_box(frame)
                });
            },
        );
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// Delta token encode/decode
// ---------------------------------------------------------------------------

/// Generate deterministic data for delta benchmarks.
fn generate_data(size: usize) -> Vec<u8> {
    (0..size).map(|i| (i % 251) as u8).collect()
}

fn bench_delta_token_encode_single(c: &mut Criterion) {
    let mut group = c.benchmark_group("wire_delta_token_encode_single");

    // Literal token
    let literal_data = generate_data(4096);
    group.bench_function("literal_4KB", |b| {
        let mut buf = Vec::with_capacity(4200);
        b.iter(|| {
            buf.clear();
            write_token_literal(&mut buf, black_box(&literal_data)).unwrap();
            black_box(&buf);
        });
    });

    // Block match token
    group.bench_function("block_match", |b| {
        let mut buf = Vec::with_capacity(8);
        b.iter(|| {
            buf.clear();
            write_token_block_match(&mut buf, black_box(42)).unwrap();
            black_box(&buf);
        });
    });

    // End marker
    group.bench_function("end_marker", |b| {
        let mut buf = Vec::with_capacity(8);
        b.iter(|| {
            buf.clear();
            write_token_end(&mut buf).unwrap();
            black_box(&buf);
        });
    });

    group.finish();
}

fn bench_delta_op_codec(c: &mut Criterion) {
    let mut group = c.benchmark_group("wire_delta_op_codec");

    let literal_data = generate_data(4096);
    let literal_op = DeltaOp::Literal(literal_data);
    let copy_op = DeltaOp::Copy {
        block_index: 42,
        length: 8192,
    };

    // Encode
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

    // Decode
    let mut literal_encoded = Vec::new();
    write_delta_op(&mut literal_encoded, &literal_op).unwrap();

    let mut copy_encoded = Vec::new();
    write_delta_op(&mut copy_encoded, &copy_op).unwrap();

    group.bench_function("read_literal_4KB", |b| {
        b.iter(|| {
            let mut cursor = Cursor::new(black_box(&literal_encoded));
            let op = read_delta_op(&mut cursor).unwrap();
            black_box(op)
        });
    });

    group.bench_function("read_copy", |b| {
        b.iter(|| {
            let mut cursor = Cursor::new(black_box(&copy_encoded));
            let op = read_delta_op(&mut cursor).unwrap();
            black_box(op)
        });
    });

    group.finish();
}

fn bench_delta_op_batch(c: &mut Criterion) {
    let mut group = c.benchmark_group("wire_delta_op_batch");

    let num_ops = 1000;
    let ops: Vec<DeltaOp> = (0..num_ops)
        .map(|i| {
            if i % 3 == 0 {
                DeltaOp::Literal(generate_data(512))
            } else {
                DeltaOp::Copy {
                    block_index: i as u32,
                    length: 8192,
                }
            }
        })
        .collect();

    let total_bytes: usize = ops
        .iter()
        .map(|op| match op {
            DeltaOp::Literal(data) => data.len() + 16,
            DeltaOp::Copy { .. } => 16,
        })
        .sum();

    group.throughput(Throughput::Elements(num_ops as u64));

    group.bench_function("write_1k_mixed", |b| {
        let mut buf = Vec::with_capacity(total_bytes);
        b.iter(|| {
            buf.clear();
            for op in black_box(&ops) {
                write_delta_op(&mut buf, op).unwrap();
            }
            black_box(&buf);
        });
    });

    // Prepare encoded batch for decode
    let mut encoded = Vec::with_capacity(total_bytes);
    for op in &ops {
        write_delta_op(&mut encoded, op).unwrap();
    }

    group.bench_function("read_1k_mixed", |b| {
        b.iter(|| {
            let mut cursor = Cursor::new(black_box(&encoded));
            let mut count = 0usize;
            // Read until EOF (each op is self-delimiting via the opcode byte)
            while cursor.position() < encoded.len() as u64 {
                let op = read_delta_op(&mut cursor).unwrap();
                black_box(&op);
                count += 1;
            }
            black_box(count)
        });
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Criterion harness
// ---------------------------------------------------------------------------

criterion_group!(
    name = wire_protocol_benchmarks;
    config = Criterion::default()
        .sample_size(50)
        .measurement_time(std::time::Duration::from_secs(3));
    targets =
        bench_varint_encode_single,
        bench_varint_decode_single,
        bench_varint_encode_batch,
        bench_varint_decode_batch,
        bench_varlong_roundtrip,
        bench_file_entry_encode_single,
        bench_file_entry_decode_single,
        bench_file_entry_encode_batch,
        bench_file_entry_decode_batch,
        bench_message_header_encode,
        bench_message_header_decode,
        bench_message_header_roundtrip_batch,
        bench_mplex_frame_roundtrip,
        bench_delta_token_encode_single,
        bench_delta_op_codec,
        bench_delta_op_batch
);

criterion_main!(wire_protocol_benchmarks);
