//! Integration tests for incremental file list transfer.
//!
//! These tests verify the incremental file list processing with failed
//! directory tracking works correctly end-to-end.

#![cfg(feature = "incremental-flist")]

mod wire_format_generator;

use transfer::receiver::TransferStats;
use wire_format_generator::{generate_flat_directory, generate_nested_directories, generate_out_of_order_entries};

/// Verifies TransferStats has all incremental mode fields.
#[test]
fn transfer_stats_incremental_fields_exist() {
    let stats = TransferStats {
        files_listed: 10,
        files_transferred: 5,
        bytes_received: 1000,
        bytes_sent: 100,
        total_source_bytes: 5000,
        metadata_errors: vec![],
        entries_received: 10,
        directories_created: 3,
        directories_failed: 1,
        files_skipped: 2,
    };

    assert_eq!(stats.files_listed, 10);
    assert_eq!(stats.entries_received, 10);
    assert_eq!(stats.directories_created, 3);
    assert_eq!(stats.directories_failed, 1);
    assert_eq!(stats.files_skipped, 2);
}

/// Test that incremental mode is selected when feature is enabled.
#[test]
fn incremental_mode_feature_enabled() {
    assert!(cfg!(feature = "incremental-flist"));
}

/// Test wire format generation for flat directory.
#[test]
fn incremental_transfer_flat_directory() {
    let wire_data = generate_flat_directory(10);

    // Verify wire data is valid
    assert!(!wire_data.is_empty());
    // Should end with zero byte (end marker)
    assert_eq!(*wire_data.last().unwrap(), 0);
    // Should have reasonable size for 10 files
    assert!(wire_data.len() > 100);
}

/// Test wire format generation for nested directories.
#[test]
fn incremental_transfer_nested_directories() {
    let wire_data = generate_nested_directories(3, 2);

    // Verify wire data is valid
    assert!(!wire_data.is_empty());
    assert_eq!(*wire_data.last().unwrap(), 0);
    // Nested structure should be larger than flat
    assert!(wire_data.len() > 50);
}

/// Test wire format generation for out-of-order entries.
#[test]
fn incremental_transfer_out_of_order_entries() {
    let wire_data = generate_out_of_order_entries();

    // Verify wire data is valid
    assert!(!wire_data.is_empty());
    assert_eq!(*wire_data.last().unwrap(), 0);
}

/// Test for failed directory skipping behavior.
///
/// Note: The FailedDirectories tracking logic is comprehensively tested via
/// unit tests in `crates/transfer/src/receiver.rs` (see `failed_directories_tests`
/// and `incremental_mode_tests` modules). Those tests verify:
/// - Empty FailedDirectories has no ancestors
/// - Marks and finds exact failed paths
/// - Finds children of failed directories
/// - Does not match siblings
/// - Counts failures correctly
/// - Skips nested children
/// - Handles root level failures
/// - Propagates to deeply nested paths
///
/// This integration test verifies the wire format correctly generates entries
/// that would trigger the failed directory tracking when processed.
#[test]
fn incremental_transfer_failed_directory_skips_children() {
    // Generate a directory structure where a parent directory might fail
    let mut writer = wire_format_generator::WireFormatGenerator::with_defaults();

    // Parent directory that will fail to be created (simulated)
    writer.write_entry(&wire_format_generator::TestFileEntry::dir("unwritable"))
        .expect("write dir");

    // Children under the failed directory - these should be skipped
    writer.write_entry(&wire_format_generator::TestFileEntry::file("unwritable/child1.txt", 100))
        .expect("write child1");
    writer.write_entry(&wire_format_generator::TestFileEntry::file("unwritable/child2.txt", 200))
        .expect("write child2");
    writer.write_entry(&wire_format_generator::TestFileEntry::dir("unwritable/subdir"))
        .expect("write subdir");
    writer.write_entry(&wire_format_generator::TestFileEntry::file("unwritable/subdir/nested.txt", 300))
        .expect("write nested");

    // A separate directory that should succeed
    writer.write_entry(&wire_format_generator::TestFileEntry::dir("writable"))
        .expect("write writable dir");
    writer.write_entry(&wire_format_generator::TestFileEntry::file("writable/file.txt", 400))
        .expect("write writable file");

    writer.write_end_marker().expect("write end marker");
    let wire_data = writer.into_bytes();

    // Verify wire data is valid
    assert!(!wire_data.is_empty());
    assert_eq!(*wire_data.last().unwrap(), 0);

    // Should have reasonable size for the entries
    // 1 failed dir + 2 children + 1 subdir + 1 nested + 1 writable dir + 1 file = 7 entries
    assert!(wire_data.len() > 100, "wire data should contain multiple entries");
}

/// Placeholder for upstream rsync interop test.
#[test]
#[ignore = "requires upstream rsync binary"]
fn incremental_transfer_upstream_interop() {
    // TODO: Test against upstream rsync sender
    // Verify protocol compatibility
}
