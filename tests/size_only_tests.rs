//! Comprehensive tests for the --size-only comparison mode.
//!
//! The --size-only flag causes rsync to skip files where the source and
//! destination have the same size, regardless of modification time or
//! content differences. This is useful for fast synchronization when
//! content rarely changes or when timestamps are unreliable.
//!
//! Run these tests with: cargo test size_only

mod integration;

use filetime::{FileTime, set_file_times};
use integration::helpers::*;
use std::fs;

// ============================================================================
// Core Size-Only Behavior
// ============================================================================

/// Test that files with the same size but different content are NOT transferred.
/// This is the key behavior of --size-only: it only compares file sizes.
#[test]
fn size_only_same_size_different_content_no_transfer() {
    let test_dir = TestDir::new().expect("create test dir");

    // Create source and dest files with same size (5 bytes) but different content
    let src_file = test_dir.write_file("source.txt", b"AAAAA").unwrap();
    let dest_file = test_dir.write_file("dest.txt", b"BBBBB").unwrap();

    // Record destination state before rsync
    let dest_mtime_before = fs::metadata(&dest_file).unwrap().modified().unwrap();
    let dest_content_before = fs::read(&dest_file).unwrap();

    // Brief sleep to ensure any modification would be detectable
    std::thread::sleep(std::time::Duration::from_millis(50));

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--size-only",
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    cmd.assert_success();

    // Verify destination was NOT modified
    let dest_mtime_after = fs::metadata(&dest_file).unwrap().modified().unwrap();
    let dest_content_after = fs::read(&dest_file).unwrap();

    assert_eq!(
        dest_mtime_before, dest_mtime_after,
        "File mtime should be unchanged when sizes match"
    );
    assert_eq!(
        dest_content_before, dest_content_after,
        "File content should be unchanged when sizes match"
    );
    assert_eq!(
        dest_content_after, b"BBBBB",
        "Destination should retain original content"
    );
}

/// Test that files with different sizes ARE transferred.
/// When sizes differ, --size-only should still trigger a transfer.
#[test]
fn size_only_different_size_transfers() {
    let test_dir = TestDir::new().expect("create test dir");

    // Create files with different sizes
    let src_file = test_dir
        .write_file("source.txt", b"longer source content")
        .unwrap();
    let dest_file = test_dir.write_file("dest.txt", b"short").unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--size-only",
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    cmd.assert_success();

    // Verify destination was updated to source content
    let dest_content = fs::read(&dest_file).unwrap();
    assert_eq!(
        dest_content, b"longer source content",
        "File should be transferred when sizes differ"
    );
}

/// Test that files with the same size AND same content are NOT transferred.
/// This confirms --size-only skips identical files (no unnecessary I/O).
#[test]
fn size_only_same_size_same_content_no_transfer() {
    let test_dir = TestDir::new().expect("create test dir");

    // Create identical files
    let content = b"identical content here";
    let src_file = test_dir.write_file("source.txt", content).unwrap();
    let dest_file = test_dir.write_file("dest.txt", content).unwrap();

    // Record destination state before rsync
    let dest_mtime_before = fs::metadata(&dest_file).unwrap().modified().unwrap();

    std::thread::sleep(std::time::Duration::from_millis(50));

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--size-only",
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    cmd.assert_success();

    // Verify destination was NOT modified
    let dest_mtime_after = fs::metadata(&dest_file).unwrap().modified().unwrap();
    assert_eq!(
        dest_mtime_before, dest_mtime_after,
        "Identical files should not be modified"
    );
}

// ============================================================================
// Size-Only with Verbose Flag
// ============================================================================

/// Test --size-only with --verbose shows appropriate output.
/// When sizes match, no transfer message should appear for that file.
#[test]
fn size_only_verbose_no_transfer_for_same_size() {
    let test_dir = TestDir::new().expect("create test dir");

    // Same size, different content
    let src_file = test_dir.write_file("same_size.txt", b"12345").unwrap();
    let dest_file = test_dir.write_file("dest.txt", b"abcde").unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--size-only",
        "-v",
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    let output = cmd.assert_success();

    // File should not be transferred
    let dest_content = fs::read(&dest_file).unwrap();
    assert_eq!(
        dest_content, b"abcde",
        "File with same size should not be overwritten"
    );

    // Verbose output should indicate no bytes were transferred.
    // The file may still be listed (rsync lists checked files in verbose mode)
    // but "sent 0 bytes" confirms no actual transfer occurred.
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("sent 0 bytes") || stdout.contains("sent 0B"),
        "Should show 0 bytes sent when sizes match: {stdout}"
    );
}

/// Test --size-only with --verbose shows transfer for different sizes.
#[test]
fn size_only_verbose_transfer_for_different_size() {
    let test_dir = TestDir::new().expect("create test dir");

    let src_file = test_dir
        .write_file("different_size.txt", b"longer content here")
        .unwrap();
    let dest_file = test_dir.write_file("dest.txt", b"short").unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--size-only",
        "-v",
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    let output = cmd.assert_success();

    // File should be transferred
    let dest_content = fs::read(&dest_file).unwrap();
    assert_eq!(
        dest_content, b"longer content here",
        "File with different size should be transferred"
    );

    // Verbose output should indicate something was transferred
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("different_size") || stdout.contains("dest"),
        "Verbose output should show transfer activity"
    );
}

// ============================================================================
// Size-Only with Other Flags
// ============================================================================

/// Test --size-only with --dry-run shows what would happen without modifying.
#[test]
fn size_only_with_dry_run() {
    let test_dir = TestDir::new().expect("create test dir");

    // Different sizes - would normally transfer
    let src_file = test_dir
        .write_file("source.txt", b"source content")
        .unwrap();
    let dest_file = test_dir.write_file("dest.txt", b"dest").unwrap();

    let dest_content_before = fs::read(&dest_file).unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--size-only",
        "--dry-run",
        "-v",
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    cmd.assert_success();

    // Dry run should not modify the file
    let dest_content_after = fs::read(&dest_file).unwrap();
    assert_eq!(
        dest_content_before, dest_content_after,
        "Dry run should not modify files"
    );
}

/// Test --size-only with recursive directory sync.
#[test]
fn size_only_recursive_directory() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Create source files
    fs::write(src_dir.join("same_size.txt"), b"AAAAA").unwrap();
    fs::write(src_dir.join("different_size.txt"), b"longer content").unwrap();
    fs::write(src_dir.join("new_file.txt"), b"new content").unwrap();

    // Create destination files with matching/different sizes
    fs::write(dest_dir.join("same_size.txt"), b"BBBBB").unwrap(); // Same size, different content
    fs::write(dest_dir.join("different_size.txt"), b"short").unwrap(); // Different size

    let dest_same_mtime = fs::metadata(dest_dir.join("same_size.txt"))
        .unwrap()
        .modified()
        .unwrap();

    std::thread::sleep(std::time::Duration::from_millis(50));

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--size-only",
        "-r",
        &format!("{}/", src_dir.display()),
        dest_dir.to_str().unwrap(),
    ]);
    cmd.assert_success();

    // same_size.txt should NOT be transferred (same size)
    let same_size_content = fs::read(dest_dir.join("same_size.txt")).unwrap();
    assert_eq!(
        same_size_content, b"BBBBB",
        "Same-size file should retain original content"
    );
    let dest_same_mtime_after = fs::metadata(dest_dir.join("same_size.txt"))
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(
        dest_same_mtime, dest_same_mtime_after,
        "Same-size file should not be modified"
    );

    // different_size.txt SHOULD be transferred (different size)
    let different_size_content = fs::read(dest_dir.join("different_size.txt")).unwrap();
    assert_eq!(
        different_size_content, b"longer content",
        "Different-size file should be updated"
    );

    // new_file.txt SHOULD be created (doesn't exist in dest)
    let new_file_content = fs::read(dest_dir.join("new_file.txt")).unwrap();
    assert_eq!(
        new_file_content, b"new content",
        "New file should be created"
    );
}

/// Test --size-only with --archive mode.
#[test]
#[cfg(unix)]
fn size_only_with_archive_mode() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Create source file
    fs::write(src_dir.join("file.txt"), b"source content").unwrap();
    // Create dest file with different content but SAME size would transfer normally
    // but with --size-only it should be skipped if sizes match
    fs::write(dest_dir.join("file.txt"), b"dest contents!").unwrap(); // 14 bytes each

    let dest_mtime_before = fs::metadata(dest_dir.join("file.txt"))
        .unwrap()
        .modified()
        .unwrap();

    std::thread::sleep(std::time::Duration::from_millis(50));

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--size-only",
        "-a", // archive mode
        &format!("{}/", src_dir.display()),
        dest_dir.to_str().unwrap(),
    ]);
    cmd.assert_success();

    // File should NOT be transferred (same size)
    let dest_content = fs::read(dest_dir.join("file.txt")).unwrap();
    assert_eq!(
        dest_content, b"dest contents!",
        "Archive mode with --size-only should skip same-size files"
    );

    let dest_mtime_after = fs::metadata(dest_dir.join("file.txt"))
        .unwrap()
        .modified()
        .unwrap();
    // Compare at second granularity since filesystem timestamp precision varies
    let before_secs = dest_mtime_before
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let after_secs = dest_mtime_after
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert_eq!(
        before_secs, after_secs,
        "File mtime should be unchanged (at second granularity)"
    );
}

/// Test --size-only with --times flag.
/// With --size-only, data transfer is skipped when sizes match, but --times
/// still causes metadata (timestamps) to be updated.
#[test]
fn size_only_with_times_flag() {
    let test_dir = TestDir::new().expect("create test dir");

    let src_file = test_dir.write_file("source.txt", b"12345").unwrap();
    let dest_file = test_dir.write_file("dest.txt", b"abcde").unwrap();

    // Make source much newer than dest
    let newer_time = FileTime::from_unix_time(2000000000, 0);
    let older_time = FileTime::from_unix_time(1000000000, 0);
    set_file_times(&src_file, newer_time, newer_time).unwrap();
    set_file_times(&dest_file, older_time, older_time).unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--size-only",
        "--times",
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    cmd.assert_success();

    // Despite different timestamps, same size means no data transfer
    let dest_content = fs::read(&dest_file).unwrap();
    assert_eq!(
        dest_content, b"abcde",
        "--size-only should skip data transfer when sizes match"
    );

    // However, --times still causes the timestamp to be updated
    let dest_mtime_after = fs::metadata(&dest_file).unwrap().modified().unwrap();
    let src_mtime = fs::metadata(&src_file).unwrap().modified().unwrap();
    assert_eq!(
        dest_mtime_after, src_mtime,
        "--times should update timestamp even when --size-only skips data transfer"
    );
}

// ============================================================================
// Edge Cases
// ============================================================================

/// Test --size-only with empty files (0 bytes).
/// Two empty files should be considered the same (size = 0).
#[test]
fn size_only_empty_files_no_transfer() {
    let test_dir = TestDir::new().expect("create test dir");

    let src_file = test_dir.write_file("source.txt", b"").unwrap();
    let dest_file = test_dir.write_file("dest.txt", b"").unwrap();

    let dest_mtime_before = fs::metadata(&dest_file).unwrap().modified().unwrap();

    std::thread::sleep(std::time::Duration::from_millis(50));

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--size-only",
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    cmd.assert_success();

    let dest_mtime_after = fs::metadata(&dest_file).unwrap().modified().unwrap();
    assert_eq!(
        dest_mtime_before, dest_mtime_after,
        "Empty files should not be modified"
    );
}

/// Test --size-only when destination is empty but source is not.
#[test]
fn size_only_empty_dest_nonempty_source_transfers() {
    let test_dir = TestDir::new().expect("create test dir");

    let src_file = test_dir.write_file("source.txt", b"content").unwrap();
    let dest_file = test_dir.write_file("dest.txt", b"").unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--size-only",
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    cmd.assert_success();

    let dest_content = fs::read(&dest_file).unwrap();
    assert_eq!(
        dest_content, b"content",
        "Should transfer when sizes differ (empty vs non-empty)"
    );
}

/// Test --size-only when source is empty but destination is not.
#[test]
fn size_only_nonempty_dest_empty_source_transfers() {
    let test_dir = TestDir::new().expect("create test dir");

    let src_file = test_dir.write_file("source.txt", b"").unwrap();
    let dest_file = test_dir
        .write_file("dest.txt", b"existing content")
        .unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--size-only",
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    cmd.assert_success();

    let dest_content = fs::read(&dest_file).unwrap();
    assert_eq!(
        dest_content, b"",
        "Should transfer when sizes differ (non-empty becomes empty)"
    );
}

/// Test --size-only when destination file doesn't exist.
/// New files should always be created regardless of --size-only.
#[test]
fn size_only_new_file_created() {
    let test_dir = TestDir::new().expect("create test dir");

    let src_file = test_dir.write_file("source.txt", b"new content").unwrap();
    let dest_file = test_dir.path().join("dest.txt");

    assert!(!dest_file.exists(), "Dest should not exist initially");

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--size-only",
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    cmd.assert_success();

    assert!(dest_file.exists(), "New file should be created");
    let dest_content = fs::read(&dest_file).unwrap();
    assert_eq!(dest_content, b"new content", "New file should have content");
}

/// Test --size-only with large files that have the same size.
#[test]
fn size_only_large_files_same_size() {
    let test_dir = TestDir::new().expect("create test dir");

    // Create 1MB files with same size but different content
    let size = 1024 * 1024;
    let src_content: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
    let dest_content: Vec<u8> = (0..size).map(|i| ((i + 1) % 256) as u8).collect();

    let src_file = test_dir.write_file("source.bin", &src_content).unwrap();
    let dest_file = test_dir.write_file("dest.bin", &dest_content).unwrap();

    let dest_mtime_before = fs::metadata(&dest_file).unwrap().modified().unwrap();

    std::thread::sleep(std::time::Duration::from_millis(50));

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--size-only",
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    cmd.assert_success();

    // Large file with same size should not be transferred
    let dest_mtime_after = fs::metadata(&dest_file).unwrap().modified().unwrap();
    assert_eq!(
        dest_mtime_before, dest_mtime_after,
        "Large same-size file should not be modified"
    );

    // Verify content unchanged by checking first and last bytes
    let final_content = fs::read(&dest_file).unwrap();
    assert_eq!(final_content.len(), size, "File size should be unchanged");
    assert_eq!(
        final_content[0], dest_content[0],
        "First byte should be unchanged"
    );
    assert_eq!(
        final_content[size - 1],
        dest_content[size - 1],
        "Last byte should be unchanged"
    );
}

// ============================================================================
// Size-Only vs Normal Comparison Behavior Contrast
// ============================================================================

/// Contrast test: Without --size-only, same size but older dest gets updated.
/// This shows the difference --size-only makes.
#[test]
fn without_size_only_older_dest_updated() {
    let test_dir = TestDir::new().expect("create test dir");

    let src_file = test_dir.write_file("source.txt", b"AAAAA").unwrap();
    let dest_file = test_dir.write_file("dest.txt", b"BBBBB").unwrap();

    // Make dest older than source
    let older_time = FileTime::from_unix_time(1000000000, 0);
    set_file_times(&dest_file, older_time, older_time).unwrap();

    // Without --size-only, normal rsync should transfer based on mtime
    let mut cmd = RsyncCommand::new();
    cmd.args([src_file.to_str().unwrap(), dest_file.to_str().unwrap()]);
    cmd.assert_success();

    // File should be updated (older dest, newer source)
    let dest_content = fs::read(&dest_file).unwrap();
    assert_eq!(
        dest_content, b"AAAAA",
        "Without --size-only, older dest should be updated"
    );
}

/// Contrast test: With --size-only, same size older dest is NOT updated.
#[test]
fn with_size_only_older_dest_not_updated() {
    let test_dir = TestDir::new().expect("create test dir");

    let src_file = test_dir.write_file("source.txt", b"AAAAA").unwrap();
    let dest_file = test_dir.write_file("dest.txt", b"BBBBB").unwrap();

    // Make dest older than source
    let older_time = FileTime::from_unix_time(1000000000, 0);
    set_file_times(&dest_file, older_time, older_time).unwrap();

    // With --size-only, same size should prevent transfer
    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--size-only",
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    cmd.assert_success();

    // File should NOT be updated (same size overrides mtime check)
    let dest_content = fs::read(&dest_file).unwrap();
    assert_eq!(
        dest_content, b"BBBBB",
        "With --size-only, same size should prevent transfer"
    );
}
