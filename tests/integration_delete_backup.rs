//! Integration tests for delete modes and backup operations.
//!
//! Tests delete timing variations, backup creation, and their interactions.

mod integration;

use integration::helpers::*;
use std::fs;

// ============================================================================
// Delete Mode Tests
// ============================================================================

#[test]
fn delete_before_removes_extra_files() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Source has only file1.txt
    fs::write(src_dir.join("file1.txt"), b"content1").unwrap();

    // Dest has file1.txt and file2.txt (extra)
    fs::write(dest_dir.join("file1.txt"), b"content1").unwrap();
    fs::write(dest_dir.join("file2.txt"), b"extra").unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        "--delete-before",
        &format!("{}/", src_dir.display()),
        &format!("{}/", dest_dir.display()),
    ]);
    cmd.assert_success();

    assert!(dest_dir.join("file1.txt").exists());
    assert!(
        !dest_dir.join("file2.txt").exists(),
        "extra file should be deleted"
    );
}

#[test]
fn delete_during_removes_extra_files() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    fs::write(src_dir.join("keep.txt"), b"content").unwrap();
    fs::write(dest_dir.join("keep.txt"), b"old").unwrap();
    fs::write(dest_dir.join("remove.txt"), b"extra").unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        "--delete-during",
        &format!("{}/", src_dir.display()),
        &format!("{}/", dest_dir.display()),
    ]);
    cmd.assert_success();

    assert!(dest_dir.join("keep.txt").exists());
    assert!(!dest_dir.join("remove.txt").exists());
}

#[test]
fn delete_after_removes_extra_files() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    fs::write(src_dir.join("file.txt"), b"new").unwrap();
    fs::write(dest_dir.join("file.txt"), b"old").unwrap();
    fs::write(dest_dir.join("extra.txt"), b"remove").unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        "--delete-after",
        &format!("{}/", src_dir.display()),
        &format!("{}/", dest_dir.display()),
    ]);
    cmd.assert_success();

    assert!(dest_dir.join("file.txt").exists());
    assert!(!dest_dir.join("extra.txt").exists());
}

#[test]
fn delete_delay_removes_extra_files() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    fs::write(src_dir.join("a.txt"), b"a").unwrap();
    fs::write(dest_dir.join("a.txt"), b"a_old").unwrap();
    fs::write(dest_dir.join("b.txt"), b"b_extra").unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        "--delete-delay",
        &format!("{}/", src_dir.display()),
        &format!("{}/", dest_dir.display()),
    ]);
    cmd.assert_success();

    assert!(dest_dir.join("a.txt").exists());
    assert!(!dest_dir.join("b.txt").exists());
}

#[test]
fn delete_with_trailing_slash_works_without_recursive() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    fs::write(src_dir.join("file.txt"), b"content").unwrap();
    fs::write(dest_dir.join("extra.txt"), b"extra").unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--delete",
        "--dirs", // Mirror upstream: --delete requires --recursive or --dirs
        &format!("{}/", src_dir.display()),
        &format!("{}/", dest_dir.display()),
    ]);

    // Delete with --dirs works with trailing slashes
    cmd.assert_success();

    assert!(dest_dir.join("file.txt").exists());
    assert!(
        !dest_dir.join("extra.txt").exists(),
        "extra file should be deleted"
    );
}

#[test]
fn delete_preserves_files_in_subdirectories() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    fs::create_dir(src_dir.join("subdir")).unwrap();
    fs::write(src_dir.join("subdir/keep.txt"), b"keep").unwrap();

    fs::create_dir(dest_dir.join("subdir")).unwrap();
    fs::write(dest_dir.join("subdir/keep.txt"), b"old").unwrap();
    fs::write(dest_dir.join("subdir/remove.txt"), b"extra").unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        "--delete",
        &format!("{}/", src_dir.display()),
        &format!("{}/", dest_dir.display()),
    ]);
    cmd.assert_success();

    assert!(dest_dir.join("subdir/keep.txt").exists());
    assert!(!dest_dir.join("subdir/remove.txt").exists());
}

// ============================================================================
// Delete with Filters Tests
// ============================================================================

#[test]
fn delete_excluded_removes_filtered_files() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Source has .txt files only
    fs::write(src_dir.join("keep.txt"), b"text").unwrap();
    fs::write(src_dir.join("data.txt"), b"data").unwrap();

    // Dest has .txt and .log files
    fs::write(dest_dir.join("keep.txt"), b"old").unwrap();
    fs::write(dest_dir.join("data.txt"), b"old").unwrap();
    fs::write(dest_dir.join("debug.log"), b"log").unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        "--delete",
        "--delete-excluded",
        "--exclude=*.log",
        &format!("{}/", src_dir.display()),
        &format!("{}/", dest_dir.display()),
    ]);
    cmd.assert_success();

    assert!(dest_dir.join("keep.txt").exists());
    assert!(dest_dir.join("data.txt").exists());
    assert!(
        !dest_dir.join("debug.log").exists(),
        "excluded files should be deleted"
    );
}

// ============================================================================
// Max Delete Tests
// ============================================================================

#[test]
fn max_delete_limits_deletions() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Source is empty
    // Dest has 3 files
    fs::write(dest_dir.join("file1.txt"), b"1").unwrap();
    fs::write(dest_dir.join("file2.txt"), b"2").unwrap();
    fs::write(dest_dir.join("file3.txt"), b"3").unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        "--delete",
        "--max-delete=1",
        &format!("{}/", src_dir.display()),
        &format!("{}/", dest_dir.display()),
    ]);

    // Should fail or warn when trying to delete more than max
    let _output = cmd.run().unwrap();

    // With max-delete exceeded, rsync typically exits with error
    // At least some files should remain
    let remaining = dest_dir.join("file1.txt").exists() as u8
        + dest_dir.join("file2.txt").exists() as u8
        + dest_dir.join("file3.txt").exists() as u8;

    assert!(
        remaining >= 2,
        "max-delete should prevent deleting all files"
    );
}

#[test]
fn max_delete_zero_allows_no_deletions() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    fs::write(src_dir.join("file.txt"), b"new").unwrap();
    fs::write(dest_dir.join("file.txt"), b"old").unwrap();
    fs::write(dest_dir.join("extra.txt"), b"extra").unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        "--delete",
        "--max-delete=0",
        &format!("{}/", src_dir.display()),
        &format!("{}/", dest_dir.display()),
    ]);

    let _output = cmd.run().unwrap();

    // Should prevent deletions
    assert!(
        dest_dir.join("extra.txt").exists(),
        "max-delete=0 should prevent all deletions"
    );
}

// ============================================================================
// Backup Tests
// ============================================================================

#[test]
fn backup_creates_backup_with_default_suffix() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    fs::write(src_dir.join("file.txt"), b"new content").unwrap();
    fs::write(dest_dir.join("file.txt"), b"old content").unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--backup",
        src_dir.join("file.txt").to_str().unwrap(),
        dest_dir.to_str().unwrap(),
    ]);
    cmd.assert_success();

    // Original file should have new content
    let content = fs::read(dest_dir.join("file.txt")).unwrap();
    assert_eq!(content, b"new content");

    // Backup should exist with default suffix (~)
    assert!(
        dest_dir.join("file.txt~").exists(),
        "backup file should exist with ~ suffix"
    );
    let backup_content = fs::read(dest_dir.join("file.txt~")).unwrap();
    assert_eq!(backup_content, b"old content");
}

#[test]
fn backup_with_custom_suffix() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    fs::write(src_dir.join("data.txt"), b"new").unwrap();
    fs::write(dest_dir.join("data.txt"), b"old").unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--backup",
        "--suffix=.bak",
        src_dir.join("data.txt").to_str().unwrap(),
        dest_dir.to_str().unwrap(),
    ]);
    cmd.assert_success();

    assert!(dest_dir.join("data.txt.bak").exists());
    let backup_content = fs::read(dest_dir.join("data.txt.bak")).unwrap();
    assert_eq!(backup_content, b"old");
}

#[test]
fn backup_dir_creates_backups_in_separate_directory() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();
    let backup_dir = test_dir.mkdir("backups").unwrap();

    fs::write(src_dir.join("file.txt"), b"new").unwrap();
    fs::write(dest_dir.join("file.txt"), b"old").unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        &format!("--backup-dir={}", backup_dir.display()),
        "--suffix=",
        src_dir.join("file.txt").to_str().unwrap(),
        dest_dir.to_str().unwrap(),
    ]);
    cmd.assert_success();

    // Original file updated
    let content = fs::read(dest_dir.join("file.txt")).unwrap();
    assert_eq!(content, b"new");

    // Backup in separate directory (no suffix with --suffix=)
    assert!(backup_dir.join("file.txt").exists());
    let backup_content = fs::read(backup_dir.join("file.txt")).unwrap();
    assert_eq!(backup_content, b"old");
}

#[test]
fn backup_dir_with_suffix() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();
    let backup_dir = test_dir.mkdir("backups").unwrap();

    fs::write(src_dir.join("doc.txt"), b"version2").unwrap();
    fs::write(dest_dir.join("doc.txt"), b"version1").unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        &format!("--backup-dir={}", backup_dir.display()),
        "--suffix=.v1",
        src_dir.join("doc.txt").to_str().unwrap(),
        dest_dir.to_str().unwrap(),
    ]);
    cmd.assert_success();

    assert!(backup_dir.join("doc.txt.v1").exists());
    let backup = fs::read(backup_dir.join("doc.txt.v1")).unwrap();
    assert_eq!(backup, b"version1");
}

#[test]
fn backup_recursive_preserves_directory_structure() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    fs::create_dir(src_dir.join("subdir")).unwrap();
    fs::write(src_dir.join("subdir/file.txt"), b"new").unwrap();

    fs::create_dir(dest_dir.join("subdir")).unwrap();
    fs::write(dest_dir.join("subdir/file.txt"), b"old").unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        "--backup",
        &format!("{}/", src_dir.display()),
        &format!("{}/", dest_dir.display()),
    ]);
    cmd.assert_success();

    assert!(dest_dir.join("subdir/file.txt~").exists());
    let backup = fs::read(dest_dir.join("subdir/file.txt~")).unwrap();
    assert_eq!(backup, b"old");
}

// ============================================================================
// Delete + Backup Interaction Tests
// ============================================================================

#[test]
fn delete_with_backup_backs_up_deleted_files() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    fs::write(src_dir.join("keep.txt"), b"keep").unwrap();
    fs::write(dest_dir.join("keep.txt"), b"old_keep").unwrap();
    fs::write(dest_dir.join("remove.txt"), b"to_remove").unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        "--delete",
        "--backup",
        &format!("{}/", src_dir.display()),
        &format!("{}/", dest_dir.display()),
    ]);
    cmd.assert_success();

    // File should be deleted
    assert!(!dest_dir.join("remove.txt").exists());

    // But backup should exist
    assert!(dest_dir.join("remove.txt~").exists());
    let backup = fs::read(dest_dir.join("remove.txt~")).unwrap();
    assert_eq!(backup, b"to_remove");
}

#[test]
fn delete_with_backup_dir_organizes_deletions() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();
    let backup_dir = test_dir.mkdir("backups").unwrap();

    fs::write(src_dir.join("a.txt"), b"a").unwrap();
    fs::write(dest_dir.join("a.txt"), b"a_old").unwrap();
    fs::write(dest_dir.join("b.txt"), b"b_delete").unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        "--delete",
        &format!("--backup-dir={}", backup_dir.display()),
        "--suffix=",
        &format!("{}/", src_dir.display()),
        &format!("{}/", dest_dir.display()),
    ]);
    cmd.assert_success();

    // Deleted file not in dest
    assert!(!dest_dir.join("b.txt").exists());

    // But preserved in backup directory (no suffix with --suffix=)
    assert!(backup_dir.join("b.txt").exists());
    let backup = fs::read(backup_dir.join("b.txt")).unwrap();
    assert_eq!(backup, b"b_delete");
}

// ============================================================================
// Additional Edge Cases
// ============================================================================

#[test]
fn delete_dry_run_shows_deletions_without_removing() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    fs::write(src_dir.join("keep.txt"), b"keep").unwrap();
    fs::write(dest_dir.join("keep.txt"), b"old").unwrap();
    fs::write(dest_dir.join("remove.txt"), b"extra").unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        "--delete",
        "--dry-run",
        &format!("{}/", src_dir.display()),
        &format!("{}/", dest_dir.display()),
    ]);
    cmd.assert_success();

    // Dry run should not actually delete
    assert!(
        dest_dir.join("remove.txt").exists(),
        "dry-run should not delete files"
    );
}

#[test]
fn backup_nested_directory_structure() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();
    let backup_dir = test_dir.mkdir("backups").unwrap();

    fs::create_dir_all(src_dir.join("a/b/c")).unwrap();
    fs::write(src_dir.join("a/b/c/file.txt"), b"new").unwrap();

    fs::create_dir_all(dest_dir.join("a/b/c")).unwrap();
    fs::write(dest_dir.join("a/b/c/file.txt"), b"old").unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        &format!("--backup-dir={}", backup_dir.display()),
        "--suffix=",
        &format!("{}/", src_dir.display()),
        &format!("{}/", dest_dir.display()),
    ]);
    cmd.assert_success();

    // Backup should preserve directory structure
    assert!(backup_dir.join("a/b/c/file.txt").exists());
    let backup = fs::read(backup_dir.join("a/b/c/file.txt")).unwrap();
    assert_eq!(backup, b"old");
}

#[test]
fn delete_empty_directories() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    fs::write(src_dir.join("file.txt"), b"content").unwrap();

    fs::write(dest_dir.join("file.txt"), b"old").unwrap();
    fs::create_dir(dest_dir.join("empty_dir")).unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        "--delete",
        &format!("{}/", src_dir.display()),
        &format!("{}/", dest_dir.display()),
    ]);
    cmd.assert_success();

    // Empty directory should be deleted
    assert!(!dest_dir.join("empty_dir").exists());
}

#[test]
fn backup_only_on_actual_changes() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Same content in source and dest
    fs::write(src_dir.join("file.txt"), b"same content").unwrap();
    fs::write(dest_dir.join("file.txt"), b"same content").unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--backup",
        src_dir.join("file.txt").to_str().unwrap(),
        dest_dir.to_str().unwrap(),
    ]);
    cmd.assert_success();

    // No backup should be created if content is identical
    assert!(!dest_dir.join("file.txt~").exists());
}
