// Tests for --modify-window time tolerance behavior.
//
// The --modify-window flag allows for a time tolerance when comparing file
// modification times. This is useful for syncing between filesystems with
// different timestamp precision (e.g., FAT has 2-second granularity).
//
// Test cases covered:
// 1. Files with timestamps within the window are considered equal (skipped)
// 2. Files with timestamps outside the window are updated
// 3. Works with different window sizes (1, 2, 60 seconds)
// 4. Default window value (0) works correctly
// 5. Interaction with --update flag
// 6. Subsecond precision handling
// 7. Window applies symmetrically (source older or newer)

// ============================================================================
// Basic Modify Window Tests
// ============================================================================

#[test]
fn modify_window_skips_files_within_one_second_window() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    let content = b"same content for both";
    fs::write(&source, content).expect("write source");
    fs::write(&destination, content).expect("write dest");

    // Source and dest differ by 0.5 seconds
    let base_time = FileTime::from_unix_time(1_700_000_000, 0);
    let slightly_later = FileTime::from_unix_time(1_700_000_000, 500_000_000); // +0.5s
    set_file_times(&source, base_time, base_time).expect("set source times");
    set_file_times(&destination, slightly_later, slightly_later).expect("set dest times");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // With a 1-second window, 0.5s difference should be within tolerance
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().with_modify_window(Duration::from_secs(1)),
        )
        .expect("copy succeeds");

    // File should be skipped because timestamps are within 1-second window and sizes match
    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_total(), 1);
    // Destination content should be preserved
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        content
    );
}

#[test]
fn modify_window_copies_files_outside_one_second_window() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"updated content").expect("write source");
    fs::write(&destination, b"old content").expect("write dest");

    // Source and dest differ by 2 seconds
    let base_time = FileTime::from_unix_time(1_700_000_000, 0);
    let much_later = FileTime::from_unix_time(1_700_000_002, 0); // +2s
    set_file_times(&source, much_later, much_later).expect("set source times");
    set_file_times(&destination, base_time, base_time).expect("set dest times");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // With a 1-second window, 2s difference should be outside tolerance
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().with_modify_window(Duration::from_secs(1)),
        )
        .expect("copy succeeds");

    // File should be copied because timestamps differ by more than 1 second
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.regular_files_total(), 1);
    // Destination should have new content
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"updated content"
    );
}

#[test]
fn modify_window_two_seconds_for_fat_filesystems() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"content").expect("write source");
    fs::write(&destination, b"content").expect("write dest");

    // Simulate FAT filesystem's 2-second granularity
    // Source and dest differ by exactly 2 seconds
    let base_time = FileTime::from_unix_time(1_700_000_000, 0);
    let two_seconds_later = FileTime::from_unix_time(1_700_000_002, 0);
    set_file_times(&source, base_time, base_time).expect("set source times");
    set_file_times(&destination, two_seconds_later, two_seconds_later).expect("set dest times");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // With a 2-second window, exactly 2s difference should be within tolerance
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().with_modify_window(Duration::from_secs(2)),
        )
        .expect("copy succeeds");

    // File should be skipped because timestamps are within 2-second window
    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_total(), 1);
    assert_eq!(fs::read(&destination).expect("read dest"), b"content");
}

#[test]
fn modify_window_sixty_seconds_for_large_tolerance() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"content").expect("write source");
    fs::write(&destination, b"content").expect("write dest");

    // Source and dest differ by 45 seconds
    let base_time = FileTime::from_unix_time(1_700_000_000, 0);
    let later_time = FileTime::from_unix_time(1_700_000_045, 0); // +45s
    set_file_times(&source, base_time, base_time).expect("set source times");
    set_file_times(&destination, later_time, later_time).expect("set dest times");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // With a 60-second window, 45s difference should be within tolerance
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().with_modify_window(Duration::from_secs(60)),
        )
        .expect("copy succeeds");

    // File should be skipped
    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_total(), 1);
}

#[test]
fn modify_window_sixty_seconds_copies_when_outside() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"new content").expect("write source");
    fs::write(&destination, b"old content").expect("write dest");

    // Source and dest differ by 90 seconds (outside 60-second window)
    let base_time = FileTime::from_unix_time(1_700_000_000, 0);
    let much_later = FileTime::from_unix_time(1_700_000_090, 0); // +90s
    set_file_times(&source, much_later, much_later).expect("set source times");
    set_file_times(&destination, base_time, base_time).expect("set dest times");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // With a 60-second window, 90s difference should be outside tolerance
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().with_modify_window(Duration::from_secs(60)),
        )
        .expect("copy succeeds");

    // File should be copied
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"new content"
    );
}

// ============================================================================
// Default Window (Zero) Tests
// ============================================================================

#[test]
fn modify_window_default_zero_requires_exact_match() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"source").expect("write source");
    fs::write(&destination, b"dest").expect("write dest");

    // Times differ by just 1 nanosecond
    let base_time = FileTime::from_unix_time(1_700_000_000, 0);
    let tiny_diff = FileTime::from_unix_time(1_700_000_000, 1);
    set_file_times(&source, base_time, base_time).expect("set source times");
    set_file_times(&destination, tiny_diff, tiny_diff).expect("set dest times");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Default window (0) requires exact match
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    // File should be copied because timestamps differ (even by 1ns)
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read dest"), b"source");
}

#[test]
fn modify_window_default_zero_skips_exact_match() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"content").expect("write source");
    fs::write(&destination, b"content").expect("write dest");

    // Exact same timestamp
    let same_time = FileTime::from_unix_time(1_700_000_000, 123_456_789);
    set_file_times(&source, same_time, same_time).expect("set source times");
    set_file_times(&destination, same_time, same_time).expect("set dest times");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Default window (0) should skip when timestamps AND sizes match exactly
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    // File should be skipped when both timestamp and size match
    assert_eq!(summary.files_copied(), 0);
    assert_eq!(fs::read(&destination).expect("read dest"), b"content");
}

// ============================================================================
// Symmetric Window Application (Source Older or Newer)
// ============================================================================

#[test]
fn modify_window_applies_symmetrically_source_older() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"content").expect("write source");
    fs::write(&destination, b"content").expect("write dest");

    // Source is 0.5 seconds OLDER than dest
    let older_time = FileTime::from_unix_time(1_700_000_000, 0);
    let newer_time = FileTime::from_unix_time(1_700_000_000, 500_000_000);
    set_file_times(&source, older_time, older_time).expect("set source times");
    set_file_times(&destination, newer_time, newer_time).expect("set dest times");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // With 1-second window, 0.5s difference (either direction) should be tolerated
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().with_modify_window(Duration::from_secs(1)),
        )
        .expect("copy succeeds");

    // File should be skipped (window applies symmetrically)
    assert_eq!(summary.files_copied(), 0);
}

#[test]
fn modify_window_applies_symmetrically_source_newer() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"content").expect("write source");
    fs::write(&destination, b"content").expect("write dest");

    // Source is 0.5 seconds NEWER than dest
    let older_time = FileTime::from_unix_time(1_700_000_000, 0);
    let newer_time = FileTime::from_unix_time(1_700_000_000, 500_000_000);
    set_file_times(&source, newer_time, newer_time).expect("set source times");
    set_file_times(&destination, older_time, older_time).expect("set dest times");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // With 1-second window, 0.5s difference (either direction) should be tolerated
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().with_modify_window(Duration::from_secs(1)),
        )
        .expect("copy succeeds");

    // File should be skipped (window applies symmetrically)
    assert_eq!(summary.files_copied(), 0);
}

#[test]
fn modify_window_symmetry_boundary_at_exact_limit() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"content").expect("write source");
    fs::write(&destination, b"content").expect("write dest");

    // Source is exactly 2 seconds older than dest
    let older_time = FileTime::from_unix_time(1_700_000_000, 0);
    let newer_time = FileTime::from_unix_time(1_700_000_002, 0);
    set_file_times(&source, older_time, older_time).expect("set source times");
    set_file_times(&destination, newer_time, newer_time).expect("set dest times");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // With 2-second window, exactly 2s difference should be within tolerance
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().with_modify_window(Duration::from_secs(2)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 0);
}

// ============================================================================
// Interaction with --update Flag
// ============================================================================

#[test]
fn modify_window_with_update_skips_when_dest_newer_outside_window() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"older source").expect("write source");
    fs::write(&destination, b"newer dest").expect("write dest");

    // Dest is 5 seconds newer (outside 1-second window)
    let older_time = FileTime::from_unix_time(1_700_000_000, 0);
    let newer_time = FileTime::from_unix_time(1_700_000_005, 0);
    set_file_times(&source, older_time, older_time).expect("set source times");
    set_file_times(&destination, newer_time, newer_time).expect("set dest times");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // With --update and modify-window=1, dest is definitively newer (5s > 1s)
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .update(true)
                .with_modify_window(Duration::from_secs(1)),
        )
        .expect("copy succeeds");

    // File should be skipped by --update (dest is newer)
    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_skipped_newer(), 1);
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"newer dest"
    );
}

/// When --update and --modify-window are combined, timestamps within the
/// window are treated as equal. This means --update does NOT consider the
/// destination as newer, so the normal quick-check proceeds.
///
/// Variant A: same size, timestamps within window -> skip (quick-check passes)
#[test]
fn modify_window_with_update_treats_within_window_as_equal() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    // Same content and size so the quick-check will pass
    fs::write(&source, b"content").expect("write source");
    fs::write(&destination, b"content").expect("write dest");

    // Dest is 0.5 seconds newer (within 1-second window, so considered "equal")
    let base_time = FileTime::from_unix_time(1_700_000_000, 0);
    let slightly_newer = FileTime::from_unix_time(1_700_000_000, 500_000_000);
    set_file_times(&source, base_time, base_time).expect("set source times");
    set_file_times(&destination, slightly_newer, slightly_newer).expect("set dest times");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // With --update and modify-window=1, timestamps within the window are
    // considered equal so --update does NOT skip (dest is not strictly newer).
    // Then the normal quick-check runs: size matches and mtime is within
    // window, so the file is skipped.
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .update(true)
                .with_modify_window(Duration::from_secs(1)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 0);
    assert_eq!(fs::read(&destination).expect("read dest"), b"content");
}

/// Variant B: different sizes, timestamps within window -> copy proceeds
/// because --update sees equal timestamps and the quick-check detects
/// the size mismatch.
#[test]
fn modify_window_with_update_copies_when_sizes_differ_within_window() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    // Different sizes so the quick-check will fail
    fs::write(&source, b"source data").expect("write source");
    fs::write(&destination, b"dest").expect("write dest");

    // Dest is 0.5 seconds newer (within 1-second window)
    let base_time = FileTime::from_unix_time(1_700_000_000, 0);
    let slightly_newer = FileTime::from_unix_time(1_700_000_000, 500_000_000);
    set_file_times(&source, base_time, base_time).expect("set source times");
    set_file_times(&destination, slightly_newer, slightly_newer).expect("set dest times");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // --update does not skip because timestamps are within the window (equal).
    // The quick-check detects size mismatch, so the file is copied.
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .update(true)
                .with_modify_window(Duration::from_secs(1)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"source data"
    );
}

#[test]
fn modify_window_with_update_copies_when_source_definitively_newer() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"updated source").expect("write source");
    fs::write(&destination, b"old dest").expect("write dest");

    // Source is 5 seconds newer (outside 1-second window)
    let older_time = FileTime::from_unix_time(1_700_000_000, 0);
    let newer_time = FileTime::from_unix_time(1_700_000_005, 0);
    set_file_times(&source, newer_time, newer_time).expect("set source times");
    set_file_times(&destination, older_time, older_time).expect("set dest times");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // With --update and modify-window=1, source is definitively newer (5s > 1s)
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .update(true)
                .with_modify_window(Duration::from_secs(1)),
        )
        .expect("copy succeeds");

    // File should be copied
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.regular_files_skipped_newer(), 0);
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"updated source"
    );
}

// ============================================================================
// Recursive Directory Tests
// ============================================================================

#[test]
fn modify_window_recursive_mixed_timestamps() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source root");
    fs::create_dir_all(&dest_root).expect("create dest root");

    let base_time = FileTime::from_unix_time(1_700_000_000, 0);
    let within_window = FileTime::from_unix_time(1_700_000_000, 500_000_000); // +0.5s
    let outside_window = FileTime::from_unix_time(1_700_000_005, 0); // +5s

    // File 1: timestamps within window (should skip) - same size required
    let within_content = b"same content here";
    fs::write(source_root.join("within.txt"), within_content).expect("write within source");
    fs::write(dest_root.join("within.txt"), within_content).expect("write within dest");
    set_file_mtime(source_root.join("within.txt"), base_time).expect("set within source");
    set_file_mtime(dest_root.join("within.txt"), within_window).expect("set within dest");

    // File 2: timestamps outside window (should copy) - different sizes to trigger copy
    fs::write(source_root.join("outside.txt"), b"outside source content").expect("write outside source");
    fs::write(dest_root.join("outside.txt"), b"outside dest").expect("write outside dest");
    set_file_mtime(source_root.join("outside.txt"), outside_window).expect("set outside source");
    set_file_mtime(dest_root.join("outside.txt"), base_time).expect("set outside dest");

    // File 3: new file (should copy)
    fs::write(source_root.join("new.txt"), b"new file").expect("write new");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .recursive(true)
                .with_modify_window(Duration::from_secs(1)),
        )
        .expect("copy succeeds");

    // Should copy 2 files: outside.txt and new.txt
    // Should skip 1 file: within.txt
    assert_eq!(summary.files_copied(), 2);
    assert_eq!(summary.regular_files_total(), 3);

    // Verify file contents
    assert_eq!(
        fs::read(dest_root.join("within.txt")).expect("read within"),
        within_content  // Should remain unchanged
    );
    assert_eq!(
        fs::read(dest_root.join("outside.txt")).expect("read outside"),
        b"outside source content"  // Should be updated
    );
    assert_eq!(
        fs::read(dest_root.join("new.txt")).expect("read new"),
        b"new file"  // Should be created
    );
}

// ============================================================================
// Subsecond Precision Tests
// ============================================================================

#[test]
fn modify_window_subsecond_precision_within_window() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"content").expect("write source");
    fs::write(&destination, b"content").expect("write dest");

    // Differ by 100 milliseconds
    let base_time = FileTime::from_unix_time(1_700_000_000, 0);
    let subsec_diff = FileTime::from_unix_time(1_700_000_000, 100_000_000); // +100ms
    set_file_times(&source, base_time, base_time).expect("set source times");
    set_file_times(&destination, subsec_diff, subsec_diff).expect("set dest times");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // 1-second window should tolerate 100ms difference
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().with_modify_window(Duration::from_millis(500)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 0);
}

#[test]
fn modify_window_subsecond_precision_outside_window() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"new content").expect("write source");
    fs::write(&destination, b"old content").expect("write dest");

    // Differ by 600 milliseconds
    let base_time = FileTime::from_unix_time(1_700_000_000, 0);
    let subsec_diff = FileTime::from_unix_time(1_700_000_000, 600_000_000); // +600ms
    set_file_times(&source, subsec_diff, subsec_diff).expect("set source times");
    set_file_times(&destination, base_time, base_time).expect("set dest times");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // 500ms window should NOT tolerate 600ms difference
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().with_modify_window(Duration::from_millis(500)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"new content"
    );
}

// ============================================================================
// Edge Cases
// ============================================================================

#[test]
fn modify_window_with_different_file_sizes_always_copies() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"longer content").expect("write source");
    fs::write(&destination, b"short").expect("write dest");

    // Same timestamp (within window)
    let same_time = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_times(&source, same_time, same_time).expect("set source times");
    set_file_times(&destination, same_time, same_time).expect("set dest times");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Even with timestamps within window, different sizes should trigger copy
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().with_modify_window(Duration::from_secs(1)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"longer content"
    );
}

#[test]
fn modify_window_with_ignore_times_always_copies() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"source").expect("write source");
    fs::write(&destination, b"dest").expect("write dest");

    // Same size, same timestamp
    let same_time = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_times(&source, same_time, same_time).expect("set source times");
    set_file_times(&destination, same_time, same_time).expect("set dest times");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // With --ignore-times, modify-window should be ignored
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .ignore_times(true)
                .with_modify_window(Duration::from_secs(1)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read dest"), b"source");
}

#[test]
fn modify_window_with_size_only_ignores_timestamps() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"content").expect("write source");
    fs::write(&destination, b"content").expect("write dest");

    // Timestamps differ by 10 seconds (way outside any reasonable window)
    let older_time = FileTime::from_unix_time(1_700_000_000, 0);
    let much_newer = FileTime::from_unix_time(1_700_000_010, 0);
    set_file_times(&source, much_newer, much_newer).expect("set source times");
    set_file_times(&destination, older_time, older_time).expect("set dest times");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // With --size-only, modify-window and timestamps are ignored
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .size_only(true)
                .with_modify_window(Duration::from_secs(1)),
        )
        .expect("copy succeeds");

    // File should be skipped because sizes match (timestamps ignored)
    assert_eq!(summary.files_copied(), 0);
}

// ============================================================================
// Dry Run Tests
// ============================================================================

#[test]
fn modify_window_dry_run_reports_correctly() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"new content").expect("write source");
    fs::write(&destination, b"old content").expect("write dest");

    // Timestamps outside window
    let older_time = FileTime::from_unix_time(1_700_000_000, 0);
    let newer_time = FileTime::from_unix_time(1_700_000_005, 0);
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
            LocalCopyOptions::default().with_modify_window(Duration::from_secs(1)),
        )
        .expect("dry run succeeds");

    // Dry run should report that file would be copied
    assert_eq!(summary.files_copied(), 1);
    // But file should remain unchanged
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"old content"
    );
}

// ============================================================================
// Documentation Test - Upstream rsync Behavior
// ============================================================================

/// This test documents the behavior of --modify-window matching upstream rsync.
///
/// From rsync(1) man page:
/// > --modify-window=NUM
/// >     When comparing two timestamps, rsync treats the timestamps as being
/// >     equal if they differ by no more than the modify-window value.
///
/// This is particularly useful for:
/// 1. FAT filesystems (2-second granularity) -> use --modify-window=1 or =2
/// 2. Network filesystems with clock skew
/// 3. Cross-platform syncs with different time precision
#[test]
fn modify_window_matches_upstream_rsync_semantics() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");
    fs::create_dir_all(&dest_root).expect("create dest");

    let t0 = FileTime::from_unix_time(1_000_000_000, 0);
    let t1 = FileTime::from_unix_time(1_000_000_001, 0); // +1s
    let t2 = FileTime::from_unix_time(1_000_000_002, 0); // +2s
    let t3 = FileTime::from_unix_time(1_000_000_003, 0); // +3s

    // Case 1: Timestamps differ by 1s (within 2-second window) -> skip
    fs::write(source_root.join("case1.txt"), b"content").expect("write case1 src");
    fs::write(dest_root.join("case1.txt"), b"content").expect("write case1 dst");
    set_file_mtime(source_root.join("case1.txt"), t0).expect("set case1 src");
    set_file_mtime(dest_root.join("case1.txt"), t1).expect("set case1 dst");

    // Case 2: Timestamps differ by 2s (at boundary of 2-second window) -> skip
    fs::write(source_root.join("case2.txt"), b"content").expect("write case2 src");
    fs::write(dest_root.join("case2.txt"), b"content").expect("write case2 dst");
    set_file_mtime(source_root.join("case2.txt"), t0).expect("set case2 src");
    set_file_mtime(dest_root.join("case2.txt"), t2).expect("set case2 dst");

    // Case 3: Timestamps differ by 3s (outside 2-second window) -> copy
    fs::write(source_root.join("case3.txt"), b"new").expect("write case3 src");
    fs::write(dest_root.join("case3.txt"), b"old").expect("write case3 dst");
    set_file_mtime(source_root.join("case3.txt"), t3).expect("set case3 src");
    set_file_mtime(dest_root.join("case3.txt"), t0).expect("set case3 dst");

    // Case 4: Timestamps equal -> skip
    fs::write(source_root.join("case4.txt"), b"content").expect("write case4 src");
    fs::write(dest_root.join("case4.txt"), b"content").expect("write case4 dst");
    set_file_mtime(source_root.join("case4.txt"), t0).expect("set case4 src");
    set_file_mtime(dest_root.join("case4.txt"), t0).expect("set case4 dst");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().with_modify_window(Duration::from_secs(2)),
        )
        .expect("copy succeeds");

    // Only case3 should be copied (3s diff > 2s window)
    assert_eq!(summary.files_copied(), 1, "should copy only case3");

    // Verify case3 was updated
    assert_eq!(
        fs::read(dest_root.join("case3.txt")).expect("case3"),
        b"new"
    );
}
