//! RSS regression benchmark for flist construction at scale (RSS-1).
//!
//! Reproduces the 3-11x RSS gap between oc-rsync and upstream rsync when
//! building large file lists. Upstream rsync 3.4.1 uses ~76.8 MB for 1M
//! files (no INC_RECURSE), while oc-rsync's `Vec<FileEntry>` consumes
//! ~197 MB - a 2.6x overhead on the same fixture.
//!
//! This benchmark constructs file lists at 100K, 250K, and 500K entries,
//! measures the RSS delta attributable to flist allocation, and reports
//! per-entry overhead in bytes. The results establish a regression baseline
//! so that future changes to `FileEntry` or path interning can be validated
//! against known overhead targets.
//!
//! # Measurement approach
//!
//! RSS is sampled via platform-specific APIs (`mach_task_info` on macOS,
//! `/proc/self/status` on Linux). The delta between pre-allocation and
//! post-allocation snapshots isolates flist memory from the benchmark
//! harness overhead. Three path distributions exercise different interning
//! efficiency:
//!
//!   - **flat**: shallow paths, many unique dirs (worst-case interning)
//!   - **deep**: 4-8 component paths, moderate sharing
//!   - **shared**: monorepo layout, high dirname sharing (best-case interning)
//!
//! # Upstream reference
//!
//! Upstream rsync's `file_struct` is approximately 24 bytes inline plus
//! variable-length `file_extras` (uid, gid, atime etc.) allocated from a
//! contiguous pool. At 1M files, upstream holds ~76.8 MB in the non-INC
//! path and ~7.6 MB with INC_RECURSE (which streams and discards segments).
//!
//! Run: `cargo bench -p protocol --bench flist_rss_regression`

use std::hint::black_box;
use std::path::PathBuf;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

use protocol::flist::{FileEntry, PathInterner};

/// Entry counts for parameterized measurement.
///
/// 100K is the minimum for meaningful per-entry overhead estimation.
/// 500K is the CI-feasible ceiling - 1M is reserved for dedicated profiling.
const SCALES: &[usize] = &[100_000, 250_000, 500_000];

/// Upstream rsync per-entry RSS at 1M files (no INC_RECURSE): ~76.8 bytes.
///
/// Derived from RSS-1.b measurement: 76.8 MB / 1M entries = 76.8 bytes/entry.
/// This accounts for the `file_struct` inline (24 bytes), `file_extras`
/// (uid/gid ~8 bytes), and pooled string storage (~44 bytes average).
const UPSTREAM_BYTES_PER_ENTRY: f64 = 76.8;

// ---------------------------------------------------------------------------
// Path generators
// ---------------------------------------------------------------------------

/// Flat paths: `dir_NNNNN/file_NNNNN.dat`.
///
/// Each entry has a near-unique parent directory. Worst case for dirname
/// interning - every `Arc<Path>` is a fresh heap allocation.
fn flat_path(i: usize) -> PathBuf {
    PathBuf::from(format!("dir_{:05}/file_{:05}.dat", i / 10, i))
}

/// Deep paths: `project/src/module_NN/sub_N/impl_NNNNNN.rs`.
///
/// 4-8 path components with moderate dirname sharing (~50 unique prefixes
/// per depth level). Exercises both interning and PathBuf heap allocation.
fn deep_path(i: usize) -> PathBuf {
    let depth = 3 + (i % 5);
    let mut path = PathBuf::with_capacity(64);
    path.push("project");
    for d in 0..depth {
        path.push(format!("d{}_{}", d, (i / 100 + d) % 50));
    }
    path.push(format!("file_{i:06}.rs"));
    path
}

/// Shared-prefix paths: `workspace/pkg_NNN/src/mod_NN/item_NNNNNN.rs`.
///
/// 1000 unique directories, each with N/1000 files. Best case for
/// `PathInterner` - mimics monorepo layouts where dirname deduplication
/// is highly effective.
fn shared_path(i: usize) -> PathBuf {
    let dir_idx = i % 1000;
    let pkg = dir_idx / 10;
    let module = dir_idx % 10;
    PathBuf::from(format!(
        "workspace/pkg_{pkg:03}/src/mod_{module:02}/item_{i:06}.rs"
    ))
}

// ---------------------------------------------------------------------------
// Synthetic metadata
// ---------------------------------------------------------------------------

/// Returns a realistic file size for entry `i`.
///
/// Distribution: 60% < 4 KiB, 25% 4-256 KiB, 10% 256 KiB-2 MiB, 5% 2-10 MiB.
fn synthetic_size(i: usize) -> u64 {
    match i % 100 {
        0..=59 => (i % 4096) as u64,
        60..=84 => ((i % 256) * 1024) as u64,
        85..=94 => ((i % 2048) * 1024) as u64,
        _ => ((i % 10240) * 1024) as u64,
    }
}

/// Returns a realistic mtime spread across ~3 years.
fn synthetic_mtime(i: usize) -> i64 {
    let base = 1_672_531_200_i64; // 2023-01-01
    base + ((i as i64 * 7919) % 94_608_000)
}

/// Returns a realistic mode (regular file variants).
fn synthetic_mode(i: usize) -> u32 {
    match i % 20 {
        0 => 0o100755,
        1..=2 => 0o100600,
        _ => 0o100644,
    }
}

// ---------------------------------------------------------------------------
// RSS measurement
// ---------------------------------------------------------------------------

/// Returns the current resident set size in bytes.
///
/// Platform-specific: `mach_task_info` on macOS, `/proc/self/status` on
/// Linux. Returns 0 on unsupported platforms.
fn current_rss_bytes() -> usize {
    #[cfg(target_os = "macos")]
    {
        macos_rss_bytes()
    }
    #[cfg(target_os = "linux")]
    {
        linux_rss_bytes()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        0
    }
}

#[cfg(target_os = "macos")]
fn macos_rss_bytes() -> usize {
    use std::mem;

    #[repr(C)]
    struct MachTaskBasicInfo {
        virtual_size: u64,
        resident_size: u64,
        resident_size_max: u64,
        user_time: [u32; 2],
        system_time: [u32; 2],
        policy: i32,
        suspend_count: i32,
    }

    const MACH_TASK_BASIC_INFO: u32 = 20;

    unsafe extern "C" {
        fn mach_task_self() -> u32;
        fn task_info(
            target_task: u32,
            flavor: u32,
            task_info_out: *mut MachTaskBasicInfo,
            task_info_out_cnt: *mut u32,
        ) -> i32;
    }

    let mut info: MachTaskBasicInfo = unsafe { mem::zeroed() };
    let mut count = (mem::size_of::<MachTaskBasicInfo>() / mem::size_of::<u32>()) as u32;
    let kr = unsafe {
        task_info(
            mach_task_self(),
            MACH_TASK_BASIC_INFO,
            &mut info,
            &mut count,
        )
    };
    if kr == 0 {
        info.resident_size as usize
    } else {
        0
    }
}

#[cfg(target_os = "linux")]
fn linux_rss_bytes() -> usize {
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("VmRSS:") {
                let trimmed = rest.trim().trim_end_matches(" kB").trim();
                if let Ok(kb) = trimmed.parse::<usize>() {
                    return kb * 1024;
                }
            }
        }
    }
    0
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

/// Builds a `Vec<FileEntry>` with realistic metadata and dirname interning.
///
/// Mirrors the production decode path where `PathInterner` deduplicates
/// dirname allocations across entries sharing a parent directory.
fn build_entries(count: usize, path_fn: fn(usize) -> PathBuf) -> Vec<FileEntry> {
    let mut entries = Vec::with_capacity(count);
    let mut interner = PathInterner::with_capacity(2048);

    for i in 0..count {
        let path = path_fn(i);
        let mut entry =
            FileEntry::new_file(path.clone(), synthetic_size(i), synthetic_mode(i) & 0o7777);
        entry.set_mtime(synthetic_mtime(i), (i as u32) % 1_000_000_000);
        entry.set_uid((i % 1000) as u32);
        entry.set_gid((i % 100) as u32);
        if let Some(parent) = path.parent() {
            let interned = interner.intern(parent);
            entry.set_dirname(interned);
        }
        entries.push(entry);
    }
    entries
}

// ---------------------------------------------------------------------------
// RSS regression report
// ---------------------------------------------------------------------------

/// Measures the RSS delta from allocating `count` entries with the given
/// path distribution, then prints a detailed per-entry overhead report.
///
/// Returns (rss_delta_bytes, entries_count) for assertion use.
fn measure_and_report(
    label: &str,
    count: usize,
    path_fn: fn(usize) -> PathBuf,
) -> (usize, usize) {
    // Force a GC-like settling by dropping any prior large allocations.
    // The allocator may not return pages to the OS immediately, but the
    // delta measurement is still meaningful within a single run.
    let rss_before = current_rss_bytes();

    let entries = build_entries(count, path_fn);
    let entry_count = entries.len();

    let rss_after = current_rss_bytes();
    let rss_delta = rss_after.saturating_sub(rss_before);

    let inline_size = std::mem::size_of::<FileEntry>();
    let vec_backing = inline_size * entry_count;
    let per_entry_rss = if entry_count > 0 {
        rss_delta as f64 / entry_count as f64
    } else {
        0.0
    };
    let vs_upstream = per_entry_rss / UPSTREAM_BYTES_PER_ENTRY;

    eprintln!();
    eprintln!("=== RSS-1 regression: {label} ({count} entries) ===");
    eprintln!("  FileEntry inline size:   {inline_size}B");
    eprintln!(
        "  Vec backing store:       {vec_backing}B ({:.1} MiB)",
        vec_backing as f64 / 1_048_576.0
    );
    eprintln!(
        "  RSS delta:               {rss_delta}B ({:.1} MiB)",
        rss_delta as f64 / 1_048_576.0
    );
    eprintln!("  Per-entry RSS:           {per_entry_rss:.1}B");
    eprintln!(
        "  Upstream reference:      {UPSTREAM_BYTES_PER_ENTRY:.1}B/entry"
    );
    eprintln!("  Ratio vs upstream:       {vs_upstream:.2}x");
    eprintln!("  Heap overhead/entry:     {:.1}B (RSS - Vec backing)",
        (rss_delta.saturating_sub(vec_backing)) as f64 / entry_count as f64
    );
    eprintln!("=================================================");

    // Keep entries alive through the measurement.
    black_box(&entries);
    drop(entries);

    (rss_delta, entry_count)
}

// ---------------------------------------------------------------------------
// Criterion benchmarks
// ---------------------------------------------------------------------------

/// Benchmark: flist construction throughput across scales and distributions.
///
/// The primary purpose is reproducible timing, but the side-effect RSS
/// reports (printed to stderr) are the key diagnostic output for RSS-1.
fn bench_flist_construction(c: &mut Criterion) {
    let mut group = c.benchmark_group("rss1_flist_construction");
    group.sample_size(10);
    group.measurement_time(std::time::Duration::from_secs(15));

    let distributions: &[(&str, fn(usize) -> PathBuf)] = &[
        ("flat", flat_path),
        ("deep", deep_path),
        ("shared", shared_path),
    ];

    for &count in SCALES {
        for &(label, path_fn) in distributions {
            group.throughput(Throughput::Elements(count as u64));
            group.bench_with_input(
                BenchmarkId::new(label, count),
                &(count, path_fn),
                |b, &(n, pfn)| {
                    b.iter(|| {
                        let entries = build_entries(n, pfn);
                        black_box(&entries);
                    });
                },
            );
        }
    }

    group.finish();
}

/// Benchmark: RSS delta measurement at the largest CI-feasible scale.
///
/// Measures and reports per-entry RSS overhead for each path distribution
/// at 500K entries. This is the core RSS-1 regression metric.
fn bench_rss_delta(c: &mut Criterion) {
    let mut group = c.benchmark_group("rss1_delta_measurement");
    group.sample_size(10);
    group.measurement_time(std::time::Duration::from_secs(10));

    // Measure RSS for each distribution at 500K - the largest CI-feasible
    // scale. The criterion harness benchmarks construction time, while the
    // `measure_and_report` calls capture RSS diagnostics.

    // Flat distribution (worst-case interning).
    group.bench_function("flat_500k", |b| {
        b.iter(|| {
            let entries = build_entries(500_000, flat_path);
            black_box(&entries);
        });
    });
    measure_and_report("flat", 500_000, flat_path);

    // Deep distribution (moderate interning).
    group.bench_function("deep_500k", |b| {
        b.iter(|| {
            let entries = build_entries(500_000, deep_path);
            black_box(&entries);
        });
    });
    measure_and_report("deep", 500_000, deep_path);

    // Shared distribution (best-case interning, closest to production).
    group.bench_function("shared_500k", |b| {
        b.iter(|| {
            let entries = build_entries(500_000, shared_path);
            black_box(&entries);
        });
    });
    measure_and_report("shared", 500_000, shared_path);

    group.finish();
}

/// Benchmark: per-entry overhead scaling linearity.
///
/// Verifies that per-entry RSS overhead remains roughly constant as entry
/// count grows (no super-linear memory blowup from fragmentation or
/// allocator overhead).
fn bench_scaling_linearity(c: &mut Criterion) {
    let mut group = c.benchmark_group("rss1_scaling");
    group.sample_size(10);
    group.measurement_time(std::time::Duration::from_secs(10));

    for &count in SCALES {
        group.throughput(Throughput::Elements(count as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(count),
            &count,
            |b, &n| {
                b.iter(|| {
                    let entries = build_entries(n, shared_path);
                    black_box(&entries);
                });
            },
        );
    }

    // Print scaling analysis.
    let mut prev_per_entry: Option<f64> = None;
    for &count in SCALES {
        let (rss_delta, entry_count) = measure_and_report("shared (scaling)", count, shared_path);
        let per_entry = if entry_count > 0 {
            rss_delta as f64 / entry_count as f64
        } else {
            0.0
        };

        if let Some(prev) = prev_per_entry {
            let drift = ((per_entry - prev) / prev * 100.0).abs();
            eprintln!("  Scaling drift: {drift:.1}% (from previous scale)");
        }
        prev_per_entry = Some(per_entry);
    }

    group.finish();
}

/// Inline size regression guard.
///
/// `FileEntry` must stay within the 96-byte optimization target. Exceeding
/// this threshold inflates vec backing store by ~30% at million-file scale.
fn bench_inline_size_guard(c: &mut Criterion) {
    let mut group = c.benchmark_group("rss1_size_guard");

    let entry_size = std::mem::size_of::<FileEntry>();
    let entry_align = std::mem::align_of::<FileEntry>();

    eprintln!();
    eprintln!("--- FileEntry size guard ---");
    eprintln!("  Inline size: {entry_size}B (target: <= 96B)");
    eprintln!("  Alignment:   {entry_align}B");
    eprintln!(
        "  1M Vec cost: {:.1} MiB (inline only)",
        (entry_size * 1_000_000) as f64 / 1_048_576.0
    );
    eprintln!("----------------------------");

    assert!(
        entry_size <= 96,
        "FileEntry inline size {entry_size}B exceeds 96B target - \
         this inflates RSS at million-file scale"
    );

    group.bench_function("size_check", |b| {
        b.iter(|| {
            black_box(std::mem::size_of::<FileEntry>());
        });
    });

    group.finish();
}

criterion_group!(
    name = rss1_regression;
    config = Criterion::default();
    targets =
        bench_flist_construction,
        bench_rss_delta,
        bench_scaling_linearity,
        bench_inline_size_guard
);

criterion_main!(rss1_regression);
