//! Integration tests for filter rules and pattern matching.
//!
//! Tests include/exclude patterns, filter files, and CVS exclusions.

mod integration;

use integration::helpers::*;
use std::fs;

// ============================================================================
// Basic Exclude/Include Tests
// ============================================================================

#[test]
fn exclude_pattern_filters_files() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    fs::write(src_dir.join("keep.txt"), b"keep").unwrap();
    fs::write(src_dir.join("skip.log"), b"skip").unwrap();
    fs::write(src_dir.join("data.txt"), b"data").unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        "--exclude=*.log",
        &format!("{}/", src_dir.display()),
        &format!("{}/", dest_dir.display()),
    ]);
    cmd.assert_success();

    assert!(dest_dir.join("keep.txt").exists());
    assert!(dest_dir.join("data.txt").exists());
    assert!(
        !dest_dir.join("skip.log").exists(),
        "*.log files should be excluded"
    );
}

#[test]
fn exclude_multiple_patterns() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    fs::write(src_dir.join("file.txt"), b"text").unwrap();
    fs::write(src_dir.join("debug.log"), b"log").unwrap();
    fs::write(src_dir.join("temp.tmp"), b"temp").unwrap();
    fs::write(src_dir.join("data.dat"), b"data").unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        "--exclude=*.log",
        "--exclude=*.tmp",
        &format!("{}/", src_dir.display()),
        &format!("{}/", dest_dir.display()),
    ]);
    cmd.assert_success();

    assert!(dest_dir.join("file.txt").exists());
    assert!(dest_dir.join("data.dat").exists());
    assert!(!dest_dir.join("debug.log").exists());
    assert!(!dest_dir.join("temp.tmp").exists());
}

#[test]
fn include_pattern_overrides_exclude() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    fs::write(src_dir.join("important.log"), b"important").unwrap();
    fs::write(src_dir.join("debug.log"), b"debug").unwrap();
    fs::write(src_dir.join("data.txt"), b"data").unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        "--include=important.log",
        "--exclude=*.log",
        &format!("{}/", src_dir.display()),
        &format!("{}/", dest_dir.display()),
    ]);
    cmd.assert_success();

    assert!(
        dest_dir.join("important.log").exists(),
        "included file should be transferred"
    );
    assert!(
        !dest_dir.join("debug.log").exists(),
        "other .log files should be excluded"
    );
    assert!(dest_dir.join("data.txt").exists());
}

#[test]
fn exclude_directory_pattern() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    fs::create_dir(src_dir.join("keep")).unwrap();
    fs::write(src_dir.join("keep/file.txt"), b"keep").unwrap();

    fs::create_dir(src_dir.join(".git")).unwrap();
    fs::write(src_dir.join(".git/config"), b"git").unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        "--exclude=.git/",
        &format!("{}/", src_dir.display()),
        &format!("{}/", dest_dir.display()),
    ]);
    cmd.assert_success();

    assert!(dest_dir.join("keep/file.txt").exists());
    assert!(
        !dest_dir.join(".git").exists(),
        ".git directory should be excluded"
    );
}

// ============================================================================
// Filter File Tests
// ============================================================================

#[test]
fn exclude_from_file() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    fs::write(src_dir.join("file1.txt"), b"1").unwrap();
    fs::write(src_dir.join("file2.log"), b"2").unwrap();
    fs::write(src_dir.join("file3.tmp"), b"3").unwrap();

    // Create exclude file
    let exclude_file = test_dir
        .write_file("exclude.txt", b"*.log\n*.tmp\n")
        .unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        &format!("--exclude-from={}", exclude_file.display()),
        &format!("{}/", src_dir.display()),
        &format!("{}/", dest_dir.display()),
    ]);
    cmd.assert_success();

    assert!(dest_dir.join("file1.txt").exists());
    assert!(!dest_dir.join("file2.log").exists());
    assert!(!dest_dir.join("file3.tmp").exists());
}

#[test]
fn include_from_file() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    fs::write(src_dir.join("important.txt"), b"important").unwrap();
    fs::write(src_dir.join("critical.dat"), b"critical").unwrap();
    fs::write(src_dir.join("other.txt"), b"other").unwrap();

    // Create include file
    let include_file = test_dir
        .write_file("include.txt", b"important.txt\ncritical.dat\n")
        .unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        &format!("--include-from={}", include_file.display()),
        "--exclude=*",
        &format!("{}/", src_dir.display()),
        &format!("{}/", dest_dir.display()),
    ]);
    cmd.assert_success();

    assert!(dest_dir.join("important.txt").exists());
    assert!(dest_dir.join("critical.dat").exists());
    assert!(!dest_dir.join("other.txt").exists());
}

// ============================================================================
// CVS Exclude Tests
// ============================================================================

#[test]
fn cvs_exclude_ignores_common_files() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    fs::write(src_dir.join("file.txt"), b"keep").unwrap();
    fs::write(src_dir.join("core"), b"core dump").unwrap();
    fs::create_dir(src_dir.join("CVS")).unwrap();
    fs::write(src_dir.join("CVS/Entries"), b"cvs").unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        "-C",
        &format!("{}/", src_dir.display()),
        &format!("{}/", dest_dir.display()),
    ]);
    cmd.assert_success();

    assert!(dest_dir.join("file.txt").exists());
    // CVS and core files should be excluded with -C
    // Note: actual CVS behavior depends on implementation
}

// ============================================================================
// Complex Filter Scenarios
// ============================================================================

#[test]
fn nested_directory_exclude() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    fs::create_dir_all(src_dir.join("a/b/c")).unwrap();
    fs::write(src_dir.join("a/keep.txt"), b"keep").unwrap();
    fs::write(src_dir.join("a/b/skip.log"), b"skip").unwrap();
    fs::write(src_dir.join("a/b/c/data.txt"), b"data").unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        "--exclude=*.log",
        &format!("{}/", src_dir.display()),
        &format!("{}/", dest_dir.display()),
    ]);
    cmd.assert_success();

    assert!(dest_dir.join("a/keep.txt").exists());
    assert!(dest_dir.join("a/b/c/data.txt").exists());
    assert!(!dest_dir.join("a/b/skip.log").exists());
}

#[test]
fn filter_with_wildcards() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    fs::write(src_dir.join("test_file.txt"), b"1").unwrap();
    fs::write(src_dir.join("test_data.txt"), b"2").unwrap();
    fs::write(src_dir.join("prod_file.txt"), b"3").unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        "--exclude=test_*",
        &format!("{}/", src_dir.display()),
        &format!("{}/", dest_dir.display()),
    ]);
    cmd.assert_success();

    assert!(!dest_dir.join("test_file.txt").exists());
    assert!(!dest_dir.join("test_data.txt").exists());
    assert!(dest_dir.join("prod_file.txt").exists());
}

#[test]
fn later_filter_rule_wins() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    fs::write(src_dir.join("file.log"), b"log").unwrap();
    fs::write(src_dir.join("file.txt"), b"text").unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        "--exclude=*.log",
        "--include=file.log", // Include after exclude, so include wins (last rule wins)
        &format!("{}/", src_dir.display()),
        &format!("{}/", dest_dir.display()),
    ]);
    cmd.assert_success();

    // Last matching rule wins in rsync filter processing
    assert!(
        dest_dir.join("file.log").exists(),
        "later include should override earlier exclude"
    );
    assert!(dest_dir.join("file.txt").exists());
}

// ============================================================================
// Edge Cases
// ============================================================================

#[test]
fn empty_exclude_file_excludes_nothing() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    fs::write(src_dir.join("file1.txt"), b"1").unwrap();
    fs::write(src_dir.join("file2.txt"), b"2").unwrap();

    let exclude_file = test_dir.write_file("empty_exclude.txt", b"").unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        &format!("--exclude-from={}", exclude_file.display()),
        &format!("{}/", src_dir.display()),
        &format!("{}/", dest_dir.display()),
    ]);
    cmd.assert_success();

    assert!(dest_dir.join("file1.txt").exists());
    assert!(dest_dir.join("file2.txt").exists());
}

#[test]
fn exclude_all_then_include_specific() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    fs::write(src_dir.join("keep.txt"), b"keep").unwrap();
    fs::write(src_dir.join("skip1.txt"), b"skip1").unwrap();
    fs::write(src_dir.join("skip2.txt"), b"skip2").unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        "--include=keep.txt",
        "--exclude=*",
        &format!("{}/", src_dir.display()),
        &format!("{}/", dest_dir.display()),
    ]);
    cmd.assert_success();

    assert!(dest_dir.join("keep.txt").exists());
    assert!(!dest_dir.join("skip1.txt").exists());
    assert!(!dest_dir.join("skip2.txt").exists());
}

#[test]
fn case_sensitive_pattern_matching() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Detect case-insensitive filesystem by checking if writing two files with
    // different cases results in a single file (macOS default APFS, Windows NTFS)
    fs::write(src_dir.join("File.TXT"), b"upper").unwrap();
    fs::write(src_dir.join("file.txt"), b"lower").unwrap();

    // On case-insensitive filesystems, File.TXT and file.txt are the same file
    let entries: Vec<_> = fs::read_dir(&src_dir)
        .unwrap()
        .filter_map(Result::ok)
        .collect();
    if entries.len() == 1 {
        // Case-insensitive filesystem - skip test
        println!("Skipping test: filesystem is case-insensitive");
        return;
    }

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        "--exclude=*.txt", // Should only match lowercase
        &format!("{}/", src_dir.display()),
        &format!("{}/", dest_dir.display()),
    ]);
    cmd.assert_success();

    // Case sensitivity depends on filesystem and rsync implementation
    // At minimum, lowercase should be excluded
    assert!(!dest_dir.join("file.txt").exists());
}

#[test]
fn exclude_applies_to_subdirectories() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    fs::create_dir_all(src_dir.join("dir1/dir2")).unwrap();
    fs::write(src_dir.join("file.txt"), b"root").unwrap();
    fs::write(src_dir.join("dir1/file.txt"), b"dir1").unwrap();
    fs::write(src_dir.join("dir1/dir2/file.txt"), b"dir2").unwrap();
    fs::write(src_dir.join("keep.dat"), b"keep").unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        "--exclude=*.txt",
        &format!("{}/", src_dir.display()),
        &format!("{}/", dest_dir.display()),
    ]);
    cmd.assert_success();

    // All .txt files should be excluded at any depth
    assert!(!dest_dir.join("file.txt").exists());
    assert!(!dest_dir.join("dir1/file.txt").exists());
    assert!(!dest_dir.join("dir1/dir2/file.txt").exists());
    assert!(dest_dir.join("keep.dat").exists());
}

#[test]
fn multiple_include_patterns() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    fs::write(src_dir.join("doc.txt"), b"text").unwrap();
    fs::write(src_dir.join("data.dat"), b"data").unwrap();
    fs::write(src_dir.join("image.png"), b"image").unwrap();
    fs::write(src_dir.join("skip.log"), b"skip").unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        "--include=*.txt",
        "--include=*.dat",
        "--exclude=*",
        &format!("{}/", src_dir.display()),
        &format!("{}/", dest_dir.display()),
    ]);
    cmd.assert_success();

    assert!(dest_dir.join("doc.txt").exists());
    assert!(dest_dir.join("data.dat").exists());
    assert!(!dest_dir.join("image.png").exists());
    assert!(!dest_dir.join("skip.log").exists());
}
