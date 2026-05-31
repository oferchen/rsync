//! Million-file RSS benchmark: FlatFileList vs legacy Vec<FileEntry> (RSS-A.9.a).
//!
//! Measures peak RSS and iteration throughput for both representations at
//! 1M entries with realistic field distributions (varied sizes, modes,
//! mtimes, name lengths). The primary metric is the RSS ratio (flat/legacy).
//!
//! Path distributions model real-world directory trees:
//!
//!   - **shared**: monorepo layout with high dirname sharing (1000 dirs x 1000 files)
//!   - **deep**: realistic project tree (4-8 components, moderate sharing)
//!
//! Run: `cargo bench -p protocol --features flat-flist --bench flat_flist_rss`

use std::hint::black_box;
use std::path::PathBuf;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

use protocol::flist::{
    ExtrasRef, FileEntry, FileEntryHeader, FlatFileList, PathInterner, PRESENT_GID, PRESENT_UID,
};

/// Entry count for the benchmark fixture.
const ENTRY_COUNT: usize = 1_000_000;

// ---------------------------------------------------------------------------
// Synthetic field distributions
// ---------------------------------------------------------------------------

/// Returns a realistic file size for entry `i`.
///
/// Distribution mirrors typical source trees: many small files (< 4 KiB),
/// some medium (4-256 KiB), few large (256 KiB - 10 MiB).
fn synthetic_size(i: usize) -> u64 {
    match i % 100 {
        0..=59 => (i % 4096) as u64,           // 60% < 4 KiB
        60..=84 => ((i % 256) * 1024) as u64,   // 25% 4-256 KiB
        85..=94 => ((i % 2048) * 1024) as u64,  // 10% 256 KiB - 2 MiB
        _ => ((i % 10240) * 1024) as u64,        // 5% 2-10 MiB
    }
}

/// Returns a realistic mtime for entry `i` (seconds since epoch).
///
/// Spread across a ~3 year window (2023-2026) to exercise varied timestamps.
fn synthetic_mtime(i: usize) -> i64 {
    let base = 1_672_531_200_i64; // 2023-01-01 00:00:00 UTC
    let spread = 94_608_000_i64;  // ~3 years in seconds
    base + ((i as i64 * 7919) % spread) // prime stride for spread
}

/// Returns a realistic mode for entry `i`.
fn synthetic_mode(i: usize) -> u32 {
    match i % 20 {
        0 => 0o100755, // executable
        1..=2 => 0o100600, // private
        _ => 0o100644, // regular
    }
}

/// Shared-prefix path: `workspace/pkg_NNN/src/mod_NN/item_NNNNNN.rs`.
///
/// 1000 unique directories, each with N/1000 files. Best case for interning.
fn shared_path(i: usize) -> PathBuf {
    let dir_idx = i % 1000;
    let pkg = dir_idx / 10;
    let module = dir_idx % 10;
    PathBuf::from(format!(
        "workspace/pkg_{pkg:03}/src/mod_{module:02}/item_{i:06}.rs"
    ))
}

/// Deep path: `project/src/module_NN/sub_N/impl_NNNNNN.rs` (4-8 components).
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

// ---------------------------------------------------------------------------
// Builder helpers
// ---------------------------------------------------------------------------

/// Builds a legacy `Vec<FileEntry>` with realistic metadata and dirname
/// interning via `PathInterner`.
fn build_legacy_entries(
    count: usize,
    path_fn: fn(usize) -> PathBuf,
) -> Vec<FileEntry> {
    let mut entries = Vec::with_capacity(count);
    let mut interner = PathInterner::with_capacity(2048);

    for i in 0..count {
        let path = path_fn(i);
        let mut entry = FileEntry::new_file(path.clone(), synthetic_size(i), synthetic_mode(i) & 0o7777);
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

/// Builds a `FlatFileList` with identical data to the legacy builder.
fn build_flat_entries(
    count: usize,
    path_fn: fn(usize) -> PathBuf,
) -> FlatFileList {
    let mut flat = FlatFileList::with_capacity(count);

    for i in 0..count {
        let path = path_fn(i);
        let basename = path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let dirname = path.parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();

        let name_handle = flat.paths_mut().intern(&basename);
        let dir_handle = flat.paths_mut().intern(&dirname);

        let header = FileEntryHeader {
            mtime: synthetic_mtime(i),
            size: synthetic_size(i),
            uid: (i % 1000) as u32,
            gid: (i % 100) as u32,
            name: name_handle,
            dirname: dir_handle,
            extras: ExtrasRef::NO_EXTRAS,
            mtime_nsec: (i as u32) % 1_000_000_000,
            mode: synthetic_mode(i),
            flags: 0,
            present: PRESENT_UID | PRESENT_GID,
        };
        flat.push(header);
    }
    flat
}

// ---------------------------------------------------------------------------
// Peak RSS measurement
// ---------------------------------------------------------------------------

/// Returns the current resident set size in bytes.
///
/// On macOS, uses `mach_task_info`. On Linux, reads `/proc/self/status`.
/// On other platforms, returns 0.
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

    // MACH_TASK_BASIC_INFO = 20
    const MACH_TASK_BASIC_INFO: u32 = 20;

    extern "C" {
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

    let kr = unsafe { task_info(mach_task_self(), MACH_TASK_BASIC_INFO, &mut info, &mut count) };
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

/// Reports RSS diagnostics and the flat/legacy ratio.
fn report_rss(
    label: &str,
    legacy_rss: usize,
    flat_rss: usize,
    legacy_count: usize,
    flat_count: usize,
) {
    let legacy_mib = legacy_rss as f64 / 1_048_576.0;
    let flat_mib = flat_rss as f64 / 1_048_576.0;
    let ratio = if legacy_rss > 0 {
        flat_rss as f64 / legacy_rss as f64
    } else {
        f64::NAN
    };

    let legacy_inline = std::mem::size_of::<FileEntry>();
    let flat_inline = std::mem::size_of::<FileEntryHeader>();

    eprintln!("=== {label} RSS comparison ({legacy_count} entries) ===");
    eprintln!("  FileEntry inline:       {legacy_inline}B");
    eprintln!("  FileEntryHeader inline: {flat_inline}B");
    eprintln!("  Legacy Vec RSS:         {legacy_rss}B ({legacy_mib:.1} MiB)");
    eprintln!("  Flat RSS:               {flat_rss}B ({flat_mib:.1} MiB)");
    eprintln!("  Ratio (flat/legacy):    {ratio:.3}x");
    eprintln!("  Legacy entries:         {legacy_count}");
    eprintln!("  Flat entries:           {flat_count}");
    eprintln!("=============================================");
}

// ---------------------------------------------------------------------------
// Benchmarks
// ---------------------------------------------------------------------------

/// Benchmark: allocate 1M legacy Vec<FileEntry> with shared paths + interning.
fn bench_legacy_allocation(c: &mut Criterion) {
    let mut group = c.benchmark_group("flat_vs_legacy_allocation");
    group.sample_size(10);
    group.measurement_time(std::time::Duration::from_secs(20));

    for (label, path_fn) in [("shared", shared_path as fn(usize) -> PathBuf), ("deep", deep_path)] {
        group.throughput(Throughput::Elements(ENTRY_COUNT as u64));
        group.bench_with_input(
            BenchmarkId::new("legacy", label),
            &(ENTRY_COUNT, path_fn),
            |b, &(count, pfn)| {
                b.iter(|| {
                    let entries = build_legacy_entries(count, pfn);
                    black_box(&entries);
                });
            },
        );
    }

    group.finish();
}

/// Benchmark: allocate 1M FlatFileList with shared paths + interning.
fn bench_flat_allocation(c: &mut Criterion) {
    let mut group = c.benchmark_group("flat_vs_legacy_allocation");
    group.sample_size(10);
    group.measurement_time(std::time::Duration::from_secs(20));

    for (label, path_fn) in [("shared", shared_path as fn(usize) -> PathBuf), ("deep", deep_path)] {
        group.throughput(Throughput::Elements(ENTRY_COUNT as u64));
        group.bench_with_input(
            BenchmarkId::new("flat", label),
            &(ENTRY_COUNT, path_fn),
            |b, &(count, pfn)| {
                b.iter(|| {
                    let flat = build_flat_entries(count, pfn);
                    black_box(&flat);
                });
            },
        );
    }

    group.finish();
}

/// Benchmark: iterate over all entries in both representations.
///
/// Measures cache-friendly sequential access throughput. The flat
/// representation stores headers contiguously without pointer chasing,
/// which should yield better cache behavior at scale.
fn bench_iteration_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("flat_vs_legacy_iterate");
    group.sample_size(20);
    group.measurement_time(std::time::Duration::from_secs(10));

    let legacy = build_legacy_entries(ENTRY_COUNT, shared_path);
    let flat = build_flat_entries(ENTRY_COUNT, shared_path);

    group.throughput(Throughput::Elements(ENTRY_COUNT as u64));

    group.bench_function(BenchmarkId::new("legacy", ENTRY_COUNT), |b| {
        b.iter(|| {
            let mut total_size: u64 = 0;
            for entry in &legacy {
                total_size = total_size.wrapping_add(entry.size());
            }
            black_box(total_size)
        });
    });

    group.bench_function(BenchmarkId::new("flat", ENTRY_COUNT), |b| {
        b.iter(|| {
            let mut total_size: u64 = 0;
            for entry in flat.iter() {
                total_size = total_size.wrapping_add(entry.header.size);
            }
            black_box(total_size)
        });
    });

    group.finish();
}

/// Benchmark: sort both representations at scale.
///
/// Flat sort operates on contiguous headers with arena-resolved comparisons.
/// Legacy sort moves 88-96 byte FileEntry structs with pointer-chasing paths.
fn bench_sort_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("flat_vs_legacy_sort");
    group.sample_size(10);
    group.measurement_time(std::time::Duration::from_secs(15));

    // Use 500K for sort - 1M sort in a tight loop is very slow.
    let sort_count = 500_000;

    let legacy_baseline = build_legacy_entries(sort_count, shared_path);

    group.throughput(Throughput::Elements(sort_count as u64));

    group.bench_function(BenchmarkId::new("legacy", sort_count), |b| {
        b.iter(|| {
            let mut list = legacy_baseline.clone();
            protocol::flist::sort_file_list(&mut list, false, false);
            black_box(&list);
        });
    });

    // FlatFileList does not implement Clone, so rebuild each iteration.
    // The sort cost dominates the build cost at this scale.
    group.bench_function(BenchmarkId::new("flat", sort_count), |b| {
        b.iter(|| {
            let mut flat = build_flat_entries(sort_count, shared_path);
            flat.sort();
            black_box(&flat);
        });
    });

    group.finish();
}

/// Standalone RSS profile: allocate 1M entries in each representation and
/// measure peak RSS between them.
///
/// This benchmark exists primarily for its diagnostic output. The RSS
/// measurements are printed to stderr; use `/usr/bin/time -v` for external
/// validation.
fn bench_rss_profile(c: &mut Criterion) {
    let mut group = c.benchmark_group("flat_vs_legacy_rss");
    group.sample_size(10);
    group.measurement_time(std::time::Duration::from_secs(10));

    // --- Legacy ---
    group.bench_function("legacy_1m_shared", |b| {
        b.iter(|| {
            let entries = build_legacy_entries(ENTRY_COUNT, shared_path);
            black_box(&entries);
        });
    });

    // Measure legacy RSS with allocation held live.
    let baseline_rss = current_rss_bytes();
    let legacy = build_legacy_entries(ENTRY_COUNT, shared_path);
    let legacy_rss = current_rss_bytes();
    let legacy_delta = legacy_rss.saturating_sub(baseline_rss);
    let legacy_count = legacy.len();
    drop(legacy);

    // --- Flat ---
    group.bench_function("flat_1m_shared", |b| {
        b.iter(|| {
            let flat = build_flat_entries(ENTRY_COUNT, shared_path);
            black_box(&flat);
        });
    });

    // Measure flat RSS with allocation held live.
    let baseline_rss = current_rss_bytes();
    let flat = build_flat_entries(ENTRY_COUNT, shared_path);
    let flat_rss = current_rss_bytes();
    let flat_delta = flat_rss.saturating_sub(baseline_rss);
    let flat_count = flat.len();
    drop(flat);

    report_rss("shared paths", legacy_delta, flat_delta, legacy_count, flat_count);

    group.finish();
}

/// Inline size regression guard.
///
/// FileEntryHeader must stay within the design's 48-64 byte target.
/// FileEntry must stay at or below the 96-byte optimization ceiling.
fn bench_size_assertions(c: &mut Criterion) {
    let mut group = c.benchmark_group("flat_vs_legacy_sizes");

    let header_size = std::mem::size_of::<FileEntryHeader>();
    let entry_size = std::mem::size_of::<FileEntry>();

    eprintln!("--- Size assertions ---");
    eprintln!("  FileEntryHeader: {header_size}B (target: <= 64B)");
    eprintln!("  FileEntry:       {entry_size}B (target: <= 96B)");
    eprintln!("  Ratio:           {:.2}x", entry_size as f64 / header_size as f64);
    eprintln!("-----------------------");

    assert!(
        header_size <= 64,
        "FileEntryHeader size {header_size}B exceeds 64B design target"
    );
    assert!(
        entry_size <= 96,
        "FileEntry size {entry_size}B exceeds 96B optimization target"
    );

    group.bench_function("size_check", |b| {
        b.iter(|| {
            black_box(std::mem::size_of::<FileEntryHeader>());
            black_box(std::mem::size_of::<FileEntry>());
        });
    });

    group.finish();
}

criterion_group!(
    name = flat_flist_rss;
    config = Criterion::default();
    targets =
        bench_legacy_allocation,
        bench_flat_allocation,
        bench_iteration_throughput,
        bench_sort_throughput,
        bench_rss_profile,
        bench_size_assertions
);

criterion_main!(flat_flist_rss);
