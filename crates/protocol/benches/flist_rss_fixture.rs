//! Million-file flist RSS benchmark fixture (RSS-1.a).
//!
//! Measures peak memory and allocation throughput for `Vec<FileEntry>` at
//! large scale (100K / 500K / 1M entries). The Vec<FileEntry> with per-entry
//! `PathBuf` + `Arc<Path>` is the dominant RSS contributor in oc-rsync vs
//! upstream rsync (3-11x overhead at million-file scale).
//!
//! Three path distributions model real-world directory trees:
//!
//!   - **flat**: shallow paths (1-2 components), many unique dirnames
//!   - **deep**: deep paths (4-8 components), realistic project layout
//!   - **shared**: deep paths with high dirname sharing (100 dirs x N files)
//!
//! The "shared" distribution uses `PathInterner` to deduplicate `Arc<Path>`
//! dirname allocations, matching the production decode path.
//!
//! Run: `cargo bench -p protocol --bench flist_rss_fixture`

use std::hint::black_box;
use std::path::PathBuf;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

use protocol::flist::{FileEntry, PathInterner};

/// Entry counts for parameterized benchmarks.
const SCALES: &[usize] = &[100_000, 500_000, 1_000_000];

/// Maximum inline size for FileEntry - regression guard.
const MAX_INLINE_SIZE: usize = 96;

// ---------------------------------------------------------------------------
// Path generators
// ---------------------------------------------------------------------------

/// Flat paths: `dir_NNNNN/file_NNNNN.dat` (1-2 components, many unique dirs).
///
/// Models a wide, shallow tree - worst case for dirname interning because
/// every entry has a unique parent.
fn flat_path(i: usize) -> PathBuf {
    PathBuf::from(format!("dir_{:05}/file_{:05}.dat", i / 10, i))
}

/// Deep paths: `project/src/module_NN/sub_N/impl_NNNNN.rs` (4-8 components).
///
/// Models a realistic source tree with moderate dirname sharing. The depth
/// varies by entry index to exercise different PathBuf allocation sizes.
fn deep_path(i: usize) -> PathBuf {
    let depth = 3 + (i % 5); // 3-7 intermediate components
    let mut path = PathBuf::with_capacity(64);
    path.push("project");
    for d in 0..depth {
        path.push(format!("d{}_{}", d, (i / 100 + d) % 50));
    }
    path.push(format!("file_{i:06}.rs"));
    path
}

/// Shared-prefix paths: `workspace/pkg_NNN/src/mod_NN/item_NNNNN.rs`.
///
/// Models a monorepo with high dirname sharing - 1000 unique directories,
/// each containing N/1000 files. Best case for `PathInterner`.
fn shared_path(i: usize) -> PathBuf {
    let dir_idx = i % 1000;
    let pkg = dir_idx / 10;
    let module = dir_idx % 10;
    PathBuf::from(format!(
        "workspace/pkg_{pkg:03}/src/mod_{module:02}/item_{i:06}.rs"
    ))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Allocate `count` FileEntry structs using the given path generator,
/// with optional dirname interning via `PathInterner`.
fn allocate_entries(
    count: usize,
    path_fn: fn(usize) -> PathBuf,
    use_interner: bool,
) -> Vec<FileEntry> {
    let mut entries = Vec::with_capacity(count);
    let mut interner = PathInterner::with_capacity(if use_interner { 1024 } else { 0 });

    for i in 0..count {
        let path = path_fn(i);
        let mut entry = FileEntry::new_file(path.clone(), (i as u64) * 512, 0o644);
        if use_interner {
            if let Some(parent) = path.parent() {
                let interned = interner.intern(parent);
                entry.set_dirname(interned);
            }
        }
        entries.push(entry);
    }
    entries
}

/// Reports memory layout and RSS diagnostics to stderr.
fn report_memory_stats(label: &str, count: usize) {
    let inline_size = std::mem::size_of::<FileEntry>();
    let vec_backing = inline_size * count;
    eprintln!("--- {label} ({count} entries) ---");
    eprintln!("  FileEntry inline: {inline_size}B");
    eprintln!(
        "  Vec backing:      {vec_backing}B ({:.1} MiB)",
        vec_backing as f64 / (1024.0 * 1024.0)
    );

    #[cfg(target_os = "linux")]
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        for line in status.lines() {
            if line.starts_with("VmRSS:") || line.starts_with("VmHWM:") {
                eprintln!("  {line}");
            }
        }
    }

    eprintln!("-------------------------------");
}

// ---------------------------------------------------------------------------
// Benchmarks
// ---------------------------------------------------------------------------

/// Benchmark: allocate Vec<FileEntry> with flat paths (no interning).
fn bench_flat_allocation(c: &mut Criterion) {
    let mut group = c.benchmark_group("rss_flat");
    group.sample_size(10);
    group.measurement_time(std::time::Duration::from_secs(15));

    assert!(
        std::mem::size_of::<FileEntry>() <= MAX_INLINE_SIZE,
        "FileEntry inline size regression: {}B > {MAX_INLINE_SIZE}B",
        std::mem::size_of::<FileEntry>()
    );

    for &count in SCALES {
        group.throughput(Throughput::Elements(count as u64));
        group.bench_with_input(BenchmarkId::from_parameter(count), &count, |b, &count| {
            b.iter(|| {
                let entries = allocate_entries(count, flat_path, false);
                black_box(&entries);
            });
        });
    }

    report_memory_stats("flat (last scale)", *SCALES.last().unwrap());
    group.finish();
}

/// Benchmark: allocate Vec<FileEntry> with deep paths (no interning).
fn bench_deep_allocation(c: &mut Criterion) {
    let mut group = c.benchmark_group("rss_deep");
    group.sample_size(10);
    group.measurement_time(std::time::Duration::from_secs(15));

    for &count in SCALES {
        group.throughput(Throughput::Elements(count as u64));
        group.bench_with_input(BenchmarkId::from_parameter(count), &count, |b, &count| {
            b.iter(|| {
                let entries = allocate_entries(count, deep_path, false);
                black_box(&entries);
            });
        });
    }

    report_memory_stats("deep (last scale)", *SCALES.last().unwrap());
    group.finish();
}

/// Benchmark: allocate Vec<FileEntry> with shared-prefix paths + interning.
///
/// This is the closest model to the production decode path where
/// `PathInterner` deduplicates dirname `Arc<Path>` allocations.
fn bench_shared_interned(c: &mut Criterion) {
    let mut group = c.benchmark_group("rss_shared_interned");
    group.sample_size(10);
    group.measurement_time(std::time::Duration::from_secs(15));

    for &count in SCALES {
        group.throughput(Throughput::Elements(count as u64));
        group.bench_with_input(BenchmarkId::from_parameter(count), &count, |b, &count| {
            b.iter(|| {
                let entries = allocate_entries(count, shared_path, true);
                black_box(&entries);
            });
        });
    }

    report_memory_stats("shared+interned (last scale)", *SCALES.last().unwrap());
    group.finish();
}

/// Benchmark: sort a pre-built million-entry file list.
///
/// Sorting is an RSS-relevant operation because it requires the full
/// Vec<FileEntry> to be resident simultaneously.
fn bench_sort_at_scale(c: &mut Criterion) {
    let mut group = c.benchmark_group("rss_sort");
    group.sample_size(10);
    group.measurement_time(std::time::Duration::from_secs(15));

    // Use 100K and 500K only - 1M sort is very slow in a tight bench loop.
    for &count in &SCALES[..2] {
        let baseline = allocate_entries(count, shared_path, true);
        group.throughput(Throughput::Elements(count as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(count),
            &baseline,
            |b, baseline| {
                b.iter(|| {
                    let mut list = baseline.clone();
                    protocol::flist::sort_file_list(&mut list, false, false);
                    black_box(&list);
                });
            },
        );
    }

    group.finish();
}

/// Benchmark: encode + decode roundtrip at scale.
///
/// The decode path is where `PathInterner` and `Vec<FileEntry>` allocation
/// happen in production. This measures realistic RSS pressure from the
/// wire-format decode path.
fn bench_roundtrip_at_scale(c: &mut Criterion) {
    let mut group = c.benchmark_group("rss_roundtrip");
    group.sample_size(10);
    group.measurement_time(std::time::Duration::from_secs(20));

    // Use 100K only for roundtrip - encode is expensive at larger scales.
    let count = SCALES[0];
    let entries = allocate_entries(count, shared_path, true);

    // Encode once.
    let mut encoded = Vec::with_capacity(count * 64);
    {
        let mut writer =
            protocol::flist::FileListWriter::new(protocol::ProtocolVersion::NEWEST);
        for entry in &entries {
            writer.write_entry(&mut encoded, entry).unwrap();
        }
        writer.write_end(&mut encoded, None).unwrap();
    }

    group.throughput(Throughput::Elements(count as u64));

    group.bench_function(BenchmarkId::new("decode", count), |b| {
        b.iter(|| {
            let mut cursor = std::io::Cursor::new(&encoded);
            let mut reader =
                protocol::flist::FileListReader::new(protocol::ProtocolVersion::NEWEST);
            let mut decoded = Vec::with_capacity(count);
            while let Some(entry) = reader.read_entry(&mut cursor).unwrap() {
                decoded.push(entry);
            }
            black_box(decoded)
        });
    });

    report_memory_stats("roundtrip decode", count);
    group.finish();
}

/// Benchmark: measure clone cost at scale (relevant for INC_RECURSE
/// segment merging where sub-lists are cloned and appended).
fn bench_clone_at_scale(c: &mut Criterion) {
    let mut group = c.benchmark_group("rss_clone");
    group.sample_size(10);
    group.measurement_time(std::time::Duration::from_secs(15));

    for &count in &SCALES[..2] {
        let baseline = allocate_entries(count, shared_path, true);
        group.throughput(Throughput::Elements(count as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(count),
            &baseline,
            |b, baseline| {
                b.iter(|| {
                    let cloned = baseline.clone();
                    black_box(&cloned);
                });
            },
        );
    }

    group.finish();
}

/// Standalone memory profile: allocate 1M entries and report RSS.
///
/// Not timed by criterion - just builds the Vec and prints diagnostics.
/// Useful with `/usr/bin/time -v` wrapper to capture peak RSS externally.
fn bench_memory_profile(c: &mut Criterion) {
    let mut group = c.benchmark_group("rss_memory_profile");
    group.sample_size(10);
    group.measurement_time(std::time::Duration::from_secs(10));

    let count = 1_000_000;

    group.bench_function("1m_shared_interned", |b| {
        b.iter(|| {
            let entries = allocate_entries(count, shared_path, true);
            black_box(&entries);
        });
    });

    // Report per-entry overhead estimate.
    {
        let entries = allocate_entries(count, shared_path, true);
        let inline = std::mem::size_of::<FileEntry>();
        let vec_bytes = inline * entries.len();

        // Estimate total heap: each PathBuf holds a heap String. Average path
        // length for shared_path is ~45 bytes. Arc<Path> dirname is interned
        // (shared), so amortized cost is near zero.
        let avg_path_len: usize = entries
            .iter()
            .take(1000)
            .map(|e| e.name().len())
            .sum::<usize>()
            / 1000;

        let estimated_heap_per_entry = avg_path_len + 24; // PathBuf capacity + overhead
        let estimated_total =
            vec_bytes + (entries.len() * estimated_heap_per_entry);

        eprintln!("=== 1M entry memory profile ===");
        eprintln!("  Inline per entry:     {inline}B");
        eprintln!("  Avg path length:      {avg_path_len}B");
        eprintln!("  Vec backing store:    {vec_bytes}B ({:.1} MiB)", vec_bytes as f64 / 1_048_576.0);
        eprintln!(
            "  Est. total (vec+heap): {estimated_total}B ({:.1} MiB)",
            estimated_total as f64 / 1_048_576.0
        );
        eprintln!(
            "  Est. bytes/entry:      {}B",
            estimated_total / entries.len()
        );
        eprintln!("===============================");
    }

    group.finish();
}

criterion_group!(
    name = rss_benchmarks;
    config = Criterion::default();
    targets =
        bench_flat_allocation,
        bench_deep_allocation,
        bench_shared_interned,
        bench_sort_at_scale,
        bench_roundtrip_at_scale,
        bench_clone_at_scale,
        bench_memory_profile
);

criterion_main!(rss_benchmarks);
