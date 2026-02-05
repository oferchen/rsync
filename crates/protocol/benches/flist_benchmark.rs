//! Benchmarks for file list encoding/decoding performance.
//!
//! This benchmark suite measures critical paths in file list operations:
//! - FileEntry creation
//! - File list writing (encoding to wire format)
//! - File list reading (decoding from wire format)
//! - Sorting and cleaning file lists
//! - Compression state efficiency
//!
//! Run with: `cargo bench -p protocol -- flist`

use std::io::Cursor;
use std::path::PathBuf;

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};

use protocol::ProtocolVersion;
use protocol::flist::{FileEntry, FileListReader, FileListWriter, sort_file_list};

// ============================================================================
// Test Data Generation
// ============================================================================

/// Generate realistic file entries with varying path depths and lengths.
fn generate_file_entries(count: usize) -> Vec<FileEntry> {
    (0..count)
        .map(|i| {
            let depth = (i % 5) + 1;
            let path: PathBuf = (0..depth)
                .map(|d| format!("dir_{}", (i + d) % 100))
                .collect::<PathBuf>()
                .join(format!("file_{i}.txt"));

            FileEntry::new_file(path, (i * 1024) as u64, 0o644)
        })
        .collect()
}

/// Generate file entries with similar prefixes (common in real transfers).
fn generate_similar_prefix_entries(count: usize) -> Vec<FileEntry> {
    let base_dir = "very/long/base/directory/path/for/the/project";
    (0..count)
        .map(|i| {
            let subdir = i / 10;
            let path: PathBuf =
                PathBuf::from(format!("{base_dir}/subdir_{subdir}/file_{i:05}.dat"));

            FileEntry::new_file(path, (i * 512) as u64, 0o644)
        })
        .collect()
}

/// Generate a directory tree (mix of dirs and files).
fn generate_directory_tree(num_dirs: usize, files_per_dir: usize) -> Vec<FileEntry> {
    let mut entries = Vec::with_capacity(num_dirs * (1 + files_per_dir));

    for d in 0..num_dirs {
        let dir_path: PathBuf = format!("project/src/module_{d}").into();
        entries.push(FileEntry::new_directory(dir_path.clone(), 0o755));

        for f in 0..files_per_dir {
            let file_path = dir_path.join(format!("impl_{f}.rs"));
            entries.push(FileEntry::new_file(
                file_path,
                ((d * f + f) * 256) as u64,
                0o644,
            ));
        }
    }

    entries
}

// ============================================================================
// Benchmark: FileEntry Creation
// ============================================================================

fn bench_file_entry_creation(c: &mut Criterion) {
    let mut group = c.benchmark_group("flist_entry_creation");

    group.throughput(Throughput::Elements(1000));

    group.bench_function("new_file_1k", |b| {
        b.iter(|| {
            let entries: Vec<_> = (0..1000)
                .map(|i| {
                    FileEntry::new_file(
                        black_box(format!("file_{i}.txt").into()),
                        black_box(i as u64 * 1024),
                        black_box(0o644),
                    )
                })
                .collect();
            black_box(entries)
        });
    });

    group.bench_function("new_dir_1k", |b| {
        b.iter(|| {
            let entries: Vec<_> = (0..1000)
                .map(|i| {
                    FileEntry::new_directory(black_box(format!("dir_{i}").into()), black_box(0o755))
                })
                .collect();
            black_box(entries)
        });
    });

    group.bench_function("new_symlink_1k", |b| {
        b.iter(|| {
            let entries: Vec<_> = (0..1000)
                .map(|i| {
                    FileEntry::new_symlink(
                        black_box(format!("link_{i}").into()),
                        black_box(format!("../target_{i}").into()),
                    )
                })
                .collect();
            black_box(entries)
        });
    });

    group.finish();
}

// ============================================================================
// Benchmark: File List Writing (Encoding)
// ============================================================================

fn bench_flist_writing(c: &mut Criterion) {
    let mut group = c.benchmark_group("flist_writing");

    // Test different list sizes
    for count in [100, 1000, 10000] {
        let entries = generate_file_entries(count);

        group.throughput(Throughput::Elements(count as u64));

        group.bench_with_input(
            BenchmarkId::new("random_paths", count),
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

    // Test with similar prefixes (better compression)
    for count in [100, 1000, 10000] {
        let entries = generate_similar_prefix_entries(count);

        group.throughput(Throughput::Elements(count as u64));

        group.bench_with_input(
            BenchmarkId::new("similar_prefix", count),
            &entries,
            |b, entries| {
                let mut buf = Vec::with_capacity(count * 50);
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

// ============================================================================
// Benchmark: File List Reading (Decoding)
// ============================================================================

fn bench_flist_reading(c: &mut Criterion) {
    let mut group = c.benchmark_group("flist_reading");

    for count in [100, 1000, 10000] {
        // Prepare encoded data
        let entries = generate_file_entries(count);
        let mut encoded = Vec::with_capacity(count * 100);
        {
            let mut writer = FileListWriter::new(ProtocolVersion::NEWEST);
            for entry in &entries {
                writer.write_entry(&mut encoded, entry).unwrap();
            }
            writer.write_end(&mut encoded, None).unwrap();
        }

        group.throughput(Throughput::Elements(count as u64));

        group.bench_with_input(BenchmarkId::new("decode", count), &encoded, |b, encoded| {
            b.iter(|| {
                let mut cursor = Cursor::new(black_box(encoded));
                let mut reader = FileListReader::new(ProtocolVersion::NEWEST);
                let mut decoded = Vec::with_capacity(count);
                while let Some(entry) = reader.read_entry(&mut cursor).unwrap() {
                    decoded.push(entry);
                }
                black_box(decoded)
            });
        });
    }

    group.finish();
}

// ============================================================================
// Benchmark: File List Sorting
// ============================================================================

fn bench_flist_sorting(c: &mut Criterion) {
    let mut group = c.benchmark_group("flist_sorting");

    for count in [100, 1000, 10000, 50000] {
        let entries = generate_file_entries(count);

        group.throughput(Throughput::Elements(count as u64));

        group.bench_with_input(BenchmarkId::new("sort", count), &entries, |b, entries| {
            b.iter(|| {
                let mut list = entries.clone();
                sort_file_list(&mut list);
                black_box(list)
            });
        });
    }

    group.finish();
}

// ============================================================================
// Benchmark: Roundtrip (encode then decode)
// ============================================================================

fn bench_flist_roundtrip(c: &mut Criterion) {
    let mut group = c.benchmark_group("flist_roundtrip");

    for count in [100, 1000, 5000] {
        let entries = generate_directory_tree(count / 10, 9);

        group.throughput(Throughput::Elements(count as u64));

        group.bench_with_input(
            BenchmarkId::new("encode_decode", count),
            &entries,
            |b, entries| {
                let mut buf = Vec::with_capacity(count * 100);
                b.iter(|| {
                    // Encode
                    buf.clear();
                    let mut writer = FileListWriter::new(ProtocolVersion::NEWEST);
                    for entry in entries {
                        writer.write_entry(&mut buf, black_box(entry)).unwrap();
                    }
                    writer.write_end(&mut buf, None).unwrap();

                    // Decode
                    let mut cursor = Cursor::new(&buf);
                    let mut reader = FileListReader::new(ProtocolVersion::NEWEST);
                    let mut decoded = Vec::with_capacity(entries.len());
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

// ============================================================================
// Benchmark: Protocol Version Comparison
// ============================================================================

fn bench_protocol_versions(c: &mut Criterion) {
    let mut group = c.benchmark_group("flist_protocol_versions");

    let entries = generate_file_entries(1000);
    let protocols = [
        ("v28", ProtocolVersion::from_supported(28).unwrap()),
        ("v30", ProtocolVersion::from_supported(30).unwrap()),
        ("v31", ProtocolVersion::from_supported(31).unwrap()),
    ];

    group.throughput(Throughput::Elements(1000));

    for (name, protocol) in protocols {
        group.bench_with_input(BenchmarkId::new("encode", name), &entries, |b, entries| {
            let mut buf = Vec::with_capacity(100_000);
            b.iter(|| {
                buf.clear();
                let mut writer = FileListWriter::new(protocol);
                for entry in entries {
                    writer.write_entry(&mut buf, black_box(entry)).unwrap();
                }
                writer.write_end(&mut buf, None).unwrap();
                black_box(&buf);
            });
        });
    }

    group.finish();
}

// ============================================================================
// Benchmark: Compression Efficiency (wire format size)
// ============================================================================

fn bench_compression_efficiency(c: &mut Criterion) {
    let mut group = c.benchmark_group("flist_compression");

    // Compare random vs sequential paths
    let random_entries = generate_file_entries(1000);
    let sequential_entries = generate_similar_prefix_entries(1000);

    group.bench_function("measure_random_size", |b| {
        let mut buf = Vec::with_capacity(100_000);
        b.iter(|| {
            buf.clear();
            let mut writer = FileListWriter::new(ProtocolVersion::NEWEST);
            for entry in &random_entries {
                writer.write_entry(&mut buf, entry).unwrap();
            }
            writer.write_end(&mut buf, None).unwrap();
            black_box(buf.len())
        });
    });

    group.bench_function("measure_sequential_size", |b| {
        let mut buf = Vec::with_capacity(100_000);
        b.iter(|| {
            buf.clear();
            let mut writer = FileListWriter::new(ProtocolVersion::NEWEST);
            for entry in &sequential_entries {
                writer.write_entry(&mut buf, entry).unwrap();
            }
            writer.write_end(&mut buf, None).unwrap();
            black_box(buf.len())
        });
    });

    group.finish();
}

// ============================================================================
// Benchmark: Large File Lists (stress test)
// ============================================================================

fn bench_large_file_lists(c: &mut Criterion) {
    let mut group = c.benchmark_group("flist_large");

    // Reduce sample size for large tests
    group.sample_size(10);
    group.measurement_time(std::time::Duration::from_secs(10));

    let count = 100_000;
    let entries = generate_similar_prefix_entries(count);

    group.throughput(Throughput::Elements(count as u64));

    group.bench_function("encode_100k", |b| {
        let mut buf = Vec::with_capacity(count * 50);
        b.iter(|| {
            buf.clear();
            let mut writer = FileListWriter::new(ProtocolVersion::NEWEST);
            for entry in &entries {
                writer.write_entry(&mut buf, entry).unwrap();
            }
            writer.write_end(&mut buf, None).unwrap();
            black_box(&buf);
        });
    });

    // Prepare encoded data
    let mut encoded = Vec::with_capacity(count * 50);
    {
        let mut writer = FileListWriter::new(ProtocolVersion::NEWEST);
        for entry in &entries {
            writer.write_entry(&mut encoded, entry).unwrap();
        }
        writer.write_end(&mut encoded, None).unwrap();
    }

    group.bench_function("decode_100k", |b| {
        b.iter(|| {
            let mut cursor = Cursor::new(&encoded);
            let mut reader = FileListReader::new(ProtocolVersion::NEWEST);
            let mut decoded = Vec::with_capacity(count);
            while let Some(entry) = reader.read_entry(&mut cursor).unwrap() {
                decoded.push(entry);
            }
            black_box(decoded)
        });
    });

    group.finish();
}

// ============================================================================
// Criterion Groups
// ============================================================================

criterion_group!(
    name = flist_benchmarks;
    config = Criterion::default()
        .sample_size(50)
        .measurement_time(std::time::Duration::from_secs(3));
    targets =
        bench_file_entry_creation,
        bench_flist_writing,
        bench_flist_reading,
        bench_flist_sorting,
        bench_flist_roundtrip,
        bench_protocol_versions,
        bench_compression_efficiency,
        bench_large_file_lists
);

criterion_main!(flist_benchmarks);
