
#[test]
fn execute_skips_rewriting_identical_destination() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"identical").expect("write source");
    fs::write(&destination, b"identical").expect("write destination");

    let source_metadata = fs::metadata(&source).expect("source metadata");
    let source_mtime = FileTime::from_last_modification_time(&source_metadata);
    set_file_mtime(&destination, source_mtime).expect("align destination mtime");

    let mut dest_perms = fs::metadata(&destination)
        .expect("destination metadata")
        .permissions();
    dest_perms.set_readonly(true);
    fs::set_permissions(&destination, dest_perms).expect("set destination readonly");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().permissions(true).times(true),
        )
        .expect("copy succeeds without rewriting");

    let final_perms = fs::metadata(&destination)
        .expect("destination metadata")
        .permissions();
    assert!(
        !final_perms.readonly(),
        "destination permissions should match writable source"
    );
    assert_eq!(
        fs::read(&destination).expect("destination contents"),
        b"identical"
    );
    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_total(), 1);
    assert_eq!(summary.regular_files_matched(), 1);
}

#[test]
fn execute_without_times_rewrites_when_checksum_disabled() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"content").expect("write source");
    fs::write(&destination, b"content").expect("write destination");

    let original_mtime = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&destination, original_mtime).expect("set mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.regular_files_total(), 1);
    assert_eq!(summary.regular_files_matched(), 0);
    let metadata = fs::metadata(&destination).expect("dest metadata");
    let new_mtime = FileTime::from_last_modification_time(&metadata);
    assert_ne!(new_mtime, original_mtime);
}

#[test]
fn execute_without_times_skips_with_checksum() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"content").expect("write source");
    fs::write(&destination, b"content").expect("write destination");

    let preserved_mtime = FileTime::from_unix_time(1_700_100_000, 0);
    set_file_mtime(&destination, preserved_mtime).expect("set mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().checksum(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_total(), 1);
    assert_eq!(summary.regular_files_matched(), 1);
    let metadata = fs::metadata(&destination).expect("dest metadata");
    let final_mtime = FileTime::from_last_modification_time(&metadata);
    assert_eq!(final_mtime, preserved_mtime);
}

#[test]
fn execute_with_size_only_skips_same_size_different_content() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let target_root = temp.path().join("target");
    fs::create_dir_all(&source_root).expect("create source root");
    fs::create_dir_all(&target_root).expect("create target root");

    let source_path = source_root.join("file.txt");
    let dest_path = target_root.join("file.txt");
    fs::write(&source_path, b"abc").expect("write source");
    fs::write(&dest_path, b"xyz").expect("write destination");

    let operands = vec![
        source_path.into_os_string(),
        dest_path.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().size_only(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.bytes_copied(), 0);
    assert_eq!(fs::read(dest_path).expect("read destination"), b"xyz");
}

#[test]
fn execute_with_ignore_times_rewrites_matching_timestamps() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("destination.txt");

    fs::write(&source, b"newdata").expect("write source");
    fs::write(&destination, b"olddata").expect("write destination");

    let timestamp = FileTime::from_unix_time(1_700_200_000, 0);
    set_file_times(&source, timestamp, timestamp).expect("set source times");
    set_file_times(&destination, timestamp, timestamp).expect("set destination times");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().ignore_times(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(&destination).expect("read destination"),
        b"newdata"
    );
}

#[test]
fn execute_with_ignore_existing_skips_existing_destination() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let target_root = temp.path().join("target");
    fs::create_dir_all(&source_root).expect("create source root");
    fs::create_dir_all(&target_root).expect("create target root");

    let source_path = source_root.join("file.txt");
    let dest_path = target_root.join("file.txt");
    fs::write(&source_path, b"updated").expect("write source");
    fs::write(&dest_path, b"original").expect("write destination");

    let operands = vec![
        source_path.into_os_string(),
        dest_path.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().ignore_existing(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_total(), 1);
    assert_eq!(summary.regular_files_matched(), 0);
    assert_eq!(summary.regular_files_ignored_existing(), 1);
    assert_eq!(fs::read(dest_path).expect("read destination"), b"original");
}

#[test]
fn execute_with_ignore_missing_args_skips_absent_sources() {
    let temp = tempdir().expect("tempdir");
    let missing = temp.path().join("missing.txt");
    let destination_root = temp.path().join("dest");
    fs::create_dir_all(&destination_root).expect("create destination root");
    let destination = destination_root.join("output.txt");
    fs::write(&destination, b"existing").expect("write destination");

    let operands = vec![
        missing.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().ignore_missing_args(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.bytes_copied(), 0);
    assert_eq!(
        fs::read(destination).expect("read destination"),
        b"existing"
    );
}

#[test]
fn execute_with_delete_missing_args_removes_destination_entries() {
    let temp = tempdir().expect("tempdir");
    let missing = temp.path().join("absent.txt");
    let destination_root = temp.path().join("dest");
    fs::create_dir_all(&destination_root).expect("create destination root");
    let destination = destination_root.join("absent.txt");
    fs::write(&destination, b"stale").expect("write destination");

    let operands = vec![
        missing.into_os_string(),
        destination_root.into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .ignore_missing_args(true)
                .delete_missing_args(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.items_deleted(), 1);
    assert!(!destination.exists());
}

#[test]
fn execute_with_existing_only_skips_missing_entries() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let nested_dir = source_root.join("nested");
    fs::create_dir_all(&nested_dir).expect("create nested dir");
    fs::write(source_root.join("file.txt"), b"payload").expect("write file");
    fs::write(nested_dir.join("inner.txt"), b"nested").expect("write nested file");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create destination root");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let report = plan
        .execute_with_report(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .existing_only(true)
                .collect_events(true),
        )
        .expect("execution succeeds");
    let summary = report.summary();

    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_total(), 1);
    assert_eq!(summary.regular_files_skipped_missing(), 1);
    assert_eq!(summary.directories_total(), 2);
    assert_eq!(summary.directories_created(), 0);
    assert!(!dest_root.join("file.txt").exists());
    assert!(!dest_root.join("nested").exists());

    let records = report.records();
    assert!(records.iter().any(|record| {
        record.action() == &LocalCopyAction::SkippedMissingDestination
            && record.relative_path() == std::path::Path::new("file.txt")
    }));
    assert!(records.iter().any(|record| {
        record.action() == &LocalCopyAction::SkippedMissingDestination
            && record.relative_path() == std::path::Path::new("nested")
    }));
}

#[test]
fn execute_skips_files_smaller_than_min_size_limit() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("tiny.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"abc").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().min_file_size(Some(10)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_total(), 1);
    assert_eq!(summary.bytes_copied(), 0);
    assert!(!destination.exists());
}

#[test]
fn execute_skips_files_larger_than_max_size_limit() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("large.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, vec![0u8; 4096]).expect("write large source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().max_file_size(Some(2048)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_total(), 1);
    assert_eq!(summary.bytes_copied(), 0);
    assert!(!destination.exists());
}

#[test]
fn execute_copies_files_matching_size_boundaries() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("boundary.bin");
    let destination = temp.path().join("dest.bin");

    let payload = vec![0xAA; 2048];
    fs::write(&source, &payload).expect("write boundary source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .min_file_size(Some(2048))
                .max_file_size(Some(2048)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.regular_files_total(), 1);
    assert_eq!(summary.bytes_copied(), 2048);
    assert_eq!(fs::read(&destination).expect("read destination"), payload);
}

#[test]
fn execute_with_update_skips_newer_destination() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let target_root = temp.path().join("target");
    fs::create_dir_all(&source_root).expect("create source root");
    fs::create_dir_all(&target_root).expect("create target root");

    let source_path = source_root.join("file.txt");
    let dest_path = target_root.join("file.txt");
    fs::write(&source_path, b"updated").expect("write source");
    fs::write(&dest_path, b"existing").expect("write destination");

    let older = FileTime::from_unix_time(1_700_000_000, 0);
    let newer = FileTime::from_unix_time(1_700_000_100, 0);
    set_file_times(&source_path, older, older).expect("set source times");
    set_file_times(&dest_path, newer, newer).expect("set dest times");

    let operands = vec![
        source_path.into_os_string(),
        dest_path.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().update(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_total(), 1);
    assert_eq!(summary.regular_files_skipped_newer(), 1);
    assert_eq!(fs::read(dest_path).expect("read destination"), b"existing");
}

/// Tests that checksum mode correctly identifies identical files and skips copying.
///
/// This test exercises the checksum comparison path, which is parallelized when
/// the `parallel` feature is enabled. The test creates multiple files with
/// identical content at source and destination to verify:
/// 1. Files with matching checksums are skipped
/// 2. Files with different checksums are copied
/// 3. Summary statistics accurately reflect the operations
#[test]
fn execute_with_checksum_skips_matching_directory_contents() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let target_root = temp.path().join("target");
    fs::create_dir_all(&source_root).expect("create source root");
    fs::create_dir_all(&target_root).expect("create target root");

    // Create multiple files - some identical, some different
    let identical_content = b"identical content here";
    let different_source = b"different source data!";
    let different_dest = b"different dest content";

    // Identical files (should be skipped)
    fs::write(source_root.join("same1.txt"), identical_content).expect("write same1 source");
    fs::write(target_root.join("same1.txt"), identical_content).expect("write same1 dest");

    fs::write(source_root.join("same2.txt"), identical_content).expect("write same2 source");
    fs::write(target_root.join("same2.txt"), identical_content).expect("write same2 dest");

    // Different content file (same size, should be copied)
    fs::write(source_root.join("diff.txt"), different_source).expect("write diff source");
    fs::write(target_root.join("diff.txt"), different_dest).expect("write diff dest");

    // New file (no destination, should be copied)
    fs::write(source_root.join("new.txt"), b"brand new file").expect("write new source");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, target_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().checksum(true).recursive(true),
        )
        .expect("copy succeeds");

    // Verify results
    assert_eq!(summary.regular_files_total(), 4, "should process 4 files");
    assert_eq!(
        summary.regular_files_matched(),
        2,
        "2 identical files should match"
    );
    assert_eq!(summary.files_copied(), 2, "2 different/new files should copy");

    // Verify file contents
    assert_eq!(
        fs::read(target_root.join("same1.txt")).expect("read same1"),
        identical_content
    );
    assert_eq!(
        fs::read(target_root.join("same2.txt")).expect("read same2"),
        identical_content
    );
    assert_eq!(
        fs::read(target_root.join("diff.txt")).expect("read diff"),
        different_source // source content should overwrite destination
    );
    assert_eq!(
        fs::read(target_root.join("new.txt")).expect("read new"),
        b"brand new file"
    );
}

// ==================== Size Comparison Edge Cases ====================

#[test]
fn execute_with_size_only_copies_different_size_files() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"longer content").expect("write source");
    fs::write(&destination, b"short").expect("write dest");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().size_only(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read"), b"longer content");
}

#[test]
fn execute_with_size_only_handles_empty_vs_nonempty() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"non-empty").expect("write source");
    fs::write(&destination, b"").expect("write empty dest");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().size_only(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read"), b"non-empty");
}

// ==================== Update Mode Edge Cases ====================

#[test]
fn execute_with_update_copies_when_destination_older() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"updated").expect("write source");
    fs::write(&destination, b"stale").expect("write dest");

    let older = FileTime::from_unix_time(1_700_000_000, 0);
    let newer = FileTime::from_unix_time(1_700_000_100, 0);
    set_file_times(&source, newer, newer).expect("set source times");
    set_file_times(&destination, older, older).expect("set dest times");

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

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read"), b"updated");
}

#[test]
fn execute_with_update_copies_when_destination_missing() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"new file").expect("write source");

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

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read"), b"new file");
}

// ==================== Ignore Existing Edge Cases ====================

#[test]
fn execute_with_ignore_existing_creates_new_destination() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"new content").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().ignore_existing(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read"), b"new content");
}

// ==================== Min/Max Size Combined Tests ====================

#[test]
fn execute_with_min_max_size_filters_correctly() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    fs::write(source_root.join("tiny.txt"), b"ab").expect("write tiny");
    fs::write(source_root.join("small.txt"), b"1234567890").expect("write small");
    fs::write(source_root.join("medium.txt"), vec![0u8; 100]).expect("write medium");
    fs::write(source_root.join("large.txt"), vec![0u8; 500]).expect("write large");

    let dest_root = temp.path().join("dest");
    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .min_file_size(Some(5))
                .max_file_size(Some(200)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 2);
    assert!(!dest_root.join("tiny.txt").exists());
    assert!(dest_root.join("small.txt").exists());
    assert!(dest_root.join("medium.txt").exists());
    assert!(!dest_root.join("large.txt").exists());
}

// ==================== Checksum Edge Cases ====================

#[test]
fn execute_with_checksum_handles_empty_files() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"").expect("write empty source");
    fs::write(&destination, b"").expect("write empty dest");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().checksum(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_matched(), 1);
}

#[test]
fn execute_with_checksum_copies_different_empty_and_nonempty() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"not empty").expect("write source");
    fs::write(&destination, b"").expect("write empty dest");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().checksum(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read"), b"not empty");
}

// ==================== Modify Window Tests ====================

#[test]
fn execute_skips_within_modify_window() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    let content = b"modify window test";
    fs::write(&source, content).expect("write source");
    fs::write(&destination, content).expect("write dest");

    let base_time = FileTime::from_unix_time(1_700_000_000, 0);
    let slightly_different = FileTime::from_unix_time(1_700_000_001, 0);
    set_file_mtime(&source, base_time).expect("set source mtime");
    set_file_mtime(&destination, slightly_different).expect("set dest mtime");

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
                .with_modify_window(Duration::from_secs(2)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_matched(), 1);
}

// ==================== Filter/Exclude Tests ====================

#[test]
fn execute_with_filter_excludes_matching_files() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
    fs::write(source_root.join("skip.bak"), b"skip").expect("write skip");
    fs::write(source_root.join("also_keep.txt"), b"also").expect("write also_keep");

    let dest_root = temp.path().join("dest");
    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let program = FilterProgram::new([FilterProgramEntry::Rule(FilterRule::exclude("*.bak"))])
        .expect("compile filter");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().with_filter_program(Some(program)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 2);
    assert!(dest_root.join("keep.txt").exists());
    assert!(dest_root.join("also_keep.txt").exists());
    assert!(!dest_root.join("skip.bak").exists());
}

// ==================== Multiple Sources Skip Tests ====================

#[test]
fn execute_with_multiple_sources_and_ignore_existing() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    fs::write(source_root.join("new.txt"), b"new").expect("write new");
    fs::write(source_root.join("exists.txt"), b"updated").expect("write exists");
    fs::write(source_root.join("another_new.txt"), b"also new").expect("write another");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest");
    fs::write(dest_root.join("exists.txt"), b"original").expect("write existing");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().ignore_existing(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 2);
    assert_eq!(summary.regular_files_ignored_existing(), 1);
    assert_eq!(fs::read(dest_root.join("new.txt")).expect("read"), b"new");
    assert_eq!(fs::read(dest_root.join("another_new.txt")).expect("read"), b"also new");
    assert_eq!(fs::read(dest_root.join("exists.txt")).expect("read"), b"original");
}

// ==================== Dry Run Skip Tests ====================

#[test]
fn execute_dry_run_reports_skipped_files_as_matched() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    let content = b"identical";
    fs::write(&source, content).expect("write source");
    fs::write(&destination, content).expect("write dest");

    let mtime = FileTime::from_last_modification_time(&fs::metadata(&source).expect("meta"));
    set_file_mtime(&destination, mtime).expect("align times");

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

    // In dry run mode, identical files are counted and matched
    assert_eq!(summary.regular_files_total(), 1);
    // File content remains unchanged since it's dry run
    assert_eq!(fs::read(&destination).expect("read"), content);
}
