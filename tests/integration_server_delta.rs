//! End-to-end integration tests for server delta transfer.
//!
//! These tests validate the complete delta transfer pipeline using
//! CLI-level execution, verifying content integrity and metadata preservation.

mod integration;

use filetime::{FileTime, set_file_times};
use integration::helpers::*;
use std::fs;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

// ============ Phase 1: Basic Delta Transfer Tests ============

#[test]
fn delta_transfer_whole_file_no_basis() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Create source file with recognizable content
    let src_file = src_dir.join("test.txt");
    let content = b"Hello, world! This is test content for delta transfer.";
    fs::write(&src_file, content).unwrap();

    // No basis file exists in dest - should transfer entire file
    let mut cmd = RsyncCommand::new();
    cmd.args([src_file.to_str().unwrap(), dest_dir.to_str().unwrap()]);
    cmd.assert_success();

    // Verify destination file matches source exactly
    let dest_file = dest_dir.join("test.txt");
    assert!(dest_file.exists(), "Destination file should exist");
    assert_eq!(
        fs::read(&dest_file).unwrap(),
        content,
        "File content should match source"
    );
}

#[test]
fn delta_transfer_with_identical_basis() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Create source file
    let src_file = src_dir.join("data.bin");
    let content: Vec<u8> = (0..8192).map(|i| (i % 256) as u8).collect();
    fs::write(&src_file, &content).unwrap();

    // Create identical basis file in destination
    let dest_file = dest_dir.join("data.bin");
    fs::write(&dest_file, &content).unwrap();

    // Modify dest timestamp to be older so rsync considers update
    let old_time = FileTime::from_unix_time(1600000000, 0);
    set_file_times(&dest_file, old_time, old_time).unwrap();

    // Run delta transfer
    let mut cmd = RsyncCommand::new();
    cmd.args([src_file.to_str().unwrap(), dest_file.to_str().unwrap()]);
    cmd.assert_success();

    // Verify content still matches (delta should use mostly copy operations)
    assert_eq!(
        fs::read(&dest_file).unwrap(),
        content,
        "File content should remain correct after delta"
    );
}

#[test]
fn delta_transfer_with_modified_middle() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Create source: [AAAA (4KB)] [BBBB (4KB)] [CCCC (4KB)]
    let mut src_content = Vec::new();
    src_content.extend(vec![b'A'; 4096]);
    src_content.extend(vec![b'B'; 4096]);
    src_content.extend(vec![b'C'; 4096]);

    let src_file = src_dir.join("data.bin");
    fs::write(&src_file, &src_content).unwrap();

    // Create basis: [AAAA (4KB)] [XXXX (4KB)] [CCCC (4KB)]
    let mut basis_content = Vec::new();
    basis_content.extend(vec![b'A'; 4096]);
    basis_content.extend(vec![b'X'; 4096]); // Different middle section
    basis_content.extend(vec![b'C'; 4096]);

    let dest_file = dest_dir.join("data.bin");
    fs::write(&dest_file, &basis_content).unwrap();

    // Make basis older
    let old_time = FileTime::from_unix_time(1600000000, 0);
    set_file_times(&dest_file, old_time, old_time).unwrap();

    // Run delta transfer
    let mut cmd = RsyncCommand::new();
    cmd.args([src_file.to_str().unwrap(), dest_file.to_str().unwrap()]);
    cmd.assert_success();

    // Verify reconstructed file matches source exactly
    assert_eq!(
        fs::read(&dest_file).unwrap(),
        src_content,
        "Reconstructed file should match source after delta"
    );
}

#[test]
fn delta_transfer_multiple_files() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Create 5 files with different content
    for i in 0..5 {
        let file = src_dir.join(format!("file{i}.txt"));
        fs::write(&file, format!("Content of file number {i}")).unwrap();
    }

    // Run recursive transfer
    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r", // Recursive
        &format!("{}/", src_dir.display()),
        &format!("{}/", dest_dir.display()),
    ]);
    cmd.assert_success();

    // Verify all files transferred correctly
    for i in 0..5 {
        let dest_file = dest_dir.join(format!("file{i}.txt"));
        assert!(dest_file.exists(), "file{i}.txt should exist");
        assert_eq!(
            fs::read_to_string(&dest_file).unwrap(),
            format!("Content of file number {i}"),
            "Content of file{i}.txt should match"
        );
    }
}

// ============ Phase 2: Metadata Preservation Tests ============

#[cfg(unix)]
#[test]
fn delta_transfer_preserves_permissions() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Create source file with specific permissions
    let src_file = src_dir.join("script.sh");
    fs::write(&src_file, b"#!/bin/bash\necho hello").unwrap();
    fs::set_permissions(&src_file, PermissionsExt::from_mode(0o755)).unwrap();

    // Create basis file with different permissions
    let dest_file = dest_dir.join("script.sh");
    fs::write(&dest_file, b"#!/bin/bash\necho old").unwrap();
    fs::set_permissions(&dest_file, PermissionsExt::from_mode(0o644)).unwrap();

    // Make basis older
    let old_time = FileTime::from_unix_time(1600000000, 0);
    set_file_times(&dest_file, old_time, old_time).unwrap();

    // Run transfer with -p flag
    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-p", // Preserve permissions
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    cmd.assert_success();

    // Verify permissions updated to match source
    let meta = fs::metadata(&dest_file).unwrap();
    assert_eq!(
        meta.permissions().mode() & 0o777,
        0o755,
        "Permissions should be updated to 0o755"
    );
}

#[test]
fn delta_transfer_preserves_timestamps_nanosecond() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Create source with specific timestamp (including nanoseconds)
    // Use a multiple of 100ns for cross-platform compatibility (Windows NTFS
    // has 100ns resolution, not full nanosecond resolution)
    let src_file = src_dir.join("timed.txt");
    fs::write(&src_file, b"timestamped content").unwrap();
    let mtime = FileTime::from_unix_time(1700000000, 123456700);
    set_file_times(&src_file, mtime, mtime).unwrap();

    // Create basis with different timestamp
    let dest_file = dest_dir.join("timed.txt");
    fs::write(&dest_file, b"old content").unwrap();
    let old_time = FileTime::from_unix_time(1600000000, 0);
    set_file_times(&dest_file, old_time, old_time).unwrap();

    // Run transfer with -t flag
    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-t", // Preserve times
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    cmd.assert_success();

    // Verify timestamp preserved with nanosecond precision
    let dest_meta = fs::metadata(&dest_file).unwrap();
    let dest_mtime = FileTime::from_last_modification_time(&dest_meta);
    assert_eq!(
        dest_mtime, mtime,
        "Timestamp should be preserved with nanosecond precision"
    );
}

#[cfg(unix)]
#[test]
fn delta_transfer_archive_mode() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Create source with full metadata
    let src_file = src_dir.join("archive.dat");
    fs::write(&src_file, b"archived data content").unwrap();
    fs::set_permissions(&src_file, PermissionsExt::from_mode(0o640)).unwrap();
    let mtime = FileTime::from_unix_time(1700000000, 999888777);
    set_file_times(&src_file, mtime, mtime).unwrap();

    // Create basis with different metadata
    let dest_file = dest_dir.join("archive.dat");
    fs::write(&dest_file, b"old data").unwrap();
    fs::set_permissions(&dest_file, PermissionsExt::from_mode(0o644)).unwrap();
    let old_time = FileTime::from_unix_time(1600000000, 0);
    set_file_times(&dest_file, old_time, old_time).unwrap();

    // Run transfer with -a flag (archive mode)
    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-a", // Archive mode (-rlptgoD)
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    cmd.assert_success();

    // Verify all metadata preserved
    let dest_meta = fs::metadata(&dest_file).unwrap();
    assert_eq!(
        dest_meta.permissions().mode() & 0o777,
        0o640,
        "Permissions should be preserved"
    );
    assert_eq!(
        FileTime::from_last_modification_time(&dest_meta),
        mtime,
        "Timestamp should be preserved with nanosecond precision"
    );
}

// ============ Phase 3: Edge Cases & Stress Tests ============

#[test]
fn delta_transfer_empty_file() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Create empty source file
    let src_file = src_dir.join("empty.txt");
    fs::write(&src_file, b"").unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([src_file.to_str().unwrap(), dest_dir.to_str().unwrap()]);
    cmd.assert_success();

    // Verify empty file created
    let dest_file = dest_dir.join("empty.txt");
    assert!(dest_file.exists(), "Empty file should exist");
    assert_eq!(
        fs::read(&dest_file).unwrap().len(),
        0,
        "File should be empty"
    );
}

#[test]
fn delta_transfer_large_file() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Create 10 MB file with pseudo-random pattern
    let src_file = src_dir.join("large.bin");
    let content: Vec<u8> = (0..10_000_000).map(|i| ((i * 7) % 256) as u8).collect();
    fs::write(&src_file, &content).unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([src_file.to_str().unwrap(), dest_dir.to_str().unwrap()]);
    cmd.assert_success();

    // Verify large file transferred correctly
    let dest_file = dest_dir.join("large.bin");
    assert_eq!(
        fs::read(&dest_file).unwrap(),
        content,
        "Large file content should match exactly"
    );
}

#[test]
fn delta_transfer_basis_smaller() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Source: 8KB
    let src_content = vec![b'A'; 8192];
    let src_file = src_dir.join("expand.bin");
    fs::write(&src_file, &src_content).unwrap();

    // Basis: 4KB
    let basis_content = vec![b'A'; 4096];
    let dest_file = dest_dir.join("expand.bin");
    fs::write(&dest_file, &basis_content).unwrap();
    let old_time = FileTime::from_unix_time(1600000000, 0);
    set_file_times(&dest_file, old_time, old_time).unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([src_file.to_str().unwrap(), dest_file.to_str().unwrap()]);
    cmd.assert_success();

    // Verify file expanded to 8KB
    let result = fs::read(&dest_file).unwrap();
    assert_eq!(result.len(), 8192, "File should be expanded to 8KB");
    assert_eq!(result, src_content, "Content should match source");
}

#[test]
fn delta_transfer_basis_larger() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Source: 4KB
    let src_content = vec![b'A'; 4096];
    let src_file = src_dir.join("shrink.bin");
    fs::write(&src_file, &src_content).unwrap();

    // Basis: 8KB
    let basis_content = vec![b'B'; 8192];
    let dest_file = dest_dir.join("shrink.bin");
    fs::write(&dest_file, &basis_content).unwrap();
    let old_time = FileTime::from_unix_time(1600000000, 0);
    set_file_times(&dest_file, old_time, old_time).unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([src_file.to_str().unwrap(), dest_file.to_str().unwrap()]);
    cmd.assert_success();

    // Verify file truncated to 4KB
    let result = fs::read(&dest_file).unwrap();
    assert_eq!(result.len(), 4096, "File should be truncated to 4KB");
    assert_eq!(result, src_content, "Content should match source");
}

#[test]
fn delta_transfer_binary_all_bytes() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Create file with all possible byte values (0-255) repeated
    let src_content: Vec<u8> = (0..=255).cycle().take(8192).collect();
    let src_file = src_dir.join("binary.bin");
    fs::write(&src_file, &src_content).unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([src_file.to_str().unwrap(), dest_dir.to_str().unwrap()]);
    cmd.assert_success();

    // Verify binary content preserved exactly
    let dest_file = dest_dir.join("binary.bin");
    assert_eq!(
        fs::read(&dest_file).unwrap(),
        src_content,
        "Binary content with all byte values should be preserved"
    );
}
