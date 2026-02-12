// ============================================================================
// Tests for timestamp preservation behavior
// ============================================================================
//
// This test module covers comprehensive timestamp preservation functionality:
// - mtime preservation (modification time)
// - atime handling (access time)
// - Subsecond precision (nanosecond accuracy)
// - --times flag behavior
// - Timestamp comparison for skip decisions
//
// Note: ctime (change time) is not user-settable on most Unix systems, so it
// is not covered in preservation tests.

// ============================================================================
// Basic mtime Preservation Tests
// ============================================================================

#[test]
fn times_flag_preserves_mtime_on_copied_file() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"content for mtime test").expect("write source");

    // Set a specific mtime
    let mtime = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source, mtime).expect("set source mtime");

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

    let dest_metadata = fs::metadata(&destination).expect("dest metadata");
    let dest_mtime = FileTime::from_last_modification_time(&dest_metadata);
    assert_eq!(dest_mtime, mtime, "mtime should be preserved when --times is set");
}

#[test]
fn times_flag_disabled_does_not_preserve_mtime() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"content").expect("write source");

    // Set a specific mtime in the past
    let old_mtime = FileTime::from_unix_time(1_500_000_000, 0);
    set_file_mtime(&source, old_mtime).expect("set source mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Execute without times flag (default is false)
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().times(false),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);

    let dest_metadata = fs::metadata(&destination).expect("dest metadata");
    let dest_mtime = FileTime::from_last_modification_time(&dest_metadata);

    // Destination mtime should NOT match the old source mtime
    // It should be close to "now" (the copy time)
    assert_ne!(dest_mtime, old_mtime, "mtime should not be preserved when --times is disabled");
}

#[test]
fn times_flag_preserves_mtime_on_multiple_files() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    // Create files with different mtimes
    let file1 = source_root.join("file1.txt");
    let file2 = source_root.join("file2.txt");
    let file3 = source_root.join("file3.txt");

    fs::write(&file1, b"content1").expect("write file1");
    fs::write(&file2, b"content2").expect("write file2");
    fs::write(&file3, b"content3").expect("write file3");

    let mtime1 = FileTime::from_unix_time(1_600_000_000, 0);
    let mtime2 = FileTime::from_unix_time(1_700_000_000, 0);
    let mtime3 = FileTime::from_unix_time(1_800_000_000, 0);

    set_file_mtime(&file1, mtime1).expect("set file1 mtime");
    set_file_mtime(&file2, mtime2).expect("set file2 mtime");
    set_file_mtime(&file3, mtime3).expect("set file3 mtime");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().times(true),
    )
    .expect("copy succeeds");

    // Verify each file has its mtime preserved
    let dest_file1 = dest_root.join("file1.txt");
    let dest_file2 = dest_root.join("file2.txt");
    let dest_file3 = dest_root.join("file3.txt");

    let dest_mtime1 = FileTime::from_last_modification_time(&fs::metadata(&dest_file1).expect("file1 meta"));
    let dest_mtime2 = FileTime::from_last_modification_time(&fs::metadata(&dest_file2).expect("file2 meta"));
    let dest_mtime3 = FileTime::from_last_modification_time(&fs::metadata(&dest_file3).expect("file3 meta"));

    assert_eq!(dest_mtime1, mtime1, "file1 mtime should be preserved");
    assert_eq!(dest_mtime2, mtime2, "file2 mtime should be preserved");
    assert_eq!(dest_mtime3, mtime3, "file3 mtime should be preserved");
}

// ============================================================================
// atime (Access Time) Handling Tests
// ============================================================================

#[test]
fn times_flag_preserves_atime_on_copied_file() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"content for atime test").expect("write source");

    // Set specific atime and mtime
    let atime = FileTime::from_unix_time(1_600_000_000, 123_000_000);
    let mtime = FileTime::from_unix_time(1_700_000_000, 456_000_000);
    set_file_times(&source, atime, mtime).expect("set source times");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().times(true),
    )
    .expect("copy succeeds");

    let dest_metadata = fs::metadata(&destination).expect("dest metadata");
    let dest_atime = FileTime::from_last_access_time(&dest_metadata);
    let dest_mtime = FileTime::from_last_modification_time(&dest_metadata);

    assert_eq!(dest_atime, atime, "atime should be preserved");
    assert_eq!(dest_mtime, mtime, "mtime should be preserved");
}

#[test]
fn atime_and_mtime_can_differ() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"content").expect("write source");

    // Set very different atime and mtime
    let atime = FileTime::from_unix_time(1_500_000_000, 100_000_000);
    let mtime = FileTime::from_unix_time(1_800_000_000, 900_000_000);
    set_file_times(&source, atime, mtime).expect("set source times");

    // Verify they're different
    let source_metadata = fs::metadata(&source).expect("source metadata");
    let source_atime = FileTime::from_last_access_time(&source_metadata);
    let source_mtime = FileTime::from_last_modification_time(&source_metadata);
    assert_ne!(source_atime, source_mtime, "atime and mtime should differ");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().times(true),
    )
    .expect("copy succeeds");

    let dest_metadata = fs::metadata(&destination).expect("dest metadata");
    let dest_atime = FileTime::from_last_access_time(&dest_metadata);
    let dest_mtime = FileTime::from_last_modification_time(&dest_metadata);

    assert_eq!(dest_atime, atime, "atime should be preserved as distinct value");
    assert_eq!(dest_mtime, mtime, "mtime should be preserved as distinct value");
    assert_ne!(dest_atime, dest_mtime, "atime and mtime should remain different");
}

// ============================================================================
// Subsecond Precision Tests
// ============================================================================

// Windows NTFS truncates nanoseconds to 100ns intervals.
#[cfg(not(target_os = "windows"))]
#[test]
fn subsecond_precision_is_preserved_full_nanoseconds() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"nanosecond precision test").expect("write source");

    // Set timestamp with full nanosecond precision
    let mtime = FileTime::from_unix_time(1_700_000_000, 123_456_789);
    set_file_mtime(&source, mtime).expect("set source mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().times(true),
    )
    .expect("copy succeeds");

    let dest_metadata = fs::metadata(&destination).expect("dest metadata");
    let dest_mtime = FileTime::from_last_modification_time(&dest_metadata);

    assert_eq!(dest_mtime, mtime, "nanosecond precision should be preserved");
    assert_eq!(dest_mtime.nanoseconds(), 123_456_789, "nanoseconds component should match exactly");
}

// Windows NTFS truncates nanoseconds to 100ns intervals.
#[cfg(not(target_os = "windows"))]
#[test]
fn subsecond_precision_various_values() {
    let temp = tempdir().expect("tempdir");

    // Test various nanosecond values
    let test_cases = vec![
        (0, "zero nanoseconds"),
        (1, "one nanosecond"),
        (999_999_999, "max nanoseconds"),
        (500_000_000, "half second"),
        (123_456_789, "arbitrary nanoseconds"),
        (100_000_000, "100 milliseconds"),
        (1_000_000, "1 millisecond"),
        (1_000, "1 microsecond"),
    ];

    for (nsec, description) in test_cases {
        let source = temp.path().join(format!("source_{nsec}.txt"));
        let destination = temp.path().join(format!("dest_{nsec}.txt"));

        fs::write(&source, format!("content for {description}").as_bytes()).expect("write source");

        let mtime = FileTime::from_unix_time(1_700_000_000, nsec);
        set_file_mtime(&source, mtime).expect("set source mtime");

        let operands = vec![
            source.into_os_string(),
            destination.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        plan.execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().times(true),
        )
        .expect("copy succeeds");

        let dest_metadata = fs::metadata(&destination).expect("dest metadata");
        let dest_mtime = FileTime::from_last_modification_time(&dest_metadata);

        assert_eq!(
            dest_mtime, mtime,
            "{description}: nanosecond value {nsec} should be preserved"
        );
    }
}

// Windows NTFS truncates nanoseconds to 100ns intervals.
#[cfg(not(target_os = "windows"))]
#[test]
fn subsecond_precision_round_trip() {
    let temp = tempdir().expect("tempdir");
    let file1 = temp.path().join("file1.txt");
    let file2 = temp.path().join("file2.txt");
    let file3 = temp.path().join("file3.txt");

    fs::write(&file1, b"round trip content").expect("write file1");

    // Set specific nanosecond value
    let original_mtime = FileTime::from_unix_time(1_700_000_000, 987_654_321);
    set_file_mtime(&file1, original_mtime).expect("set file1 mtime");

    // First copy: file1 -> file2
    let operands = vec![
        file1.into_os_string(),
        file2.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan1");
    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().times(true),
    )
    .expect("first copy");

    // Second copy: file2 -> file3
    let operands = vec![
        file2.into_os_string(),
        file3.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan2");
    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().times(true),
    )
    .expect("second copy");

    // Verify final file has exact same timestamp
    let final_mtime = FileTime::from_last_modification_time(&fs::metadata(&file3).expect("file3 meta"));
    assert_eq!(
        final_mtime, original_mtime,
        "nanosecond precision should be preserved through round trip"
    );
}

// ============================================================================
// --times Flag Behavior Tests
// ============================================================================

#[test]
fn times_flag_with_archive_mode() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"archive mode test").expect("write source");

    let atime = FileTime::from_unix_time(1_600_000_000, 111_000_000);
    let mtime = FileTime::from_unix_time(1_700_000_000, 222_000_000);
    set_file_times(&source, atime, mtime).expect("set source times");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Simulate archive mode: times + permissions
    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default()
            .times(true)
            .permissions(true),
    )
    .expect("copy succeeds");

    let dest_metadata = fs::metadata(&destination).expect("dest metadata");
    let dest_atime = FileTime::from_last_access_time(&dest_metadata);
    let dest_mtime = FileTime::from_last_modification_time(&dest_metadata);

    assert_eq!(dest_atime, atime);
    assert_eq!(dest_mtime, mtime);
}

#[test]
fn times_flag_on_directory() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source_dir");
    let dest_dir = temp.path().join("dest_dir");

    fs::create_dir(&source_dir).expect("create source dir");
    fs::write(source_dir.join("file.txt"), b"content").expect("write file");

    // Set directory mtime
    let dir_mtime = FileTime::from_unix_time(1_700_000_000, 333_000_000);
    set_file_mtime(&source_dir, dir_mtime).expect("set dir mtime");

    let operands = vec![
        source_dir.into_os_string(),
        dest_dir.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().times(true),
    )
    .expect("copy succeeds");

    let dest_metadata = fs::metadata(&dest_dir).expect("dest metadata");
    let dest_mtime = FileTime::from_last_modification_time(&dest_metadata);

    assert_eq!(dest_mtime, dir_mtime, "directory mtime should be preserved with --times");
}

#[cfg(unix)]
#[test]
fn times_flag_on_symlink() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let target = temp.path().join("target.txt");
    let source_link = temp.path().join("source_link");
    let dest_link = temp.path().join("dest_link");

    fs::write(&target, b"target content").expect("write target");
    symlink(&target, &source_link).expect("create source symlink");
    symlink(&target, &dest_link).expect("create dest symlink");

    // Set symlink mtime
    let link_atime = FileTime::from_unix_time(1_600_000_000, 444_000_000);
    let link_mtime = FileTime::from_unix_time(1_700_000_000, 555_000_000);
    filetime::set_symlink_file_times(&source_link, link_atime, link_mtime).expect("set link times");

    let source_meta = fs::symlink_metadata(&source_link).expect("source link meta");

    // Apply symlink metadata
    metadata::apply_symlink_metadata(&dest_link, &source_meta).expect("apply symlink metadata");

    let dest_meta = fs::symlink_metadata(&dest_link).expect("dest link meta");
    let dest_atime = FileTime::from_last_access_time(&dest_meta);
    let dest_mtime = FileTime::from_last_modification_time(&dest_meta);

    assert_eq!(dest_atime, link_atime, "symlink atime should be preserved");
    assert_eq!(dest_mtime, link_mtime, "symlink mtime should be preserved");
}

// ============================================================================
// Timestamp Comparison for Skip Decisions
// ============================================================================

#[test]
fn skip_file_when_timestamps_match() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    let content = b"identical content";
    fs::write(&source, content).expect("write source");
    fs::write(&destination, content).expect("write dest");

    // Set identical timestamps
    let mtime = FileTime::from_unix_time(1_700_000_000, 123_456_789);
    set_file_mtime(&source, mtime).expect("set source mtime");
    set_file_mtime(&destination, mtime).expect("set dest mtime");

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

    // File should be skipped because size and mtime match
    assert_eq!(summary.files_copied(), 0, "file should be skipped when timestamps match");
    assert_eq!(summary.regular_files_total(), 1);
}

#[test]
fn copy_file_when_timestamps_differ() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    let content = b"same content different time";
    fs::write(&source, content).expect("write source");
    fs::write(&destination, content).expect("write dest");

    // Set different timestamps
    let source_mtime = FileTime::from_unix_time(1_700_000_100, 0);
    let dest_mtime = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source, source_mtime).expect("set source mtime");
    set_file_mtime(&destination, dest_mtime).expect("set dest mtime");

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

    // File should be copied because timestamps differ
    assert_eq!(summary.files_copied(), 1, "file should be copied when timestamps differ");

    // Verify destination now has source's timestamp
    let final_mtime = FileTime::from_last_modification_time(&fs::metadata(&destination).expect("dest meta"));
    assert_eq!(final_mtime, source_mtime);
}

// 1ns difference is invisible to Windows NTFS (100ns resolution).
#[cfg(not(target_os = "windows"))]
#[test]
fn nanosecond_difference_triggers_copy() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    let content = b"content";
    fs::write(&source, content).expect("write source");
    fs::write(&destination, content).expect("write dest");

    // Set timestamps that differ only in nanoseconds
    let source_mtime = FileTime::from_unix_time(1_700_000_000, 1);  // 1 nanosecond
    let dest_mtime = FileTime::from_unix_time(1_700_000_000, 0);     // 0 nanoseconds
    set_file_mtime(&source, source_mtime).expect("set source mtime");
    set_file_mtime(&destination, dest_mtime).expect("set dest mtime");

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

    // File should be copied because nanoseconds differ
    assert_eq!(summary.files_copied(), 1, "1ns timestamp difference should trigger copy");
}

#[test]
fn modify_window_tolerates_small_differences() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    let content = b"content";
    fs::write(&source, content).expect("write source");
    fs::write(&destination, content).expect("write dest");

    // Set timestamps that differ by 0.5 seconds
    let source_mtime = FileTime::from_unix_time(1_700_000_000, 0);
    let dest_mtime = FileTime::from_unix_time(1_700_000_000, 500_000_000);
    set_file_mtime(&source, source_mtime).expect("set source mtime");
    set_file_mtime(&destination, dest_mtime).expect("set dest mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .times(true)
                .with_modify_window(Duration::from_secs(1)),  // 1 second tolerance
        )
        .expect("copy succeeds");

    // File should be skipped because 0.5s difference is within 1s window
    assert_eq!(summary.files_copied(), 0, "file should be skipped with modify_window tolerance");
}

// ============================================================================
// Edge Cases and Special Scenarios
// ============================================================================

#[test]
fn unix_epoch_timestamp_preserved() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"epoch test").expect("write source");

    // Set timestamp to Unix epoch (January 1, 1970)
    let epoch_time = FileTime::from_unix_time(0, 0);
    set_file_mtime(&source, epoch_time).expect("set epoch time");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().times(true),
    )
    .expect("copy succeeds");

    let dest_mtime = FileTime::from_last_modification_time(&fs::metadata(&destination).expect("dest meta"));
    assert_eq!(dest_mtime, epoch_time, "Unix epoch timestamp should be preserved");
}

// Windows NTFS truncates nanoseconds to 100ns intervals.
#[cfg(not(target_os = "windows"))]
#[test]
fn far_future_timestamp_preserved() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"future test").expect("write source");

    // Set timestamp to year 2100
    let future_time = FileTime::from_unix_time(4_102_444_800, 123_456_789);
    set_file_mtime(&source, future_time).expect("set future time");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().times(true),
    )
    .expect("copy succeeds");

    let dest_mtime = FileTime::from_last_modification_time(&fs::metadata(&destination).expect("dest meta"));
    assert_eq!(dest_mtime, future_time, "far future timestamp should be preserved");
}

// Windows NTFS truncates nanoseconds to 100ns intervals.
#[cfg(not(target_os = "windows"))]
#[test]
fn year_2038_boundary_timestamp_preserved() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"2038 boundary test").expect("write source");

    // Set timestamp at the Y2K38 boundary (2^31 - 1 seconds)
    let y2038_time = FileTime::from_unix_time(2_147_483_647, 999_999_999);
    set_file_mtime(&source, y2038_time).expect("set 2038 boundary time");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().times(true),
    )
    .expect("copy succeeds");

    let dest_mtime = FileTime::from_last_modification_time(&fs::metadata(&destination).expect("dest meta"));
    assert_eq!(dest_mtime, y2038_time, "Y2K38 boundary timestamp should be preserved");
}

#[test]
fn post_2038_timestamp_preserved() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"post 2038 test").expect("write source");

    // Set timestamp just after the Y2K38 boundary
    let post_2038_time = FileTime::from_unix_time(2_147_483_648, 0);
    set_file_mtime(&source, post_2038_time).expect("set post-2038 time");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().times(true),
    )
    .expect("copy succeeds");

    let dest_mtime = FileTime::from_last_modification_time(&fs::metadata(&destination).expect("dest meta"));
    assert_eq!(dest_mtime, post_2038_time, "post-Y2K38 timestamp should be preserved");
}

#[test]
fn empty_file_timestamp_preserved() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("empty_source.txt");
    let destination = temp.path().join("empty_dest.txt");

    fs::write(&source, b"").expect("write empty source");

    // Use 100ns-aligned nanoseconds for Windows NTFS compatibility.
    let mtime = FileTime::from_unix_time(1_700_000_000, 111_222_300);
    set_file_mtime(&source, mtime).expect("set mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().times(true),
    )
    .expect("copy succeeds");

    let dest_mtime = FileTime::from_last_modification_time(&fs::metadata(&destination).expect("dest meta"));
    assert_eq!(dest_mtime, mtime, "empty file timestamp should be preserved");
}

#[test]
fn large_file_timestamp_preserved() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("large_source.bin");
    let destination = temp.path().join("large_dest.bin");

    // Create a larger file (1MB)
    let large_content = vec![0xABu8; 1_000_000];
    fs::write(&source, &large_content).expect("write large source");

    let mtime = FileTime::from_unix_time(1_700_000_000, 999_000_000);
    set_file_mtime(&source, mtime).expect("set mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().times(true),
    )
    .expect("copy succeeds");

    let dest_mtime = FileTime::from_last_modification_time(&fs::metadata(&destination).expect("dest meta"));
    assert_eq!(dest_mtime, mtime, "large file timestamp should be preserved");
}

// ============================================================================
// Timestamp Interaction with Other Flags
// ============================================================================

#[test]
fn times_with_checksum_and_matching_timestamps() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    let content = b"identical content for checksum test";
    fs::write(&source, content).expect("write source");
    fs::write(&destination, content).expect("write dest");

    // Set matching timestamps
    let mtime = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source, mtime).expect("set source mtime");
    set_file_mtime(&destination, mtime).expect("set dest mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .times(true)
                .checksum(true),
        )
        .expect("copy succeeds");

    // With checksum enabled, should skip because content matches
    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_matched(), 1);
}

#[test]
fn times_with_update_flag() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"older source").expect("write source");
    fs::write(&destination, b"newer dest").expect("write dest");

    // Source is older than destination
    let older_mtime = FileTime::from_unix_time(1_600_000_000, 0);
    let newer_mtime = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source, older_mtime).expect("set source mtime");
    set_file_mtime(&destination, newer_mtime).expect("set dest mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .times(true)
                .update(true),
        )
        .expect("copy succeeds");

    // With --update, should skip because dest is newer
    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_skipped_newer(), 1);

    // Destination should retain its newer timestamp
    let final_mtime = FileTime::from_last_modification_time(&fs::metadata(&destination).expect("dest meta"));
    assert_eq!(final_mtime, newer_mtime);
}

#[test]
fn times_flag_in_dry_run_mode() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"dry run test").expect("write source");
    fs::write(&destination, b"original").expect("write dest");

    let source_mtime = FileTime::from_unix_time(1_700_000_000, 0);
    let original_dest_mtime = FileTime::from_unix_time(1_600_000_000, 0);
    set_file_mtime(&source, source_mtime).expect("set source mtime");
    set_file_mtime(&destination, original_dest_mtime).expect("set dest mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::DryRun,
            LocalCopyOptions::default().times(true),
        )
        .expect("dry run succeeds");

    // Dry run should report file would be copied
    assert_eq!(summary.files_copied(), 1);

    // But destination should be unchanged
    let final_mtime = FileTime::from_last_modification_time(&fs::metadata(&destination).expect("dest meta"));
    assert_eq!(final_mtime, original_dest_mtime, "dry run should not modify destination");
    assert_eq!(fs::read(&destination).expect("read dest"), b"original");
}

// ============================================================================
// Nested Directory Timestamp Tests
// ============================================================================

#[test]
fn nested_directory_timestamps_preserved() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let nested = source_root.join("level1").join("level2");
    fs::create_dir_all(&nested).expect("create nested dirs");
    fs::write(nested.join("file.txt"), b"nested content").expect("write file");

    // Set specific mtimes for each directory level
    let level2_mtime = FileTime::from_unix_time(1_500_000_000, 0);
    let level1_mtime = FileTime::from_unix_time(1_600_000_000, 0);
    let root_mtime = FileTime::from_unix_time(1_700_000_000, 0);

    set_file_mtime(&nested, level2_mtime).expect("set level2 mtime");
    set_file_mtime(source_root.join("level1"), level1_mtime).expect("set level1 mtime");
    set_file_mtime(&source_root, root_mtime).expect("set root mtime");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().times(true),
    )
    .expect("copy succeeds");

    // Verify each directory level has correct mtime
    let dest_root_mtime = FileTime::from_last_modification_time(&fs::metadata(&dest_root).expect("root meta"));
    let dest_level1_mtime = FileTime::from_last_modification_time(&fs::metadata(dest_root.join("level1")).expect("level1 meta"));
    let dest_level2_mtime = FileTime::from_last_modification_time(&fs::metadata(dest_root.join("level1/level2")).expect("level2 meta"));

    assert_eq!(dest_root_mtime, root_mtime, "root directory mtime should be preserved");
    assert_eq!(dest_level1_mtime, level1_mtime, "level1 directory mtime should be preserved");
    assert_eq!(dest_level2_mtime, level2_mtime, "level2 directory mtime should be preserved");
}

// ============================================================================
// Incremental Sync Timestamp Behavior
// ============================================================================

#[test]
fn incremental_sync_skips_unchanged_files() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");
    fs::create_dir_all(&dest_root).expect("create dest");

    // Create files with same content and timestamps
    let mtime = FileTime::from_unix_time(1_700_000_000, 0);

    for i in 1..=5 {
        let filename = format!("file{i}.txt");
        let content = format!("content {i}");

        let source_file = source_root.join(&filename);
        let dest_file = dest_root.join(&filename);

        fs::write(&source_file, content.as_bytes()).expect("write source");
        fs::write(&dest_file, content.as_bytes()).expect("write dest");

        set_file_mtime(&source_file, mtime).expect("set source mtime");
        set_file_mtime(&dest_file, mtime).expect("set dest mtime");
    }

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().times(true),
        )
        .expect("copy succeeds");

    // All files should be skipped
    assert_eq!(summary.files_copied(), 0, "unchanged files should be skipped");
    assert_eq!(summary.regular_files_total(), 5);
}

#[test]
fn incremental_sync_updates_changed_files_only() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");
    fs::create_dir_all(&dest_root).expect("create dest");

    let old_mtime = FileTime::from_unix_time(1_600_000_000, 0);
    let new_mtime = FileTime::from_unix_time(1_700_000_000, 0);

    // Create 3 unchanged files
    for i in 1..=3 {
        let filename = format!("unchanged{i}.txt");
        let content = format!("unchanged content {i}");

        let source_file = source_root.join(&filename);
        let dest_file = dest_root.join(&filename);

        fs::write(&source_file, content.as_bytes()).expect("write source");
        fs::write(&dest_file, content.as_bytes()).expect("write dest");

        set_file_mtime(&source_file, old_mtime).expect("set source mtime");
        set_file_mtime(&dest_file, old_mtime).expect("set dest mtime");
    }

    // Create 2 changed files (newer source)
    for i in 1..=2 {
        let filename = format!("changed{i}.txt");
        let source_file = source_root.join(&filename);
        let dest_file = dest_root.join(&filename);

        fs::write(&source_file, format!("new content {i}").as_bytes()).expect("write source");
        fs::write(&dest_file, format!("old content {i}").as_bytes()).expect("write dest");

        set_file_mtime(&source_file, new_mtime).expect("set source mtime");
        set_file_mtime(&dest_file, old_mtime).expect("set dest mtime");
    }

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().times(true),
        )
        .expect("copy succeeds");

    // Only 2 changed files should be copied
    assert_eq!(summary.files_copied(), 2, "only changed files should be copied");
    assert_eq!(summary.regular_files_total(), 5);

    // Verify changed files have new content and timestamp
    for i in 1..=2 {
        let dest_file = dest_root.join(format!("changed{i}.txt"));
        let content = fs::read(&dest_file).expect("read changed file");
        assert_eq!(content, format!("new content {i}").as_bytes());

        let final_mtime = FileTime::from_last_modification_time(&fs::metadata(&dest_file).expect("meta"));
        assert_eq!(final_mtime, new_mtime);
    }
}
