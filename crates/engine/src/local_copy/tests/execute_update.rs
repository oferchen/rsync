// Tests for --update (-u) flag behavior.
//
// The --update flag skips files where the destination is newer than or equal
// to the source modification time. This matches upstream rsync behavior.
//
// Test cases covered:
// 1. Skip files that are newer on destination
// 2. Transfer files that are older on destination
// 3. Transfer files that don't exist on destination
// 4. Handle same mtime (should skip)
// 5. Directory recursive behavior with --update
// 6. Edge cases: subsecond precision, boundary times

// ============================================================================
// Basic --update Flag Tests
// ============================================================================

#[test]
fn update_skips_file_when_destination_is_newer() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"source content").expect("write source");
    fs::write(&destination, b"dest content").expect("write dest");

    // Source: older (2023-11-14 22:13:20 UTC)
    // Dest: newer (2023-11-14 22:15:00 UTC)
    let older_time = FileTime::from_unix_time(1_700_000_000, 0);
    let newer_time = FileTime::from_unix_time(1_700_000_100, 0);
    set_file_times(&source, older_time, older_time).expect("set source times");
    set_file_times(&destination, newer_time, newer_time).expect("set dest times");

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

    // File should be skipped because destination is newer
    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_total(), 1);
    assert_eq!(summary.regular_files_skipped_newer(), 1);
    // Destination content should be preserved
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"dest content"
    );
}

#[test]
fn update_copies_file_when_destination_is_older() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"updated content").expect("write source");
    fs::write(&destination, b"stale content").expect("write dest");

    // Source: newer (2023-11-14 22:15:00 UTC)
    // Dest: older (2023-11-14 22:13:20 UTC)
    let older_time = FileTime::from_unix_time(1_700_000_000, 0);
    let newer_time = FileTime::from_unix_time(1_700_000_100, 0);
    set_file_times(&source, newer_time, newer_time).expect("set source times");
    set_file_times(&destination, older_time, older_time).expect("set dest times");

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

    // File should be copied because destination is older
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.regular_files_total(), 1);
    assert_eq!(summary.regular_files_skipped_newer(), 0);
    // Destination should have new content
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"updated content"
    );
}

#[test]
fn update_copies_file_when_destination_missing() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"new file content").expect("write source");
    // No destination file exists

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

    // File should be copied because destination doesn't exist
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.regular_files_total(), 1);
    assert_eq!(summary.regular_files_skipped_newer(), 0);
    // Destination should have content
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"new file content"
    );
}

#[test]
fn update_skips_file_when_mtime_is_equal() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    // Use same size so skip is due to mtime equality (both 16 bytes)
    fs::write(&source, b"source_content_x").expect("write source");
    fs::write(&destination, b"dest_content_xyx").expect("write dest");

    // Both files have the same mtime (equal times should skip)
    let same_time = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_times(&source, same_time, same_time).expect("set source times");
    set_file_times(&destination, same_time, same_time).expect("set dest times");

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

    // File should be skipped because destination mtime is equal (not older)
    // With equal mtime and same size, file is skipped but may not be counted in skipped_newer
    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_total(), 1);
    // Destination content should be preserved
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"dest_content_xyx"
    );
}

// ============================================================================
// Directory Recursive Tests
// ============================================================================

#[test]
fn update_recursive_mixed_timestamps() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source root");
    fs::create_dir_all(&dest_root).expect("create dest root");

    // Create multiple files with different timestamp relationships
    let older_time = FileTime::from_unix_time(1_700_000_000, 0);
    let newer_time = FileTime::from_unix_time(1_700_000_100, 0);

    // File 1: source newer than dest (should copy)
    fs::write(source_root.join("copy_me.txt"), b"newer_source_").expect("write copy_me source");
    fs::write(dest_root.join("copy_me.txt"), b"older_dest__").expect("write copy_me dest");
    set_file_mtime(source_root.join("copy_me.txt"), newer_time).expect("set copy_me source time");
    set_file_mtime(dest_root.join("copy_me.txt"), older_time).expect("set copy_me dest time");

    // File 2: source older than dest (should skip)
    // Use same size so --update skips based on mtime
    fs::write(source_root.join("skip_me.txt"), b"older_source").expect("write skip_me source");
    fs::write(dest_root.join("skip_me.txt"), b"newer_dest__").expect("write skip_me dest");
    set_file_mtime(source_root.join("skip_me.txt"), older_time).expect("set skip_me source time");
    set_file_mtime(dest_root.join("skip_me.txt"), newer_time).expect("set skip_me dest time");

    // File 3: no dest file (should copy)
    fs::write(source_root.join("new_file.txt"), b"brand new").expect("write new_file source");

    // File 4: same mtime (should skip per upstream behavior)
    // Use same size so skip is based on mtime equality
    fs::write(source_root.join("same_time.txt"), b"same_source_").expect("write same_time source");
    fs::write(dest_root.join("same_time.txt"), b"same_dest___").expect("write same_time dest");
    set_file_mtime(source_root.join("same_time.txt"), older_time).expect("set same_time source");
    set_file_mtime(dest_root.join("same_time.txt"), older_time).expect("set same_time dest");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().update(true).recursive(true),
        )
        .expect("copy succeeds");

    // Should copy: copy_me.txt (dest older) + new_file.txt (no dest)
    // Should skip: skip_me.txt (dest newer) + same_time.txt (equal time)
    assert_eq!(summary.files_copied(), 2);
    assert_eq!(summary.regular_files_total(), 4);
    // Only skip_me.txt is counted as skipped_newer; same_time.txt with equal mtime+same size is skipped differently
    assert_eq!(summary.regular_files_skipped_newer(), 1);

    // Verify file contents
    assert_eq!(
        fs::read(dest_root.join("copy_me.txt")).expect("read copy_me"),
        b"newer_source_"
    );
    assert_eq!(
        fs::read(dest_root.join("skip_me.txt")).expect("read skip_me"),
        b"newer_dest__"
    );
    assert_eq!(
        fs::read(dest_root.join("new_file.txt")).expect("read new_file"),
        b"brand new"
    );
    assert_eq!(
        fs::read(dest_root.join("same_time.txt")).expect("read same_time"),
        b"same_dest___"
    );
}

#[test]
fn update_nested_directories_selective_copy() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");

    // Create nested directory structure
    fs::create_dir_all(source_root.join("level1/level2")).expect("create source dirs");
    fs::create_dir_all(dest_root.join("level1/level2")).expect("create dest dirs");

    let older_time = FileTime::from_unix_time(1_700_000_000, 0);
    let newer_time = FileTime::from_unix_time(1_700_000_100, 0);

    // Root level file: dest is newer (skip)
    fs::write(source_root.join("root.txt"), b"root source").expect("write root source");
    fs::write(dest_root.join("root.txt"), b"root dest").expect("write root dest");
    set_file_mtime(source_root.join("root.txt"), older_time).expect("set root source time");
    set_file_mtime(dest_root.join("root.txt"), newer_time).expect("set root dest time");

    // Level 1 file: source is newer (copy)
    fs::write(source_root.join("level1/l1.txt"), b"l1 source").expect("write l1 source");
    fs::write(dest_root.join("level1/l1.txt"), b"l1 dest").expect("write l1 dest");
    set_file_mtime(source_root.join("level1/l1.txt"), newer_time).expect("set l1 source time");
    set_file_mtime(dest_root.join("level1/l1.txt"), older_time).expect("set l1 dest time");

    // Level 2 file: no dest file (copy)
    fs::write(source_root.join("level1/level2/l2.txt"), b"l2 new").expect("write l2 source");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().update(true).recursive(true),
        )
        .expect("copy succeeds");

    // Should copy 2 files (l1.txt and l2.txt), skip 1 (root.txt)
    assert_eq!(summary.files_copied(), 2);
    assert_eq!(summary.regular_files_skipped_newer(), 1);

    // Verify content preservation
    assert_eq!(
        fs::read(dest_root.join("root.txt")).expect("read root"),
        b"root dest"
    );
    assert_eq!(
        fs::read(dest_root.join("level1/l1.txt")).expect("read l1"),
        b"l1 source"
    );
    assert_eq!(
        fs::read(dest_root.join("level1/level2/l2.txt")).expect("read l2"),
        b"l2 new"
    );
}

// ============================================================================
// Edge Cases and Boundary Conditions
// ============================================================================

#[test]
fn update_handles_subsecond_precision() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"source").expect("write source");
    fs::write(&destination, b"dest").expect("write dest");

    // Same second but different nanoseconds
    // Source: .100 seconds, Dest: .200 seconds
    let source_time = FileTime::from_unix_time(1_700_000_000, 100_000_000);
    let dest_time = FileTime::from_unix_time(1_700_000_000, 200_000_000);
    set_file_times(&source, source_time, source_time).expect("set source times");
    set_file_times(&destination, dest_time, dest_time).expect("set dest times");

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

    // Destination is newer (by nanoseconds), should skip
    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_skipped_newer(), 1);
    assert_eq!(fs::read(&destination).expect("read"), b"dest");
}

#[test]
fn update_handles_epoch_boundary() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"source").expect("write source");
    fs::write(&destination, b"dest").expect("write dest");

    // Use times near Unix epoch
    let epoch_time = FileTime::from_unix_time(1, 0);
    let later_time = FileTime::from_unix_time(2, 0);
    set_file_times(&source, epoch_time, epoch_time).expect("set source times");
    set_file_times(&destination, later_time, later_time).expect("set dest times");

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
fn update_handles_far_future_timestamp() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"source").expect("write source");
    fs::write(&destination, b"dest").expect("write dest");

    // Far future timestamp (year 2100)
    let current_time = FileTime::from_unix_time(1_700_000_000, 0);
    let future_time = FileTime::from_unix_time(4_102_444_800, 0); // 2100-01-01
    set_file_times(&source, current_time, current_time).expect("set source times");
    set_file_times(&destination, future_time, future_time).expect("set dest times");

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

    // Destination has future timestamp, should skip
    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_skipped_newer(), 1);
}

#[test]
fn update_one_second_difference_copies_when_source_newer() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"newer").expect("write source");
    fs::write(&destination, b"older").expect("write dest");

    // Source is exactly 1 second newer
    let older_time = FileTime::from_unix_time(1_700_000_000, 0);
    let newer_time = FileTime::from_unix_time(1_700_000_001, 0);
    set_file_times(&source, newer_time, newer_time).expect("set source times");
    set_file_times(&destination, older_time, older_time).expect("set dest times");

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

    // Source is newer by 1 second, should copy
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.regular_files_skipped_newer(), 0);
    assert_eq!(fs::read(&destination).expect("read"), b"newer");
}

// ============================================================================
// Combined Options Tests
// ============================================================================

#[test]
fn update_combined_with_times_preserves_mtime() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"content").expect("write source");

    let source_mtime = FileTime::from_unix_time(1_700_000_100, 123_456_789);
    set_file_mtime(&source, source_mtime).expect("set source mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().update(true).times(true),
        )
        .expect("copy succeeds");

    // File should be copied (no dest exists)
    assert_eq!(summary.files_copied(), 1);

    // Verify mtime is preserved
    let dest_mtime = FileTime::from_last_modification_time(
        &fs::metadata(&destination).expect("dest metadata")
    );
    // Note: Some filesystems don't preserve full nanosecond precision
    assert_eq!(dest_mtime.unix_seconds(), source_mtime.unix_seconds());
}

#[test]
fn update_combined_with_checksum_still_skips_newer() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    // Same content but different timestamps
    fs::write(&source, b"identical content").expect("write source");
    fs::write(&destination, b"identical content").expect("write dest");

    let older_time = FileTime::from_unix_time(1_700_000_000, 0);
    let newer_time = FileTime::from_unix_time(1_700_000_100, 0);
    set_file_times(&source, older_time, older_time).expect("set source times");
    set_file_times(&destination, newer_time, newer_time).expect("set dest times");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().update(true).checksum(true),
        )
        .expect("copy succeeds");

    // Even with checksum mode, --update should skip because dest is newer
    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_skipped_newer(), 1);
}

#[test]
fn update_without_flag_copies_even_when_dest_newer() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"source").expect("write source");
    fs::write(&destination, b"dest").expect("write dest");

    let older_time = FileTime::from_unix_time(1_700_000_000, 0);
    let newer_time = FileTime::from_unix_time(1_700_000_100, 0);
    set_file_times(&source, older_time, older_time).expect("set source times");
    set_file_times(&destination, newer_time, newer_time).expect("set dest times");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // WITHOUT update flag
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default(), // no .update(true)
        )
        .expect("copy succeeds");

    // Without --update, file should be copied regardless of timestamps
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.regular_files_skipped_newer(), 0);
    assert_eq!(fs::read(&destination).expect("read"), b"source");
}

// ============================================================================
// Dry Run Tests
// ============================================================================

#[test]
fn update_dry_run_reports_but_preserves_files() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"newer source").expect("write source");
    fs::write(&destination, b"older dest").expect("write dest");

    let older_time = FileTime::from_unix_time(1_700_000_000, 0);
    let newer_time = FileTime::from_unix_time(1_700_000_100, 0);
    set_file_times(&source, newer_time, newer_time).expect("set source times");
    set_file_times(&destination, older_time, older_time).expect("set dest times");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::DryRun,
            LocalCopyOptions::default().update(true),
        )
        .expect("dry run succeeds");

    // Dry run should report that it would copy
    assert_eq!(summary.files_copied(), 1);
    // But destination should remain unchanged
    assert_eq!(fs::read(&destination).expect("read"), b"older dest");
}

#[test]
fn update_dry_run_reports_skipped_files() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"older source").expect("write source");
    fs::write(&destination, b"newer dest").expect("write dest");

    let older_time = FileTime::from_unix_time(1_700_000_000, 0);
    let newer_time = FileTime::from_unix_time(1_700_000_100, 0);
    set_file_times(&source, older_time, older_time).expect("set source times");
    set_file_times(&destination, newer_time, newer_time).expect("set dest times");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::DryRun,
            LocalCopyOptions::default().update(true),
        )
        .expect("dry run succeeds");

    // Dry run should report that file would be skipped
    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_skipped_newer(), 1);
    // Destination unchanged
    assert_eq!(fs::read(&destination).expect("read"), b"newer dest");
}

// ============================================================================
// Empty File Tests
// ============================================================================

#[test]
fn update_handles_empty_source_file() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"").expect("write empty source");
    fs::write(&destination, b"has content").expect("write dest");

    let older_time = FileTime::from_unix_time(1_700_000_000, 0);
    let newer_time = FileTime::from_unix_time(1_700_000_100, 0);
    set_file_times(&source, newer_time, newer_time).expect("set source times");
    set_file_times(&destination, older_time, older_time).expect("set dest times");

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

    // Source is newer, should copy even though empty
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read"), b"");
}

#[test]
fn update_handles_empty_dest_file() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"has content").expect("write source");
    fs::write(&destination, b"").expect("write empty dest");

    let older_time = FileTime::from_unix_time(1_700_000_000, 0);
    let newer_time = FileTime::from_unix_time(1_700_000_100, 0);
    set_file_times(&source, older_time, older_time).expect("set source times");
    set_file_times(&destination, newer_time, newer_time).expect("set dest times");

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

    // Dest is newer (even though empty), should skip
    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_skipped_newer(), 1);
    assert_eq!(fs::read(&destination).expect("read"), b"");
}

// ============================================================================
// Upstream rsync Behavior Comparison
// ============================================================================

/// This test documents upstream rsync's --update behavior.
///
/// From rsync(1) man page:
/// > -u, --update
/// >     This forces rsync to skip any files which exist on the destination
/// >     and have a modified time that is newer than the source file.
///
/// Key behaviors:
/// 1. Skip if dest mtime > source mtime
/// 2. Skip if dest mtime == source mtime (equal counts as "not older")
/// 3. Copy if dest doesn't exist
/// 4. Copy if dest mtime < source mtime
#[test]
fn update_matches_upstream_rsync_semantics() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");
    fs::create_dir_all(&dest_root).expect("create dest");

    let t1 = FileTime::from_unix_time(1_000_000_000, 0);
    let t2 = FileTime::from_unix_time(1_000_000_001, 0);

    // Case 1: dest newer than source -> skip
    // Use same size to ensure --update skips based on mtime alone
    fs::write(source_root.join("case1.txt"), b"source1").expect("write case1 src");
    fs::write(dest_root.join("case1.txt"), b"destin1").expect("write case1 dst");
    set_file_mtime(source_root.join("case1.txt"), t1).expect("set case1 src time");
    set_file_mtime(dest_root.join("case1.txt"), t2).expect("set case1 dst time");

    // Case 2: dest equal to source -> skip (upstream behavior)
    // Note: With equal mtimes, rsync still checks size. If sizes differ, it transfers.
    // To ensure skip, we use same size but different content.
    fs::write(source_root.join("case2.txt"), b"source2").expect("write case2 src");
    fs::write(dest_root.join("case2.txt"), b"destin2").expect("write case2 dst");
    set_file_mtime(source_root.join("case2.txt"), t1).expect("set case2 src time");
    set_file_mtime(dest_root.join("case2.txt"), t1).expect("set case2 dst time");

    // Case 3: dest doesn't exist -> copy
    fs::write(source_root.join("case3.txt"), b"new").expect("write case3 src");

    // Case 4: dest older than source -> copy
    fs::write(source_root.join("case4.txt"), b"source4").expect("write case4 src");
    fs::write(dest_root.join("case4.txt"), b"dest4").expect("write case4 dst");
    set_file_mtime(source_root.join("case4.txt"), t2).expect("set case4 src time");
    set_file_mtime(dest_root.join("case4.txt"), t1).expect("set case4 dst time");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().update(true),
        )
        .expect("copy succeeds");

    // Cases 3 and 4 should be copied, cases 1 and 2 should be skipped
    assert_eq!(summary.files_copied(), 2, "should copy case3 and case4");
    // Case1 is skipped because dest is newer (counted in skipped_newer)
    // Case2 is skipped because dest mtime == source mtime (also counted in skipped_newer with --update flag)
    assert_eq!(summary.regular_files_skipped_newer(), 1, "should skip case1 (newer)");
    // Note: case2 with equal mtime + same size is skipped, but may not increment skipped_newer counter

    // Verify specific file states
    assert_eq!(
        fs::read(dest_root.join("case1.txt")).expect("case1"),
        b"destin1",
        "case1: dest newer -> preserved"
    );
    assert_eq!(
        fs::read(dest_root.join("case2.txt")).expect("case2"),
        b"destin2",
        "case2: dest equal -> preserved"
    );
    assert_eq!(
        fs::read(dest_root.join("case3.txt")).expect("case3"),
        b"new",
        "case3: no dest -> copied"
    );
    assert_eq!(
        fs::read(dest_root.join("case4.txt")).expect("case4"),
        b"source4",
        "case4: dest older -> copied"
    );
}
