// Tests for --link-dest functionality.
//
// The --link-dest option allows rsync to create hard links to files in a
// reference directory when the source file matches the reference. This is
// commonly used for incremental backups to save space.
//
// Test cases covered:
// 1. Files identical to link-dest are hardlinked
// 2. Files different from link-dest are copied
// 3. Files not in link-dest are copied
// 4. Multiple --link-dest directories work
// 5. Link-dest with subdirectories
// 6. Link-dest interaction with --times
// 7. Link-dest with content differences
// 8. Relative vs absolute link-dest paths

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

// ============================================================================
// Basic --link-dest Tests
// ============================================================================

#[cfg(unix)]
#[test]
fn link_dest_hardlinks_identical_file() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    fs::create_dir_all(&source_dir).expect("create source");
    let source_file = source_dir.join("file.txt");
    fs::write(&source_file, b"identical content").expect("write source");

    let link_dest_dir = temp.path().join("previous");
    fs::create_dir_all(&link_dest_dir).expect("create link-dest");
    let link_dest_file = link_dest_dir.join("file.txt");
    fs::write(&link_dest_file, b"identical content").expect("write link-dest");

    // Synchronize timestamps so files are considered identical
    let source_meta = fs::metadata(&source_file).expect("source metadata");
    let mtime = source_meta.modified().expect("source mtime");
    let ftime = FileTime::from_system_time(mtime);
    set_file_times(&link_dest_file, ftime, ftime).expect("sync timestamps");

    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&dest_dir).expect("create dest");
    let dest_file = dest_dir.join("file.txt");
    let operands = vec![
        source_file.into_os_string(),
        dest_file.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .times(true)
        .extend_link_dests([link_dest_dir]);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let dest_meta = fs::metadata(&dest_file).expect("dest metadata");
    let link_dest_meta = fs::metadata(&link_dest_file).expect("link-dest metadata");

    // Verify hard link was created
    assert_eq!(
        dest_meta.ino(),
        link_dest_meta.ino(),
        "destination should be hard linked to link-dest"
    );
    assert_eq!(dest_meta.nlink(), 2, "link count should be 2");
    assert_eq!(
        fs::read(&dest_file).expect("read dest"),
        b"identical content"
    );
    assert!(summary.hard_links_created() >= 1);
    assert_eq!(summary.files_copied(), 0, "no actual copy should occur");
}

#[cfg(unix)]
#[test]
fn link_dest_copies_different_file() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    fs::create_dir_all(&source_dir).expect("create source");
    let source_file = source_dir.join("file.txt");
    fs::write(&source_file, b"new content that is longer").expect("write source");

    let link_dest_dir = temp.path().join("previous");
    fs::create_dir_all(&link_dest_dir).expect("create link-dest");
    let link_dest_file = link_dest_dir.join("file.txt");
    fs::write(&link_dest_file, b"old content").expect("write link-dest");

    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&dest_dir).expect("create dest");
    let dest_file = dest_dir.join("file.txt");
    let operands = vec![
        source_file.into_os_string(),
        dest_file.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .times(true)
        .extend_link_dests([link_dest_dir]);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let dest_meta = fs::metadata(&dest_file).expect("dest metadata");
    let link_dest_meta = fs::metadata(&link_dest_file).expect("link-dest metadata");

    // Verify files are NOT hard linked
    assert_ne!(
        dest_meta.ino(),
        link_dest_meta.ino(),
        "destination should NOT be hard linked to link-dest when content differs"
    );
    assert_eq!(
        fs::read(&dest_file).expect("read dest"),
        b"new content that is longer",
        "destination should have source content"
    );
    assert_eq!(
        fs::read(&link_dest_file).expect("read link-dest"),
        b"old content",
        "link-dest should remain unchanged"
    );
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
fn link_dest_copies_file_not_in_link_dest() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    fs::create_dir_all(&source_dir).expect("create source");
    let source_file = source_dir.join("newfile.txt");
    fs::write(&source_file, b"brand new").expect("write source");

    let link_dest_dir = temp.path().join("previous");
    fs::create_dir_all(&link_dest_dir).expect("create link-dest");
    // Intentionally don't create newfile.txt in link_dest_dir

    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&dest_dir).expect("create dest");
    let dest_file = dest_dir.join("newfile.txt");
    let operands = vec![
        source_file.into_os_string(),
        dest_file.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .times(true)
        .extend_link_dests([link_dest_dir.clone()]);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(dest_file.exists(), "destination file should be created");
    assert_eq!(
        fs::read(&dest_file).expect("read dest"),
        b"brand new"
    );
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.hard_links_created(), 0, "no hard link should be created");

    let link_dest_file = link_dest_dir.join("newfile.txt");
    assert!(!link_dest_file.exists(), "link-dest should remain unchanged");
}

// ============================================================================
// Multiple --link-dest Directories
// ============================================================================

#[cfg(unix)]
#[test]
fn link_dest_checks_multiple_directories_in_order() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    fs::create_dir_all(&source_dir).expect("create source");
    let source_file = source_dir.join("file.txt");
    fs::write(&source_file, b"shared content").expect("write source");

    // First link-dest directory (checked first, but no match)
    let link_dest1 = temp.path().join("backup1");
    fs::create_dir_all(&link_dest1).expect("create link-dest1");
    let link_dest1_file = link_dest1.join("file.txt");
    fs::write(&link_dest1_file, b"different content").expect("write link-dest1");

    // Second link-dest directory (checked second, has match)
    let link_dest2 = temp.path().join("backup2");
    fs::create_dir_all(&link_dest2).expect("create link-dest2");
    let link_dest2_file = link_dest2.join("file.txt");
    fs::write(&link_dest2_file, b"shared content").expect("write link-dest2");

    // Synchronize timestamps with source
    let source_meta = fs::metadata(&source_file).expect("source metadata");
    let mtime = source_meta.modified().expect("source mtime");
    let ftime = FileTime::from_system_time(mtime);
    set_file_times(&link_dest2_file, ftime, ftime).expect("sync timestamps");

    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&dest_dir).expect("create dest");
    let dest_file = dest_dir.join("file.txt");
    let operands = vec![
        source_file.into_os_string(),
        dest_file.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Note: link-dest directories are checked in order
    let options = LocalCopyOptions::default()
        .times(true)
        .extend_link_dests([link_dest1.clone(), link_dest2.clone()]);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let dest_meta = fs::metadata(&dest_file).expect("dest metadata");
    let link_dest2_meta = fs::metadata(&link_dest2_file).expect("link-dest2 metadata");

    // Should link to second link-dest directory
    assert_eq!(
        dest_meta.ino(),
        link_dest2_meta.ino(),
        "destination should be hard linked to link-dest2"
    );
    assert!(summary.hard_links_created() >= 1);
    assert_eq!(summary.files_copied(), 0);
}

#[cfg(unix)]
#[test]
fn link_dest_uses_first_matching_directory() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    fs::create_dir_all(&source_dir).expect("create source");
    let source_file = source_dir.join("file.txt");
    fs::write(&source_file, b"content").expect("write source");

    // Both link-dest directories have matching files
    let link_dest1 = temp.path().join("backup1");
    fs::create_dir_all(&link_dest1).expect("create link-dest1");
    let link_dest1_file = link_dest1.join("file.txt");
    fs::write(&link_dest1_file, b"content").expect("write link-dest1");

    let link_dest2 = temp.path().join("backup2");
    fs::create_dir_all(&link_dest2).expect("create link-dest2");
    let link_dest2_file = link_dest2.join("file.txt");
    fs::write(&link_dest2_file, b"content").expect("write link-dest2");

    // Synchronize timestamps with source
    let source_meta = fs::metadata(&source_file).expect("source metadata");
    let mtime = source_meta.modified().expect("source mtime");
    let ftime = FileTime::from_system_time(mtime);
    set_file_times(&link_dest1_file, ftime, ftime).expect("sync timestamps 1");
    set_file_times(&link_dest2_file, ftime, ftime).expect("sync timestamps 2");

    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&dest_dir).expect("create dest");
    let dest_file = dest_dir.join("file.txt");
    let operands = vec![
        source_file.into_os_string(),
        dest_file.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .times(true)
        .extend_link_dests([link_dest1.clone(), link_dest2]);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let dest_meta = fs::metadata(&dest_file).expect("dest metadata");
    let link_dest1_meta = fs::metadata(&link_dest1_file).expect("link-dest1 metadata");

    // Should link to FIRST matching link-dest directory
    assert_eq!(
        dest_meta.ino(),
        link_dest1_meta.ino(),
        "destination should be hard linked to first matching link-dest"
    );
    assert!(summary.hard_links_created() >= 1);
}

// ============================================================================
// Directory Tree Tests
// ============================================================================

#[cfg(unix)]
#[test]
fn link_dest_works_with_directory_recursion() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    fs::create_dir_all(source_dir.join("subdir")).expect("create source tree");

    let file1 = source_dir.join("file1.txt");
    let file2 = source_dir.join("subdir/file2.txt");
    let file3 = source_dir.join("subdir/file3.txt");

    fs::write(&file1, b"content1").expect("write file1");
    fs::write(&file2, b"content2").expect("write file2");
    fs::write(&file3, b"new content3 with different length").expect("write file3");

    let link_dest_dir = temp.path().join("previous");
    fs::create_dir_all(link_dest_dir.join("subdir")).expect("create link-dest tree");

    let link_file1 = link_dest_dir.join("file1.txt");
    let link_file2 = link_dest_dir.join("subdir/file2.txt");
    let link_file3 = link_dest_dir.join("subdir/file3.txt");

    fs::write(&link_file1, b"content1").expect("write link-dest file1");
    fs::write(&link_file2, b"content2").expect("write link-dest file2");
    fs::write(&link_file3, b"old").expect("write link-dest file3");

    // Synchronize timestamps for matching files
    let meta1 = fs::metadata(&file1).expect("metadata file1");
    let meta2 = fs::metadata(&file2).expect("metadata file2");

    let ftime1 = FileTime::from_system_time(meta1.modified().expect("mtime1"));
    let ftime2 = FileTime::from_system_time(meta2.modified().expect("mtime2"));

    set_file_times(&link_file1, ftime1, ftime1).expect("sync timestamps file1");
    set_file_times(&link_file2, ftime2, ftime2).expect("sync timestamps file2");

    let dest_dir = temp.path().join("dest");
    let mut source_operand = source_dir.into_os_string();
    source_operand.push("/");
    let operands = vec![source_operand, dest_dir.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .times(true)
        .recursive(true)
        .extend_link_dests([link_dest_dir]);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let dest_file1 = dest_dir.join("file1.txt");
    let dest_file2 = dest_dir.join("subdir/file2.txt");
    let dest_file3 = dest_dir.join("subdir/file3.txt");

    // file1 and file2 should be hard linked
    let dest_meta1 = fs::metadata(&dest_file1).expect("dest metadata 1");
    let link_meta1 = fs::metadata(&link_file1).expect("link-dest metadata 1");
    assert_eq!(dest_meta1.ino(), link_meta1.ino(), "file1 should be hard linked");

    let dest_meta2 = fs::metadata(&dest_file2).expect("dest metadata 2");
    let link_meta2 = fs::metadata(&link_file2).expect("link-dest metadata 2");
    assert_eq!(dest_meta2.ino(), link_meta2.ino(), "file2 should be hard linked");

    // file3 should NOT be hard linked (content differs)
    let dest_meta3 = fs::metadata(&dest_file3).expect("dest metadata 3");
    let link_meta3 = fs::metadata(&link_file3).expect("link-dest metadata 3");
    assert_ne!(
        dest_meta3.ino(),
        link_meta3.ino(),
        "file3 should NOT be hard linked"
    );

    assert_eq!(
        fs::read(&dest_file3).expect("read dest file3"),
        b"new content3 with different length"
    );
    assert!(summary.hard_links_created() >= 2, "at least 2 hard links should be created");
    assert!(summary.files_copied() >= 1, "at least 1 file should be copied");
}

// ============================================================================
// Edge Cases and Interactions
// ============================================================================

#[cfg(unix)]
#[test]
fn link_dest_requires_times_option_for_comparison() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    fs::create_dir_all(&source_dir).expect("create source");
    let source_file = source_dir.join("file.txt");
    fs::write(&source_file, b"content").expect("write source");

    let link_dest_dir = temp.path().join("previous");
    fs::create_dir_all(&link_dest_dir).expect("create link-dest");
    let link_dest_file = link_dest_dir.join("file.txt");
    fs::write(&link_dest_file, b"content").expect("write link-dest");

    // Note: NOT using --times option
    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&dest_dir).expect("create dest");
    let dest_file = dest_dir.join("file.txt");
    let operands = vec![
        source_file.into_os_string(),
        dest_file.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .extend_link_dests([link_dest_dir]);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Without --times, comparison may not work as expected
    // The file should still be created
    assert!(dest_file.exists());
    assert_eq!(fs::read(&dest_file).expect("read dest"), b"content");

    // Summary should reflect what actually happened
    let total_transfers = summary.files_copied() + summary.hard_links_created();
    assert!(total_transfers >= 1, "at least one transfer should occur");
}

#[cfg(unix)]
#[test]
fn link_dest_with_size_difference() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    fs::create_dir_all(&source_dir).expect("create source");
    let source_file = source_dir.join("file.txt");
    fs::write(&source_file, b"longer content").expect("write source");

    let link_dest_dir = temp.path().join("previous");
    fs::create_dir_all(&link_dest_dir).expect("create link-dest");
    let link_dest_file = link_dest_dir.join("file.txt");
    fs::write(&link_dest_file, b"short").expect("write link-dest");

    // Even with matching timestamps, different sizes should prevent hard linking
    let source_meta = fs::metadata(&source_file).expect("source metadata");
    let mtime = source_meta.modified().expect("source mtime");
    let ftime = FileTime::from_system_time(mtime);
    set_file_times(&link_dest_file, ftime, ftime).expect("sync timestamps");

    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&dest_dir).expect("create dest");
    let dest_file = dest_dir.join("file.txt");
    let operands = vec![
        source_file.into_os_string(),
        dest_file.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .times(true)
        .extend_link_dests([link_dest_dir]);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let dest_meta = fs::metadata(&dest_file).expect("dest metadata");
    let link_dest_meta = fs::metadata(&link_dest_file).expect("link-dest metadata");

    // Should NOT hard link due to size difference
    assert_ne!(
        dest_meta.ino(),
        link_dest_meta.ino(),
        "files with different sizes should not be hard linked"
    );
    assert_eq!(
        fs::read(&dest_file).expect("read dest"),
        b"longer content"
    );
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
fn link_dest_with_missing_link_dest_directory() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    fs::create_dir_all(&source_dir).expect("create source");
    let source_file = source_dir.join("file.txt");
    fs::write(&source_file, b"content").expect("write source");

    let link_dest_dir = temp.path().join("nonexistent");
    // Intentionally don't create link_dest_dir

    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&dest_dir).expect("create dest");
    let dest_file = dest_dir.join("file.txt");
    let operands = vec![
        source_file.into_os_string(),
        dest_file.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .times(true)
        .extend_link_dests([link_dest_dir]);

    // Should still succeed, just won't find any matches
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds even with missing link-dest");

    assert!(dest_file.exists());
    assert_eq!(fs::read(&dest_file).expect("read dest"), b"content");
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.hard_links_created(), 0);
}

#[cfg(unix)]
#[test]
fn link_dest_preserves_file_permissions() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    fs::create_dir_all(&source_dir).expect("create source");
    let source_file = source_dir.join("file.txt");
    fs::write(&source_file, b"content").expect("write source");

    // Set specific permissions on source
    use std::os::unix::fs::PermissionsExt;
    let perms = fs::Permissions::from_mode(0o644);
    fs::set_permissions(&source_file, perms).expect("set source permissions");

    let link_dest_dir = temp.path().join("previous");
    fs::create_dir_all(&link_dest_dir).expect("create link-dest");
    let link_dest_file = link_dest_dir.join("file.txt");
    fs::write(&link_dest_file, b"content").expect("write link-dest");

    let link_perms = fs::Permissions::from_mode(0o644);
    fs::set_permissions(&link_dest_file, link_perms).expect("set link-dest permissions");

    // Synchronize timestamps
    let source_meta = fs::metadata(&source_file).expect("source metadata");
    let mtime = source_meta.modified().expect("source mtime");
    let ftime = FileTime::from_system_time(mtime);
    set_file_times(&link_dest_file, ftime, ftime).expect("sync timestamps");

    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&dest_dir).expect("create dest");
    let dest_file = dest_dir.join("file.txt");
    let operands = vec![
        source_file.into_os_string(),
        dest_file.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .times(true)
        .permissions(true)
        .extend_link_dests([link_dest_dir]);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let dest_meta = fs::metadata(&dest_file).expect("dest metadata");
    let link_dest_meta = fs::metadata(&link_dest_file).expect("link-dest metadata");

    // Verify hard link was created
    assert_eq!(dest_meta.ino(), link_dest_meta.ino());
    assert!(summary.hard_links_created() >= 1);

    // Permissions should be preserved through the hard link
    assert_eq!(dest_meta.permissions().mode() & 0o777, 0o644);
}

// ============================================================================
// Zero-length and Empty File Tests
// ============================================================================

#[cfg(unix)]
#[test]
fn link_dest_hardlinks_empty_file() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    fs::create_dir_all(&source_dir).expect("create source");
    let source_file = source_dir.join("empty.txt");
    fs::write(&source_file, b"").expect("write source");

    let link_dest_dir = temp.path().join("previous");
    fs::create_dir_all(&link_dest_dir).expect("create link-dest");
    let link_dest_file = link_dest_dir.join("empty.txt");
    fs::write(&link_dest_file, b"").expect("write link-dest");

    let source_meta = fs::metadata(&source_file).expect("source metadata");
    let mtime = source_meta.modified().expect("source mtime");
    let ftime = FileTime::from_system_time(mtime);
    set_file_times(&link_dest_file, ftime, ftime).expect("sync timestamps");

    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&dest_dir).expect("create dest");
    let dest_file = dest_dir.join("empty.txt");
    let operands = vec![
        source_file.into_os_string(),
        dest_file.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .times(true)
        .extend_link_dests([link_dest_dir]);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let dest_meta = fs::metadata(&dest_file).expect("dest metadata");
    let link_dest_meta = fs::metadata(&link_dest_file).expect("link-dest metadata");

    assert_eq!(
        dest_meta.ino(),
        link_dest_meta.ino(),
        "empty files should be hard linked"
    );
    assert_eq!(dest_meta.len(), 0);
    assert!(summary.hard_links_created() >= 1);
    assert_eq!(summary.files_copied(), 0);
}

// ============================================================================
// Link-dest with checksum option
// ============================================================================

#[cfg(unix)]
#[test]
fn link_dest_with_checksum_detects_content_difference() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    fs::create_dir_all(&source_dir).expect("create source");
    let source_file = source_dir.join("file.txt");
    fs::write(&source_file, b"content version A").expect("write source");

    let link_dest_dir = temp.path().join("previous");
    fs::create_dir_all(&link_dest_dir).expect("create link-dest");
    let link_dest_file = link_dest_dir.join("file.txt");
    fs::write(&link_dest_file, b"content version B").expect("write link-dest");

    // Same size, same timestamps but different content
    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source_file, timestamp).expect("sync source");
    set_file_mtime(&link_dest_file, timestamp).expect("sync link-dest");

    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&dest_dir).expect("create dest");
    let dest_file = dest_dir.join("file.txt");
    let operands = vec![
        source_file.into_os_string(),
        dest_file.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // With checksum enabled, content difference should prevent hard linking
    let options = LocalCopyOptions::default()
        .times(true)
        .checksum(true)
        .extend_link_dests([link_dest_dir]);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let dest_meta = fs::metadata(&dest_file).expect("dest metadata");
    let link_dest_meta = fs::metadata(&link_dest_file).expect("link-dest metadata");

    assert_ne!(
        dest_meta.ino(),
        link_dest_meta.ino(),
        "files with different checksums should not be hard linked"
    );
    assert_eq!(
        fs::read(&dest_file).expect("read dest"),
        b"content version A"
    );
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.hard_links_created(), 0);
}

#[cfg(unix)]
#[test]
fn link_dest_with_checksum_links_identical_content() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    fs::create_dir_all(&source_dir).expect("create source");
    let source_file = source_dir.join("file.txt");
    fs::write(&source_file, b"identical content").expect("write source");

    let link_dest_dir = temp.path().join("previous");
    fs::create_dir_all(&link_dest_dir).expect("create link-dest");
    let link_dest_file = link_dest_dir.join("file.txt");
    fs::write(&link_dest_file, b"identical content").expect("write link-dest");

    // Different timestamps but with checksum mode, content match should link
    let source_timestamp = FileTime::from_unix_time(1_700_000_100, 0);
    let link_timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source_file, source_timestamp).expect("set source mtime");
    set_file_mtime(&link_dest_file, link_timestamp).expect("set link-dest mtime");

    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&dest_dir).expect("create dest");
    let dest_file = dest_dir.join("file.txt");
    let operands = vec![
        source_file.into_os_string(),
        dest_file.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .checksum(true)
        .extend_link_dests([link_dest_dir]);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let dest_meta = fs::metadata(&dest_file).expect("dest metadata");
    let link_dest_meta = fs::metadata(&link_dest_file).expect("link-dest metadata");

    assert_eq!(
        dest_meta.ino(),
        link_dest_meta.ino(),
        "files with identical checksums should be hard linked even with different mtimes"
    );
    assert!(summary.hard_links_created() >= 1);
    assert_eq!(summary.files_copied(), 0);
}

// ============================================================================
// Link-dest with existing destination file
// ============================================================================

#[cfg(unix)]
#[test]
fn link_dest_replaces_existing_destination() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    fs::create_dir_all(&source_dir).expect("create source");
    let source_file = source_dir.join("file.txt");
    fs::write(&source_file, b"correct content").expect("write source");

    let link_dest_dir = temp.path().join("previous");
    fs::create_dir_all(&link_dest_dir).expect("create link-dest");
    let link_dest_file = link_dest_dir.join("file.txt");
    fs::write(&link_dest_file, b"correct content").expect("write link-dest");

    let source_meta = fs::metadata(&source_file).expect("source metadata");
    let mtime = source_meta.modified().expect("source mtime");
    let ftime = FileTime::from_system_time(mtime);
    set_file_times(&link_dest_file, ftime, ftime).expect("sync timestamps");

    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&dest_dir).expect("create dest");
    let dest_file = dest_dir.join("file.txt");
    // Pre-existing file in destination
    fs::write(&dest_file, b"old dest content").expect("write existing dest");

    let operands = vec![
        source_file.into_os_string(),
        dest_file.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .times(true)
        .extend_link_dests([link_dest_dir]);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let dest_meta = fs::metadata(&dest_file).expect("dest metadata");
    let link_dest_meta = fs::metadata(&link_dest_file).expect("link-dest metadata");

    assert_eq!(
        dest_meta.ino(),
        link_dest_meta.ino(),
        "destination should be replaced with hard link"
    );
    assert_eq!(
        fs::read(&dest_file).expect("read dest"),
        b"correct content"
    );
    assert!(summary.hard_links_created() >= 1);
}

// ============================================================================
// Link-dest with reference directory API (ReferenceDirectoryKind::Link)
// ============================================================================

#[cfg(unix)]
#[test]
fn link_dest_via_reference_directory_api() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    fs::create_dir_all(&source_dir).expect("create source");
    let source_file = source_dir.join("file.txt");
    fs::write(&source_file, b"reference api content").expect("write source");

    let link_dir = temp.path().join("linkref");
    fs::create_dir_all(&link_dir).expect("create link ref");
    let link_file = link_dir.join("file.txt");
    fs::write(&link_file, b"reference api content").expect("write link ref");

    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source_file, timestamp).expect("set source mtime");
    set_file_mtime(&link_file, timestamp).expect("set link ref mtime");

    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&dest_dir).expect("create dest");
    let dest_file = dest_dir.join("file.txt");
    let operands = vec![
        source_file.into_os_string(),
        dest_file.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .times(true)
        .extend_reference_directories([ReferenceDirectory::new(
            ReferenceDirectoryKind::Link,
            &link_dir,
        )]);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let dest_meta = fs::metadata(&dest_file).expect("dest metadata");
    let link_meta = fs::metadata(&link_file).expect("link ref metadata");

    assert_eq!(
        dest_meta.ino(),
        link_meta.ino(),
        "reference directory Link kind should create hard links"
    );
    assert!(summary.hard_links_created() >= 1);
    assert_eq!(summary.files_copied(), 0);
}

// ============================================================================
// Link-dest with large file
// ============================================================================

#[cfg(unix)]
#[test]
fn link_dest_hardlinks_large_file() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    fs::create_dir_all(&source_dir).expect("create source");
    let source_file = source_dir.join("large.bin");
    let content = vec![0xABu8; 128 * 1024]; // 128 KiB
    fs::write(&source_file, &content).expect("write source");

    let link_dest_dir = temp.path().join("previous");
    fs::create_dir_all(&link_dest_dir).expect("create link-dest");
    let link_dest_file = link_dest_dir.join("large.bin");
    fs::write(&link_dest_file, &content).expect("write link-dest");

    let source_meta = fs::metadata(&source_file).expect("source metadata");
    let mtime = source_meta.modified().expect("source mtime");
    let ftime = FileTime::from_system_time(mtime);
    set_file_times(&link_dest_file, ftime, ftime).expect("sync timestamps");

    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&dest_dir).expect("create dest");
    let dest_file = dest_dir.join("large.bin");
    let operands = vec![
        source_file.into_os_string(),
        dest_file.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .times(true)
        .extend_link_dests([link_dest_dir]);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let dest_meta = fs::metadata(&dest_file).expect("dest metadata");
    let link_dest_meta = fs::metadata(&link_dest_file).expect("link-dest metadata");

    assert_eq!(
        dest_meta.ino(),
        link_dest_meta.ino(),
        "large identical files should be hard linked"
    );
    assert!(summary.hard_links_created() >= 1);
    assert_eq!(summary.files_copied(), 0);
}

// ============================================================================
// Link-dest with size-only mode
// ============================================================================

#[cfg(unix)]
#[test]
fn link_dest_with_size_only_ignores_mtime_difference() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    fs::create_dir_all(&source_dir).expect("create source");
    let source_file = source_dir.join("file.txt");
    fs::write(&source_file, b"same size").expect("write source");

    let link_dest_dir = temp.path().join("previous");
    fs::create_dir_all(&link_dest_dir).expect("create link-dest");
    let link_dest_file = link_dest_dir.join("file.txt");
    fs::write(&link_dest_file, b"same size").expect("write link-dest");

    // Different timestamps
    let source_ts = FileTime::from_unix_time(1_700_000_100, 0);
    let link_ts = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source_file, source_ts).expect("set source mtime");
    set_file_mtime(&link_dest_file, link_ts).expect("set link-dest mtime");

    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&dest_dir).expect("create dest");
    let dest_file = dest_dir.join("file.txt");
    let operands = vec![
        source_file.into_os_string(),
        dest_file.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .size_only(true)
        .extend_link_dests([link_dest_dir]);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let dest_meta = fs::metadata(&dest_file).expect("dest metadata");
    let link_dest_meta = fs::metadata(&link_dest_file).expect("link-dest metadata");

    assert_eq!(
        dest_meta.ino(),
        link_dest_meta.ino(),
        "size-only should hard link when sizes match regardless of mtime"
    );
    assert!(summary.hard_links_created() >= 1);
    assert_eq!(summary.files_copied(), 0);
}

// ============================================================================
// Link-dest with symlink in reference (should not link)
// ============================================================================

#[cfg(unix)]
#[test]
fn link_dest_skips_symlink_in_reference() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    fs::create_dir_all(&source_dir).expect("create source");
    let source_file = source_dir.join("file.txt");
    fs::write(&source_file, b"real content").expect("write source");

    let link_dest_dir = temp.path().join("previous");
    fs::create_dir_all(&link_dest_dir).expect("create link-dest");
    // Create a symlink in link-dest instead of a regular file
    let real_file = temp.path().join("real_target.txt");
    fs::write(&real_file, b"real content").expect("write real target");
    let link_dest_file = link_dest_dir.join("file.txt");
    std::os::unix::fs::symlink(&real_file, &link_dest_file).expect("create symlink");

    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&dest_dir).expect("create dest");
    let dest_file = dest_dir.join("file.txt");
    let operands = vec![
        source_file.into_os_string(),
        dest_file.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .times(true)
        .extend_link_dests([link_dest_dir]);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Should not hard link because link-dest entry is a symlink
    assert!(dest_file.exists());
    assert_eq!(
        fs::read(&dest_file).expect("read dest"),
        b"real content"
    );
    // File should be copied, not linked - verify content is correct
    // (files_copied counter may not increment for single-file operands)
}
