/// Integration tests for error recovery scenarios during local transfers.
///
/// These tests verify that oc-rsync continues transferring remaining files
/// when individual files or directories encounter errors, matching upstream
/// rsync's resilient behaviour. Each test sets up a scenario where some
/// entries are inaccessible or problematic and asserts that:
///   - Accessible files are still transferred correctly.
///   - The process exits with an appropriate non-zero code.
///   - Errors appear in stderr when expected.
///
/// References:
/// - upstream: main.c - io_error to exit code mapping
/// - upstream: flist.c - permission errors during file list build
/// - upstream: generator.c:recv_generator() - error handling per entry
/// - upstream: rsync.h - IOERR_GENERAL, RERR_PARTIAL (23), RERR_VANISHED (24)
use super::common::*;
use super::*;

/// When a subdirectory inside a recursive transfer is unreadable (mode 0o000),
/// the transfer should still copy all accessible files and report a non-zero
/// exit code. Upstream rsync produces exit code 23 (partial transfer) for
/// permission errors during readdir.
///
/// upstream: flist.c - "opendir failed" warning + IOERR_GENERAL
#[cfg(unix)]
#[test]
fn unreadable_subdirectory_continues_transfer_of_remaining_files() {
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let src = tmp.path().join("src");
    let dst = tmp.path().join("dst");

    std::fs::create_dir_all(src.join("accessible")).expect("create accessible dir");
    std::fs::create_dir_all(src.join("blocked")).expect("create blocked dir");

    std::fs::write(src.join("top.txt"), b"top-level file").expect("write top");
    std::fs::write(src.join("accessible/inner.txt"), b"inner file").expect("write inner");
    std::fs::write(src.join("blocked/secret.txt"), b"secret file").expect("write secret");

    // Remove read+execute from the blocked directory so readdir fails with EACCES
    let mut perms = std::fs::metadata(src.join("blocked"))
        .expect("blocked metadata")
        .permissions();
    perms.set_mode(0o000);
    std::fs::set_permissions(src.join("blocked"), perms.clone()).expect("chmod blocked");

    let mut src_trailing = src.clone().into_os_string();
    src_trailing.push("/");

    let (code, _stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-r"),
        src_trailing,
        dst.clone().into_os_string(),
    ]);

    // Restore permissions so tempdir cleanup can remove it
    perms.set_mode(0o755);
    let _ = std::fs::set_permissions(src.join("blocked"), perms);

    // Accessible files should have been transferred
    assert_eq!(
        std::fs::read(dst.join("top.txt")).expect("read top"),
        b"top-level file",
        "top.txt should be transferred despite blocked sibling"
    );
    assert_eq!(
        std::fs::read(dst.join("accessible/inner.txt")).expect("read inner"),
        b"inner file",
        "accessible/inner.txt should be transferred"
    );

    // The blocked directory's contents should not appear at the destination
    assert!(
        !dst.join("blocked/secret.txt").exists(),
        "secret.txt inside blocked dir should not be transferred"
    );

    // Upstream rsync returns 23 (partial transfer) for permission errors
    let stderr_text = String::from_utf8_lossy(&stderr);
    assert!(
        code != 0,
        "expected non-zero exit code due to unreadable directory, got {code}: {stderr_text}"
    );
}

/// When a single source file is unreadable (mode 0o000), the transfer should
/// skip it, copy the remaining files, and exit with a non-zero code.
///
/// upstream: sender.c - "send_files failed to open" + IOERR_GENERAL
#[cfg(unix)]
#[test]
fn unreadable_source_file_skipped_remaining_files_transfer() {
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let src = tmp.path().join("src");
    let dst = tmp.path().join("dst");

    std::fs::create_dir(&src).expect("create src");

    std::fs::write(src.join("readable.txt"), b"readable content").expect("write readable");
    std::fs::write(src.join("blocked.txt"), b"blocked content").expect("write blocked");
    std::fs::write(src.join("also_ok.txt"), b"also ok content").expect("write also_ok");

    // Make one file completely unreadable
    let mut perms = std::fs::metadata(src.join("blocked.txt"))
        .expect("blocked metadata")
        .permissions();
    perms.set_mode(0o000);
    std::fs::set_permissions(src.join("blocked.txt"), perms.clone()).expect("chmod blocked");

    let mut src_trailing = src.clone().into_os_string();
    src_trailing.push("/");

    let (code, _stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-r"),
        src_trailing,
        dst.clone().into_os_string(),
    ]);

    // Restore permissions for cleanup
    perms.set_mode(0o644);
    let _ = std::fs::set_permissions(src.join("blocked.txt"), perms);

    // Readable files should have been transferred successfully
    assert_eq!(
        std::fs::read(dst.join("readable.txt")).expect("read readable"),
        b"readable content",
        "readable.txt should be at destination"
    );
    assert_eq!(
        std::fs::read(dst.join("also_ok.txt")).expect("read also_ok"),
        b"also ok content",
        "also_ok.txt should be at destination"
    );

    let stderr_text = String::from_utf8_lossy(&stderr);
    assert!(
        code != 0,
        "expected non-zero exit code for unreadable file, got {code}: {stderr_text}"
    );
}

/// When the destination directory is read-only, writes should fail and the
/// transfer should report an appropriate error. Upstream rsync returns exit
/// code 23 (partial transfer) when the destination cannot be written.
///
/// upstream: receiver.c - "mkstemp failed" / "Permission denied"
#[cfg(unix)]
#[test]
fn read_only_destination_reports_error() {
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let src = tmp.path().join("src");
    let dst = tmp.path().join("dst");

    std::fs::create_dir(&src).expect("create src");
    std::fs::create_dir(&dst).expect("create dst");

    std::fs::write(src.join("file.txt"), b"some data").expect("write file");

    // Make destination directory read-only so file creation fails
    let mut perms = std::fs::metadata(&dst).expect("dst metadata").permissions();
    perms.set_mode(0o555);
    std::fs::set_permissions(&dst, perms.clone()).expect("chmod dst");

    let (code, _stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        src.join("file.txt").into_os_string(),
        dst.clone().into_os_string(),
    ]);

    // Restore permissions for cleanup
    perms.set_mode(0o755);
    let _ = std::fs::set_permissions(&dst, perms);

    let stderr_text = String::from_utf8_lossy(&stderr);
    assert!(
        code != 0,
        "expected non-zero exit code for read-only destination, got {code}: {stderr_text}"
    );
}

/// Dangling symlinks (symlinks whose target does not exist) should be
/// preserved when using --links (-l). Upstream rsync copies the symlink
/// value as-is regardless of whether the target exists.
///
/// upstream: flist.c - readlink() captures the link value, not the target
#[cfg(unix)]
#[test]
fn dangling_symlink_preserved_with_links_flag() {
    use std::os::unix::fs::symlink;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let src = tmp.path().join("src");
    let dst = tmp.path().join("dst");

    std::fs::create_dir(&src).expect("create src");

    // Create a symlink pointing to a non-existent target
    symlink("nonexistent_target", src.join("dangling")).expect("create dangling symlink");

    // Also create a regular file so we verify the transfer does useful work
    std::fs::write(src.join("regular.txt"), b"regular content").expect("write regular");

    let mut src_trailing = src.clone().into_os_string();
    src_trailing.push("/");

    let (code, _stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-rl"),
        src_trailing,
        dst.clone().into_os_string(),
    ]);

    let stderr_text = String::from_utf8_lossy(&stderr);
    assert_eq!(
        code, 0,
        "dangling symlink transfer should succeed with exit 0: {stderr_text}"
    );

    // The dangling symlink should be preserved at the destination
    let dest_link = dst.join("dangling");
    let link_meta = std::fs::symlink_metadata(&dest_link).expect("dangling link metadata");
    assert!(
        link_meta.file_type().is_symlink(),
        "dangling should be a symlink at destination"
    );

    let target = std::fs::read_link(&dest_link).expect("read dangling link target");
    assert_eq!(
        target.to_string_lossy(),
        "nonexistent_target",
        "dangling symlink should preserve original target path"
    );

    // Regular file should also be transferred
    assert_eq!(
        std::fs::read(dst.join("regular.txt")).expect("read regular"),
        b"regular content",
        "regular.txt should be transferred alongside dangling symlink"
    );
}

/// Multiple dangling symlinks in a directory should all be preserved.
/// This exercises the batch handling path for symlinks.
#[cfg(unix)]
#[test]
fn multiple_dangling_symlinks_all_preserved() {
    use std::os::unix::fs::symlink;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let src = tmp.path().join("src");
    let dst = tmp.path().join("dst");

    std::fs::create_dir(&src).expect("create src");

    symlink("missing_a", src.join("link_a")).expect("create link_a");
    symlink("missing_b", src.join("link_b")).expect("create link_b");
    symlink("/absolute/missing", src.join("link_abs")).expect("create link_abs");

    let mut src_trailing = src.clone().into_os_string();
    src_trailing.push("/");

    let (code, _stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-rl"),
        src_trailing,
        dst.clone().into_os_string(),
    ]);

    let stderr_text = String::from_utf8_lossy(&stderr);
    assert_eq!(
        code, 0,
        "multiple dangling symlinks should transfer successfully: {stderr_text}"
    );

    for (name, expected_target) in [
        ("link_a", "missing_a"),
        ("link_b", "missing_b"),
        ("link_abs", "/absolute/missing"),
    ] {
        let dest_link = dst.join(name);
        let meta = std::fs::symlink_metadata(&dest_link)
            .unwrap_or_else(|e| panic!("{name} should exist at destination: {e}"));
        assert!(meta.file_type().is_symlink(), "{name} should be a symlink");
        let target =
            std::fs::read_link(&dest_link).unwrap_or_else(|e| panic!("read_link {name}: {e}"));
        assert_eq!(
            target.to_string_lossy(),
            expected_target,
            "{name} should point to {expected_target}"
        );
    }
}

/// When a source file is replaced by a larger version between the initial
/// sync and a re-sync (simulating growth), the transfer should succeed and
/// copy the new content. This verifies the delta algorithm handles size
/// increases gracefully.
///
/// upstream: match.c - mismatched file end handled by literal data emission
#[cfg(unix)]
#[test]
fn file_grows_between_syncs_transfers_updated_content() {
    use filetime::{FileTime, set_file_times};
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let src = tmp.path().join("src");
    let dst = tmp.path().join("dst");

    std::fs::create_dir(&src).expect("create src");

    // Initial content
    let original = b"original content here";
    std::fs::write(src.join("growing.txt"), original).expect("write original");

    let mut src_trailing = src.clone().into_os_string();
    src_trailing.push("/");

    // First sync to establish baseline at destination
    let (code, _stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-r"),
        src_trailing.clone(),
        dst.clone().into_os_string(),
    ]);
    assert_eq!(
        code,
        0,
        "initial sync should succeed: {}",
        String::from_utf8_lossy(&stderr)
    );
    assert_eq!(
        std::fs::read(dst.join("growing.txt")).expect("read initial"),
        original
    );

    // Grow the file significantly (more than double) and update mtime to
    // defeat the quick-check algorithm
    let grown = b"original content here - now with a much larger payload appended to simulate file growth during an active transfer";
    std::fs::write(src.join("growing.txt"), grown).expect("write grown");
    let new_mtime = FileTime::from_unix_time(1_800_000_000, 0);
    set_file_times(src.join("growing.txt"), new_mtime, new_mtime).expect("set grown mtime");

    // Re-sync: the destination has the old (smaller) version
    let (code, _stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-r"),
        src_trailing,
        dst.clone().into_os_string(),
    ]);

    let stderr_text = String::from_utf8_lossy(&stderr);
    assert_eq!(
        code, 0,
        "re-sync with grown file should succeed: {stderr_text}"
    );

    assert_eq!(
        std::fs::read(dst.join("growing.txt")).expect("read grown"),
        grown,
        "destination should have the grown file content"
    );
}

/// When a source file shrinks between syncs, the transfer should succeed and
/// copy the new (smaller) content. The delta algorithm must handle the basis
/// file being larger than the new source.
///
/// upstream: match.c - basis file larger than source handled by shorter
///           literal emission
#[cfg(unix)]
#[test]
fn file_shrinks_between_syncs_transfers_updated_content() {
    use filetime::{FileTime, set_file_times};
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let src = tmp.path().join("src");
    let dst = tmp.path().join("dst");

    std::fs::create_dir(&src).expect("create src");

    // Initial large content
    let large = b"this is a substantially longer piece of content that will later be replaced by something much shorter to test shrink handling";
    std::fs::write(src.join("shrinking.txt"), large).expect("write large");

    let mut src_trailing = src.clone().into_os_string();
    src_trailing.push("/");

    // First sync
    let (code, _stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-r"),
        src_trailing.clone(),
        dst.clone().into_os_string(),
    ]);
    assert_eq!(
        code,
        0,
        "initial sync should succeed: {}",
        String::from_utf8_lossy(&stderr)
    );
    assert_eq!(
        std::fs::read(dst.join("shrinking.txt")).expect("read initial"),
        large
    );

    // Shrink the file and update mtime
    let small = b"short";
    std::fs::write(src.join("shrinking.txt"), small).expect("write small");
    let new_mtime = FileTime::from_unix_time(1_800_000_000, 0);
    set_file_times(src.join("shrinking.txt"), new_mtime, new_mtime).expect("set shrunk mtime");

    // Re-sync
    let (code, _stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-r"),
        src_trailing,
        dst.clone().into_os_string(),
    ]);

    let stderr_text = String::from_utf8_lossy(&stderr);
    assert_eq!(
        code, 0,
        "re-sync with shrunk file should succeed: {stderr_text}"
    );

    assert_eq!(
        std::fs::read(dst.join("shrinking.txt")).expect("read shrunk"),
        small,
        "destination should have the shrunk file content"
    );
}

/// A mixed scenario: a recursive transfer with some readable files, an
/// unreadable file, and a dangling symlink. The transfer should copy the
/// accessible regular files, preserve the dangling symlink, skip the
/// unreadable file, and report a non-zero exit code.
#[cfg(unix)]
#[test]
fn mixed_errors_readable_files_and_symlinks_transfer_unreadable_skipped() {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::fs::symlink;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let src = tmp.path().join("src");
    let dst = tmp.path().join("dst");

    std::fs::create_dir(&src).expect("create src");

    std::fs::write(src.join("good.txt"), b"good data").expect("write good");
    std::fs::write(src.join("blocked.txt"), b"blocked data").expect("write blocked");
    symlink("nowhere", src.join("dangle")).expect("create dangling link");

    // Make one file unreadable
    let mut perms = std::fs::metadata(src.join("blocked.txt"))
        .expect("blocked meta")
        .permissions();
    perms.set_mode(0o000);
    std::fs::set_permissions(src.join("blocked.txt"), perms.clone()).expect("chmod blocked");

    let mut src_trailing = src.clone().into_os_string();
    src_trailing.push("/");

    let (code, _stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-rl"),
        src_trailing,
        dst.clone().into_os_string(),
    ]);

    // Restore for cleanup
    perms.set_mode(0o644);
    let _ = std::fs::set_permissions(src.join("blocked.txt"), perms);

    // Good file should be transferred
    assert_eq!(
        std::fs::read(dst.join("good.txt")).expect("read good"),
        b"good data",
        "good.txt should be at destination"
    );

    // Dangling symlink should be preserved
    let link_meta = std::fs::symlink_metadata(dst.join("dangle")).expect("dangle metadata");
    assert!(
        link_meta.file_type().is_symlink(),
        "dangling symlink should be preserved at destination"
    );
    let target = std::fs::read_link(dst.join("dangle")).expect("read dangle target");
    assert_eq!(target.to_string_lossy(), "nowhere");

    let stderr_text = String::from_utf8_lossy(&stderr);
    assert!(
        code != 0,
        "expected non-zero exit code due to unreadable file, got {code}: {stderr_text}"
    );
}

/// When a vanished source directory (deleted between file-list build and
/// transfer) is encountered during recursive transfer, remaining entries
/// should still be processed. This extends the existing vanished-file test
/// to directories.
///
/// upstream: flist.c - "file has vanished" for directories
#[cfg(unix)]
#[test]
fn vanished_source_directory_yields_error_remaining_files_transfer() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let src = tmp.path().join("src");
    let dst = tmp.path().join("dst");

    std::fs::create_dir_all(src.join("keep")).expect("create keep dir");
    std::fs::create_dir_all(src.join("gone")).expect("create gone dir");

    std::fs::write(src.join("keep/kept.txt"), b"kept").expect("write kept");
    std::fs::write(src.join("gone/lost.txt"), b"lost").expect("write lost");

    // Initial full sync
    let mut src_trailing = src.clone().into_os_string();
    src_trailing.push("/");

    let (code, _stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-r"),
        src_trailing.clone(),
        dst.clone().into_os_string(),
    ]);
    assert_eq!(
        code,
        0,
        "initial sync should succeed: {}",
        String::from_utf8_lossy(&stderr)
    );

    // Remove the "gone" directory to simulate vanishing
    std::fs::remove_dir_all(src.join("gone")).expect("remove gone dir");

    // Update the kept file so it triggers a transfer (different content + size)
    std::fs::write(src.join("keep/kept.txt"), b"kept - updated content").expect("update kept");

    // Re-sync with explicit paths including the vanished directory
    let (code, _stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-r"),
        src.join("keep").into_os_string(),
        src.join("gone").into_os_string(),
        dst.clone().into_os_string(),
    ]);

    let stderr_text = String::from_utf8_lossy(&stderr);

    // Should get exit code 24 (vanished) or 23 (partial)
    assert!(
        code == 23 || code == 24,
        "expected exit code 23 or 24 for vanished directory, got {code}: {stderr_text}"
    );

    // The kept directory should still have been synced
    assert!(
        dst.join("keep/kept.txt").exists(),
        "kept.txt should still exist at destination after vanished sibling"
    );
}

/// An empty source directory should transfer successfully with exit code 0.
/// This is a control test for the error scenarios above, verifying that
/// recursive transfer of an empty tree does not spuriously report errors.
#[cfg(unix)]
#[test]
fn empty_source_directory_transfers_successfully() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let src = tmp.path().join("src");
    let dst = tmp.path().join("dst");

    std::fs::create_dir(&src).expect("create src");

    let mut src_trailing = src.into_os_string();
    src_trailing.push("/");

    let (code, _stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-r"),
        src_trailing,
        dst.into_os_string(),
    ]);

    assert_eq!(
        code,
        0,
        "empty directory sync should exit 0: {}",
        String::from_utf8_lossy(&stderr)
    );
}

/// When the destination directory does not exist and --mkpath is not used,
/// a single-file transfer into a new path should still succeed (the
/// destination is treated as the target file name).
#[cfg(unix)]
#[test]
fn transfer_to_nonexistent_destination_file_succeeds() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let src = tmp.path().join("input.txt");
    let dst = tmp.path().join("output.txt");

    std::fs::write(&src, b"direct copy").expect("write source");

    let (code, _stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        src.into_os_string(),
        dst.clone().into_os_string(),
    ]);

    assert_eq!(
        code,
        0,
        "single file transfer to new name should succeed: {}",
        String::from_utf8_lossy(&stderr)
    );
    assert_eq!(
        std::fs::read(&dst).expect("read dst"),
        b"direct copy",
        "destination should have source content"
    );
}
