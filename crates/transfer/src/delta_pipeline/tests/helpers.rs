use std::path::PathBuf;

use engine::concurrent_delta::DeltaWork;

use crate::delta_pipeline::parallel::adaptive_capacity;
use crate::delta_pipeline::threshold::average_target_size;

#[test]
fn adaptive_capacity_small_files_uses_8x_multiplier() {
    // Small-file workloads (< 64 KiB) need a deeper queue to avoid
    // worker starvation since per-file syscall overhead dominates.
    assert_eq!(adaptive_capacity(4, 4096), 32);
    assert_eq!(adaptive_capacity(4, 63 * 1024), 32);
}

#[test]
fn adaptive_capacity_medium_files_uses_4x_multiplier() {
    // Medium files (64 KiB - 1 MiB) interpolate at 4x.
    assert_eq!(adaptive_capacity(4, 64 * 1024), 16);
    assert_eq!(adaptive_capacity(4, 512 * 1024), 16);
    assert_eq!(adaptive_capacity(4, 1024 * 1024), 16);
}

#[test]
fn adaptive_capacity_large_files_uses_2x_multiplier() {
    // Large files (> 1 MiB) are I/O-bound; a deeper queue wastes memory.
    assert_eq!(adaptive_capacity(4, 1024 * 1024 + 1), 8);
    assert_eq!(adaptive_capacity(4, 100 * 1024 * 1024), 8);
}

#[test]
fn adaptive_capacity_unknown_workload_falls_back_to_default() {
    // avg_target_size == 0 means "workload unknown"; preserve the
    // pre-existing 2x multiplier so the change is opt-in.
    assert_eq!(adaptive_capacity(4, 0), 8);
}

#[test]
fn adaptive_capacity_floor_keeps_pipeline_usable() {
    // A degenerate worker_count of 0 must still produce a non-zero
    // capacity, otherwise the bounded channel would panic on create.
    assert_eq!(adaptive_capacity(0, 0), 2);
    assert_eq!(adaptive_capacity(0, 4096), 2);
}

#[test]
fn average_target_size_empty_returns_zero() {
    assert_eq!(average_target_size(&[]), 0);
}

#[test]
fn average_target_size_computes_arithmetic_mean() {
    let items = vec![
        DeltaWork::whole_file(0, PathBuf::from("/a"), 1024),
        DeltaWork::whole_file(1, PathBuf::from("/b"), 2048),
        DeltaWork::whole_file(2, PathBuf::from("/c"), 3072),
    ];
    assert_eq!(average_target_size(&items), 2048);
}

#[test]
fn average_target_size_saturates_at_u64_max() {
    // Saturating accumulator must not panic on a long tail of huge files.
    let items = vec![
        DeltaWork::whole_file(0, PathBuf::from("/a"), u64::MAX),
        DeltaWork::whole_file(1, PathBuf::from("/b"), u64::MAX),
        DeltaWork::whole_file(2, PathBuf::from("/c"), u64::MAX),
    ];
    // Average of three u64::MAX values is u64::MAX (within rounding).
    assert!(average_target_size(&items) >= u64::MAX - 1);
}
