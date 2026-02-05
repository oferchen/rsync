// Comprehensive tests for --inplace flag behavior
//
// The --inplace flag tells rsync to update destination files directly without
// using temporary files. This is useful when:
// - The destination is on a filesystem that doesn't support temp files
// - Space is limited and you can't afford to have two copies of a file
// - You need to preserve hard links to the destination file
//
// Key behaviors tested:
// 1. File is updated in place (no temp file created)
// 2. Partial updates work correctly
// 3. Works with delta transfer
// 4. Works with whole-file transfer
// 5. File permissions preserved during update
// 6. Inode preservation (unlike temp file replacement)

// ==================== Basic Inplace Tests ====================

#[test]
fn execute_inplace_updates_file_directly() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"new content here").expect("write source");
    fs::write(&destination, b"old content").expect("write destination");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().inplace(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read dest"), b"new content here");
}

#[test]
fn execute_inplace_creates_new_file_when_destination_missing() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("new_dest.txt");

    fs::write(&source, b"brand new file").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().inplace(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(destination.exists());
    assert_eq!(fs::read(&destination).expect("read dest"), b"brand new file");
}

// ==================== Inode Preservation Tests ====================

#[cfg(unix)]
#[test]
fn execute_inplace_preserves_inode() {
    use std::os::unix::fs::MetadataExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"replacement content").expect("write source");
    fs::write(&destination, b"original").expect("write destination");

    let original_inode = fs::metadata(&destination)
        .expect("destination metadata")
        .ino();

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().inplace(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);

    let updated_inode = fs::metadata(&destination)
        .expect("destination metadata")
        .ino();

    assert_eq!(
        updated_inode, original_inode,
        "inplace update should preserve inode"
    );
    assert_eq!(
        fs::read(&destination).expect("read destination"),
        b"replacement content"
    );
}

#[cfg(unix)]
#[test]
fn execute_without_inplace_changes_inode() {
    use std::os::unix::fs::MetadataExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&dest_dir).expect("create dest dir");
    let destination = dest_dir.join("file.txt");

    fs::write(&source, b"new content").expect("write source");
    fs::write(&destination, b"original").expect("write destination");

    let original_inode = fs::metadata(&destination)
        .expect("destination metadata")
        .ino();

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);

    let new_inode = fs::metadata(&destination)
        .expect("destination metadata")
        .ino();

    assert_ne!(
        new_inode, original_inode,
        "non-inplace update should change inode (uses temp file)"
    );
}

// ==================== No Temp File Tests ====================

#[test]
fn execute_inplace_does_not_leave_temp_files() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&dest_dir).expect("create dest dir");
    let destination = dest_dir.join("target.txt");

    fs::write(&source, b"data").expect("write source");
    fs::write(&destination, b"old").expect("write destination");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().inplace(true),
    )
    .expect("copy succeeds");

    // Verify no temp files (patterns: .rsync-tmp-*, .~tmp~, .*.XXXXXX)
    let entries: Vec<_> = fs::read_dir(&dest_dir)
        .expect("list dest dir")
        .filter_map(|e| e.ok())
        .collect();

    assert_eq!(entries.len(), 1, "only destination file should exist");
    assert_eq!(
        entries[0].file_name().to_string_lossy(),
        "target.txt"
    );
}

// ==================== Partial Update Tests ====================

#[test]
fn execute_inplace_partial_update_small_to_large() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    // Source is larger than destination
    let large_content = vec![b'X'; 10000];
    fs::write(&source, &large_content).expect("write large source");
    fs::write(&destination, b"small").expect("write small destination");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().inplace(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read dest"), large_content);
    assert_eq!(
        fs::metadata(&destination).expect("metadata").len(),
        10000
    );
}

#[test]
fn execute_inplace_partial_update_large_to_small() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    // Source is smaller than destination
    fs::write(&source, b"tiny").expect("write small source");
    let large_content = vec![b'Y'; 10000];
    fs::write(&destination, &large_content).expect("write large destination");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().inplace(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read dest"), b"tiny");
    // File should be truncated to new size
    assert_eq!(fs::metadata(&destination).expect("metadata").len(), 4);
}

// ==================== Whole-File Transfer Tests ====================

#[test]
fn execute_inplace_with_whole_file_transfer() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    let content = b"whole file transfer content";
    fs::write(&source, content).expect("write source");
    fs::write(&destination, b"existing").expect("write destination");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .inplace(true)
                .whole_file(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read dest"), content);
}

#[test]
fn execute_inplace_with_whole_file_large_file() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("large.bin");
    let destination = temp.path().join("dest.bin");

    // Create a file larger than typical copy buffer (256KB)
    let large_content: Vec<u8> = (0..=255).cycle().take(300 * 1024).collect();
    fs::write(&source, &large_content).expect("write large source");
    fs::write(&destination, b"placeholder").expect("write destination");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .inplace(true)
                .whole_file(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.bytes_copied(), 300 * 1024);
    assert_eq!(fs::read(&destination).expect("read dest"), large_content);
}

// ==================== Permission Preservation Tests ====================

#[cfg(unix)]
#[test]
fn execute_inplace_preserves_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"new content").expect("write source");
    let mut perms = fs::metadata(&source).expect("metadata").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&source, perms).expect("set source perms");

    fs::write(&destination, b"original").expect("write destination");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .inplace(true)
                .permissions(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);

    let dest_perms = fs::metadata(&destination).expect("dest metadata").permissions();
    assert_eq!(dest_perms.mode() & 0o777, 0o755);
}

#[cfg(unix)]
#[test]
fn execute_inplace_preserves_restricted_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"secret data").expect("write source");
    let mut perms = fs::metadata(&source).expect("metadata").permissions();
    perms.set_mode(0o600); // rw-------
    fs::set_permissions(&source, perms).expect("set source perms");

    fs::write(&destination, b"old secret").expect("write destination");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .inplace(true)
                .permissions(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);

    let dest_perms = fs::metadata(&destination).expect("dest metadata").permissions();
    assert_eq!(dest_perms.mode() & 0o777, 0o600);
}

// ==================== Timestamp Preservation Tests ====================

#[test]
fn execute_inplace_preserves_timestamps() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"timestamped content").expect("write source");
    let past_time = FileTime::from_unix_time(1_600_000_000, 0);
    set_file_mtime(&source, past_time).expect("set source mtime");

    fs::write(&destination, b"old").expect("write destination");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .inplace(true)
                .times(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);

    let dest_mtime = FileTime::from_last_modification_time(
        &fs::metadata(&destination).expect("dest metadata"),
    );
    assert_eq!(dest_mtime, past_time);
}

// ==================== Edge Cases ====================

#[test]
fn execute_inplace_with_empty_source() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("empty.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"").expect("write empty source");
    fs::write(&destination, b"has content").expect("write destination");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().inplace(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read dest"), b"");
    assert_eq!(fs::metadata(&destination).expect("metadata").len(), 0);
}

#[test]
fn execute_inplace_with_empty_destination() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("empty.txt");

    fs::write(&source, b"content to write").expect("write source");
    fs::write(&destination, b"").expect("write empty destination");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().inplace(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read dest"), b"content to write");
}

#[test]
fn execute_inplace_identical_files_skips_copy() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    let content = b"identical content";
    fs::write(&source, content).expect("write source");
    fs::write(&destination, content).expect("write destination");

    // Set identical timestamps
    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source, timestamp).expect("set source mtime");
    set_file_mtime(&destination, timestamp).expect("set dest mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .inplace(true)
                .times(true),
        )
        .expect("copy succeeds");

    // File should be skipped (size and mtime match)
    assert_eq!(summary.files_copied(), 0);
}

// ==================== Directory Tree Tests ====================

#[test]
fn execute_inplace_recursive_directory() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");

    // Create source tree
    fs::create_dir_all(source_root.join("subdir")).expect("create subdir");
    fs::write(source_root.join("file1.txt"), b"content1").expect("write file1");
    fs::write(source_root.join("subdir/file2.txt"), b"content2").expect("write file2");

    // Create destination tree with existing files
    fs::create_dir_all(dest_root.join("subdir")).expect("create dest subdir");
    fs::write(dest_root.join("file1.txt"), b"old1").expect("write dest file1");
    fs::write(dest_root.join("subdir/file2.txt"), b"old2").expect("write dest file2");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().inplace(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 2);
    assert_eq!(
        fs::read(dest_root.join("file1.txt")).expect("read"),
        b"content1"
    );
    assert_eq!(
        fs::read(dest_root.join("subdir/file2.txt")).expect("read"),
        b"content2"
    );
}

// ==================== Read-Only Directory Tests ====================

#[cfg(unix)]
#[test]
fn execute_inplace_in_read_only_directory() {
    use rustix::fs::{chmod, Mode};
    use std::os::unix::fs::MetadataExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let dest_dir = temp.path().join("readonly");
    fs::create_dir_all(&dest_dir).expect("create dest dir");
    let destination = dest_dir.join("file.txt");

    fs::write(&source, b"update").expect("write source");
    fs::write(&destination, b"original").expect("write destination");

    let original_inode = fs::metadata(&destination)
        .expect("destination metadata")
        .ino();

    // Make directory read-only (blocks creating temp files)
    let readonly = Mode::from_bits_truncate(0o555);
    chmod(&dest_dir, readonly).expect("restrict directory");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // With --inplace, this should succeed because we update in place
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().inplace(true),
        )
        .expect("inplace copy succeeds in read-only dir");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read"), b"update");

    // Verify inode preserved
    let updated_inode = fs::metadata(&destination)
        .expect("destination metadata")
        .ino();
    assert_eq!(updated_inode, original_inode);

    // Restore permissions for cleanup
    let restore = Mode::from_bits_truncate(0o755);
    chmod(&dest_dir, restore).expect("restore directory");
}

#[cfg(unix)]
#[test]
fn execute_without_inplace_fails_in_read_only_directory() {
    use rustix::fs::{chmod, Mode};

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let dest_dir = temp.path().join("readonly");
    fs::create_dir_all(&dest_dir).expect("create dest dir");
    let destination = dest_dir.join("file.txt");

    fs::write(&source, b"update").expect("write source");
    fs::write(&destination, b"original").expect("write destination");

    // Make directory read-only (blocks creating temp files)
    let readonly = Mode::from_bits_truncate(0o555);
    chmod(&dest_dir, readonly).expect("restrict directory");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Without --inplace, this should fail because we can't create temp file
    let result = plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().inplace(false),
    );

    // Expect failure due to permission denied creating temp file
    assert!(result.is_err(), "should fail without inplace in read-only dir");

    // Original should be unchanged
    assert_eq!(fs::read(&destination).expect("read"), b"original");

    // Restore permissions for cleanup
    let restore = Mode::from_bits_truncate(0o755);
    chmod(&dest_dir, restore).expect("restore directory");
}

// ==================== Combined Flag Tests ====================

#[test]
fn execute_inplace_combined_with_checksum() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    let content = b"checksum verified content";
    fs::write(&source, content).expect("write source");
    fs::write(&destination, b"different content").expect("write destination");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .inplace(true)
                .checksum(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read dest"), content);
}

#[test]
fn execute_inplace_combined_with_size_only() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    // Same size, different content
    fs::write(&source, b"AAAA").expect("write source");
    fs::write(&destination, b"BBBB").expect("write destination");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .inplace(true)
                .size_only(true),
        )
        .expect("copy succeeds");

    // Should skip because sizes are identical
    assert_eq!(summary.files_copied(), 0);
    assert_eq!(fs::read(&destination).expect("read dest"), b"BBBB");
}

#[test]
fn execute_inplace_combined_with_ignore_times() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    let content = b"content";
    fs::write(&source, content).expect("write source");
    fs::write(&destination, content).expect("write destination");

    // Set identical timestamps
    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source, timestamp).expect("set source mtime");
    set_file_mtime(&destination, timestamp).expect("set dest mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .inplace(true)
                .ignore_times(true),
        )
        .expect("copy succeeds");

    // With ignore_times, should copy even though size/mtime match
    assert_eq!(summary.files_copied(), 1);
}

// ==================== Binary Content Tests ====================

#[test]
fn execute_inplace_with_binary_content() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("binary.bin");
    let destination = temp.path().join("dest.bin");

    // Binary content with null bytes and all byte values
    let binary_content: Vec<u8> = (0..=255).collect();
    fs::write(&source, &binary_content).expect("write binary source");
    fs::write(&destination, b"text placeholder").expect("write destination");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().inplace(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read dest"), binary_content);
}

// ==================== Report/Events Tests ====================

#[test]
fn execute_inplace_records_copy_event() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"event data").expect("write source");
    fs::write(&destination, b"old").expect("write destination");

    let operands = vec![
        source.into_os_string(),
        destination.into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .inplace(true)
        .collect_events(true);

    let report = plan
        .execute_with_report(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let records = report.records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].action(), &LocalCopyAction::DataCopied);
    assert_eq!(records[0].bytes_transferred(), 10);
}

// ==================== Dry Run Tests ====================

#[test]
fn execute_inplace_dry_run_does_not_modify() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"new content").expect("write source");
    fs::write(&destination, b"original").expect("write destination");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::DryRun,
            LocalCopyOptions::default().inplace(true),
        )
        .expect("dry run succeeds");

    assert_eq!(summary.files_copied(), 1);
    // Original content should be preserved
    assert_eq!(fs::read(&destination).expect("read dest"), b"original");
}
