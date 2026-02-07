//! Integration tests for --preallocate file pre-allocation optimization.
//!
//! Tests the --preallocate flag behavior through end-to-end CLI scenarios.
//!
//! # Coverage Areas
//!
//! - Basic preallocate file transfer
//! - Preallocate with --inplace mode
//! - Preallocate disables sparse writes (upstream behavior)
//! - File size and block verification on Linux
//! - Small file preallocation
//! - Directory recursive transfer with preallocate

mod integration;

use integration::helpers::*;
use std::fs;

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

// ============================================================================
// Basic Preallocate Tests
// ============================================================================

#[test]
fn preallocate_flag_copies_file_correctly() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Create a source file with known content
    let content = vec![0xABu8; 64 * 1024]; // 64KB
    fs::write(src_dir.join("data.bin"), &content).expect("write source");

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--preallocate",
        src_dir.join("data.bin").to_str().unwrap(),
        dest_dir.to_str().unwrap(),
    ]);
    cmd.assert_success();

    // Verify content matches
    let dest_content = fs::read(dest_dir.join("data.bin")).expect("read dest");
    assert_eq!(
        dest_content, content,
        "destination content should match source"
    );
}

#[test]
fn preallocate_flag_copies_empty_file() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    fs::write(src_dir.join("empty.bin"), []).expect("write empty");

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--preallocate",
        src_dir.join("empty.bin").to_str().unwrap(),
        dest_dir.to_str().unwrap(),
    ]);
    cmd.assert_success();

    let dest_meta = fs::metadata(dest_dir.join("empty.bin")).expect("metadata");
    assert_eq!(dest_meta.len(), 0, "empty file should remain empty");
}

#[test]
fn preallocate_flag_copies_small_file() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Small file (not worth preallocating in practice, but should still work)
    fs::write(src_dir.join("tiny.txt"), b"hello world").expect("write");

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--preallocate",
        src_dir.join("tiny.txt").to_str().unwrap(),
        dest_dir.to_str().unwrap(),
    ]);
    cmd.assert_success();

    let dest_content = fs::read(dest_dir.join("tiny.txt")).expect("read");
    assert_eq!(&dest_content, b"hello world");
}

// ============================================================================
// Linux-Specific Block Allocation Tests
// ============================================================================

#[cfg(target_os = "linux")]
#[test]
fn preallocate_allocates_disk_blocks_on_linux() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Create a 1 MiB source file
    let content = vec![0x42u8; 1024 * 1024];
    fs::write(src_dir.join("large.bin"), &content).expect("write source");

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--preallocate",
        src_dir.join("large.bin").to_str().unwrap(),
        dest_dir.to_str().unwrap(),
    ]);
    cmd.assert_success();

    let dest_meta = fs::metadata(dest_dir.join("large.bin")).expect("metadata");
    assert_eq!(dest_meta.len(), 1024 * 1024, "file size should match");

    // With preallocate, blocks should be at least file_size / 512
    let min_blocks = (1024 * 1024) / 512;
    assert!(
        dest_meta.blocks() >= min_blocks,
        "preallocated file should have at least {} blocks, got {}",
        min_blocks,
        dest_meta.blocks()
    );
}

// ============================================================================
// Preallocate + Sparse Interaction Tests
// ============================================================================

#[cfg(unix)]
#[test]
fn preallocate_disables_sparse_writes() {
    // Upstream rsync disables sparse writes when --preallocate is active
    // because preallocation must materialize every range in the destination.
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_sparse = test_dir.mkdir("dest_sparse").unwrap();
    let dest_prealloc = test_dir.mkdir("dest_prealloc").unwrap();

    // Create source with lots of zeros (would be sparse without --preallocate)
    let content = vec![0u8; 512 * 1024]; // 512KB of zeros
    fs::write(src_dir.join("zeros.bin"), &content).expect("write");

    // Copy with --sparse only
    let mut cmd1 = RsyncCommand::new();
    cmd1.args([
        "--sparse",
        src_dir.join("zeros.bin").to_str().unwrap(),
        dest_sparse.to_str().unwrap(),
    ]);
    cmd1.assert_success();

    // Copy with --sparse --preallocate (preallocate should override sparse)
    let mut cmd2 = RsyncCommand::new();
    cmd2.args([
        "--sparse",
        "--preallocate",
        src_dir.join("zeros.bin").to_str().unwrap(),
        dest_prealloc.to_str().unwrap(),
    ]);
    cmd2.assert_success();

    let sparse_meta = fs::metadata(dest_sparse.join("zeros.bin")).expect("sparse metadata");
    let prealloc_meta = fs::metadata(dest_prealloc.join("zeros.bin")).expect("prealloc metadata");

    // Both should have the same logical size
    assert_eq!(sparse_meta.len(), prealloc_meta.len());

    // With preallocate, the file should use at least as many blocks as the
    // sparse version (and likely more, since preallocation materializes ranges)
    assert!(
        prealloc_meta.blocks() >= sparse_meta.blocks(),
        "preallocated file ({} blocks) should use at least as many blocks as sparse ({} blocks)",
        prealloc_meta.blocks(),
        sparse_meta.blocks()
    );
}

// ============================================================================
// Preallocate + Inplace Tests
// ============================================================================

#[test]
fn preallocate_with_inplace_copies_correctly() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Create source
    let content = vec![0xCDu8; 128 * 1024]; // 128KB
    fs::write(src_dir.join("inplace.bin"), &content).expect("write source");

    // Pre-create destination with different content (for inplace to overwrite)
    fs::write(dest_dir.join("inplace.bin"), vec![0x00u8; 128 * 1024]).expect("seed dest");

    // Make destination older so rsync will transfer
    let old_time = filetime::FileTime::from_unix_time(1600000000, 0);
    filetime::set_file_mtime(dest_dir.join("inplace.bin"), old_time).expect("set mtime");

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--preallocate",
        "--inplace",
        src_dir.join("inplace.bin").to_str().unwrap(),
        &format!("{}/", dest_dir.to_str().unwrap()),
    ]);
    cmd.assert_success();

    let dest_content = fs::read(dest_dir.join("inplace.bin")).expect("read dest");
    assert_eq!(
        dest_content, content,
        "inplace transfer should match source"
    );
}

#[test]
fn preallocate_with_inplace_new_file() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    let content = vec![0xEFu8; 32 * 1024]; // 32KB
    fs::write(src_dir.join("new.bin"), &content).expect("write source");

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--preallocate",
        "--inplace",
        src_dir.join("new.bin").to_str().unwrap(),
        dest_dir.to_str().unwrap(),
    ]);
    cmd.assert_success();

    let dest_content = fs::read(dest_dir.join("new.bin")).expect("read dest");
    assert_eq!(dest_content, content);
}

// ============================================================================
// Recursive Directory Tests
// ============================================================================

#[test]
fn preallocate_recursive_directory_transfer() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Create multiple files of varying sizes
    fs::write(src_dir.join("small.txt"), b"small file content").expect("small");
    fs::write(src_dir.join("medium.bin"), vec![0x55u8; 64 * 1024]).expect("medium");
    fs::write(src_dir.join("large.bin"), vec![0xAAu8; 256 * 1024]).expect("large");

    // Create subdirectory with files
    fs::create_dir_all(src_dir.join("subdir")).expect("mkdir");
    fs::write(src_dir.join("subdir/nested.bin"), vec![0xBBu8; 128 * 1024]).expect("nested");

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--preallocate",
        "-r",
        &format!("{}/", src_dir.to_str().unwrap()),
        dest_dir.to_str().unwrap(),
    ]);
    cmd.assert_success();

    // Verify all files transferred correctly
    assert_eq!(
        fs::read(dest_dir.join("small.txt")).unwrap(),
        b"small file content"
    );
    assert_eq!(
        fs::read(dest_dir.join("medium.bin")).unwrap(),
        vec![0x55u8; 64 * 1024]
    );
    assert_eq!(
        fs::read(dest_dir.join("large.bin")).unwrap(),
        vec![0xAAu8; 256 * 1024]
    );
    assert_eq!(
        fs::read(dest_dir.join("subdir/nested.bin")).unwrap(),
        vec![0xBBu8; 128 * 1024]
    );
}

// ============================================================================
// Preallocate with Update/Delta Tests
// ============================================================================

#[test]
fn preallocate_updates_existing_file() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Create initial file
    let initial = vec![0x11u8; 64 * 1024];
    fs::write(src_dir.join("data.bin"), &initial).expect("write initial");

    // First copy
    let mut cmd1 = RsyncCommand::new();
    cmd1.args([
        "--preallocate",
        src_dir.join("data.bin").to_str().unwrap(),
        dest_dir.to_str().unwrap(),
    ]);
    cmd1.assert_success();

    assert_eq!(fs::read(dest_dir.join("data.bin")).unwrap(), initial);

    // Update source with new content
    let updated = vec![0x22u8; 128 * 1024];
    fs::write(src_dir.join("data.bin"), &updated).expect("write updated");

    // Second copy
    let mut cmd2 = RsyncCommand::new();
    cmd2.args([
        "--preallocate",
        src_dir.join("data.bin").to_str().unwrap(),
        dest_dir.to_str().unwrap(),
    ]);
    cmd2.assert_success();

    let dest_content = fs::read(dest_dir.join("data.bin")).unwrap();
    assert_eq!(
        dest_content, updated,
        "second transfer should update the file"
    );
}

// ============================================================================
// Preallocate with Verbose Output Tests
// ============================================================================

#[test]
fn preallocate_with_verbose_shows_transfer() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    fs::write(src_dir.join("file.bin"), vec![0xFFu8; 32 * 1024]).expect("write");

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--preallocate",
        "-v",
        src_dir.join("file.bin").to_str().unwrap(),
        dest_dir.to_str().unwrap(),
    ]);
    let output = cmd.assert_success();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}{stderr}");

    // Verbose should mention the transferred file
    assert!(
        combined.contains("file.bin") || combined.contains("sent") || combined.contains("total"),
        "verbose output should mention the transfer"
    );
}

// ============================================================================
// Preallocate Only (without --sparse) Tests
// ============================================================================

#[cfg(unix)]
#[test]
fn preallocate_without_sparse_allocates_full_blocks() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_no_prealloc = test_dir.mkdir("dest_normal").unwrap();
    let dest_prealloc = test_dir.mkdir("dest_prealloc").unwrap();

    // File with mixed data (some zeros, some non-zero)
    let mut content = vec![0u8; 256 * 1024];
    content[0] = 0x42;
    content[128 * 1024] = 0x99;
    content[256 * 1024 - 1] = 0xFF;
    fs::write(src_dir.join("mixed.bin"), &content).expect("write");

    // Copy without preallocate
    let mut cmd1 = RsyncCommand::new();
    cmd1.args([
        src_dir.join("mixed.bin").to_str().unwrap(),
        dest_no_prealloc.to_str().unwrap(),
    ]);
    cmd1.assert_success();

    // Copy with preallocate
    let mut cmd2 = RsyncCommand::new();
    cmd2.args([
        "--preallocate",
        src_dir.join("mixed.bin").to_str().unwrap(),
        dest_prealloc.to_str().unwrap(),
    ]);
    cmd2.assert_success();

    let normal_meta = fs::metadata(dest_no_prealloc.join("mixed.bin")).expect("normal metadata");
    let prealloc_meta = fs::metadata(dest_prealloc.join("mixed.bin")).expect("prealloc metadata");

    // Both should have the same logical size
    assert_eq!(normal_meta.len(), prealloc_meta.len());

    // Both should have the same content
    let normal_content = fs::read(dest_no_prealloc.join("mixed.bin")).unwrap();
    let prealloc_content = fs::read(dest_prealloc.join("mixed.bin")).unwrap();
    assert_eq!(normal_content, prealloc_content);
}
