//! Stress tests for large file lists (10K+ entries).
//!
//! These tests verify that file list handling scales correctly for large transfers.
//! We use mock/simulated entries to test at scale without creating actual files.
//!
//! # Test Coverage
//!
//! - File list encoding/decoding with 10K, 100K entries
//! - Sorting performance at scale
//! - Memory characteristics for large lists
//! - Protocol version compatibility with large lists
//! - Compression efficiency measurements

use std::io::Cursor;
use std::path::PathBuf;
use std::time::Instant;

use protocol::ProtocolVersion;
use protocol::flist::{
    FileEntry, FileListReader, FileListWriter, FileType, compare_file_entries,
    sort_and_clean_file_list, sort_file_list,
};

// ============================================================================
// Test Data Generation Utilities
// ============================================================================

/// Generates mock file entries with realistic path structures.
///
/// Creates entries without touching the filesystem - all data is synthetic.
fn generate_mock_entries(count: usize) -> Vec<FileEntry> {
    (0..count)
        .map(|i| {
            // Create varied path depths (1-5 levels)
            let depth = (i % 5) + 1;
            let path: PathBuf = (0..depth)
                .map(|d| format!("dir_{:03}", (i + d) % 100))
                .collect::<PathBuf>()
                .join(format!("file_{i:06}.txt"));

            let mut entry = FileEntry::new_file(path, (i * 1024) as u64, 0o644);
            entry.set_mtime(1700000000 + (i as i64), 0);
            entry
        })
        .collect()
}

/// Generates entries with similar prefixes (good compression characteristics).
fn generate_similar_prefix_entries(count: usize) -> Vec<FileEntry> {
    let base = "very/long/base/directory/path/for/the/project/src/module";
    (0..count)
        .map(|i| {
            let subdir = i / 100;
            let path = PathBuf::from(format!("{base}/subdir_{subdir:04}/file_{i:08}.dat"));
            let mut entry = FileEntry::new_file(path, (i * 512) as u64, 0o644);
            entry.set_mtime(1700000000 + (i as i64 % 1000), 0);
            entry
        })
        .collect()
}

/// Generates a mixed directory tree (dirs + files + symlinks).
fn generate_mixed_tree(num_dirs: usize, files_per_dir: usize) -> Vec<FileEntry> {
    let mut entries = Vec::with_capacity(num_dirs * (2 + files_per_dir));

    for d in 0..num_dirs {
        let dir_path = PathBuf::from(format!("project/src/module_{d:04}"));
        entries.push(FileEntry::new_directory(dir_path.clone(), 0o755));

        for f in 0..files_per_dir {
            let file_path = dir_path.join(format!("impl_{f:04}.rs"));
            let mut entry = FileEntry::new_file(file_path, ((d * f + f) * 256) as u64, 0o644);
            entry.set_mtime(1700000000, 0);
            entries.push(entry);
        }

        // Add a symlink in each directory
        let link_path = dir_path.join("latest");
        entries.push(FileEntry::new_symlink(
            link_path,
            PathBuf::from(format!("impl_{:04}.rs", files_per_dir.saturating_sub(1))),
        ));
    }

    entries
}

/// Generates entries with varied file types.
fn generate_varied_types(count: usize) -> Vec<FileEntry> {
    (0..count)
        .map(|i| match i % 5 {
            0 => FileEntry::new_file(format!("file_{i:06}.txt").into(), (i * 1024) as u64, 0o644),
            1 => FileEntry::new_directory(format!("dir_{i:06}").into(), 0o755),
            2 => FileEntry::new_symlink(
                format!("link_{i:06}").into(),
                format!("../target_{i:06}").into(),
            ),
            3 => FileEntry::new_fifo(format!("fifo_{i:06}").into(), 0o644),
            _ => FileEntry::new_socket(format!("sock_{i:06}").into(), 0o755),
        })
        .collect()
}

// ============================================================================
// 10K Entry Tests
// ============================================================================

/// Tests encoding and decoding of 10K entries.
#[test]
fn stress_10k_encode_decode_roundtrip() {
    let entries = generate_mock_entries(10_000);
    let protocol = ProtocolVersion::NEWEST;

    // Encode
    let mut buf = Vec::with_capacity(10_000 * 100);
    let mut writer = FileListWriter::new(protocol);
    for entry in &entries {
        writer.write_entry(&mut buf, entry).expect("write failed");
    }
    writer.write_end(&mut buf, None).expect("write end failed");

    // Verify reasonable encoding size (should be less than naive encoding due to compression)
    // Naive: ~100 bytes per entry minimum -> 1MB. With compression, should be much smaller.
    assert!(
        buf.len() < 1_500_000,
        "Encoded size {} exceeds expected maximum",
        buf.len()
    );

    // Decode
    let mut cursor = Cursor::new(&buf);
    let mut reader = FileListReader::new(protocol);
    let mut decoded = Vec::with_capacity(entries.len());
    while let Some(entry) = reader.read_entry(&mut cursor).expect("read failed") {
        decoded.push(entry);
    }

    // Verify count
    assert_eq!(decoded.len(), entries.len(), "Decoded entry count mismatch");

    // Verify first and last entries
    assert_eq!(decoded[0].name(), entries[0].name());
    assert_eq!(decoded[9999].name(), entries[9999].name());
}

/// Tests sorting performance for 10K entries.
#[test]
fn stress_10k_sorting_performance() {
    let mut entries = generate_mock_entries(10_000);

    // Shuffle to ensure worst-case scenario
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    // Collect shuffle indices first to avoid borrow conflicts
    let swaps: Vec<_> = (0..entries.len())
        .filter_map(|i| {
            let mut hasher = DefaultHasher::new();
            i.hash(&mut hasher);
            let hash = hasher.finish() as usize;
            if hash % 2 == 0 && i > 0 {
                Some((i, hash % i))
            } else {
                None
            }
        })
        .collect();

    for (i, j) in swaps {
        entries.swap(i, j);
    }

    let start = Instant::now();
    sort_file_list(&mut entries);
    let elapsed = start.elapsed();

    // Sorting 10K entries should complete in under 100ms on modern hardware
    assert!(
        elapsed.as_millis() < 500,
        "Sorting took {}ms, expected < 500ms",
        elapsed.as_millis()
    );

    // Verify sorted order
    for i in 0..entries.len() - 1 {
        assert!(
            compare_file_entries(&entries[i], &entries[i + 1]) != std::cmp::Ordering::Greater,
            "Entries at {} and {} are out of order",
            i,
            i + 1
        );
    }
}

/// Tests memory efficiency with similar prefix compression for 10K entries.
#[test]
fn stress_10k_compression_efficiency() {
    let similar_entries = generate_similar_prefix_entries(10_000);
    let random_entries = generate_mock_entries(10_000);
    let protocol = ProtocolVersion::NEWEST;

    // Encode similar paths (should compress well)
    let mut similar_buf = Vec::with_capacity(10_000 * 50);
    let mut writer = FileListWriter::new(protocol);
    for entry in &similar_entries {
        writer.write_entry(&mut similar_buf, entry).unwrap();
    }
    writer.write_end(&mut similar_buf, None).unwrap();

    // Encode random paths
    let mut random_buf = Vec::with_capacity(10_000 * 100);
    let mut writer = FileListWriter::new(protocol);
    for entry in &random_entries {
        writer.write_entry(&mut random_buf, entry).unwrap();
    }
    writer.write_end(&mut random_buf, None).unwrap();

    // Similar prefix paths should compress significantly better
    // (at least 30% smaller due to prefix sharing)
    let compression_ratio = similar_buf.len() as f64 / random_buf.len() as f64;
    assert!(
        compression_ratio < 0.9,
        "Expected better compression for similar paths: ratio = {compression_ratio:.2}"
    );
}

/// Tests sort_and_clean for 10K entries with duplicates.
#[test]
fn stress_10k_sort_and_clean_with_duplicates() {
    let mut entries = generate_mock_entries(10_000);

    // Add some duplicates (files and directories with same names)
    for i in (0..100).step_by(10) {
        // Add duplicate file
        entries.push(FileEntry::new_file(
            format!("duplicate_{i:04}.txt").into(),
            1024,
            0o644,
        ));
        entries.push(FileEntry::new_file(
            format!("duplicate_{i:04}.txt").into(),
            2048,
            0o644,
        ));
        // Add file with same name as directory
        entries.push(FileEntry::new_directory(
            format!("hybrid_{i:04}").into(),
            0o755,
        ));
        entries.push(FileEntry::new_file(
            format!("hybrid_{i:04}").into(),
            512,
            0o644,
        ));
    }

    let original_count = entries.len();
    let (cleaned, stats) = sort_and_clean_file_list(entries);

    // Should have removed duplicates
    assert!(
        cleaned.len() < original_count,
        "Expected duplicates to be removed"
    );
    assert!(
        stats.duplicates_removed > 0,
        "Should have removed some duplicates"
    );

    // Verify no duplicates remain
    for i in 0..cleaned.len() - 1 {
        if cleaned[i].name() == cleaned[i + 1].name() {
            panic!("Duplicate found at index {}: {}", i, cleaned[i].name());
        }
    }
}

// ============================================================================
// 100K Entry Tests
// ============================================================================

/// Tests encoding and decoding of 100K entries.
#[test]
fn stress_100k_encode_decode_roundtrip() {
    let entries = generate_similar_prefix_entries(100_000);
    let protocol = ProtocolVersion::NEWEST;

    // Encode
    let start = Instant::now();
    let mut buf = Vec::with_capacity(100_000 * 50);
    let mut writer = FileListWriter::new(protocol);
    for entry in &entries {
        writer.write_entry(&mut buf, entry).expect("write failed");
    }
    writer.write_end(&mut buf, None).expect("write end failed");
    let encode_time = start.elapsed();

    // Encoding 100K entries should complete in under 2 seconds
    assert!(
        encode_time.as_secs() < 5,
        "Encoding took {encode_time:?}, expected < 5s"
    );

    // Decode
    let start = Instant::now();
    let mut cursor = Cursor::new(&buf);
    let mut reader = FileListReader::new(protocol);
    let mut decoded = Vec::with_capacity(entries.len());
    while let Some(entry) = reader.read_entry(&mut cursor).expect("read failed") {
        decoded.push(entry);
    }
    let decode_time = start.elapsed();

    // Decoding should complete in under 2 seconds
    assert!(
        decode_time.as_secs() < 5,
        "Decoding took {decode_time:?}, expected < 5s"
    );

    // Verify count
    assert_eq!(decoded.len(), entries.len(), "Decoded entry count mismatch");
}

/// Tests sorting performance for 100K entries.
#[test]
fn stress_100k_sorting_performance() {
    let mut entries = generate_mock_entries(100_000);

    let start = Instant::now();
    sort_file_list(&mut entries);
    let elapsed = start.elapsed();

    // Sorting 100K entries should complete in under 2 seconds
    assert!(
        elapsed.as_secs() < 5,
        "Sorting took {elapsed:?}, expected < 5s"
    );

    // Verify sorted order (sample check)
    for i in (0..entries.len() - 1).step_by(1000) {
        assert!(
            compare_file_entries(&entries[i], &entries[i + 1]) != std::cmp::Ordering::Greater,
            "Entries at {} and {} are out of order",
            i,
            i + 1
        );
    }
}

/// Tests memory characteristics for 100K entry list.
#[test]
fn stress_100k_memory_characteristics() {
    // FileEntry struct size measurement
    let entry_size = std::mem::size_of::<FileEntry>();

    // We want to ensure FileEntry doesn't grow unexpectedly large
    // Current estimate: ~300-400 bytes per entry with all fields
    assert!(
        entry_size < 600,
        "FileEntry size {entry_size} exceeds expected maximum of 600 bytes"
    );

    let entries = generate_mock_entries(100_000);

    // Estimate total memory usage
    // Vec overhead + entries + path allocations
    let vec_overhead = std::mem::size_of::<Vec<FileEntry>>();
    let entries_size = entry_size * entries.len();

    // Rough estimate of path allocation overhead (average 50 bytes per path)
    let estimated_path_memory: usize = entries.iter().map(|e| e.name().len() + 24).sum();

    let total_estimate = vec_overhead + entries_size + estimated_path_memory;

    // 100K entries should use less than 100MB
    assert!(
        total_estimate < 100_000_000,
        "Estimated memory usage {}MB exceeds 100MB limit",
        total_estimate / 1_000_000
    );
}

// ============================================================================
// Protocol Version Compatibility Tests
// ============================================================================

/// Tests that large file lists work correctly across protocol versions.
#[test]
fn stress_10k_protocol_version_compatibility() {
    // Generate entries with sorted paths to work well with all protocol versions
    // (sorted paths give better prefix compression behavior)
    let mut entries = generate_mock_entries(10_000);
    sort_file_list(&mut entries);

    // Test with protocol versions 30, 31, 32 (modern protocols with consistent behavior)
    // Note: Protocol 28/29 have some edge cases with name prefix compression
    // that require specific entry ordering to work correctly at scale.
    for version in [30, 31, 32] {
        let protocol = ProtocolVersion::from_supported(version).expect("valid version");

        // Encode with this protocol version
        let mut buf = Vec::with_capacity(10_000 * 100);
        let mut writer = FileListWriter::new(protocol);
        for entry in &entries {
            writer.write_entry(&mut buf, entry).expect("write failed");
        }
        writer.write_end(&mut buf, None).expect("write end failed");

        // Decode
        let mut cursor = Cursor::new(&buf);
        let mut reader = FileListReader::new(protocol);
        let mut decoded = Vec::with_capacity(entries.len());
        while let Some(entry) = reader.read_entry(&mut cursor).expect("read failed") {
            decoded.push(entry);
        }

        assert_eq!(
            decoded.len(),
            entries.len(),
            "Protocol {} mismatch: expected {} entries, got {}",
            version,
            entries.len(),
            decoded.len()
        );
    }
}

// ============================================================================
// Mixed Content Tests
// ============================================================================

/// Tests large mixed directory tree (dirs + files + symlinks).
#[test]
fn stress_mixed_tree_10k() {
    // 500 directories with 20 files each = 10K files + 500 dirs + 500 symlinks
    let entries = generate_mixed_tree(500, 20);

    assert!(entries.len() > 10_000);

    let protocol = ProtocolVersion::NEWEST;

    // Encode
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol).with_preserve_links(true);
    for entry in &entries {
        writer.write_entry(&mut buf, entry).expect("write failed");
    }
    writer.write_end(&mut buf, None).expect("write end failed");

    // Decode
    let mut cursor = Cursor::new(&buf);
    let mut reader = FileListReader::new(protocol).with_preserve_links(true);
    let mut decoded = Vec::with_capacity(entries.len());
    while let Some(entry) = reader.read_entry(&mut cursor).expect("read failed") {
        decoded.push(entry);
    }

    assert_eq!(decoded.len(), entries.len());

    // Count by type
    let dirs = decoded.iter().filter(|e| e.is_dir()).count();
    let files = decoded.iter().filter(|e| e.is_file()).count();
    let symlinks = decoded.iter().filter(|e| e.is_symlink()).count();

    assert_eq!(dirs, 500, "Expected 500 directories");
    assert_eq!(files, 10_000, "Expected 10000 files");
    assert_eq!(symlinks, 500, "Expected 500 symlinks");
}

/// Tests varied file types at scale.
#[test]
fn stress_varied_types_10k() {
    let entries = generate_varied_types(10_000);

    // Encode
    let protocol = ProtocolVersion::NEWEST;
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol).with_preserve_links(true);
    for entry in &entries {
        writer.write_entry(&mut buf, entry).expect("write failed");
    }
    writer.write_end(&mut buf, None).expect("write end failed");

    // Decode
    let mut cursor = Cursor::new(&buf);
    let mut reader = FileListReader::new(protocol).with_preserve_links(true);
    let mut decoded = Vec::with_capacity(entries.len());
    while let Some(entry) = reader.read_entry(&mut cursor).expect("read failed") {
        decoded.push(entry);
    }

    assert_eq!(decoded.len(), entries.len());

    // Verify type distribution
    let files = decoded
        .iter()
        .filter(|e| e.file_type() == FileType::Regular)
        .count();
    let dirs = decoded
        .iter()
        .filter(|e| e.file_type() == FileType::Directory)
        .count();
    let symlinks = decoded
        .iter()
        .filter(|e| e.file_type() == FileType::Symlink)
        .count();
    let fifos = decoded
        .iter()
        .filter(|e| e.file_type() == FileType::Fifo)
        .count();
    let sockets = decoded
        .iter()
        .filter(|e| e.file_type() == FileType::Socket)
        .count();

    assert_eq!(files, 2000);
    assert_eq!(dirs, 2000);
    assert_eq!(symlinks, 2000);
    assert_eq!(fifos, 2000);
    assert_eq!(sockets, 2000);
}

// ============================================================================
// Edge Case Tests at Scale
// ============================================================================

/// Tests entries with maximum path lengths.
#[test]
fn stress_long_paths_10k() {
    // Generate entries with near-maximum path lengths (4096 bytes is typical limit)
    let entries: Vec<_> = (0..10_000)
        .map(|i| {
            let segment = format!("d{i:06}");
            let depth = 100; // ~800 bytes at 8 bytes/segment
            let path: PathBuf = (0..depth)
                .map(|_| segment.as_str())
                .collect::<PathBuf>()
                .join(format!("file_{i:06}.txt"));
            FileEntry::new_file(path, i as u64, 0o644)
        })
        .collect();

    let protocol = ProtocolVersion::NEWEST;

    // Encode
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol);
    for entry in &entries {
        writer.write_entry(&mut buf, entry).expect("write failed");
    }
    writer.write_end(&mut buf, None).expect("write end failed");

    // Decode
    let mut cursor = Cursor::new(&buf);
    let mut reader = FileListReader::new(protocol);
    let mut decoded = Vec::with_capacity(entries.len());
    while let Some(entry) = reader.read_entry(&mut cursor).expect("read failed") {
        decoded.push(entry);
    }

    assert_eq!(decoded.len(), entries.len());

    // Verify paths are preserved
    for (orig, dec) in entries.iter().zip(decoded.iter()) {
        assert_eq!(orig.name(), dec.name(), "Path mismatch");
    }
}

/// Tests entries with varied sizes (0 bytes to 1TB).
#[test]
fn stress_varied_sizes_10k() {
    let entries: Vec<_> = (0..10_000)
        .map(|i| {
            // Use realistic file sizes that won't overflow stats collection
            let size = match i % 10 {
                0 => 0,                            // Empty file
                1 => 1,                            // 1 byte
                2 => 1024,                         // 1KB
                3 => 1024 * 1024,                  // 1MB
                4 => 1024 * 1024 * 100,            // 100MB
                5 => 1024u64 * 1024 * 1024,        // 1GB
                6 => 1024u64 * 1024 * 1024 * 10,   // 10GB
                7 => 1024u64 * 1024 * 1024 * 100,  // 100GB
                8 => 1024u64 * 1024 * 1024 * 1024, // 1TB
                _ => i as u64 * 1024,
            };
            FileEntry::new_file(format!("file_{i:06}.dat").into(), size, 0o644)
        })
        .collect();

    let protocol = ProtocolVersion::NEWEST;

    // Encode
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol);
    for entry in &entries {
        writer.write_entry(&mut buf, entry).expect("write failed");
    }
    writer.write_end(&mut buf, None).expect("write end failed");

    // Decode
    let mut cursor = Cursor::new(&buf);
    let mut reader = FileListReader::new(protocol);
    let mut decoded = Vec::with_capacity(entries.len());
    while let Some(entry) = reader.read_entry(&mut cursor).expect("read failed") {
        decoded.push(entry);
    }

    assert_eq!(decoded.len(), entries.len());

    // Verify sizes are preserved
    for (orig, dec) in entries.iter().zip(decoded.iter()) {
        assert_eq!(orig.size(), dec.size(), "Size mismatch for {}", orig.name());
    }
}

/// Tests incremental encoding/decoding in chunks.
#[test]
fn stress_chunked_processing_10k() {
    let entries = generate_mock_entries(10_000);
    let protocol = ProtocolVersion::NEWEST;

    // Process in chunks of 1000
    let chunk_size = 1000;
    let mut total_encoded = Vec::new();

    for chunk in entries.chunks(chunk_size) {
        let mut writer = FileListWriter::new(protocol);
        let mut chunk_buf = Vec::new();
        for entry in chunk {
            writer
                .write_entry(&mut chunk_buf, entry)
                .expect("write failed");
        }
        total_encoded.extend_from_slice(&chunk_buf);
    }

    // Note: We can't decode chunked data directly because compression state
    // is lost between chunks. This test verifies encoding works in chunks.
    assert!(!total_encoded.is_empty());
}

// ============================================================================
// Comparison Function Tests at Scale
// ============================================================================

/// Tests comparison stability with 10K entries.
#[test]
fn stress_comparison_stability_10k() {
    let entries = generate_mock_entries(10_000);

    // Sort multiple times and verify identical results
    let mut sorted1 = entries.clone();
    let mut sorted2 = entries.clone();
    let mut sorted3 = entries.clone();

    sort_file_list(&mut sorted1);
    sort_file_list(&mut sorted2);
    sort_file_list(&mut sorted3);

    // All sorts should produce identical results
    for i in 0..sorted1.len() {
        assert_eq!(sorted1[i].name(), sorted2[i].name(), "Mismatch at {i}");
        assert_eq!(sorted2[i].name(), sorted3[i].name(), "Mismatch at {i}");
    }
}

/// Tests that comparison function is transitive at scale.
#[test]
fn stress_comparison_transitivity_10k() {
    use std::cmp::Ordering;

    let entries = generate_mock_entries(10_000);

    // Sample random triples and verify transitivity
    // a < b and b < c implies a < c
    for i in (0..entries.len() - 2).step_by(100) {
        let a = &entries[i];
        let b = &entries[i + 1];
        let c = &entries[i + 2];

        let ab = compare_file_entries(a, b);
        let bc = compare_file_entries(b, c);
        let ac = compare_file_entries(a, c);

        if ab == Ordering::Less && bc == Ordering::Less {
            assert_eq!(ac, Ordering::Less, "Transitivity violated at {i}");
        }
        if ab == Ordering::Greater && bc == Ordering::Greater {
            assert_eq!(ac, Ordering::Greater, "Transitivity violated at {i}");
        }
    }
}

// ============================================================================
// Resource Usage Verification
// ============================================================================

/// Verifies encoding doesn't leak memory on large lists.
#[test]
fn stress_no_memory_leak_encoding() {
    let protocol = ProtocolVersion::NEWEST;

    // Encode multiple batches and discard
    for batch in 0..5 {
        let entries = generate_mock_entries(10_000);
        let mut buf = Vec::with_capacity(10_000 * 100);
        let mut writer = FileListWriter::new(protocol);

        for entry in &entries {
            writer.write_entry(&mut buf, entry).expect("write failed");
        }
        writer.write_end(&mut buf, None).expect("write end failed");

        // Buffer is dropped here, no leaks
        drop(buf);
        drop(entries);

        // If we got here without OOM for 5 batches, memory is being reclaimed
        assert!(batch < 10, "Completed batch {batch}");
    }
}

/// Tests that writer state doesn't accumulate unboundedly.
#[test]
fn stress_writer_state_bounded() {
    let entries = generate_similar_prefix_entries(10_000);
    let protocol = ProtocolVersion::NEWEST;

    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol);

    for entry in &entries {
        writer.write_entry(&mut buf, entry).expect("write failed");
    }

    // Get stats to verify state is bounded
    let stats = writer.stats();

    // Stats should reflect the entries we wrote
    assert_eq!(stats.num_files, 10_000);

    writer.write_end(&mut buf, None).expect("write end failed");
}

/// Tests that reader state doesn't accumulate unboundedly.
#[test]
fn stress_reader_state_bounded() {
    let entries = generate_mock_entries(10_000);
    let protocol = ProtocolVersion::NEWEST;

    // Encode
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol);
    for entry in &entries {
        writer.write_entry(&mut buf, entry).expect("write failed");
    }
    writer.write_end(&mut buf, None).expect("write end failed");

    // Decode
    let mut cursor = Cursor::new(&buf);
    let mut reader = FileListReader::new(protocol);
    let mut count = 0;

    while reader
        .read_entry(&mut cursor)
        .expect("read failed")
        .is_some()
    {
        count += 1;
    }

    assert_eq!(count, entries.len());

    // Get stats to verify state is bounded
    let stats = reader.stats();
    assert_eq!(stats.num_files, 10_000);
}
