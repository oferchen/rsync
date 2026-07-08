//! Integration tests for the `--max-size` / `--min-size` skip boundary.
//!
//! Upstream rsync skips a regular file whose length is strictly greater than
//! `max_size` or strictly less than `min_size`, so a file whose size is exactly
//! equal to the limit is always kept:
//!
//! ```c
//! // upstream: generator.c:1704
//! if (max_size >= 0 && F_LENGTH(file) > max_size) { ... goto cleanup; }
//! // upstream: generator.c:1712
//! if (min_size >= 0 && F_LENGTH(file) < min_size) { ... goto cleanup; }
//! ```
//!
//! These tests pin the boundary at 1024 bytes (`1K`) using three files sized
//! 1023, 1024, and 1025 bytes, matching an empirical comparison against
//! upstream rsync 3.4.4.

use std::ffi::OsString;
use std::fs;

use engine::local_copy::{LocalCopyExecution, LocalCopyOptions, LocalCopyPlan};
use tempfile::tempdir;

/// Copies a single `size`-byte file with the given options and reports whether
/// the destination was written (i.e. the file was not skipped).
fn copy_is_kept(size: usize, configure: impl FnOnce(LocalCopyOptions) -> LocalCopyOptions) -> bool {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("src.bin");
    let destination = temp.path().join("dst.bin");
    fs::write(&source, vec![0u8; size]).expect("write source");

    let operands = vec![
        OsString::from(source.as_os_str()),
        OsString::from(destination.as_os_str()),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan succeeds");
    let options = configure(LocalCopyOptions::new());
    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    destination.exists()
}

#[test]
fn max_size_keeps_file_exactly_at_limit() {
    // A 1024-byte file equals --max-size=1K and must be kept (boundary is
    // exclusive); 1025 bytes exceeds it and is skipped.
    assert!(
        copy_is_kept(1023, |o| o.max_file_size(Some(1024))),
        "1023 < 1024 must be kept under --max-size=1K"
    );
    assert!(
        copy_is_kept(1024, |o| o.max_file_size(Some(1024))),
        "1024 == 1024 must be kept under --max-size=1K (upstream: > max_size)"
    );
    assert!(
        !copy_is_kept(1025, |o| o.max_file_size(Some(1024))),
        "1025 > 1024 must be skipped under --max-size=1K"
    );
}

#[test]
fn min_size_keeps_file_exactly_at_limit() {
    // A 1024-byte file equals --min-size=1K and must be kept; 1023 bytes is
    // below it and is skipped.
    assert!(
        !copy_is_kept(1023, |o| o.min_file_size(Some(1024))),
        "1023 < 1024 must be skipped under --min-size=1K"
    );
    assert!(
        copy_is_kept(1024, |o| o.min_file_size(Some(1024))),
        "1024 == 1024 must be kept under --min-size=1K (upstream: < min_size)"
    );
    assert!(
        copy_is_kept(1025, |o| o.min_file_size(Some(1024))),
        "1025 > 1024 must be kept under --min-size=1K"
    );
}
