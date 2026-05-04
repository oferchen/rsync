//! Memory benchmark for `FileEntry` with 100K entries.
//!
//! Measures inline struct size, total heap allocation for a Vec of 100K
//! regular file entries (no extras/symlinks), and per-entry overhead.
//!
//! Run with: `cargo bench -p protocol -- file_entry_memory`

use std::path::PathBuf;

use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

use protocol::flist::FileEntry;

/// Number of entries to allocate in the bulk test.
const ENTRY_COUNT: usize = 100_000;

/// Maximum allowed inline size for `FileEntry` - the Box optimization target.
const MAX_INLINE_SIZE: usize = 96;

/// Allocate 100K `FileEntry` structs and measure memory characteristics.
fn bench_file_entry_memory(c: &mut Criterion) {
    let inline_size = std::mem::size_of::<FileEntry>();
    let align = std::mem::align_of::<FileEntry>();

    eprintln!("--- FileEntry Memory Layout ---");
    eprintln!("  Inline size : {inline_size} bytes");
    eprintln!("  Alignment   : {align} bytes");
    eprintln!("  Vec capacity: {ENTRY_COUNT} entries");
    eprintln!(
        "  Vec backing : {} bytes ({:.1} MiB)",
        inline_size * ENTRY_COUNT,
        (inline_size * ENTRY_COUNT) as f64 / (1024.0 * 1024.0)
    );
    eprintln!("-------------------------------");

    assert!(
        inline_size <= MAX_INLINE_SIZE,
        "FileEntry inline size {inline_size}B exceeds {MAX_INLINE_SIZE}B optimization target"
    );

    let mut group = c.benchmark_group("file_entry_memory");

    // Benchmark: allocate 100K regular file entries (no extras).
    group.bench_function("allocate_100k_regular_files", |b| {
        b.iter(|| {
            let entries: Vec<FileEntry> = (0..ENTRY_COUNT)
                .map(|i| {
                    let depth = (i % 5) + 1;
                    let path: PathBuf = (0..depth)
                        .map(|d| format!("dir_{}", (i + d) % 100))
                        .collect::<PathBuf>()
                        .join(format!("file_{i}.txt"));
                    FileEntry::new_file(path, (i * 1024) as u64, 0o644)
                })
                .collect();
            black_box(&entries);
        });
    });

    // Benchmark: allocate 100K entries with pre-allocated Vec capacity.
    group.bench_function("allocate_100k_preallocated", |b| {
        b.iter(|| {
            let mut entries = Vec::with_capacity(ENTRY_COUNT);
            for i in 0..ENTRY_COUNT {
                let depth = (i % 5) + 1;
                let path: PathBuf = (0..depth)
                    .map(|d| format!("dir_{}", (i + d) % 100))
                    .collect::<PathBuf>()
                    .join(format!("file_{i}.txt"));
                entries.push(FileEntry::new_file(path, (i * 1024) as u64, 0o644));
            }
            black_box(&entries);
        });
    });

    // Benchmark: measure clone cost of 100K entries.
    let baseline: Vec<FileEntry> = (0..ENTRY_COUNT)
        .map(|i| {
            let depth = (i % 5) + 1;
            let path: PathBuf = (0..depth)
                .map(|d| format!("dir_{}", (i + d) % 100))
                .collect::<PathBuf>()
                .join(format!("file_{i}.txt"));
            FileEntry::new_file(path, (i * 1024) as u64, 0o644)
        })
        .collect();

    group.bench_function("clone_100k_regular_files", |b| {
        b.iter(|| {
            let cloned = baseline.clone();
            black_box(&cloned);
        });
    });

    group.finish();

    // Print RSS estimate (Linux only, informational).
    #[cfg(target_os = "linux")]
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        for line in status.lines() {
            if line.starts_with("VmRSS:") || line.starts_with("VmHWM:") {
                eprintln!("  {line}");
            }
        }
    }
}

criterion_group!(benches, bench_file_entry_memory);
criterion_main!(benches);
