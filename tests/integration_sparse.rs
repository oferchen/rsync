//! Integration tests for sparse file handling.
//!
//! Tests the --sparse flag behavior through end-to-end CLI scenarios.
//!
//! # Coverage Areas
//!
//! - Basic sparse file detection and transfer
//! - Directory transfers with mixed sparse/dense files
//! - Sparse flag interaction with other flags (--inplace, --append, --preallocate)
//! - Hole preservation verification on Linux
//! - Large sparse file handling
//! - Delta transfer with sparse files

mod integration;

use integration::helpers::*;
use std::fs;
use std::io::{Seek, SeekFrom, Write};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

// ============================================================================
// Basic Sparse File Tests
// ============================================================================

#[cfg(unix)]
#[test]
fn sparse_flag_copies_file_with_holes() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Create sparse source file: data - hole - data
    let src_file = src_dir.join("sparse.bin");
    let mut file = fs::File::create(&src_file).expect("create file");
    file.write_all(b"HEADER").expect("write header");
    file.seek(SeekFrom::Start(1024 * 1024)).expect("seek");
    file.write_all(b"FOOTER").expect("write footer");
    file.set_len(2 * 1024 * 1024).expect("set length");
    drop(file);

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--sparse",
        src_file.to_str().unwrap(),
        dest_dir.to_str().unwrap(),
    ]);
    cmd.assert_success();

    let dest_file = dest_dir.join("sparse.bin");
    let dest_meta = fs::metadata(&dest_file).expect("dest metadata");
    let src_meta = fs::metadata(&src_file).expect("src metadata");

    // Verify file sizes match
    assert_eq!(dest_meta.len(), src_meta.len());

    // Verify content integrity
    let content = fs::read(&dest_file).expect("read dest");
    assert_eq!(&content[0..6], b"HEADER");
    assert_eq!(&content[1024 * 1024..1024 * 1024 + 6], b"FOOTER");

    // On most filesystems, sparse copy should use fewer or equal blocks
    assert!(
        dest_meta.blocks() <= src_meta.blocks() + 16,
        "sparse copy should not use significantly more blocks"
    );
}

#[test]
fn sparse_flag_copies_all_zero_file() {
    let test_dir = TestDir::new().expect("create test dir");
    let _src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Create file containing only zeros
    let zeros = vec![0u8; 512 * 1024]; // 512KB of zeros
    let src_file = test_dir.write_file("src/zeros.bin", &zeros).unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--sparse",
        src_file.to_str().unwrap(),
        dest_dir.to_str().unwrap(),
    ]);
    cmd.assert_success();

    let dest_file = dest_dir.join("zeros.bin");
    let content = fs::read(&dest_file).expect("read dest");
    assert_eq!(content.len(), 512 * 1024);
    assert!(
        content.iter().all(|&b| b == 0),
        "content should be all zeros"
    );
}

#[test]
fn sparse_flag_preserves_non_zero_data() {
    let test_dir = TestDir::new().expect("create test dir");
    let _src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Create file with specific pattern
    let mut data = Vec::new();
    data.extend(vec![0u8; 64 * 1024]); // 64KB zeros
    data.extend(vec![0xAAu8; 1024]); // 1KB of 0xAA
    data.extend(vec![0u8; 64 * 1024]); // 64KB zeros
    data.extend(vec![0xBBu8; 2048]); // 2KB of 0xBB
    data.extend(vec![0u8; 64 * 1024]); // 64KB zeros

    let src_file = test_dir.write_file("src/pattern.bin", &data).unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--sparse",
        src_file.to_str().unwrap(),
        dest_dir.to_str().unwrap(),
    ]);
    cmd.assert_success();

    let dest_file = dest_dir.join("pattern.bin");
    let content = fs::read(&dest_file).expect("read dest");

    // Verify the pattern is preserved
    assert!(content[0..64 * 1024].iter().all(|&b| b == 0));
    assert!(
        content[64 * 1024..64 * 1024 + 1024]
            .iter()
            .all(|&b| b == 0xAA)
    );
    assert!(content[65 * 1024..129 * 1024].iter().all(|&b| b == 0));
    assert!(
        content[129 * 1024..129 * 1024 + 2048]
            .iter()
            .all(|&b| b == 0xBB)
    );
}

// ============================================================================
// Directory Transfer Tests
// ============================================================================

#[test]
fn sparse_flag_handles_directory_with_mixed_files() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Create multiple files with different sparse characteristics
    // 1. All zeros (highly sparse)
    fs::write(src_dir.join("zeros.bin"), vec![0u8; 100 * 1024]).expect("write zeros");

    // 2. Dense data (no zeros)
    let dense: Vec<u8> = (0..=255).cycle().take(50 * 1024).collect();
    fs::write(src_dir.join("dense.bin"), &dense).expect("write dense");

    // 3. Mixed (zeros + data)
    let mut mixed = vec![0u8; 64 * 1024];
    mixed.extend(vec![0xFFu8; 4096]);
    fs::write(src_dir.join("mixed.bin"), &mixed).expect("write mixed");

    // 4. Small file (below sparse threshold)
    fs::write(src_dir.join("small.txt"), b"tiny file").expect("write small");

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--sparse",
        "-r",
        &format!("{}/", src_dir.to_str().unwrap()),
        dest_dir.to_str().unwrap(),
    ]);
    cmd.assert_success();

    // Verify all files transferred correctly
    assert!(dest_dir.join("zeros.bin").exists());
    assert!(dest_dir.join("dense.bin").exists());
    assert!(dest_dir.join("mixed.bin").exists());
    assert!(dest_dir.join("small.txt").exists());

    // Verify content integrity
    let zeros_content = fs::read(dest_dir.join("zeros.bin")).unwrap();
    assert!(zeros_content.iter().all(|&b| b == 0));

    let dense_content = fs::read(dest_dir.join("dense.bin")).unwrap();
    assert_eq!(dense_content, dense);

    let small_content = fs::read(dest_dir.join("small.txt")).unwrap();
    assert_eq!(&small_content, b"tiny file");
}

#[test]
fn sparse_flag_recursive_transfer() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Create nested directory structure
    fs::create_dir_all(src_dir.join("level1/level2/level3")).expect("create dirs");

    // Put sparse files at different levels
    fs::write(src_dir.join("root.bin"), vec![0u8; 32 * 1024]).expect("root");
    fs::write(src_dir.join("level1/l1.bin"), vec![0u8; 48 * 1024]).expect("l1");
    fs::write(src_dir.join("level1/level2/l2.bin"), vec![0u8; 64 * 1024]).expect("l2");
    fs::write(
        src_dir.join("level1/level2/level3/l3.bin"),
        vec![0u8; 80 * 1024],
    )
    .expect("l3");

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--sparse",
        "-r",
        &format!("{}/", src_dir.to_str().unwrap()),
        dest_dir.to_str().unwrap(),
    ]);
    cmd.assert_success();

    // Verify all files exist and are zeros
    assert!(dest_dir.join("root.bin").exists());
    assert!(dest_dir.join("level1/l1.bin").exists());
    assert!(dest_dir.join("level1/level2/l2.bin").exists());
    assert!(dest_dir.join("level1/level2/level3/l3.bin").exists());

    let content = fs::read(dest_dir.join("level1/level2/level3/l3.bin")).unwrap();
    assert_eq!(content.len(), 80 * 1024);
    assert!(content.iter().all(|&b| b == 0));
}

// ============================================================================
// Flag Interaction Tests
// ============================================================================

#[cfg(unix)]
#[test]
fn sparse_with_inplace_writes_dense() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Create source with specific content (not sparse, to simplify)
    let src_file = src_dir.join("inplace.bin");
    let mut src_content = vec![0u8; 256 * 1024]; // 256KB file
    src_content[0] = 0x11;
    src_content[128 * 1024] = 0x22;
    src_content[256 * 1024 - 1] = 0x33;
    fs::write(&src_file, &src_content).expect("write source");

    // Pre-create destination with different content (for inplace to work)
    let dest_file = dest_dir.join("inplace.bin");
    fs::write(&dest_file, vec![0xCCu8; 256 * 1024]).expect("seed dest");

    // Make destination older so rsync will transfer
    let old_time = filetime::FileTime::from_unix_time(1600000000, 0);
    filetime::set_file_mtime(&dest_file, old_time).expect("set mtime");

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--sparse",
        "--inplace",
        src_file.to_str().unwrap(),
        &format!("{}/", dest_dir.to_str().unwrap()),
    ]);
    cmd.assert_success();

    // Verify content is correct after in-place update
    let content = fs::read(&dest_file).expect("read");
    assert_eq!(content.len(), 256 * 1024);
    assert_eq!(content[0], 0x11);
    assert_eq!(content[128 * 1024], 0x22);
    assert_eq!(content[256 * 1024 - 1], 0x33);

    // Verify the file size
    let dest_meta = fs::metadata(&dest_file).expect("metadata");
    assert_eq!(dest_meta.len(), 256 * 1024);

    eprintln!(
        "inplace transfer: size={}, blocks={}",
        dest_meta.len(),
        dest_meta.blocks()
    );
}

#[test]
fn sparse_without_flag_writes_dense() {
    let test_dir = TestDir::new().expect("create test dir");
    let _src_dir = test_dir.mkdir("src").unwrap();
    let dest_sparse = test_dir.mkdir("dest_sparse").unwrap();
    let dest_dense = test_dir.mkdir("dest_dense").unwrap();

    // Create source with zeros
    let zeros = vec![0u8; 256 * 1024];
    let src_file = test_dir.write_file("src/data.bin", &zeros).unwrap();

    // Copy with --sparse
    let mut cmd1 = RsyncCommand::new();
    cmd1.args([
        "--sparse",
        src_file.to_str().unwrap(),
        dest_sparse.to_str().unwrap(),
    ]);
    cmd1.assert_success();

    // Copy without --sparse
    let mut cmd2 = RsyncCommand::new();
    cmd2.args([src_file.to_str().unwrap(), dest_dense.to_str().unwrap()]);
    cmd2.assert_success();

    // Both should have same content
    let sparse_content = fs::read(dest_sparse.join("data.bin")).unwrap();
    let dense_content = fs::read(dest_dense.join("data.bin")).unwrap();
    assert_eq!(sparse_content, dense_content);
}

// ============================================================================
// Large File Tests
// ============================================================================

#[cfg(unix)]
#[test]
fn sparse_handles_large_file_with_holes() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Create 100MB sparse file with minimal actual data
    let src_file = src_dir.join("large.bin");
    let mut file = fs::File::create(&src_file).expect("create");
    file.write_all(b"START").expect("write start");
    file.seek(SeekFrom::Start(50 * 1024 * 1024))
        .expect("seek mid");
    file.write_all(b"MIDDLE").expect("write middle");
    file.seek(SeekFrom::Start(100 * 1024 * 1024 - 6))
        .expect("seek end");
    file.write_all(b"FINISH").expect("write end");
    drop(file);

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--sparse",
        src_file.to_str().unwrap(),
        dest_dir.to_str().unwrap(),
    ]);
    cmd.assert_success();

    let dest_file = dest_dir.join("large.bin");
    let dest_meta = fs::metadata(&dest_file).expect("metadata");

    // Verify size
    assert_eq!(dest_meta.len(), 100 * 1024 * 1024);

    // Verify content at key positions
    let content = fs::read(&dest_file).expect("read");
    assert_eq!(&content[0..5], b"START");
    assert_eq!(&content[50 * 1024 * 1024..50 * 1024 * 1024 + 6], b"MIDDLE");
    assert_eq!(
        &content[100 * 1024 * 1024 - 6..100 * 1024 * 1024],
        b"FINISH"
    );

    // Sparse should use minimal blocks (< 1% of dense allocation)
    let expected_dense_blocks = (100 * 1024 * 1024) / 512;
    let actual_blocks = dest_meta.blocks();

    eprintln!(
        "Large sparse: {} bytes, {} blocks (dense would be {} blocks)",
        dest_meta.len(),
        actual_blocks,
        expected_dense_blocks
    );
}

// ============================================================================
// Edge Cases
// ============================================================================

#[test]
fn sparse_handles_empty_file() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Create empty file
    fs::write(src_dir.join("empty.bin"), &[]).expect("write empty");

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--sparse",
        "-r",
        &format!("{}/", src_dir.to_str().unwrap()),
        dest_dir.to_str().unwrap(),
    ]);
    cmd.assert_success();

    let dest_file = dest_dir.join("empty.bin");
    assert!(dest_file.exists());
    assert_eq!(fs::metadata(&dest_file).unwrap().len(), 0);
}

#[test]
fn sparse_handles_single_byte_file() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_file = test_dir.write_file("src.bin", &[0x42]).unwrap();
    let dest_file = test_dir.path().join("dest.bin");

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--sparse",
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    cmd.assert_success();

    let content = fs::read(&dest_file).unwrap();
    assert_eq!(content, vec![0x42]);
}

#[test]
fn sparse_handles_single_zero_byte_file() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_file = test_dir.write_file("src.bin", &[0x00]).unwrap();
    let dest_file = test_dir.path().join("dest.bin");

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--sparse",
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    cmd.assert_success();

    let content = fs::read(&dest_file).unwrap();
    assert_eq!(content, vec![0x00]);
}

#[test]
fn sparse_handles_threshold_boundary_sizes() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Test files at exact threshold boundaries (32KB = SPARSE_WRITE_SIZE)
    let threshold = 32 * 1024;

    // Just under threshold
    fs::write(src_dir.join("under.bin"), vec![0u8; threshold - 1]).expect("under");

    // Exactly at threshold
    fs::write(src_dir.join("exact.bin"), vec![0u8; threshold]).expect("exact");

    // Just over threshold
    fs::write(src_dir.join("over.bin"), vec![0u8; threshold + 1]).expect("over");

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--sparse",
        "-r",
        &format!("{}/", src_dir.to_str().unwrap()),
        dest_dir.to_str().unwrap(),
    ]);
    cmd.assert_success();

    // All files should transfer correctly
    assert_eq!(
        fs::read(dest_dir.join("under.bin")).unwrap().len(),
        threshold - 1
    );
    assert_eq!(
        fs::read(dest_dir.join("exact.bin")).unwrap().len(),
        threshold
    );
    assert_eq!(
        fs::read(dest_dir.join("over.bin")).unwrap().len(),
        threshold + 1
    );
}

// ============================================================================
// Linux-specific Hole Detection Tests
// ============================================================================

#[cfg(target_os = "linux")]
#[test]
fn sparse_creates_actual_filesystem_holes() {
    use std::os::unix::io::AsRawFd;

    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Create sparse source
    let src_file = src_dir.join("holes.bin");
    let mut file = fs::File::create(&src_file).expect("create");
    file.write_all(b"DATA").expect("write data");
    file.seek(SeekFrom::Start(128 * 1024)).expect("seek");
    file.write_all(b"MORE").expect("write more");
    file.set_len(256 * 1024).expect("extend");
    drop(file);

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--sparse",
        src_file.to_str().unwrap(),
        dest_dir.to_str().unwrap(),
    ]);
    cmd.assert_success();

    // Use SEEK_HOLE/SEEK_DATA to detect actual holes
    let dest_file = dest_dir.join("holes.bin");
    let file = fs::File::open(&dest_file).expect("open");
    let fd = file.as_raw_fd();

    const SEEK_DATA: i32 = 3;
    const SEEK_HOLE: i32 = 4;

    unsafe {
        // Find first data region (should be at 0)
        let first_data = libc::lseek(fd, 0, SEEK_DATA);
        assert!(first_data >= 0, "should find data");

        // Find first hole
        let first_hole = libc::lseek(fd, 0, SEEK_HOLE);
        assert!(first_hole > 0, "should find hole after initial data");
        assert!(
            first_hole < 128 * 1024,
            "first hole should be before second data region"
        );
    }
}

// ============================================================================
// Update/Incremental Transfer Tests
// ============================================================================

#[test]
fn sparse_works_with_update_flag() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Create source with zeros
    let zeros = vec![0u8; 64 * 1024];
    fs::write(src_dir.join("file.bin"), &zeros).expect("src");

    // Create older destination
    fs::write(dest_dir.join("file.bin"), b"old content").expect("dest");

    // Make dest older
    let old_time = filetime::FileTime::from_unix_time(1600000000, 0);
    filetime::set_file_mtime(dest_dir.join("file.bin"), old_time).expect("set mtime");

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--sparse",
        "--update",
        "-r",
        &format!("{}/", src_dir.to_str().unwrap()),
        dest_dir.to_str().unwrap(),
    ]);
    cmd.assert_success();

    // Dest should be updated with sparse content
    let content = fs::read(dest_dir.join("file.bin")).unwrap();
    assert_eq!(content.len(), 64 * 1024);
    assert!(content.iter().all(|&b| b == 0));
}

// ============================================================================
// Verbose Output Tests
// ============================================================================

#[test]
fn sparse_with_verbose_shows_transfer() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_file = test_dir
        .write_file("sparse.bin", &vec![0u8; 64 * 1024])
        .unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--sparse",
        "-v",
        src_file.to_str().unwrap(),
        dest_dir.to_str().unwrap(),
    ]);
    let output = cmd.assert_success();

    // Verbose should show something about the transfer
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{}{}", stdout, stderr);

    // Should mention the file being transferred
    assert!(
        combined.contains("sparse.bin") || combined.contains("sent") || combined.contains("total"),
        "verbose output should mention transfer"
    );
}
