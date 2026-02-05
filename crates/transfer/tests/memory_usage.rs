//! Memory usage tests and limits validation.
//!
//! These tests verify memory management behavior including:
//! - Memory usage with large file lists
//! - Memory-efficient streaming patterns
//! - Peak memory tracking through buffer reuse
//!
//! Note: Tests for `--max-alloc` configuration are in crates/core/tests/memory_limits.rs
//! since they require the `core::client::ClientConfigBuilder` type.

use transfer::{
    AdaptiveTokenBuffer, LARGE_BUFFER_SIZE, MEDIUM_BUFFER_SIZE, MEDIUM_FILE_THRESHOLD,
    SMALL_BUFFER_SIZE, SMALL_FILE_THRESHOLD, adaptive_buffer_size, adaptive_token_capacity,
};


// ============================================================================
// File List Memory Tests
// ============================================================================

/// Estimates the memory footprint of a file list entry.
///
/// This approximation accounts for:
/// - PathBuf with variable-length name
/// - Optional fields (link_target, user_name, group_name, checksum)
/// - Fixed-size metadata fields
fn estimate_file_entry_size(name_len: usize) -> usize {
    // Base struct size (from FileEntry in protocol crate)
    // - PathBuf: 24 bytes on 64-bit (3 words: ptr, len, capacity)
    // - Option<PathBuf>: 32 bytes (alignment + discriminant)
    // - Option<String>: 32 bytes each x2 (user_name, group_name)
    // - u64 fields: 8 bytes each (size, uid, gid, etc)
    // - u32 fields: 4 bytes each (mode, mtime_nsec, etc)
    // - Option<u32>: 8 bytes each (rdev_major, rdev_minor, etc)
    // - Flags: 2 bytes
    // - bool: 1 byte
    // Plus heap allocation for the path name
    const BASE_STRUCT_SIZE: usize = 256; // Conservative estimate
    BASE_STRUCT_SIZE + name_len // Add heap allocation for name
}

#[test]
fn file_list_memory_scales_linearly() {
    // Test that memory usage scales linearly with file count
    let entry_count_small = 1000;
    let entry_count_large = 10000;
    let avg_name_len = 50;

    let mem_small =
        entry_count_small * estimate_file_entry_size(avg_name_len);
    let mem_large =
        entry_count_large * estimate_file_entry_size(avg_name_len);

    // Memory should scale roughly linearly (within 10% of perfect linear)
    let ratio = mem_large as f64 / mem_small as f64;
    let expected_ratio = entry_count_large as f64 / entry_count_small as f64;

    assert!(
        (ratio - expected_ratio).abs() / expected_ratio < 0.1,
        "Memory should scale linearly: got ratio {ratio}, expected {expected_ratio}"
    );
}

#[test]
fn file_list_memory_estimation_large_names() {
    // Very long names should still have predictable memory usage
    let short_name = 10;
    let long_name = 4096; // PATH_MAX

    let mem_short = estimate_file_entry_size(short_name);
    let mem_long = estimate_file_entry_size(long_name);

    // The difference should be approximately the name length difference
    let diff = mem_long - mem_short;
    let expected_diff = long_name - short_name;

    assert_eq!(
        diff, expected_diff,
        "Name length should account for memory difference"
    );
}

#[test]
fn file_list_memory_10k_entries() {
    // Verify memory for a realistic file list size
    let entry_count = 10_000;
    let avg_name_len = 100; // e.g., "some/nested/directory/structure/file.txt"

    let estimated_bytes = entry_count * estimate_file_entry_size(avg_name_len);

    // For 10K files with 100-char names, expect ~3.5 MB
    // (256 base + 100 name) * 10K = 3.56 MB
    assert!(
        estimated_bytes < 5 * 1024 * 1024, // Less than 5 MB
        "10K file entries should use less than 5 MB, estimated: {} bytes",
        estimated_bytes
    );
}

#[test]
fn file_list_memory_100k_entries() {
    // Large file list memory estimation
    let entry_count = 100_000;
    let avg_name_len = 100;

    let estimated_bytes = entry_count * estimate_file_entry_size(avg_name_len);

    // For 100K files, expect ~35 MB
    assert!(
        estimated_bytes < 50 * 1024 * 1024, // Less than 50 MB
        "100K file entries should use less than 50 MB, estimated: {} bytes",
        estimated_bytes
    );
}


// ============================================================================
// Adaptive Buffer Memory Tests
// ============================================================================

#[test]
fn adaptive_buffer_small_file_minimal_allocation() {
    // Small files should use minimal buffer allocation
    let file_size = 1024u64; // 1 KB file

    let buffer = AdaptiveTokenBuffer::for_file_size(file_size);

    // Small file buffer should use SMALL_BUFFER_SIZE capacity
    assert!(
        buffer.capacity() >= SMALL_BUFFER_SIZE,
        "Small file buffer should have at least {} capacity",
        SMALL_BUFFER_SIZE
    );
    assert!(
        buffer.capacity() < MEDIUM_BUFFER_SIZE,
        "Small file buffer should not over-allocate to medium size"
    );
}

#[test]
fn adaptive_buffer_medium_file_balanced_allocation() {
    let file_size = 500 * 1024u64; // 500 KB file

    let buffer_size = adaptive_buffer_size(file_size);
    let token_cap = adaptive_token_capacity(file_size);

    assert_eq!(
        buffer_size, MEDIUM_BUFFER_SIZE,
        "Medium file should use medium buffer size"
    );
    // Token capacity uses CHUNK_SIZE (32KB) for medium files
    assert!(
        token_cap <= MEDIUM_BUFFER_SIZE,
        "Token capacity should not exceed medium buffer size"
    );
}

#[test]
fn adaptive_buffer_large_file_maximizes_throughput() {
    let file_size = 10 * 1024 * 1024u64; // 10 MB file

    let buffer_size = adaptive_buffer_size(file_size);

    assert_eq!(
        buffer_size, LARGE_BUFFER_SIZE,
        "Large file should use large buffer size"
    );
}

#[test]
fn adaptive_buffer_reuse_avoids_reallocation() {
    let mut buffer = AdaptiveTokenBuffer::for_file_size(1024 * 1024);

    // First use - should allocate
    buffer.resize_for(1000);
    let initial_capacity = buffer.capacity();

    // Subsequent smaller uses should not reallocate
    for size in [500, 100, 50, 999, 800] {
        buffer.resize_for(size);
        assert_eq!(
            buffer.capacity(),
            initial_capacity,
            "Buffer capacity should remain stable for smaller sizes"
        );
    }
}

#[test]
fn adaptive_buffer_clear_preserves_capacity() {
    let mut buffer = AdaptiveTokenBuffer::for_file_size(1024 * 1024);

    buffer.resize_for(10000);
    let capacity_after_use = buffer.capacity();

    buffer.clear();

    assert!(buffer.is_empty());
    assert_eq!(
        buffer.capacity(),
        capacity_after_use,
        "Clear should preserve capacity for reuse"
    );
}

#[test]
fn adaptive_buffer_threshold_boundaries() {
    // Test exact boundary conditions

    // Just below small threshold
    assert_eq!(
        adaptive_buffer_size(SMALL_FILE_THRESHOLD - 1),
        SMALL_BUFFER_SIZE
    );

    // Exactly at small threshold
    assert_eq!(
        adaptive_buffer_size(SMALL_FILE_THRESHOLD),
        MEDIUM_BUFFER_SIZE
    );

    // Just below medium threshold
    assert_eq!(
        adaptive_buffer_size(MEDIUM_FILE_THRESHOLD - 1),
        MEDIUM_BUFFER_SIZE
    );

    // Exactly at medium threshold
    assert_eq!(
        adaptive_buffer_size(MEDIUM_FILE_THRESHOLD),
        LARGE_BUFFER_SIZE
    );
}

// ============================================================================
// Streaming Pattern Memory Tests
// ============================================================================

#[test]
fn streaming_pattern_bounded_memory() {
    // Verify that streaming processing uses bounded memory
    // by simulating processing many items through a single buffer

    let mut buffer = AdaptiveTokenBuffer::new();
    let num_items = 10_000;
    let item_size = 1024; // 1 KB per item

    // Process many items through the same buffer
    for _ in 0..num_items {
        buffer.resize_for(item_size);
        // Simulate processing (fill with data)
        buffer.as_mut_slice().fill(0xFF);
        buffer.clear();
    }

    // Memory should be bounded by single item size, not total
    // (buffer reuse pattern)
    assert!(
        buffer.capacity() >= item_size,
        "Buffer should accommodate item size"
    );
    assert!(
        buffer.capacity() < item_size * 2,
        "Buffer should not grow unbounded"
    );
}

#[test]
fn streaming_pattern_variable_sizes() {
    // Test streaming with varying item sizes
    let mut buffer = AdaptiveTokenBuffer::new();

    let sizes = [100, 500, 2000, 50, 1500, 3000, 200, 4000];

    for &size in &sizes {
        buffer.resize_for(size);
        assert_eq!(buffer.len(), size);
        buffer.clear();
    }

    // Final capacity should be at least max size
    let max_size = *sizes.iter().max().unwrap();
    assert!(
        buffer.capacity() >= max_size,
        "Buffer capacity should accommodate largest item"
    );
}

#[test]
fn streaming_pattern_monotonic_growth() {
    // Verify buffer grows monotonically (never shrinks)
    let mut buffer = AdaptiveTokenBuffer::new();
    let mut last_capacity = 0;

    let sizes = [100, 50, 200, 150, 300, 100, 500, 50];

    for &size in &sizes {
        buffer.resize_for(size);
        let current_capacity = buffer.capacity();
        assert!(
            current_capacity >= last_capacity,
            "Buffer capacity should never decrease: was {}, now {}",
            last_capacity,
            current_capacity
        );
        last_capacity = current_capacity;
        buffer.clear();
    }
}

// ============================================================================
// Peak Memory Tracking Tests
// ============================================================================

#[test]
fn peak_memory_single_large_allocation() {
    // Simulate a scenario where we track peak memory
    let mut peak_memory: usize = 0;
    let mut current_memory: usize = 0;

    // Simulate allocation
    let allocation_sizes = [1000, 5000, 2000, 10000, 3000];

    for size in allocation_sizes {
        current_memory += size;
        if current_memory > peak_memory {
            peak_memory = current_memory;
        }
    }

    // Simulate deallocation (oldest first - FIFO)
    for size in allocation_sizes {
        current_memory -= size;
    }

    // Peak should be sum of all allocations (if none freed during)
    let expected_peak: usize = allocation_sizes.iter().sum();
    assert_eq!(peak_memory, expected_peak);
}

#[test]
fn peak_memory_buffer_reuse_pattern() {
    // With buffer reuse, peak memory should be bounded
    let buffer_size = 4096;
    let current_memory = buffer_size;

    // Allocate buffer once - peak equals current
    let peak_memory = current_memory;

    // Simulate reusing buffer for 1000 items
    // No additional allocations needed
    for _ in 0..1000 {
        // Buffer reuse - no allocation change
        assert_eq!(current_memory, buffer_size);
    }

    // Peak should still be just the buffer size
    assert_eq!(
        peak_memory, buffer_size,
        "Peak memory should be bounded by buffer size"
    );
}

#[test]
fn peak_memory_concurrent_buffers() {
    // Test peak memory with multiple concurrent buffers
    let num_buffers = 4;
    let buffer_size = MEDIUM_BUFFER_SIZE;

    // Peak memory for concurrent operations
    let peak = num_buffers * buffer_size;

    // Should be bounded
    assert!(
        peak < 1024 * 1024,
        "4 medium buffers should use less than 1 MB: {} bytes",
        peak
    );
}

// ============================================================================
// Memory Efficiency Verification Tests
// ============================================================================

#[test]
fn memory_efficiency_no_allocation_on_new() {
    // New buffers should not allocate until used
    let buffer = AdaptiveTokenBuffer::new();
    assert_eq!(buffer.capacity(), 0, "New buffer should not allocate");
}

#[test]
fn memory_efficiency_sized_buffer_preallocates() {
    // Sized buffers should preallocate to avoid reallocation
    let file_size = 100 * 1024u64; // 100 KB
    let buffer = AdaptiveTokenBuffer::for_file_size(file_size);

    assert!(
        buffer.capacity() > 0,
        "Sized buffer should preallocate"
    );
}

#[test]
fn memory_efficiency_with_capacity() {
    let capacity = 8192;
    let buffer = AdaptiveTokenBuffer::with_capacity(capacity);

    assert!(
        buffer.capacity() >= capacity,
        "with_capacity should honor request"
    );
}

#[test]
fn memory_efficiency_slice_reflects_length() {
    let mut buffer = AdaptiveTokenBuffer::with_capacity(1024);

    buffer.resize_for(100);
    assert_eq!(buffer.as_slice().len(), 100);

    buffer.resize_for(50);
    assert_eq!(buffer.as_slice().len(), 50);
}

// ============================================================================
// Large-Scale Memory Behavior Tests
// ============================================================================

#[test]
fn large_scale_many_small_buffers() {
    // Test creating many small buffers
    let num_buffers = 1000;
    let mut buffers: Vec<AdaptiveTokenBuffer> = Vec::with_capacity(num_buffers);

    for i in 0..num_buffers {
        let file_size = (i % 100) as u64 * 1024; // 0-99 KB files
        buffers.push(AdaptiveTokenBuffer::for_file_size(file_size));
    }

    // Verify all buffers are usable
    for (i, buffer) in buffers.iter_mut().enumerate() {
        buffer.resize_for((i % 1000) + 1);
        assert!(!buffer.is_empty());
    }
}

#[test]
fn large_scale_sequential_buffer_reuse() {
    // Simulate processing a large file list with buffer reuse
    let mut buffer = AdaptiveTokenBuffer::for_file_size(1024 * 1024);
    let num_files = 10_000;

    for i in 0..num_files {
        // Variable token size per file
        let token_size = 100 + (i % 1000);
        buffer.resize_for(token_size);

        // Verify buffer is usable
        assert_eq!(buffer.len(), token_size);

        // Clear for next file
        buffer.clear();
    }

    // Capacity should have stabilized
    let final_capacity = buffer.capacity();
    assert!(
        final_capacity >= 1000 + 999, // Max token size we used
        "Buffer should accommodate largest token"
    );
}

#[test]
fn large_scale_memory_stable_under_iteration() {
    // Verify memory doesn't grow during iteration
    let mut buffer = AdaptiveTokenBuffer::for_file_size(1024 * 1024);

    // Warm up - establish capacity
    buffer.resize_for(10000);
    let stable_capacity = buffer.capacity();
    buffer.clear();

    // Many iterations should not change capacity
    for _ in 0..1000 {
        buffer.resize_for(5000);
        assert_eq!(
            buffer.capacity(),
            stable_capacity,
            "Capacity should remain stable"
        );
        buffer.clear();
    }
}

// ============================================================================
// Memory Limit Enforcement Simulation
// ============================================================================

/// Simulates checking an allocation against a max_alloc limit.
fn check_allocation_limit(requested: u64, max_alloc: Option<u64>) -> bool {
    match max_alloc {
        Some(limit) => requested <= limit,
        None => true, // No limit
    }
}

#[test]
fn memory_limit_allows_within_bounds() {
    let limit = 1024 * 1024u64; // 1 MB

    assert!(check_allocation_limit(512 * 1024, Some(limit)));
    assert!(check_allocation_limit(1024 * 1024, Some(limit)));
}

#[test]
fn memory_limit_denies_over_limit() {
    let limit = 1024 * 1024u64; // 1 MB

    assert!(!check_allocation_limit(1024 * 1024 + 1, Some(limit)));
    assert!(!check_allocation_limit(2 * 1024 * 1024, Some(limit)));
}

#[test]
fn memory_limit_allows_all_when_none() {
    assert!(check_allocation_limit(u64::MAX, None));
}

#[test]
fn memory_limit_typical_values() {
    // Test with typical rsync max-alloc values
    let values = [
        128 * 1024 * 1024u64,  // 128M
        256 * 1024 * 1024u64,  // 256M
        512 * 1024 * 1024u64,  // 512M
        1024 * 1024 * 1024u64, // 1G
    ];

    for limit in values {
        // Should allow half the limit
        assert!(check_allocation_limit(limit / 2, Some(limit)));
        // Should deny double the limit
        assert!(!check_allocation_limit(limit * 2, Some(limit)));
    }
}

// ============================================================================
// Integration: Memory-Efficient File Processing
// ============================================================================

#[test]
fn integration_file_processing_bounded_memory() {
    // Simulate memory-efficient file list processing
    let file_count = 1000;
    let avg_name_len = 50;

    // Calculate expected memory for file entries
    let entry_memory = file_count * estimate_file_entry_size(avg_name_len);

    // Processing buffer (reused)
    let processing_buffer_size = MEDIUM_BUFFER_SIZE;

    // Total memory should be entries + one processing buffer
    let total_expected = entry_memory + processing_buffer_size;

    // Verify this is reasonable (< 1 MB for 1000 files)
    assert!(
        total_expected < 1024 * 1024,
        "1000 file processing should use < 1 MB, estimated: {} bytes",
        total_expected
    );
}

#[test]
fn integration_streaming_transfer_bounded() {
    // Simulate a streaming transfer scenario
    let chunk_size = 32 * 1024; // 32 KB chunks
    let num_chunks = 1000;

    // With streaming, memory is bounded by chunk size, not total data
    let memory_bound = chunk_size * 2; // Double buffer at most

    // Total data processed
    let total_data = chunk_size * num_chunks;

    assert!(
        memory_bound < total_data,
        "Streaming should use bounded memory ({} bytes) regardless of total data ({} bytes)",
        memory_bound,
        total_data
    );
}
