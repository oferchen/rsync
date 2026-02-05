// Tests for zero-length (empty) file transfer edge cases.
//
// This test module comprehensively validates that empty files are handled correctly
// during transfer operations, including:
// 1. Basic transfer of empty files
// 2. Preservation of permissions and timestamps
// 3. Delta transfer mode behavior
// 4. Multiple empty files in recursive operations
// 5. Interaction with various options (checksum, update, inplace, etc.)
//
// Empty files are an important edge case because they bypass many code paths
// that handle file content, so it's critical to ensure metadata operations
// and file creation still work correctly.

// ============================================================================
// Basic Empty File Transfer Tests
// ============================================================================

#[test]
fn execute_empty_file_basic_transfer() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("empty.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"").expect("write empty source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1, "empty file should be copied");
    assert!(destination.exists(), "destination should exist");
    assert_eq!(fs::read(&destination).expect("read dest"), b"", "destination should be empty");
    assert_eq!(fs::metadata(&destination).expect("metadata").len(), 0, "file size should be 0");
}

#[test]
fn execute_empty_file_replaces_nonempty_destination() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("empty.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"").expect("write empty source");
    fs::write(&destination, b"old content").expect("write existing dest");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read dest"), b"", "destination should now be empty");
    assert_eq!(fs::metadata(&destination).expect("metadata").len(), 0);
}

#[test]
fn execute_empty_file_dry_run_does_not_create_file() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("empty.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"").expect("write empty source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::DryRun, LocalCopyOptions::default())
        .expect("dry run succeeds");

    assert_eq!(summary.files_copied(), 1, "dry run should report file would be copied");
    assert!(!destination.exists(), "dry run should not create destination");
}

// ============================================================================
// Metadata Preservation Tests
// ============================================================================

#[cfg(unix)]
#[test]
fn execute_empty_file_preserves_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("empty.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"").expect("write empty source");
    fs::set_permissions(&source, PermissionsExt::from_mode(0o600)).expect("set permissions");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().permissions(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    let dest_mode = fs::metadata(&destination)
        .expect("dest metadata")
        .permissions()
        .mode() & 0o777;
    assert_eq!(dest_mode, 0o600, "permissions should be preserved for empty file");
}

#[test]
fn execute_empty_file_preserves_timestamp() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("empty.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"").expect("write empty source");

    let source_mtime = FileTime::from_unix_time(1_600_000_000, 500_000_000);
    set_file_mtime(&source, source_mtime).expect("set source mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().times(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    let dest_mtime = FileTime::from_last_modification_time(
        &fs::metadata(&destination).expect("dest metadata")
    );
    assert_eq!(dest_mtime.unix_seconds(), source_mtime.unix_seconds(),
        "timestamp should be preserved for empty file");
}

#[cfg(unix)]
#[test]
fn execute_empty_file_preserves_both_permissions_and_timestamp() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("empty.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"").expect("write empty source");
    fs::set_permissions(&source, PermissionsExt::from_mode(0o644)).expect("set permissions");

    let source_time = FileTime::from_unix_time(1_650_000_000, 123_456_789);
    set_file_mtime(&source, source_time).expect("set source time");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .permissions(true)
                .times(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);

    let dest_metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(dest_metadata.len(), 0, "file should be empty");
    assert_eq!(
        dest_metadata.permissions().mode() & 0o777,
        0o644,
        "permissions should be preserved"
    );

    let dest_time = FileTime::from_last_modification_time(&dest_metadata);
    assert_eq!(
        dest_time.unix_seconds(),
        source_time.unix_seconds(),
        "timestamp should be preserved"
    );
}

// ============================================================================
// Delta Transfer Tests
// ============================================================================

#[test]
fn execute_empty_file_delta_transfer_from_empty() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("empty.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"").expect("write empty source");
    fs::write(&destination, b"").expect("write empty dest");

    // Set different mtimes to force delta evaluation
    set_file_mtime(&source, FileTime::from_unix_time(2_000_000_000, 0)).expect("set source mtime");
    set_file_mtime(&destination, FileTime::from_unix_time(1_000_000_000, 0)).expect("set dest mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().whole_file(false),
        )
        .expect("delta copy succeeds");

    // Delta transfer of empty to empty should be trivial
    assert_eq!(summary.files_copied(), 1, "file should be counted as copied");
    assert_eq!(summary.bytes_copied(), 0, "no data bytes should be transferred");
    assert_eq!(summary.matched_bytes(), 0, "no bytes should be matched");
    assert_eq!(fs::metadata(&destination).expect("metadata").len(), 0);
}

#[test]
fn execute_empty_file_delta_transfer_from_nonempty() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("empty.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"").expect("write empty source");
    fs::write(&destination, b"old content that will be removed").expect("write dest");

    set_file_mtime(&source, FileTime::from_unix_time(2_000_000_000, 0)).expect("set source mtime");
    set_file_mtime(&destination, FileTime::from_unix_time(1_000_000_000, 0)).expect("set dest mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().whole_file(false),
        )
        .expect("delta copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read dest"), b"", "destination should be truncated to empty");
    assert_eq!(fs::metadata(&destination).expect("metadata").len(), 0);
}

#[test]
fn execute_empty_file_delta_transfer_to_nonempty() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"new content").expect("write source");
    fs::write(&destination, b"").expect("write empty dest");

    set_file_mtime(&source, FileTime::from_unix_time(2_000_000_000, 0)).expect("set source mtime");
    set_file_mtime(&destination, FileTime::from_unix_time(1_000_000_000, 0)).expect("set dest mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().whole_file(false),
        )
        .expect("delta copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read dest"), b"new content");
    assert_eq!(summary.bytes_copied(), 11);
}

// ============================================================================
// Multiple Empty Files Tests
// ============================================================================

#[test]
fn execute_multiple_empty_files_recursive() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");

    fs::create_dir_all(&source_root).expect("create source root");

    // Create multiple empty files
    fs::write(source_root.join("empty1.txt"), b"").expect("write empty1");
    fs::write(source_root.join("empty2.txt"), b"").expect("write empty2");
    fs::write(source_root.join("empty3.txt"), b"").expect("write empty3");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().recursive(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 3, "all three empty files should be copied");
    assert!(dest_root.join("empty1.txt").exists());
    assert!(dest_root.join("empty2.txt").exists());
    assert!(dest_root.join("empty3.txt").exists());
    assert_eq!(fs::read(dest_root.join("empty1.txt")).expect("read"), b"");
    assert_eq!(fs::read(dest_root.join("empty2.txt")).expect("read"), b"");
    assert_eq!(fs::read(dest_root.join("empty3.txt")).expect("read"), b"");
}

#[test]
fn execute_mixed_empty_and_nonempty_files() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");

    fs::create_dir_all(&source_root).expect("create source root");

    // Mix of empty and non-empty files
    fs::write(source_root.join("empty.txt"), b"").expect("write empty");
    fs::write(source_root.join("content.txt"), b"has content").expect("write content");
    fs::write(source_root.join("another_empty.txt"), b"").expect("write another empty");
    fs::write(source_root.join("more_content.txt"), b"more data").expect("write more content");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().recursive(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 4, "all files should be copied");

    // Verify empty files
    assert_eq!(fs::read(dest_root.join("empty.txt")).expect("read"), b"");
    assert_eq!(fs::read(dest_root.join("another_empty.txt")).expect("read"), b"");

    // Verify non-empty files
    assert_eq!(fs::read(dest_root.join("content.txt")).expect("read"), b"has content");
    assert_eq!(fs::read(dest_root.join("more_content.txt")).expect("read"), b"more data");
}

#[test]
fn execute_nested_directories_with_empty_files() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");

    fs::create_dir_all(source_root.join("level1/level2")).expect("create nested dirs");

    // Empty files at different nesting levels
    fs::write(source_root.join("root_empty.txt"), b"").expect("write root empty");
    fs::write(source_root.join("level1/l1_empty.txt"), b"").expect("write l1 empty");
    fs::write(source_root.join("level1/level2/l2_empty.txt"), b"").expect("write l2 empty");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().recursive(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 3);
    assert!(dest_root.join("root_empty.txt").exists());
    assert!(dest_root.join("level1/l1_empty.txt").exists());
    assert!(dest_root.join("level1/level2/l2_empty.txt").exists());
    assert_eq!(fs::read(dest_root.join("root_empty.txt")).expect("read"), b"");
    assert_eq!(fs::read(dest_root.join("level1/l1_empty.txt")).expect("read"), b"");
    assert_eq!(fs::read(dest_root.join("level1/level2/l2_empty.txt")).expect("read"), b"");
}

// ============================================================================
// Interaction with Other Options
// ============================================================================

#[test]
fn execute_empty_file_with_checksum_mode() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("empty.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"").expect("write empty source");
    fs::write(&destination, b"").expect("write empty dest");

    // Same content (both empty), same timestamp
    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source, timestamp).expect("set source time");
    set_file_mtime(&destination, timestamp).expect("set dest time");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .checksum(true)
                .times(true),
        )
        .expect("copy succeeds");

    // Checksum of two empty files should match, so file should be skipped
    assert_eq!(summary.files_copied(), 0, "identical empty files should be skipped in checksum mode");
    assert_eq!(summary.regular_files_matched(), 1);
}

#[test]
fn execute_empty_file_with_update_flag() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("empty.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"").expect("write empty source");
    fs::write(&destination, b"old content").expect("write dest");

    // Source is newer
    set_file_mtime(&source, FileTime::from_unix_time(2_000_000_000, 0)).expect("set source time");
    set_file_mtime(&destination, FileTime::from_unix_time(1_000_000_000, 0)).expect("set dest time");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().update(true),
        )
        .expect("copy succeeds");

    // Source is newer, should copy even though it's empty
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read dest"), b"");
}

#[test]
fn execute_empty_file_update_skips_when_dest_newer() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("empty.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"").expect("write empty source");
    fs::write(&destination, b"").expect("write empty dest");

    // Destination is newer
    set_file_mtime(&source, FileTime::from_unix_time(1_000_000_000, 0)).expect("set source time");
    set_file_mtime(&destination, FileTime::from_unix_time(2_000_000_000, 0)).expect("set dest time");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().update(true),
        )
        .expect("copy succeeds");

    // Destination is newer, should skip
    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_skipped_newer(), 1);
}

#[test]
fn execute_empty_file_with_inplace() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("empty.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"").expect("write empty source");
    fs::write(&destination, b"content to be truncated").expect("write dest");

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
    assert_eq!(fs::read(&destination).expect("read dest"), b"", "destination should be truncated in-place");
    assert_eq!(fs::metadata(&destination).expect("metadata").len(), 0);
}

#[test]
fn execute_empty_file_with_temp_dir() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("empty.txt");
    let destination = temp.path().join("dest.txt");
    let temp_dir = temp.path().join("staging");
    fs::create_dir_all(&temp_dir).expect("create temp dir");

    fs::write(&source, b"").expect("write empty source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().with_temp_directory(Some(temp_dir.clone())),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(destination.exists(), "destination should exist");
    assert_eq!(fs::read(&destination).expect("read dest"), b"");

    // Verify no temp files left behind
    let staging_files: Vec<_> = fs::read_dir(&temp_dir)
        .expect("read temp dir")
        .collect();
    assert!(staging_files.is_empty(), "no temp files should remain after empty file transfer");
}

#[cfg(unix)]
#[test]
fn execute_empty_file_preserves_permissions_with_chmod() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("empty.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"").expect("write empty source");
    fs::set_permissions(&source, PermissionsExt::from_mode(0o640)).expect("set source perms");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .permissions(true)
                .with_chmod(Some(ChmodModifiers::parse("+x").expect("parse chmod"))),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);

    let dest_mode = fs::metadata(&destination)
        .expect("dest metadata")
        .permissions()
        .mode() & 0o777;

    // Should have 0o640 from source + execute bits added
    assert_eq!(dest_mode, 0o751, "chmod should be applied to empty file permissions");
}

// ============================================================================
// Edge Cases and Boundary Conditions
// ============================================================================

#[test]
fn execute_empty_file_with_bytes_transferred_counter() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("empty.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"").expect("write empty source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.bytes_copied(), 0, "zero bytes should be transferred for empty file");
    assert_eq!(summary.regular_files_total(), 1);
}

#[test]
fn execute_empty_file_to_empty_file_skipped_with_times() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("empty.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"").expect("write empty source");
    fs::write(&destination, b"").expect("write empty dest");

    // Same timestamp
    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source, timestamp).expect("set source time");
    set_file_mtime(&destination, timestamp).expect("set dest time");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().times(true),
        )
        .expect("copy succeeds");

    // With times preservation and identical timestamps, should skip
    assert_eq!(summary.files_copied(), 0, "identical empty files should be skipped");
    assert_eq!(summary.regular_files_matched(), 1);
}

#[test]
fn execute_creates_empty_file_in_nonexistent_directory() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("empty.txt");
    let destination = temp.path().join("newdir/dest.txt");

    fs::write(&source, b"").expect("write empty source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Should fail without --create-directories equivalent
    // (Default behavior should create parent directories for single file copy)
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(destination.exists());
    assert_eq!(fs::read(&destination).expect("read"), b"");
}

#[test]
fn execute_empty_file_collect_events() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("empty.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"").expect("write empty source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let report = plan
        .execute_with_report(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().collect_events(true),
        )
        .expect("copy succeeds");

    assert_eq!(report.summary().files_copied(), 1);

    let records = report.records();
    assert_eq!(records.len(), 1, "should have one record for empty file");
    let record = &records[0];
    assert_eq!(record.action(), &LocalCopyAction::DataCopied);
    assert_eq!(record.bytes_transferred(), 0, "record should show 0 bytes transferred");
}

#[test]
fn execute_empty_file_remove_source_files() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("empty.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"").expect("write empty source");

    let operands = vec![
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().remove_source_files(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.sources_removed(), 1);
    assert!(!source.exists(), "source should be removed after copy");
    assert!(destination.exists());
    assert_eq!(fs::read(&destination).expect("read dest"), b"");
}
