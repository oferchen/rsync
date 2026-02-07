//! Comprehensive tests for --max-size and --min-size file filtering.
//!
//! These options filter files based on their size:
//! - --max-size=SIZE: Don't transfer any file larger than SIZE
//! - --min-size=SIZE: Don't transfer any file smaller than SIZE
//!
//! SIZE can be specified with optional suffixes:
//! - K (kibibytes, 1024 bytes)
//! - M (mebibytes, 1024^2 bytes)
//! - G (gibibytes, 1024^3 bytes)
//! - T (tebibytes, 1024^4 bytes)
//! - KB (kilobytes, 1000 bytes) - decimal
//! - KiB (kibibytes, 1024 bytes) - explicit binary
//!
//! Run these tests with: cargo nextest run size_filter

mod integration;

use integration::helpers::*;
use std::fs;

// ============================================================================
// --max-size Basic Behavior
// ============================================================================

/// Test that files larger than --max-size are excluded from transfer.
#[test]
fn max_size_excludes_files_larger_than_limit() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Create files of various sizes
    fs::write(src_dir.join("small.txt"), vec![0u8; 500]).unwrap(); // 500 bytes
    fs::write(src_dir.join("medium.txt"), vec![0u8; 1024]).unwrap(); // 1024 bytes (1K)
    fs::write(src_dir.join("large.txt"), vec![0u8; 2048]).unwrap(); // 2048 bytes (2K)

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        "--max-size=1K",
        &format!("{}/", src_dir.display()),
        dest_dir.to_str().unwrap(),
    ]);
    cmd.assert_success();

    // Files <= 1K should be transferred
    assert!(
        dest_dir.join("small.txt").exists(),
        "500-byte file should be transferred"
    );
    assert!(
        dest_dir.join("medium.txt").exists(),
        "1024-byte file should be transferred (exactly at limit)"
    );
    // Files > 1K should be excluded
    assert!(
        !dest_dir.join("large.txt").exists(),
        "2048-byte file should be excluded"
    );
}

/// Test that files exactly at the --max-size limit are included.
#[test]
fn max_size_includes_files_at_exact_boundary() {
    let test_dir = TestDir::new().expect("create test dir");

    let src_file = test_dir.write_file("source.txt", &vec![0u8; 1000]).unwrap();
    let dest_file = test_dir.path().join("dest.txt");

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--max-size=1000",
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    cmd.assert_success();

    assert!(
        dest_file.exists(),
        "File exactly at max-size limit should be transferred"
    );
    assert_eq!(
        fs::read(&dest_file).unwrap().len(),
        1000,
        "File content should match"
    );
}

/// Test that files one byte over --max-size are excluded.
#[test]
fn max_size_excludes_files_one_byte_over() {
    let test_dir = TestDir::new().expect("create test dir");

    let src_file = test_dir.write_file("source.txt", &vec![0u8; 1001]).unwrap();
    let dest_file = test_dir.path().join("dest.txt");

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--max-size=1000",
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    cmd.assert_success();

    assert!(
        !dest_file.exists(),
        "File one byte over max-size should be excluded"
    );
}

// ============================================================================
// --min-size Basic Behavior
// ============================================================================

/// Test that files smaller than --min-size are excluded from transfer.
#[test]
fn min_size_excludes_files_smaller_than_limit() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Create files of various sizes
    fs::write(src_dir.join("tiny.txt"), b"ab").unwrap(); // 2 bytes
    fs::write(src_dir.join("small.txt"), vec![0u8; 99]).unwrap(); // 99 bytes
    fs::write(src_dir.join("medium.txt"), vec![0u8; 100]).unwrap(); // 100 bytes
    fs::write(src_dir.join("large.txt"), vec![0u8; 500]).unwrap(); // 500 bytes

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        "--min-size=100",
        &format!("{}/", src_dir.display()),
        dest_dir.to_str().unwrap(),
    ]);
    cmd.assert_success();

    // Files < 100 bytes should be excluded
    assert!(
        !dest_dir.join("tiny.txt").exists(),
        "2-byte file should be excluded"
    );
    assert!(
        !dest_dir.join("small.txt").exists(),
        "99-byte file should be excluded"
    );
    // Files >= 100 bytes should be transferred
    assert!(
        dest_dir.join("medium.txt").exists(),
        "100-byte file should be transferred (exactly at limit)"
    );
    assert!(
        dest_dir.join("large.txt").exists(),
        "500-byte file should be transferred"
    );
}

/// Test that files exactly at --min-size limit are included.
#[test]
fn min_size_includes_files_at_exact_boundary() {
    let test_dir = TestDir::new().expect("create test dir");

    let src_file = test_dir.write_file("source.txt", &vec![0u8; 500]).unwrap();
    let dest_file = test_dir.path().join("dest.txt");

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--min-size=500",
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    cmd.assert_success();

    assert!(
        dest_file.exists(),
        "File exactly at min-size limit should be transferred"
    );
}

/// Test that files one byte under --min-size are excluded.
#[test]
fn min_size_excludes_files_one_byte_under() {
    let test_dir = TestDir::new().expect("create test dir");

    let src_file = test_dir.write_file("source.txt", &vec![0u8; 499]).unwrap();
    let dest_file = test_dir.path().join("dest.txt");

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--min-size=500",
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    cmd.assert_success();

    assert!(
        !dest_file.exists(),
        "File one byte under min-size should be excluded"
    );
}

// ============================================================================
// Size Units (K, M, G)
// ============================================================================

/// Test --max-size with kilobyte suffix (K = 1024 bytes).
#[test]
fn max_size_with_kilobyte_suffix() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Create files around 2K boundary
    fs::write(src_dir.join("under_2k.txt"), vec![0u8; 2000]).unwrap(); // 2000 bytes < 2K
    fs::write(src_dir.join("at_2k.txt"), vec![0u8; 2048]).unwrap(); // 2048 bytes = 2K
    fs::write(src_dir.join("over_2k.txt"), vec![0u8; 2100]).unwrap(); // 2100 bytes > 2K

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        "--max-size=2K",
        &format!("{}/", src_dir.display()),
        dest_dir.to_str().unwrap(),
    ]);
    cmd.assert_success();

    assert!(
        dest_dir.join("under_2k.txt").exists(),
        "File under 2K should be transferred"
    );
    assert!(
        dest_dir.join("at_2k.txt").exists(),
        "File at exactly 2K should be transferred"
    );
    assert!(
        !dest_dir.join("over_2k.txt").exists(),
        "File over 2K should be excluded"
    );
}

/// Test --min-size with kilobyte suffix (K = 1024 bytes).
#[test]
fn min_size_with_kilobyte_suffix() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Create files around 1K boundary
    fs::write(src_dir.join("under_1k.txt"), vec![0u8; 1000]).unwrap(); // 1000 bytes < 1K
    fs::write(src_dir.join("at_1k.txt"), vec![0u8; 1024]).unwrap(); // 1024 bytes = 1K
    fs::write(src_dir.join("over_1k.txt"), vec![0u8; 1100]).unwrap(); // 1100 bytes > 1K

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        "--min-size=1K",
        &format!("{}/", src_dir.display()),
        dest_dir.to_str().unwrap(),
    ]);
    cmd.assert_success();

    assert!(
        !dest_dir.join("under_1k.txt").exists(),
        "File under 1K should be excluded"
    );
    assert!(
        dest_dir.join("at_1k.txt").exists(),
        "File at exactly 1K should be transferred"
    );
    assert!(
        dest_dir.join("over_1k.txt").exists(),
        "File over 1K should be transferred"
    );
}

/// Test --max-size with megabyte suffix (M = 1024*1024 bytes).
#[test]
fn max_size_with_megabyte_suffix() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    let one_mb = 1024 * 1024;

    // Create files around 1M boundary
    fs::write(src_dir.join("under_1m.txt"), vec![0u8; one_mb - 1024]).unwrap();
    fs::write(src_dir.join("at_1m.txt"), vec![0u8; one_mb]).unwrap();
    fs::write(src_dir.join("over_1m.txt"), vec![0u8; one_mb + 1024]).unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        "--max-size=1M",
        &format!("{}/", src_dir.display()),
        dest_dir.to_str().unwrap(),
    ]);
    cmd.assert_success();

    assert!(
        dest_dir.join("under_1m.txt").exists(),
        "File under 1M should be transferred"
    );
    assert!(
        dest_dir.join("at_1m.txt").exists(),
        "File at exactly 1M should be transferred"
    );
    assert!(
        !dest_dir.join("over_1m.txt").exists(),
        "File over 1M should be excluded"
    );
}

/// Test --min-size with megabyte suffix (M = 1024*1024 bytes).
#[test]
fn min_size_with_megabyte_suffix() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    let one_mb = 1024 * 1024;

    // Create files around 1M boundary
    fs::write(src_dir.join("under_1m.txt"), vec![0u8; one_mb - 1024]).unwrap();
    fs::write(src_dir.join("at_1m.txt"), vec![0u8; one_mb]).unwrap();
    fs::write(src_dir.join("over_1m.txt"), vec![0u8; one_mb + 1024]).unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        "--min-size=1M",
        &format!("{}/", src_dir.display()),
        dest_dir.to_str().unwrap(),
    ]);
    cmd.assert_success();

    assert!(
        !dest_dir.join("under_1m.txt").exists(),
        "File under 1M should be excluded"
    );
    assert!(
        dest_dir.join("at_1m.txt").exists(),
        "File at exactly 1M should be transferred"
    );
    assert!(
        dest_dir.join("over_1m.txt").exists(),
        "File over 1M should be transferred"
    );
}

/// Test --max-size with gigabyte suffix (G = 1024^3 bytes).
/// Note: We use small files that are all under 1G for practical testing.
#[test]
fn max_size_with_gigabyte_suffix_includes_small_files() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Create small files (all under 1G)
    fs::write(src_dir.join("small.txt"), vec![0u8; 1024]).unwrap();
    fs::write(src_dir.join("medium.txt"), vec![0u8; 1024 * 1024]).unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        "--max-size=1G",
        &format!("{}/", src_dir.display()),
        dest_dir.to_str().unwrap(),
    ]);
    cmd.assert_success();

    // All files should be transferred (they're all under 1G)
    assert!(
        dest_dir.join("small.txt").exists(),
        "Small file should be transferred"
    );
    assert!(
        dest_dir.join("medium.txt").exists(),
        "Medium file should be transferred"
    );
}

/// Test --min-size with gigabyte suffix excludes all small files.
#[test]
fn min_size_with_gigabyte_suffix_excludes_small_files() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Create small files (all under 1G)
    fs::write(src_dir.join("small.txt"), vec![0u8; 1024]).unwrap();
    fs::write(src_dir.join("medium.txt"), vec![0u8; 1024 * 1024]).unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        "--min-size=1G",
        &format!("{}/", src_dir.display()),
        dest_dir.to_str().unwrap(),
    ]);
    cmd.assert_success();

    // All files should be excluded (they're all under 1G)
    assert!(
        !dest_dir.join("small.txt").exists(),
        "Small file should be excluded"
    );
    assert!(
        !dest_dir.join("medium.txt").exists(),
        "Medium file should be excluded"
    );
}

/// Test lowercase suffix (k, m, g) works the same as uppercase.
#[test]
fn size_suffix_case_insensitive() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    fs::write(src_dir.join("file.txt"), vec![0u8; 2048]).unwrap(); // 2K

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        "--max-size=1k", // lowercase k
        &format!("{}/", src_dir.display()),
        dest_dir.to_str().unwrap(),
    ]);
    cmd.assert_success();

    // 2K file should be excluded with 1k limit
    assert!(
        !dest_dir.join("file.txt").exists(),
        "File should be excluded with lowercase suffix"
    );
}

/// Test fractional size values (e.g., 1.5K).
#[test]
fn max_size_fractional_value() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // 1.5K = 1536 bytes
    fs::write(src_dir.join("at_limit.txt"), vec![0u8; 1536]).unwrap();
    fs::write(src_dir.join("over_limit.txt"), vec![0u8; 1537]).unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        "--max-size=1.5K",
        &format!("{}/", src_dir.display()),
        dest_dir.to_str().unwrap(),
    ]);
    cmd.assert_success();

    assert!(
        dest_dir.join("at_limit.txt").exists(),
        "File at 1.5K should be transferred"
    );
    assert!(
        !dest_dir.join("over_limit.txt").exists(),
        "File over 1.5K should be excluded"
    );
}

// ============================================================================
// Combined --max-size and --min-size
// ============================================================================

/// Test combined --max-size and --min-size creates a size range.
#[test]
fn combined_max_and_min_size_filters() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Create files to test range: min=100, max=500
    fs::write(src_dir.join("too_small.txt"), vec![0u8; 50]).unwrap(); // < 100 (excluded)
    fs::write(src_dir.join("at_min.txt"), vec![0u8; 100]).unwrap(); // = 100 (included)
    fs::write(src_dir.join("in_range.txt"), vec![0u8; 300]).unwrap(); // 100-500 (included)
    fs::write(src_dir.join("at_max.txt"), vec![0u8; 500]).unwrap(); // = 500 (included)
    fs::write(src_dir.join("too_large.txt"), vec![0u8; 600]).unwrap(); // > 500 (excluded)

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        "--min-size=100",
        "--max-size=500",
        &format!("{}/", src_dir.display()),
        dest_dir.to_str().unwrap(),
    ]);
    cmd.assert_success();

    assert!(
        !dest_dir.join("too_small.txt").exists(),
        "File under min-size should be excluded"
    );
    assert!(
        dest_dir.join("at_min.txt").exists(),
        "File at min-size should be included"
    );
    assert!(
        dest_dir.join("in_range.txt").exists(),
        "File in range should be included"
    );
    assert!(
        dest_dir.join("at_max.txt").exists(),
        "File at max-size should be included"
    );
    assert!(
        !dest_dir.join("too_large.txt").exists(),
        "File over max-size should be excluded"
    );
}

/// Test combined filters with units.
#[test]
fn combined_filters_with_units() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Range: 1K to 10K
    fs::write(src_dir.join("too_small.txt"), vec![0u8; 512]).unwrap();
    fs::write(src_dir.join("in_range.txt"), vec![0u8; 5 * 1024]).unwrap();
    fs::write(src_dir.join("too_large.txt"), vec![0u8; 20 * 1024]).unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        "--min-size=1K",
        "--max-size=10K",
        &format!("{}/", src_dir.display()),
        dest_dir.to_str().unwrap(),
    ]);
    cmd.assert_success();

    assert!(
        !dest_dir.join("too_small.txt").exists(),
        "File under 1K should be excluded"
    );
    assert!(
        dest_dir.join("in_range.txt").exists(),
        "File between 1K-10K should be included"
    );
    assert!(
        !dest_dir.join("too_large.txt").exists(),
        "File over 10K should be excluded"
    );
}

// ============================================================================
// Edge Cases
// ============================================================================

/// Test that empty files (0 bytes) are included with --max-size (always under limit).
#[test]
fn max_size_includes_empty_files() {
    let test_dir = TestDir::new().expect("create test dir");

    let src_file = test_dir.write_file("empty.txt", b"").unwrap();
    let dest_file = test_dir.path().join("dest.txt");

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--max-size=1K",
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    cmd.assert_success();

    assert!(
        dest_file.exists(),
        "Empty file should be transferred (0 < max-size)"
    );
    assert_eq!(
        fs::metadata(&dest_file).unwrap().len(),
        0,
        "File should be empty"
    );
}

/// Test that empty files (0 bytes) are excluded with --min-size > 0.
#[test]
fn min_size_excludes_empty_files() {
    let test_dir = TestDir::new().expect("create test dir");

    let src_file = test_dir.write_file("empty.txt", b"").unwrap();
    let dest_file = test_dir.path().join("dest.txt");

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--min-size=1",
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    cmd.assert_success();

    assert!(
        !dest_file.exists(),
        "Empty file should be excluded (0 < min-size)"
    );
}

/// Test that empty files are included with --min-size=0.
#[test]
fn min_size_zero_includes_empty_files() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    fs::write(src_dir.join("empty.txt"), b"").unwrap();
    fs::write(src_dir.join("nonempty.txt"), b"content").unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        "--min-size=0",
        &format!("{}/", src_dir.display()),
        dest_dir.to_str().unwrap(),
    ]);
    cmd.assert_success();

    assert!(
        dest_dir.join("empty.txt").exists(),
        "Empty file should be included with min-size=0"
    );
    assert!(
        dest_dir.join("nonempty.txt").exists(),
        "Non-empty file should be included"
    );
}

/// Test that directories are always created regardless of size filters.
#[test]
fn size_filters_do_not_affect_directories() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Create nested directory structure with files of various sizes
    fs::create_dir_all(src_dir.join("subdir/nested")).unwrap();
    fs::write(src_dir.join("subdir/small.txt"), b"x").unwrap(); // 1 byte, should be excluded
    fs::write(src_dir.join("subdir/nested/large.txt"), vec![0u8; 200]).unwrap(); // 200 bytes, included

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        "--min-size=100",
        &format!("{}/", src_dir.display()),
        dest_dir.to_str().unwrap(),
    ]);
    cmd.assert_success();

    // Directories should exist
    assert!(dest_dir.join("subdir").is_dir(), "Subdir should be created");
    assert!(
        dest_dir.join("subdir/nested").is_dir(),
        "Nested dir should be created"
    );

    // Small file should be excluded, large file should exist
    assert!(
        !dest_dir.join("subdir/small.txt").exists(),
        "Small file should be excluded"
    );
    assert!(
        dest_dir.join("subdir/nested/large.txt").exists(),
        "Large file should be transferred"
    );
}

/// Test size filter with --dry-run shows what would happen.
#[test]
fn max_size_with_dry_run() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    fs::write(src_dir.join("small.txt"), vec![0u8; 100]).unwrap();
    fs::write(src_dir.join("large.txt"), vec![0u8; 2000]).unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        "--dry-run",
        "--max-size=1K",
        &format!("{}/", src_dir.display()),
        dest_dir.to_str().unwrap(),
    ]);
    cmd.assert_success();

    // Neither file should be created in dry-run mode
    assert!(
        !dest_dir.join("small.txt").exists(),
        "Dry run should not create files"
    );
    assert!(
        !dest_dir.join("large.txt").exists(),
        "Dry run should not create files"
    );
}

// ============================================================================
// Size Filter with Other Options
// ============================================================================

/// Test --max-size with --archive mode.
#[test]
fn max_size_with_archive_mode() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    fs::write(src_dir.join("small.txt"), vec![0u8; 100]).unwrap();
    fs::write(src_dir.join("large.txt"), vec![0u8; 2000]).unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-a",
        "--max-size=1K",
        &format!("{}/", src_dir.display()),
        dest_dir.to_str().unwrap(),
    ]);
    cmd.assert_success();

    assert!(
        dest_dir.join("small.txt").exists(),
        "Small file should be transferred"
    );
    assert!(
        !dest_dir.join("large.txt").exists(),
        "Large file should be excluded"
    );
}

/// Test --min-size with --archive mode.
#[test]
fn min_size_with_archive_mode() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    fs::write(src_dir.join("small.txt"), vec![0u8; 100]).unwrap();
    fs::write(src_dir.join("large.txt"), vec![0u8; 2000]).unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-a",
        "--min-size=500",
        &format!("{}/", src_dir.display()),
        dest_dir.to_str().unwrap(),
    ]);
    cmd.assert_success();

    assert!(
        !dest_dir.join("small.txt").exists(),
        "Small file should be excluded"
    );
    assert!(
        dest_dir.join("large.txt").exists(),
        "Large file should be transferred"
    );
}

/// Test size filters with --verbose.
#[test]
fn max_size_with_verbose() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    fs::write(src_dir.join("small.txt"), vec![0u8; 100]).unwrap();
    fs::write(src_dir.join("large.txt"), vec![0u8; 2000]).unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-rv",
        "--max-size=1K",
        &format!("{}/", src_dir.display()),
        dest_dir.to_str().unwrap(),
    ]);
    let output = cmd.assert_success();

    // Small file should be in output (transferred)
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("small.txt"),
        "Verbose output should show transferred file"
    );

    // Verify file state
    assert!(
        dest_dir.join("small.txt").exists(),
        "Small file should be transferred"
    );
    assert!(
        !dest_dir.join("large.txt").exists(),
        "Large file should be excluded"
    );
}

// ============================================================================
// Decimal Size Suffixes (KB, MB, GB)
// ============================================================================

/// Test --max-size with decimal kilobyte suffix (KB = 1000 bytes).
#[test]
fn max_size_decimal_kilobyte_suffix() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // KB = 1000 bytes (decimal), K = 1024 bytes (binary)
    fs::write(src_dir.join("under_1kb.txt"), vec![0u8; 999]).unwrap();
    fs::write(src_dir.join("at_1kb.txt"), vec![0u8; 1000]).unwrap();
    fs::write(src_dir.join("over_1kb.txt"), vec![0u8; 1001]).unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        "--max-size=1KB", // decimal kilobyte = 1000 bytes
        &format!("{}/", src_dir.display()),
        dest_dir.to_str().unwrap(),
    ]);
    cmd.assert_success();

    assert!(
        dest_dir.join("under_1kb.txt").exists(),
        "File under 1KB should be transferred"
    );
    assert!(
        dest_dir.join("at_1kb.txt").exists(),
        "File at exactly 1KB should be transferred"
    );
    assert!(
        !dest_dir.join("over_1kb.txt").exists(),
        "File over 1KB should be excluded"
    );
}

/// Test explicit binary suffix (KiB = 1024 bytes).
#[test]
fn max_size_explicit_binary_suffix() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // KiB = 1024 bytes (same as K)
    fs::write(src_dir.join("at_1k.txt"), vec![0u8; 1024]).unwrap();
    fs::write(src_dir.join("over_1k.txt"), vec![0u8; 1025]).unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        "--max-size=1KiB", // explicit binary kibibyte = 1024 bytes
        &format!("{}/", src_dir.display()),
        dest_dir.to_str().unwrap(),
    ]);
    cmd.assert_success();

    assert!(
        dest_dir.join("at_1k.txt").exists(),
        "File at exactly 1KiB should be transferred"
    );
    assert!(
        !dest_dir.join("over_1k.txt").exists(),
        "File over 1KiB should be excluded"
    );
}

// ============================================================================
// Update Behavior with Size Filters
// ============================================================================

/// Test that existing files in dest are not deleted when source files are filtered out.
#[test]
fn max_size_does_not_delete_existing_dest_files() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Create a large file in source (will be filtered)
    fs::write(src_dir.join("large.txt"), vec![0u8; 2000]).unwrap();

    // Create the same file in dest (smaller, already exists)
    fs::write(dest_dir.join("large.txt"), b"existing content").unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        "--max-size=1K",
        &format!("{}/", src_dir.display()),
        dest_dir.to_str().unwrap(),
    ]);
    cmd.assert_success();

    // Existing dest file should remain unchanged (source file was filtered, not transferred)
    assert!(
        dest_dir.join("large.txt").exists(),
        "Existing dest file should remain"
    );
    let content = fs::read(dest_dir.join("large.txt")).unwrap();
    assert_eq!(
        content, b"existing content",
        "Dest file content should be unchanged"
    );
}

/// Test size filter with multiple transfers of the same file.
#[test]
fn size_filter_incremental_transfer() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // First transfer: small file
    fs::write(src_dir.join("file.txt"), vec![0u8; 100]).unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        "--max-size=1K",
        &format!("{}/", src_dir.display()),
        dest_dir.to_str().unwrap(),
    ]);
    cmd.assert_success();

    assert!(
        dest_dir.join("file.txt").exists(),
        "First transfer should succeed"
    );
    assert_eq!(fs::metadata(dest_dir.join("file.txt")).unwrap().len(), 100);

    // Second transfer: file grew larger than limit
    fs::write(src_dir.join("file.txt"), vec![0u8; 2000]).unwrap();

    let mut cmd2 = RsyncCommand::new();
    cmd2.args([
        "-r",
        "--max-size=1K",
        &format!("{}/", src_dir.display()),
        dest_dir.to_str().unwrap(),
    ]);
    cmd2.assert_success();

    // Dest file should remain at original size (new version filtered out)
    assert_eq!(fs::metadata(dest_dir.join("file.txt")).unwrap().len(), 100);
}
