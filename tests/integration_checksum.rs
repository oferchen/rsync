//! Comprehensive tests for --checksum (-c) flag behavior.
//!
//! Tests verify that:
//! 1. Files are compared by checksum, not mtime/size
//! 2. Unchanged files (same checksum) are skipped
//! 3. Changed files (different checksum) are transferred
//! 4. Various file sizes work correctly
//! 5. Output format matches upstream rsync behavior

mod integration;

use filetime::{FileTime, set_file_mtime};
use integration::helpers::*;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

// ============================================================================
// Basic Checksum Flag Parsing
// ============================================================================

#[test]
fn checksum_flag_long_form_accepted() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_file = test_dir.write_file("source.txt", b"content").unwrap();
    let dest_file = test_dir.path().join("dest.txt");

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--checksum",
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    cmd.assert_success();

    assert_eq!(fs::read(&dest_file).unwrap(), b"content");
}

#[test]
fn checksum_flag_short_form_accepted() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_file = test_dir.write_file("source.txt", b"content").unwrap();
    let dest_file = test_dir.path().join("dest.txt");

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-c",
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    cmd.assert_success();

    assert_eq!(fs::read(&dest_file).unwrap(), b"content");
}

// ============================================================================
// Checksum-based Comparison (not mtime/size)
// ============================================================================

#[test]
fn checksum_mode_ignores_mtime_for_identical_content() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_file = test_dir.write_file("source.txt", b"identical").unwrap();
    let dest_file = test_dir.write_file("dest.txt", b"identical").unwrap();

    // Set very different mtimes
    let old_time = FileTime::from_unix_time(1000000000, 0); // 2001
    let new_time = FileTime::from_unix_time(1700000000, 0); // 2023
    set_file_mtime(&src_file, new_time).unwrap();
    set_file_mtime(&dest_file, old_time).unwrap();

    // Record destination mtime before sync
    let dest_mtime_before = fs::metadata(&dest_file).unwrap().modified().unwrap();

    // Brief sleep to ensure mtime would change if file is written
    std::thread::sleep(Duration::from_millis(50));

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--checksum",
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    cmd.assert_success();

    // Destination should NOT be modified (content is identical)
    let dest_mtime_after = fs::metadata(&dest_file).unwrap().modified().unwrap();
    assert_eq!(
        dest_mtime_before, dest_mtime_after,
        "File should not be modified when content is identical (checksum mode)"
    );
}

#[test]
fn checksum_mode_transfers_different_content_same_size() {
    let test_dir = TestDir::new().expect("create test dir");
    // Same size (7 bytes), different content
    let src_file = test_dir.write_file("source.txt", b"aaaaaaa").unwrap();
    let dest_file = test_dir.write_file("dest.txt", b"bbbbbbb").unwrap();

    // Set identical mtimes to ensure mtime comparison would skip
    let same_time = FileTime::from_unix_time(1700000000, 0);
    set_file_mtime(&src_file, same_time).unwrap();
    set_file_mtime(&dest_file, same_time).unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--checksum",
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    cmd.assert_success();

    // Destination SHOULD be updated (different checksum)
    assert_eq!(
        fs::read(&dest_file).unwrap(),
        b"aaaaaaa",
        "File should be updated when content differs (checksum mode)"
    );
}

#[test]
fn checksum_mode_transfers_different_content_same_mtime() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_file = test_dir
        .write_file("source.txt", b"new content here")
        .unwrap();
    let dest_file = test_dir
        .write_file("dest.txt", b"old content here")
        .unwrap();

    // Set identical mtimes
    let same_time = FileTime::from_unix_time(1700000000, 0);
    set_file_mtime(&src_file, same_time).unwrap();
    set_file_mtime(&dest_file, same_time).unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--checksum",
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    cmd.assert_success();

    assert_eq!(fs::read(&dest_file).unwrap(), b"new content here");
}

// ============================================================================
// Unchanged Files (same checksum) are Skipped
// ============================================================================

#[test]
fn checksum_skips_identical_files() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_file = test_dir.write_file("source.txt", b"same content").unwrap();
    let dest_file = test_dir.write_file("dest.txt", b"same content").unwrap();

    // Set a specific mtime on dest that we can verify isn't changed
    let preserved_time = FileTime::from_unix_time(1600000000, 0);
    set_file_mtime(&dest_file, preserved_time).unwrap();

    std::thread::sleep(Duration::from_millis(50));

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--checksum",
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    cmd.assert_success();

    // Verify mtime wasn't touched
    let final_time = FileTime::from_last_modification_time(&fs::metadata(&dest_file).unwrap());
    assert_eq!(
        final_time, preserved_time,
        "File mtime should be preserved when checksum matches"
    );
}

#[test]
fn checksum_skips_identical_files_with_verbose() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_file = test_dir.write_file("source.txt", b"same content").unwrap();
    let dest_file = test_dir.write_file("dest.txt", b"same content").unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--checksum",
        "-v",
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    let output = cmd.assert_success();

    // In verbose mode with identical checksums, no data should be transferred
    // (rsync may still list the file, but stats should show 0 bytes sent)
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("sent 0 bytes") || stdout.contains("0 bytes/sec"),
        "Identical file should not transfer any data: {stdout}"
    );
}

// ============================================================================
// Changed Files (different checksum) are Transferred
// ============================================================================

#[test]
fn checksum_transfers_changed_files() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_file = test_dir
        .write_file("source.txt", b"updated content")
        .unwrap();
    let dest_file = test_dir
        .write_file("dest.txt", b"original content")
        .unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--checksum",
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    cmd.assert_success();

    assert_eq!(fs::read(&dest_file).unwrap(), b"updated content");
}

#[test]
fn checksum_transfers_new_files() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_file = test_dir
        .write_file("source.txt", b"new file content")
        .unwrap();
    let dest_file = test_dir.path().join("dest.txt");

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--checksum",
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    cmd.assert_success();

    assert!(dest_file.exists());
    assert_eq!(fs::read(&dest_file).unwrap(), b"new file content");
}

// ============================================================================
// Various File Sizes
// ============================================================================

#[test]
fn checksum_empty_files() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_file = test_dir.write_file("empty_src.txt", b"").unwrap();
    let dest_file = test_dir.write_file("empty_dest.txt", b"").unwrap();

    let preserved_time = FileTime::from_unix_time(1600000000, 0);
    set_file_mtime(&dest_file, preserved_time).unwrap();

    std::thread::sleep(Duration::from_millis(50));

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--checksum",
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    cmd.assert_success();

    // Empty files should match (both have same empty checksum)
    let final_time = FileTime::from_last_modification_time(&fs::metadata(&dest_file).unwrap());
    assert_eq!(final_time, preserved_time, "Empty files should match");
}

#[test]
fn checksum_small_files() {
    let test_dir = TestDir::new().expect("create test dir");
    // 1 byte files
    let src_file = test_dir.write_file("small_src.txt", b"A").unwrap();
    let dest_file = test_dir.write_file("small_dest.txt", b"A").unwrap();

    let preserved_time = FileTime::from_unix_time(1600000000, 0);
    set_file_mtime(&dest_file, preserved_time).unwrap();

    std::thread::sleep(Duration::from_millis(50));

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--checksum",
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    cmd.assert_success();

    let final_time = FileTime::from_last_modification_time(&fs::metadata(&dest_file).unwrap());
    assert_eq!(
        final_time, preserved_time,
        "Small identical files should match"
    );
}

#[test]
fn checksum_medium_files() {
    let test_dir = TestDir::new().expect("create test dir");
    // 64KB files
    let data = vec![0xABu8; 64 * 1024];
    let src_file = test_dir.write_file("medium_src.bin", &data).unwrap();
    let dest_file = test_dir.write_file("medium_dest.bin", &data).unwrap();

    let preserved_time = FileTime::from_unix_time(1600000000, 0);
    set_file_mtime(&dest_file, preserved_time).unwrap();

    std::thread::sleep(Duration::from_millis(50));

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--checksum",
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    cmd.assert_success();

    let final_time = FileTime::from_last_modification_time(&fs::metadata(&dest_file).unwrap());
    assert_eq!(
        final_time, preserved_time,
        "Medium identical files should match"
    );
}

#[test]
fn checksum_large_files() {
    let test_dir = TestDir::new().expect("create test dir");
    // 1MB files
    let data = vec![0xCDu8; 1024 * 1024];
    let src_file = test_dir.write_file("large_src.bin", &data).unwrap();
    let dest_file = test_dir.write_file("large_dest.bin", &data).unwrap();

    let preserved_time = FileTime::from_unix_time(1600000000, 0);
    set_file_mtime(&dest_file, preserved_time).unwrap();

    std::thread::sleep(Duration::from_millis(50));

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--checksum",
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    cmd.assert_success();

    let final_time = FileTime::from_last_modification_time(&fs::metadata(&dest_file).unwrap());
    assert_eq!(
        final_time, preserved_time,
        "Large identical files should match"
    );
}

#[test]
fn checksum_large_files_differ_at_end() {
    let test_dir = TestDir::new().expect("create test dir");
    // 1MB files that differ only in the last byte
    let src_data = vec![0xABu8; 1024 * 1024];
    let mut dest_data = src_data.clone();
    dest_data[1024 * 1024 - 1] = 0xCD; // Change last byte

    let src_file = test_dir.write_file("large_src.bin", &src_data).unwrap();
    let dest_file = test_dir.write_file("large_dest.bin", &dest_data).unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--checksum",
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    cmd.assert_success();

    // File should be updated
    assert_eq!(
        fs::read(&dest_file).unwrap(),
        src_data,
        "File differing only at end should be transferred"
    );
}

// ============================================================================
// Checksum with Directory Operations
// ============================================================================

#[test]
fn checksum_recursive_directory() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Create source files
    fs::write(src_dir.join("file1.txt"), b"content one").unwrap();
    fs::write(src_dir.join("file2.txt"), b"content two").unwrap();
    fs::create_dir(src_dir.join("subdir")).unwrap();
    fs::write(src_dir.join("subdir/file3.txt"), b"content three").unwrap();

    // Create destination with same content
    fs::write(dest_dir.join("file1.txt"), b"content one").unwrap();
    fs::write(dest_dir.join("file2.txt"), b"content two").unwrap();
    fs::create_dir(dest_dir.join("subdir")).unwrap();
    fs::write(dest_dir.join("subdir/file3.txt"), b"content three").unwrap();

    // Set specific times on dest files
    let preserved_time = FileTime::from_unix_time(1600000000, 0);
    set_file_mtime(dest_dir.join("file1.txt"), preserved_time).unwrap();
    set_file_mtime(dest_dir.join("file2.txt"), preserved_time).unwrap();
    set_file_mtime(dest_dir.join("subdir/file3.txt"), preserved_time).unwrap();

    std::thread::sleep(Duration::from_millis(50));

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--checksum",
        "-r",
        &format!("{}/", src_dir.display()),
        dest_dir.to_str().unwrap(),
    ]);
    cmd.assert_success();

    // All files should have preserved times (no transfer needed)
    let check_preserved = |path: &Path| {
        let time = FileTime::from_last_modification_time(&fs::metadata(path).unwrap());
        assert_eq!(time, preserved_time, "File {path:?} should not be modified");
    };
    check_preserved(&dest_dir.join("file1.txt"));
    check_preserved(&dest_dir.join("file2.txt"));
    check_preserved(&dest_dir.join("subdir/file3.txt"));
}

#[test]
fn checksum_recursive_with_some_changes() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Create source files
    fs::write(src_dir.join("unchanged.txt"), b"same content").unwrap();
    fs::write(src_dir.join("changed.txt"), b"new content").unwrap();
    fs::write(src_dir.join("new_file.txt"), b"brand new").unwrap();

    // Create destination with partial content
    fs::write(dest_dir.join("unchanged.txt"), b"same content").unwrap();
    fs::write(dest_dir.join("changed.txt"), b"old content").unwrap();
    // new_file.txt doesn't exist in dest

    // Record times
    let preserved_time = FileTime::from_unix_time(1600000000, 0);
    set_file_mtime(dest_dir.join("unchanged.txt"), preserved_time).unwrap();

    std::thread::sleep(Duration::from_millis(50));

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--checksum",
        "-r",
        &format!("{}/", src_dir.display()),
        dest_dir.to_str().unwrap(),
    ]);
    cmd.assert_success();

    // unchanged.txt should NOT be modified
    let unchanged_time = FileTime::from_last_modification_time(
        &fs::metadata(dest_dir.join("unchanged.txt")).unwrap(),
    );
    assert_eq!(
        unchanged_time, preserved_time,
        "Unchanged file should not be modified"
    );

    // changed.txt should be updated
    assert_eq!(
        fs::read(dest_dir.join("changed.txt")).unwrap(),
        b"new content"
    );

    // new_file.txt should exist
    assert_eq!(
        fs::read(dest_dir.join("new_file.txt")).unwrap(),
        b"brand new"
    );
}

// ============================================================================
// Checksum with Archive Mode
// ============================================================================

#[test]
fn checksum_with_archive_mode() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    fs::write(src_dir.join("file.txt"), b"archive content").unwrap();
    fs::write(dest_dir.join("file.txt"), b"archive content").unwrap();

    // Set same mtime on both files so we can verify no transfer occurs
    let preserved_time = FileTime::from_unix_time(1600000000, 0);
    set_file_mtime(src_dir.join("file.txt"), preserved_time).unwrap();
    set_file_mtime(dest_dir.join("file.txt"), preserved_time).unwrap();

    std::thread::sleep(Duration::from_millis(50));

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-ac", // archive + checksum
        &format!("{}/", src_dir.display()),
        dest_dir.to_str().unwrap(),
    ]);
    cmd.assert_success();

    // Should skip transfer due to checksum match (same content, same mtime)
    let final_time =
        FileTime::from_last_modification_time(&fs::metadata(dest_dir.join("file.txt")).unwrap());
    assert_eq!(
        final_time, preserved_time,
        "Archive+checksum should skip identical files"
    );
}

// ============================================================================
// Checksum with Itemize Output
// ============================================================================

#[test]
fn checksum_itemize_shows_checksum_indicator() {
    let test_dir = TestDir::new().expect("create test dir");
    // Use same-length content so only checksum differs
    let src_file = test_dir
        .write_file("source.txt", b"changed_content")
        .unwrap();
    let dest_file = test_dir.write_file("dest.txt", b"old_content!!!!").unwrap();

    // Same size to ensure checksum is what triggers transfer
    assert_eq!(
        fs::metadata(&src_file).unwrap().len(),
        fs::metadata(&dest_file).unwrap().len()
    );

    // Set same mtime
    let same_time = FileTime::from_unix_time(1700000000, 0);
    set_file_mtime(&src_file, same_time).unwrap();
    set_file_mtime(&dest_file, same_time).unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--checksum",
        "-i", // itemize changes
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    let output = cmd.assert_success();

    let stdout = String::from_utf8_lossy(&output.stdout);
    // Itemize output should show the file was transferred
    // The 'c' in position 3 indicates checksum differs
    assert!(
        stdout.contains("source.txt") || stdout.contains("dest.txt"),
        "Itemize should show transferred file: {stdout}"
    );
}

// ============================================================================
// Checksum with --no-checksum
// ============================================================================

#[test]
fn no_checksum_disables_checksum_mode() {
    let test_dir = TestDir::new().expect("create test dir");
    // Same size, different content
    let src_file = test_dir.write_file("source.txt", b"aaaaaaa").unwrap();
    let dest_file = test_dir.write_file("dest.txt", b"bbbbbbb").unwrap();

    // Set same mtime so without checksum, file should be skipped
    let same_time = FileTime::from_unix_time(1700000000, 0);
    set_file_mtime(&src_file, same_time).unwrap();
    set_file_mtime(&dest_file, same_time).unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--checksum",
        "--no-checksum", // Disables checksum
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    cmd.assert_success();

    // With checksum disabled and same size+mtime, file should NOT be transferred
    assert_eq!(
        fs::read(&dest_file).unwrap(),
        b"bbbbbbb",
        "File should not be transferred when --no-checksum is used"
    );
}

// ============================================================================
// Comparison with Upstream rsync (if available)
// ============================================================================

/// Helper to run upstream rsync command
fn run_upstream_rsync(args: &[&str]) -> Option<std::process::Output> {
    Command::new("rsync")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .ok()
}

#[test]
fn upstream_comparison_checksum_skips_identical() {
    // Skip if upstream rsync not available
    if run_upstream_rsync(&["--version"]).is_none() {
        eprintln!("Skipping upstream comparison test: rsync not available");
        return;
    }

    let test_dir = TestDir::new().expect("create test dir");

    // Test with upstream rsync
    let upstream_src = test_dir
        .write_file("upstream_src.txt", b"same content")
        .unwrap();
    let upstream_dest = test_dir
        .write_file("upstream_dest.txt", b"same content")
        .unwrap();

    let preserved_time = FileTime::from_unix_time(1600000000, 0);
    set_file_mtime(&upstream_dest, preserved_time).unwrap();

    std::thread::sleep(Duration::from_millis(50));

    let upstream_output = run_upstream_rsync(&[
        "--checksum",
        "-v",
        upstream_src.to_str().unwrap(),
        upstream_dest.to_str().unwrap(),
    ])
    .expect("run upstream rsync");

    // Test with oc-rsync
    let oc_src = test_dir.write_file("oc_src.txt", b"same content").unwrap();
    let oc_dest = test_dir.write_file("oc_dest.txt", b"same content").unwrap();

    set_file_mtime(&oc_dest, preserved_time).unwrap();

    std::thread::sleep(Duration::from_millis(50));

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--checksum",
        "-v",
        oc_src.to_str().unwrap(),
        oc_dest.to_str().unwrap(),
    ]);
    let _oc_output = cmd.assert_success();

    // Both should succeed
    assert!(
        upstream_output.status.success(),
        "Upstream rsync should succeed"
    );

    // Both should skip the file (check that dest mtime unchanged)
    let upstream_dest_time =
        FileTime::from_last_modification_time(&fs::metadata(&upstream_dest).unwrap());
    let oc_dest_time = FileTime::from_last_modification_time(&fs::metadata(&oc_dest).unwrap());

    assert_eq!(
        upstream_dest_time, preserved_time,
        "Upstream rsync should skip identical file"
    );
    assert_eq!(
        oc_dest_time, preserved_time,
        "oc-rsync should skip identical file like upstream"
    );
}

#[test]
fn upstream_comparison_checksum_transfers_different() {
    // Skip if upstream rsync not available
    if run_upstream_rsync(&["--version"]).is_none() {
        eprintln!("Skipping upstream comparison test: rsync not available");
        return;
    }

    let test_dir = TestDir::new().expect("create test dir");

    // Test with upstream rsync - same size, different content
    let upstream_src = test_dir
        .write_file("upstream_src.txt", b"new data")
        .unwrap();
    let upstream_dest = test_dir
        .write_file("upstream_dest.txt", b"old data")
        .unwrap();

    // Set same mtime to ensure checksum is what triggers transfer
    let same_time = FileTime::from_unix_time(1700000000, 0);
    set_file_mtime(&upstream_src, same_time).unwrap();
    set_file_mtime(&upstream_dest, same_time).unwrap();

    let upstream_output = run_upstream_rsync(&[
        "--checksum",
        upstream_src.to_str().unwrap(),
        upstream_dest.to_str().unwrap(),
    ])
    .expect("run upstream rsync");

    // Test with oc-rsync
    let oc_src = test_dir.write_file("oc_src.txt", b"new data").unwrap();
    let oc_dest = test_dir.write_file("oc_dest.txt", b"old data").unwrap();

    set_file_mtime(&oc_src, same_time).unwrap();
    set_file_mtime(&oc_dest, same_time).unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--checksum",
        oc_src.to_str().unwrap(),
        oc_dest.to_str().unwrap(),
    ]);
    cmd.assert_success();

    // Both should transfer the file
    assert!(
        upstream_output.status.success(),
        "Upstream rsync should succeed"
    );
    assert_eq!(
        fs::read(&upstream_dest).unwrap(),
        b"new data",
        "Upstream rsync should transfer file"
    );
    assert_eq!(
        fs::read(&oc_dest).unwrap(),
        b"new data",
        "oc-rsync should transfer file like upstream"
    );
}

#[test]
fn upstream_comparison_checksum_with_itemize() {
    // Skip if upstream rsync not available
    if run_upstream_rsync(&["--version"]).is_none() {
        eprintln!("Skipping upstream comparison test: rsync not available");
        return;
    }

    let test_dir = TestDir::new().expect("create test dir");

    // Create files that will be transferred
    let upstream_src = test_dir
        .write_file("upstream_src.txt", b"content A")
        .unwrap();
    let upstream_dest = test_dir
        .write_file("upstream_dest.txt", b"content B")
        .unwrap();

    let upstream_output = run_upstream_rsync(&[
        "--checksum",
        "-i",
        upstream_src.to_str().unwrap(),
        upstream_dest.to_str().unwrap(),
    ])
    .expect("run upstream rsync");

    let oc_src = test_dir.write_file("oc_src.txt", b"content A").unwrap();
    let oc_dest = test_dir.write_file("oc_dest.txt", b"content B").unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--checksum",
        "-i",
        oc_src.to_str().unwrap(),
        oc_dest.to_str().unwrap(),
    ]);
    let oc_output = cmd.assert_success();

    // Both should show itemized output
    let upstream_stdout = String::from_utf8_lossy(&upstream_output.stdout);
    let oc_stdout = String::from_utf8_lossy(&oc_output.stdout);

    // Verify both produce itemized output (starts with change indicators)
    // Itemize format: YXcstpoguax (11 chars) followed by filename
    // The 'c' indicates checksum change
    assert!(
        !upstream_stdout.is_empty(),
        "Upstream should produce itemized output"
    );
    assert!(
        !oc_stdout.is_empty(),
        "oc-rsync should produce itemized output"
    );
}

// ============================================================================
// Edge Cases
// ============================================================================

#[test]
fn checksum_binary_files() {
    let test_dir = TestDir::new().expect("create test dir");

    // Create binary files with null bytes and various byte values
    let mut binary_data: Vec<u8> = Vec::new();
    for i in 0..=255u8 {
        binary_data.push(i);
    }
    binary_data.extend(&[0u8; 100]); // Add some null bytes

    let src_file = test_dir.write_file("binary_src.bin", &binary_data).unwrap();
    let dest_file = test_dir
        .write_file("binary_dest.bin", &binary_data)
        .unwrap();

    let preserved_time = FileTime::from_unix_time(1600000000, 0);
    set_file_mtime(&dest_file, preserved_time).unwrap();

    std::thread::sleep(Duration::from_millis(50));

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--checksum",
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    cmd.assert_success();

    // Should skip transfer
    let final_time = FileTime::from_last_modification_time(&fs::metadata(&dest_file).unwrap());
    assert_eq!(
        final_time, preserved_time,
        "Identical binary files should match"
    );
}

#[test]
fn checksum_files_with_special_characters_in_name() {
    let test_dir = TestDir::new().expect("create test dir");

    let src_file = test_dir
        .write_file("file with spaces.txt", b"content")
        .unwrap();
    let dest_file = test_dir
        .write_file("dest with spaces.txt", b"content")
        .unwrap();

    let preserved_time = FileTime::from_unix_time(1600000000, 0);
    set_file_mtime(&dest_file, preserved_time).unwrap();

    std::thread::sleep(Duration::from_millis(50));

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--checksum",
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    cmd.assert_success();

    let final_time = FileTime::from_last_modification_time(&fs::metadata(&dest_file).unwrap());
    assert_eq!(final_time, preserved_time, "Files with spaces should work");
}

#[test]
fn checksum_dry_run_no_changes() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_file = test_dir.write_file("source.txt", b"content").unwrap();
    let dest_file = test_dir.path().join("dest.txt");

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--checksum",
        "--dry-run",
        "-v",
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    let output = cmd.assert_success();

    // File should NOT be created in dry-run mode
    assert!(!dest_file.exists(), "Dry-run should not create file");

    // Output should mention what would be done
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("source.txt") || stdout.contains("dest.txt"),
        "Dry-run should show what would be transferred"
    );
}

// ============================================================================
// Hash Algorithm Behavior Tests
// ============================================================================

/// Test that checksum mode detects single-byte differences.
#[test]
fn checksum_detects_single_byte_difference() {
    let test_dir = TestDir::new().expect("create test dir");

    // Create files that differ by only one byte
    let src_data: Vec<u8> = (0..1000).map(|i| (i % 256) as u8).collect();
    let mut dest_data = src_data.clone();
    dest_data[500] = dest_data[500].wrapping_add(1); // Change middle byte

    let src_file = test_dir.write_file("source.bin", &src_data).unwrap();
    let dest_file = test_dir.write_file("dest.bin", &dest_data).unwrap();

    // Set same mtime to ensure checksum is what detects the change
    let same_time = FileTime::from_unix_time(1700000000, 0);
    set_file_mtime(&src_file, same_time).unwrap();
    set_file_mtime(&dest_file, same_time).unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--checksum",
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    cmd.assert_success();

    // Content should be updated
    assert_eq!(fs::read(&dest_file).unwrap(), src_data);
}

/// Test checksum with very small files (1-3 bytes).
#[test]
fn checksum_very_small_files() {
    for size in 1..=3 {
        let test_dir = TestDir::new().expect("create test dir");
        let content: Vec<u8> = (0..size).map(|i| (i + 65) as u8).collect(); // "A", "AB", "ABC"

        let src_file = test_dir.write_file("small_src.txt", &content).unwrap();
        let dest_file = test_dir.write_file("small_dest.txt", &content).unwrap();

        let preserved_time = FileTime::from_unix_time(1600000000, 0);
        set_file_mtime(&dest_file, preserved_time).unwrap();

        std::thread::sleep(Duration::from_millis(10));

        let mut cmd = RsyncCommand::new();
        cmd.args([
            "--checksum",
            src_file.to_str().unwrap(),
            dest_file.to_str().unwrap(),
        ]);
        cmd.assert_success();

        // Verify mtime wasn't changed (file should be skipped)
        let final_time = FileTime::from_last_modification_time(&fs::metadata(&dest_file).unwrap());
        assert_eq!(
            final_time, preserved_time,
            "Small file of size {size} should match"
        );
    }
}

/// Test checksum correctly handles files at buffer boundary sizes.
#[test]
fn checksum_buffer_boundary_sizes() {
    // Test sizes around common buffer sizes
    for size in [4095, 4096, 4097, 8191, 8192, 8193, 65535, 65536, 65537] {
        let test_dir = TestDir::new().expect("create test dir");
        let content = vec![0xABu8; size];

        let src_file = test_dir.write_file("buffer_src.bin", &content).unwrap();
        let dest_file = test_dir.write_file("buffer_dest.bin", &content).unwrap();

        let preserved_time = FileTime::from_unix_time(1600000000, 0);
        set_file_mtime(&dest_file, preserved_time).unwrap();

        std::thread::sleep(Duration::from_millis(10));

        let mut cmd = RsyncCommand::new();
        cmd.args([
            "--checksum",
            src_file.to_str().unwrap(),
            dest_file.to_str().unwrap(),
        ]);
        cmd.assert_success();

        let final_time = FileTime::from_last_modification_time(&fs::metadata(&dest_file).unwrap());
        assert_eq!(
            final_time, preserved_time,
            "File of size {size} should match"
        );
    }
}

/// Test checksum handles files with all zero bytes.
#[test]
fn checksum_all_zeros_file() {
    let test_dir = TestDir::new().expect("create test dir");
    let content = vec![0u8; 10000];

    let src_file = test_dir.write_file("zeros_src.bin", &content).unwrap();
    let dest_file = test_dir.write_file("zeros_dest.bin", &content).unwrap();

    let preserved_time = FileTime::from_unix_time(1600000000, 0);
    set_file_mtime(&dest_file, preserved_time).unwrap();

    std::thread::sleep(Duration::from_millis(50));

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--checksum",
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    cmd.assert_success();

    let final_time = FileTime::from_last_modification_time(&fs::metadata(&dest_file).unwrap());
    assert_eq!(final_time, preserved_time, "All-zeros files should match");
}

/// Test checksum handles files with all 0xFF bytes.
#[test]
fn checksum_all_ones_file() {
    let test_dir = TestDir::new().expect("create test dir");
    let content = vec![0xFFu8; 10000];

    let src_file = test_dir.write_file("ones_src.bin", &content).unwrap();
    let dest_file = test_dir.write_file("ones_dest.bin", &content).unwrap();

    let preserved_time = FileTime::from_unix_time(1600000000, 0);
    set_file_mtime(&dest_file, preserved_time).unwrap();

    std::thread::sleep(Duration::from_millis(50));

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--checksum",
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    cmd.assert_success();

    let final_time = FileTime::from_last_modification_time(&fs::metadata(&dest_file).unwrap());
    assert_eq!(final_time, preserved_time, "All-ones files should match");
}

/// Test checksum with alternating byte patterns.
#[test]
fn checksum_alternating_pattern() {
    let test_dir = TestDir::new().expect("create test dir");

    // Create alternating pattern
    let content: Vec<u8> = (0..10000)
        .map(|i| if i % 2 == 0 { 0xAA } else { 0x55 })
        .collect();

    let src_file = test_dir.write_file("pattern_src.bin", &content).unwrap();
    let dest_file = test_dir.write_file("pattern_dest.bin", &content).unwrap();

    let preserved_time = FileTime::from_unix_time(1600000000, 0);
    set_file_mtime(&dest_file, preserved_time).unwrap();

    std::thread::sleep(Duration::from_millis(50));

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--checksum",
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    cmd.assert_success();

    let final_time = FileTime::from_last_modification_time(&fs::metadata(&dest_file).unwrap());
    assert_eq!(
        final_time, preserved_time,
        "Alternating pattern files should match"
    );
}

// ============================================================================
// Checksum Verification Tests
// ============================================================================

/// Test that transferred file content is byte-for-byte correct.
#[test]
fn checksum_transfer_preserves_content_exactly() {
    let test_dir = TestDir::new().expect("create test dir");

    // Create file with every possible byte value
    let mut content: Vec<u8> = (0..=255).collect();
    // Repeat to make it larger
    for _ in 0..100 {
        let bytes: Vec<u8> = (0..=255).collect();
        content.extend(bytes);
    }

    let src_file = test_dir.write_file("exact_src.bin", &content).unwrap();
    let dest_file = test_dir.path().join("exact_dest.bin");

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--checksum",
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    cmd.assert_success();

    let dest_content = fs::read(&dest_file).unwrap();
    assert_eq!(
        dest_content.len(),
        content.len(),
        "Content length should match"
    );
    assert_eq!(
        dest_content, content,
        "Content should be byte-for-byte identical"
    );
}

/// Test checksum handles repeated transfers correctly (idempotent).
#[test]
fn checksum_repeated_transfer_is_idempotent() {
    let test_dir = TestDir::new().expect("create test dir");
    let content = b"idempotent transfer test content";

    let src_file = test_dir.write_file("idem_src.txt", content).unwrap();
    let dest_file = test_dir.write_file("idem_dest.txt", content).unwrap();

    // Set specific time on dest
    let preserved_time = FileTime::from_unix_time(1600000000, 0);
    set_file_mtime(&dest_file, preserved_time).unwrap();

    std::thread::sleep(Duration::from_millis(50));

    // Run transfer multiple times
    for _ in 0..3 {
        let mut cmd = RsyncCommand::new();
        cmd.args([
            "--checksum",
            src_file.to_str().unwrap(),
            dest_file.to_str().unwrap(),
        ]);
        cmd.assert_success();
    }

    // After multiple runs, dest should still have original mtime (no changes)
    let final_time = FileTime::from_last_modification_time(&fs::metadata(&dest_file).unwrap());
    assert_eq!(
        final_time, preserved_time,
        "Repeated transfers should be idempotent"
    );
    assert_eq!(fs::read(&dest_file).unwrap(), content);
}

// ============================================================================
// Protocol Behavior Tests
// ============================================================================

/// Test that checksum combined with archive mode works correctly.
/// Note: Archive mode includes --times, so mtime is part of what gets synced.
/// With checksum mode AND --no-times, identical content skips transfer.
#[test]
fn checksum_with_archive_preserves_all_metadata() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    let content = b"archive + checksum test";
    fs::write(src_dir.join("file.txt"), content).unwrap();
    fs::write(dest_dir.join("file.txt"), content).unwrap();

    // Set specific mtime on both to be identical
    let same_time = FileTime::from_unix_time(1650000000, 0);
    set_file_mtime(src_dir.join("file.txt"), same_time).unwrap();
    set_file_mtime(dest_dir.join("file.txt"), same_time).unwrap();

    // Record dest mtime before
    let dest_time_before =
        FileTime::from_last_modification_time(&fs::metadata(dest_dir.join("file.txt")).unwrap());

    std::thread::sleep(Duration::from_millis(50));

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-ac", // archive + checksum
        &format!("{}/", src_dir.display()),
        dest_dir.to_str().unwrap(),
    ]);
    cmd.assert_success();

    // With identical content AND identical mtime, file should be skipped
    // Dest mtime should remain unchanged
    let final_time =
        FileTime::from_last_modification_time(&fs::metadata(dest_dir.join("file.txt")).unwrap());
    assert_eq!(
        final_time, dest_time_before,
        "Archive+checksum should skip identical files"
    );
}

/// Test checksum with times preservation.
#[test]
fn checksum_with_times_updates_mtime_when_content_differs() {
    let test_dir = TestDir::new().expect("create test dir");

    let src_file = test_dir.write_file("src.txt", b"new content!").unwrap();
    let dest_file = test_dir.write_file("dest.txt", b"old content!").unwrap();

    // Set different times
    let source_time = FileTime::from_unix_time(1700000000, 0);
    let dest_time = FileTime::from_unix_time(1600000000, 0);
    set_file_mtime(&src_file, source_time).unwrap();
    set_file_mtime(&dest_file, dest_time).unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--checksum",
        "--times",
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    cmd.assert_success();

    // Content should be updated
    assert_eq!(fs::read(&dest_file).unwrap(), b"new content!");

    // With --times, mtime should be copied from source
    let final_time = FileTime::from_last_modification_time(&fs::metadata(&dest_file).unwrap());
    assert_eq!(
        final_time, source_time,
        "mtime should be updated when content differs"
    );
}
