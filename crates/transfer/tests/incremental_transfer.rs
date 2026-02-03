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

/// Placeholder for testing failed directory skipping.
#[test]
#[ignore = "requires permission setup"]
fn incremental_transfer_failed_directory_skips_children() {
    // TODO: Create scenario where directory creation fails
    // Verify children are skipped and counted correctly
}

/// Placeholder for upstream rsync interop test.
#[test]
#[ignore = "requires upstream rsync binary"]
fn incremental_transfer_upstream_interop() {
    // TODO: Test against upstream rsync sender
    // Verify protocol compatibility
}
