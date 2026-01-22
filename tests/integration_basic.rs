//! Basic file operation integration tests.
//!
//! Tests core file copying scenarios through the CLI.

mod integration;

use filetime::{FileTime, set_file_times};
use integration::helpers::*;
use std::fs;

// ============================================================================
// Single File Operations
// ============================================================================

#[test]
fn copy_single_file_to_directory() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    fs::write(src_dir.join("file.txt"), b"hello world").unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        src_dir.join("file.txt").to_str().unwrap(),
        dest_dir.to_str().unwrap(),
    ]);
    cmd.assert_success();

    let dest_content = fs::read(dest_dir.join("file.txt")).unwrap();
    assert_eq!(dest_content, b"hello world");
}

#[test]
fn copy_single_file_with_rename() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_file = test_dir.write_file("source.txt", b"content").unwrap();
    let dest_file = test_dir.path().join("destination.txt");

    let mut cmd = RsyncCommand::new();
    cmd.args([src_file.to_str().unwrap(), dest_file.to_str().unwrap()]);
    cmd.assert_success();

    let content = fs::read(&dest_file).unwrap();
    assert_eq!(content, b"content");
}

#[test]
fn copy_multiple_files_to_directory() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    fs::write(src_dir.join("file1.txt"), b"one").unwrap();
    fs::write(src_dir.join("file2.txt"), b"two").unwrap();
    fs::write(src_dir.join("file3.txt"), b"three").unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        src_dir.join("file1.txt").to_str().unwrap(),
        src_dir.join("file2.txt").to_str().unwrap(),
        src_dir.join("file3.txt").to_str().unwrap(),
        dest_dir.to_str().unwrap(),
    ]);
    cmd.assert_success();

    assert_eq!(fs::read(dest_dir.join("file1.txt")).unwrap(), b"one");
    assert_eq!(fs::read(dest_dir.join("file2.txt")).unwrap(), b"two");
    assert_eq!(fs::read(dest_dir.join("file3.txt")).unwrap(), b"three");
}

#[test]
fn overwrite_existing_file() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_file = test_dir.write_file("source.txt", b"new content").unwrap();
    let dest_file = test_dir.write_file("dest.txt", b"old content").unwrap();

    // Make destination explicitly older so rsync will transfer
    // Both files have same size (11 bytes), so rsync uses mtime to decide
    let old_time = FileTime::from_unix_time(1600000000, 0);
    set_file_times(&dest_file, old_time, old_time).unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([src_file.to_str().unwrap(), dest_file.to_str().unwrap()]);
    cmd.assert_success();

    let content = fs::read(&dest_file).unwrap();
    assert_eq!(content, b"new content");
}

#[test]
fn skip_unchanged_files_with_checksum() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_file = test_dir.write_file("source.txt", b"same content").unwrap();
    let dest_file = test_dir.write_file("dest.txt", b"same content").unwrap();

    // Get original mtime
    let original_mtime = fs::metadata(&dest_file).unwrap().modified().unwrap();

    // Sleep briefly to ensure mtime would change if file is written
    std::thread::sleep(std::time::Duration::from_millis(10));

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--checksum",
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    cmd.assert_success();

    // File should not be modified (mtime unchanged)
    let new_mtime = fs::metadata(&dest_file).unwrap().modified().unwrap();
    assert_eq!(
        original_mtime, new_mtime,
        "File should not be modified when content is identical"
    );
}

// ============================================================================
// Directory Operations
// ============================================================================

#[test]
fn copy_empty_directory_recursive() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        &format!("{}/", src_dir.display()),
        dest_dir.to_str().unwrap(),
    ]);
    cmd.assert_success();

    assert!(dest_dir.exists());
}

#[test]
fn copy_directory_with_files_recursive() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    fs::write(src_dir.join("file1.txt"), b"content1").unwrap();
    fs::write(src_dir.join("file2.txt"), b"content2").unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        &format!("{}/", src_dir.display()),
        dest_dir.to_str().unwrap(),
    ]);
    cmd.assert_success();

    assert_eq!(fs::read(dest_dir.join("file1.txt")).unwrap(), b"content1");
    assert_eq!(fs::read(dest_dir.join("file2.txt")).unwrap(), b"content2");
}

#[test]
fn copy_nested_directories_recursive() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    fs::create_dir(src_dir.join("subdir")).unwrap();
    fs::write(src_dir.join("subdir/file.txt"), b"nested").unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r",
        &format!("{}/", src_dir.display()),
        dest_dir.to_str().unwrap(),
    ]);
    cmd.assert_success();

    assert_eq!(
        fs::read(dest_dir.join("subdir/file.txt")).unwrap(),
        b"nested"
    );
}

#[test]
fn recursive_required_for_directory_contents() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    fs::create_dir(src_dir.join("subdir")).unwrap();
    fs::write(src_dir.join("subdir/file.txt"), b"content").unwrap();

    // Without -r, only the directory itself is created, not contents
    let mut cmd = RsyncCommand::new();
    cmd.args([src_dir.to_str().unwrap(), dest_dir.to_str().unwrap()]);

    // This may succeed or fail depending on implementation
    // The key point is nested contents should not be copied
    let _ = cmd.run();

    // Nested file should not exist (no recursion)
    assert!(!dest_dir.join("subdir/file.txt").exists());
}

// ============================================================================
// Update Operations
// ============================================================================

#[test]
fn update_flag_skips_newer_files() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_file = test_dir.write_file("source.txt", b"old").unwrap();
    let dest_file = test_dir.write_file("dest.txt", b"new").unwrap();

    // Make dest newer than source
    let src_mtime = fs::metadata(&src_file).unwrap().modified().unwrap();
    let newer_time = src_mtime + std::time::Duration::from_secs(10);
    filetime::set_file_mtime(&dest_file, filetime::FileTime::from_system_time(newer_time)).unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--update",
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    cmd.assert_success();

    // Dest should remain unchanged (it's newer)
    let content = fs::read(&dest_file).unwrap();
    assert_eq!(content, b"new");
}

#[test]
fn update_flag_transfers_when_source_newer() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_file = test_dir.write_file("source.txt", b"new").unwrap();
    let dest_file = test_dir.write_file("dest.txt", b"old").unwrap();

    // Make source newer than dest
    let src_mtime = fs::metadata(&src_file).unwrap().modified().unwrap();
    let older_time = src_mtime - std::time::Duration::from_secs(10);
    filetime::set_file_mtime(&dest_file, filetime::FileTime::from_system_time(older_time)).unwrap();

    std::thread::sleep(std::time::Duration::from_millis(10));

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--update",
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    cmd.assert_success();

    // Dest should be updated (source is newer)
    let content = fs::read(&dest_file).unwrap();
    assert_eq!(content, b"new");
}

// ============================================================================
// Metadata Preservation
// ============================================================================

#[test]
fn preserve_modification_times_with_times_flag() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_file = test_dir.write_file("source.txt", b"content").unwrap();

    // Set a specific mtime on source
    let target_time = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
    filetime::set_file_mtime(&src_file, filetime::FileTime::from_system_time(target_time)).unwrap();

    let dest_file = test_dir.path().join("dest.txt");

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--times",
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    cmd.assert_success();

    let src_mtime = fs::metadata(&src_file).unwrap().modified().unwrap();
    let dest_mtime = fs::metadata(&dest_file).unwrap().modified().unwrap();

    // Allow 1 second tolerance for filesystem precision
    let diff = if src_mtime > dest_mtime {
        src_mtime.duration_since(dest_mtime).unwrap()
    } else {
        dest_mtime.duration_since(src_mtime).unwrap()
    };

    assert!(
        diff < std::time::Duration::from_secs(2),
        "Modification time should be preserved (diff: {diff:?})"
    );
}

#[test]
#[cfg(unix)]
fn preserve_permissions_with_perms_flag() {
    use std::os::unix::fs::PermissionsExt;

    let test_dir = TestDir::new().expect("create test dir");
    let src_file = test_dir.write_file("source.txt", b"content").unwrap();

    // Set specific permissions
    fs::set_permissions(&src_file, fs::Permissions::from_mode(0o644)).unwrap();

    let dest_file = test_dir.path().join("dest.txt");

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--perms",
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    cmd.assert_success();

    let src_perms = fs::metadata(&src_file).unwrap().permissions().mode() & 0o777;
    let dest_perms = fs::metadata(&dest_file).unwrap().permissions().mode() & 0o777;

    assert_eq!(src_perms, dest_perms, "Permissions should be preserved");
}

// ============================================================================
// Archive Mode
// ============================================================================

#[test]
#[cfg(unix)]
fn archive_mode_preserves_attributes() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    fs::write(src_dir.join("file.txt"), b"content").unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-a",
        &format!("{}/", src_dir.display()),
        dest_dir.to_str().unwrap(),
    ]);
    cmd.assert_success();

    assert_eq!(fs::read(dest_dir.join("file.txt")).unwrap(), b"content");

    // Verify mtime is preserved (archive implies --times)
    let src_mtime = fs::metadata(src_dir.join("file.txt"))
        .unwrap()
        .modified()
        .unwrap();
    let dest_mtime = fs::metadata(dest_dir.join("file.txt"))
        .unwrap()
        .modified()
        .unwrap();

    let diff = if src_mtime > dest_mtime {
        src_mtime.duration_since(dest_mtime).unwrap()
    } else {
        dest_mtime.duration_since(src_mtime).unwrap()
    };

    assert!(
        diff < std::time::Duration::from_secs(2),
        "Archive mode should preserve modification times"
    );
}

#[test]
#[cfg(unix)]
fn archive_mode_recursive_by_default() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    fs::create_dir(src_dir.join("subdir")).unwrap();
    fs::write(src_dir.join("subdir/file.txt"), b"nested").unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-a",
        &format!("{}/", src_dir.display()),
        dest_dir.to_str().unwrap(),
    ]);
    cmd.assert_success();

    assert_eq!(
        fs::read(dest_dir.join("subdir/file.txt")).unwrap(),
        b"nested",
        "Archive mode should be recursive"
    );
}

// ============================================================================
// Dry Run Mode
// ============================================================================

#[test]
fn dry_run_shows_changes_without_modifying() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_file = test_dir.write_file("source.txt", b"new content").unwrap();
    let dest_file = test_dir.path().join("dest.txt");

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--dry-run",
        "-v",
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    let output = cmd.assert_success();

    // File should not actually be created
    assert!(
        !dest_file.exists(),
        "Dry run should not create destination file"
    );

    // Output should mention the file
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("source.txt") || stdout.contains("dest.txt"),
        "Dry run should show what would be transferred"
    );
}

// ============================================================================
// Error Cases
// ============================================================================

#[test]
fn error_on_nonexistent_source() {
    let test_dir = TestDir::new().expect("create test dir");
    let nonexistent = test_dir.path().join("does_not_exist.txt");
    let dest = test_dir.path().join("dest.txt");

    let mut cmd = RsyncCommand::new();
    cmd.args([nonexistent.to_str().unwrap(), dest.to_str().unwrap()]);
    cmd.assert_failure();
}

#[test]
fn create_missing_destination_directory() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_file = test_dir.write_file("source.txt", b"content").unwrap();
    let dest_file = test_dir.path().join("newdir/dest.txt");

    let mut cmd = RsyncCommand::new();
    cmd.args([src_file.to_str().unwrap(), dest_file.to_str().unwrap()]);
    cmd.assert_success();

    assert_eq!(fs::read(&dest_file).unwrap(), b"content");
}

// ============================================================================
// Size-based Operations
// ============================================================================

#[test]
fn size_only_skips_files_with_same_size() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_file = test_dir.write_file("source.txt", b"12345").unwrap();
    let dest_file = test_dir.write_file("dest.txt", b"abcde").unwrap();

    // Same size (5 bytes) but different content
    let dest_mtime_before = fs::metadata(&dest_file).unwrap().modified().unwrap();

    std::thread::sleep(std::time::Duration::from_millis(10));

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--size-only",
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    cmd.assert_success();

    // Dest should not be modified (same size)
    let dest_mtime_after = fs::metadata(&dest_file).unwrap().modified().unwrap();
    assert_eq!(
        dest_mtime_before, dest_mtime_after,
        "File should not be modified when using --size-only with same size"
    );
    assert_eq!(fs::read(&dest_file).unwrap(), b"abcde");
}

#[test]
fn size_only_transfers_files_with_different_size() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_file = test_dir
        .write_file("source.txt", b"longer content")
        .unwrap();
    let dest_file = test_dir.write_file("dest.txt", b"short").unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--size-only",
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    cmd.assert_success();

    // Dest should be updated (different size)
    assert_eq!(fs::read(&dest_file).unwrap(), b"longer content");
}

// ============================================================================
// Ignore Existing
// ============================================================================

#[test]
fn ignore_existing_skips_existing_files() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_file = test_dir.write_file("source.txt", b"new content").unwrap();
    let dest_file = test_dir.write_file("dest.txt", b"old content").unwrap();

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--ignore-existing",
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    cmd.assert_success();

    // Dest should not be modified
    assert_eq!(fs::read(&dest_file).unwrap(), b"old content");
}

#[test]
fn ignore_existing_transfers_missing_files() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_file = test_dir.write_file("source.txt", b"content").unwrap();
    let dest_file = test_dir.path().join("dest.txt");

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "--ignore-existing",
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    cmd.assert_success();

    // New file should be created
    assert_eq!(fs::read(&dest_file).unwrap(), b"content");
}

// ============================================================================
// Verbose Output
// ============================================================================

#[test]
fn verbose_shows_transferred_files() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_file = test_dir.write_file("test_file.txt", b"content").unwrap();
    let dest_file = test_dir.path().join("dest.txt");

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-v",
        src_file.to_str().unwrap(),
        dest_file.to_str().unwrap(),
    ]);
    let output = cmd.assert_success();

    let stdout = String::from_utf8_lossy(&output.stdout);
    // Should mention the file in output
    assert!(
        stdout.contains("test_file") || stdout.contains("dest"),
        "Verbose output should mention transferred files"
    );
}
