
#[test]
fn compare_dest_skips_identical_file() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let compare_dir = temp.path().join("compare");
    let dest_dir = temp.path().join("dest");

    fs::create_dir_all(&source_dir).expect("create source");
    fs::create_dir_all(&compare_dir).expect("create compare");
    fs::create_dir_all(&dest_dir).expect("create dest");

    let source_file = source_dir.join("file.txt");
    let compare_file = compare_dir.join("file.txt");
    let dest_file = dest_dir.join("file.txt");

    fs::write(&source_file, b"identical content").expect("write source");
    fs::write(&compare_file, b"identical content").expect("write compare");

    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source_file, timestamp).expect("set source mtime");
    set_file_mtime(&compare_file, timestamp).expect("set compare mtime");

    let operands = vec![
        source_file.into_os_string(),
        dest_file.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .times(true)
        .push_reference_directory(ReferenceDirectory::new(
            ReferenceDirectoryKind::Compare,
            &compare_dir,
        ));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("execution succeeds");

    assert!(!dest_file.exists(), "file should not be created when identical to compare-dest");
    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_matched(), 1);
}

#[test]
fn compare_dest_transfers_different_file() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let compare_dir = temp.path().join("compare");
    let dest_dir = temp.path().join("dest");

    fs::create_dir_all(&source_dir).expect("create source");
    fs::create_dir_all(&compare_dir).expect("create compare");
    fs::create_dir_all(&dest_dir).expect("create dest");

    let source_file = source_dir.join("file.txt");
    let compare_file = compare_dir.join("file.txt");
    let dest_file = dest_dir.join("file.txt");

    fs::write(&source_file, b"source content").expect("write source");
    fs::write(&compare_file, b"compare content").expect("write compare");

    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source_file, timestamp).expect("set source mtime");
    set_file_mtime(&compare_file, timestamp).expect("set compare mtime");

    let operands = vec![
        source_file.into_os_string(),
        dest_file.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .times(true)
        .push_reference_directory(ReferenceDirectory::new(
            ReferenceDirectoryKind::Compare,
            &compare_dir,
        ));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("execution succeeds");

    assert!(dest_file.exists(), "file should be created when different from compare-dest");
    assert_eq!(fs::read(&dest_file).expect("read dest"), b"source content");
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.regular_files_matched(), 0);
}

#[test]
fn compare_dest_transfers_when_mtime_differs() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let compare_dir = temp.path().join("compare");
    let dest_dir = temp.path().join("dest");

    fs::create_dir_all(&source_dir).expect("create source");
    fs::create_dir_all(&compare_dir).expect("create compare");
    fs::create_dir_all(&dest_dir).expect("create dest");

    let source_file = source_dir.join("file.txt");
    let compare_file = compare_dir.join("file.txt");
    let dest_file = dest_dir.join("file.txt");

    fs::write(&source_file, b"identical content").expect("write source");
    fs::write(&compare_file, b"identical content").expect("write compare");

    let source_timestamp = FileTime::from_unix_time(1_700_000_100, 0);
    let compare_timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source_file, source_timestamp).expect("set source mtime");
    set_file_mtime(&compare_file, compare_timestamp).expect("set compare mtime");

    let operands = vec![
        source_file.into_os_string(),
        dest_file.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .times(true)
        .push_reference_directory(ReferenceDirectory::new(
            ReferenceDirectoryKind::Compare,
            &compare_dir,
        ));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("execution succeeds");

    assert!(dest_file.exists(), "file should be created when mtime differs");
    assert_eq!(fs::read(&dest_file).expect("read dest"), b"identical content");
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.regular_files_matched(), 0);
}

#[test]
fn compare_dest_transfers_file_not_in_compare() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let compare_dir = temp.path().join("compare");
    let dest_dir = temp.path().join("dest");

    fs::create_dir_all(&source_dir).expect("create source");
    fs::create_dir_all(&compare_dir).expect("create compare");
    fs::create_dir_all(&dest_dir).expect("create dest");

    let source_file = source_dir.join("file.txt");
    let dest_file = dest_dir.join("file.txt");

    fs::write(&source_file, b"new file").expect("write source");

    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source_file, timestamp).expect("set source mtime");

    let operands = vec![
        source_file.into_os_string(),
        dest_file.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .times(true)
        .push_reference_directory(ReferenceDirectory::new(
            ReferenceDirectoryKind::Compare,
            &compare_dir,
        ));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("execution succeeds");

    assert!(dest_file.exists(), "file should be created when not in compare-dest");
    assert_eq!(fs::read(&dest_file).expect("read dest"), b"new file");
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.regular_files_matched(), 0);
}

#[test]
fn compare_dest_multiple_directories_first_match_wins() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let compare1_dir = temp.path().join("compare1");
    let compare2_dir = temp.path().join("compare2");
    let dest_dir = temp.path().join("dest");

    fs::create_dir_all(&source_dir).expect("create source");
    fs::create_dir_all(&compare1_dir).expect("create compare1");
    fs::create_dir_all(&compare2_dir).expect("create compare2");
    fs::create_dir_all(&dest_dir).expect("create dest");

    let source_file = source_dir.join("file.txt");
    let compare1_file = compare1_dir.join("file.txt");
    let compare2_file = compare2_dir.join("file.txt");
    let dest_file = dest_dir.join("file.txt");

    fs::write(&source_file, b"content").expect("write source");
    fs::write(&compare1_file, b"content").expect("write compare1");
    fs::write(&compare2_file, b"different").expect("write compare2");

    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source_file, timestamp).expect("set source mtime");
    set_file_mtime(&compare1_file, timestamp).expect("set compare1 mtime");
    set_file_mtime(&compare2_file, timestamp).expect("set compare2 mtime");

    let operands = vec![
        source_file.into_os_string(),
        dest_file.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .times(true)
        .extend_reference_directories([
            ReferenceDirectory::new(ReferenceDirectoryKind::Compare, &compare1_dir),
            ReferenceDirectory::new(ReferenceDirectoryKind::Compare, &compare2_dir),
        ]);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("execution succeeds");

    assert!(!dest_file.exists(), "file should not be created when first compare-dest matches");
    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_matched(), 1);
}

#[test]
fn compare_dest_multiple_directories_checks_all_until_match() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let compare1_dir = temp.path().join("compare1");
    let compare2_dir = temp.path().join("compare2");
    let dest_dir = temp.path().join("dest");

    fs::create_dir_all(&source_dir).expect("create source");
    fs::create_dir_all(&compare1_dir).expect("create compare1");
    fs::create_dir_all(&compare2_dir).expect("create compare2");
    fs::create_dir_all(&dest_dir).expect("create dest");

    let source_file = source_dir.join("file.txt");
    let compare1_file = compare1_dir.join("file.txt");
    let compare2_file = compare2_dir.join("file.txt");
    let dest_file = dest_dir.join("file.txt");

    fs::write(&source_file, b"content").expect("write source");
    fs::write(&compare1_file, b"different1").expect("write compare1");
    fs::write(&compare2_file, b"content").expect("write compare2");

    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source_file, timestamp).expect("set source mtime");
    set_file_mtime(&compare1_file, timestamp).expect("set compare1 mtime");
    set_file_mtime(&compare2_file, timestamp).expect("set compare2 mtime");

    let operands = vec![
        source_file.into_os_string(),
        dest_file.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .times(true)
        .extend_reference_directories([
            ReferenceDirectory::new(ReferenceDirectoryKind::Compare, &compare1_dir),
            ReferenceDirectory::new(ReferenceDirectoryKind::Compare, &compare2_dir),
        ]);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("execution succeeds");

    assert!(!dest_file.exists(), "file should not be created when second compare-dest matches");
    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_matched(), 1);
}

#[test]
fn compare_dest_multiple_directories_transfers_when_none_match() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let compare1_dir = temp.path().join("compare1");
    let compare2_dir = temp.path().join("compare2");
    let dest_dir = temp.path().join("dest");

    fs::create_dir_all(&source_dir).expect("create source");
    fs::create_dir_all(&compare1_dir).expect("create compare1");
    fs::create_dir_all(&compare2_dir).expect("create compare2");
    fs::create_dir_all(&dest_dir).expect("create dest");

    let source_file = source_dir.join("file.txt");
    let compare1_file = compare1_dir.join("file.txt");
    let compare2_file = compare2_dir.join("file.txt");
    let dest_file = dest_dir.join("file.txt");

    fs::write(&source_file, b"source content").expect("write source");
    fs::write(&compare1_file, b"different1").expect("write compare1");
    fs::write(&compare2_file, b"different2").expect("write compare2");

    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source_file, timestamp).expect("set source mtime");
    set_file_mtime(&compare1_file, timestamp).expect("set compare1 mtime");
    set_file_mtime(&compare2_file, timestamp).expect("set compare2 mtime");

    let operands = vec![
        source_file.into_os_string(),
        dest_file.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .times(true)
        .extend_reference_directories([
            ReferenceDirectory::new(ReferenceDirectoryKind::Compare, &compare1_dir),
            ReferenceDirectory::new(ReferenceDirectoryKind::Compare, &compare2_dir),
        ]);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("execution succeeds");

    assert!(dest_file.exists(), "file should be created when no compare-dest matches");
    assert_eq!(fs::read(&dest_file).expect("read dest"), b"source content");
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.regular_files_matched(), 0);
}

/// Tests recursive transfer with compare-dest.
///
/// When syncing directories recursively, the relative path from source base
/// should be used to look up files in compare-dest (e.g., source/subdir/file.txt
/// should check compare-dest/subdir/file.txt, not compare-dest/file.txt).
#[test]
fn compare_dest_with_recursive_transfer() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let compare_dir = temp.path().join("compare");
    let dest_dir = temp.path().join("dest");

    fs::create_dir_all(source_dir.join("subdir")).expect("create source");
    fs::create_dir_all(compare_dir.join("subdir")).expect("create compare");
    fs::create_dir_all(&dest_dir).expect("create dest");

    let source_file1 = source_dir.join("file1.txt");
    let source_file2 = source_dir.join("subdir/file2.txt");
    let compare_file1 = compare_dir.join("file1.txt");
    let compare_file2 = compare_dir.join("subdir/file2.txt");

    fs::write(&source_file1, b"content1").expect("write source file1");
    fs::write(&source_file2, b"content2").expect("write source file2");
    fs::write(&compare_file1, b"content1").expect("write compare file1");
    fs::write(&compare_file2, b"different").expect("write compare file2");

    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source_file1, timestamp).expect("set source file1 mtime");
    set_file_mtime(&source_file2, timestamp).expect("set source file2 mtime");
    set_file_mtime(&compare_file1, timestamp).expect("set compare file1 mtime");
    set_file_mtime(&compare_file2, timestamp).expect("set compare file2 mtime");

    // Use trailing separator so contents of source are synced into dest
    let mut source_operand = source_dir.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let operands = vec![
        source_operand,
        dest_dir.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .times(true)
        .recursive(true)
        .push_reference_directory(ReferenceDirectory::new(
            ReferenceDirectoryKind::Compare,
            &compare_dir,
        ));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("execution succeeds");

    let dest_file1 = dest_dir.join("file1.txt");
    let dest_file2 = dest_dir.join("subdir/file2.txt");

    assert!(!dest_file1.exists(), "file1 should not be created when identical");
    assert!(dest_file2.exists(), "file2 should be created when different");
    assert_eq!(fs::read(&dest_file2).expect("read dest file2"), b"content2");
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.regular_files_matched(), 1);
}

#[test]
fn compare_dest_with_size_only_option() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let compare_dir = temp.path().join("compare");
    let dest_dir = temp.path().join("dest");

    fs::create_dir_all(&source_dir).expect("create source");
    fs::create_dir_all(&compare_dir).expect("create compare");
    fs::create_dir_all(&dest_dir).expect("create dest");

    let source_file = source_dir.join("file.txt");
    let compare_file = compare_dir.join("file.txt");
    let dest_file = dest_dir.join("file.txt");

    // Same size, different content
    fs::write(&source_file, b"1234567890").expect("write source");
    fs::write(&compare_file, b"abcdefghij").expect("write compare");

    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source_file, timestamp).expect("set source mtime");
    set_file_mtime(&compare_file, timestamp).expect("set compare mtime");

    let operands = vec![
        source_file.into_os_string(),
        dest_file.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .times(true)
        .size_only(true)
        .push_reference_directory(ReferenceDirectory::new(
            ReferenceDirectoryKind::Compare,
            &compare_dir,
        ));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("execution succeeds");

    assert!(!dest_file.exists(), "file should not be created when size matches with --size-only");
    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_matched(), 1);
}

#[test]
fn compare_dest_with_checksum_option() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let compare_dir = temp.path().join("compare");
    let dest_dir = temp.path().join("dest");

    fs::create_dir_all(&source_dir).expect("create source");
    fs::create_dir_all(&compare_dir).expect("create compare");
    fs::create_dir_all(&dest_dir).expect("create dest");

    let source_file = source_dir.join("file.txt");
    let compare_file = compare_dir.join("file.txt");
    let dest_file = dest_dir.join("file.txt");

    fs::write(&source_file, b"identical content").expect("write source");
    fs::write(&compare_file, b"identical content").expect("write compare");

    // Different mtimes
    let source_timestamp = FileTime::from_unix_time(1_700_000_100, 0);
    let compare_timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source_file, source_timestamp).expect("set source mtime");
    set_file_mtime(&compare_file, compare_timestamp).expect("set compare mtime");

    let operands = vec![
        source_file.into_os_string(),
        dest_file.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .checksum(true)
        .push_reference_directory(ReferenceDirectory::new(
            ReferenceDirectoryKind::Compare,
            &compare_dir,
        ));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("execution succeeds");

    assert!(!dest_file.exists(), "file should not be created when checksum matches even with different mtime");
    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_matched(), 1);
}

/// Tests compare-dest with a file in a subdirectory.
///
/// When copying a single file, rsync uses just the filename (not the full path)
/// to look up files in compare-dest. The compare file must be at compare_dir/file.txt,
/// not compare_dir/sub/file.txt.
#[test]
fn compare_dest_relative_path_resolution() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let compare_dir = temp.path().join("compare");
    let dest_dir = temp.path().join("dest");

    fs::create_dir_all(source_dir.join("sub")).expect("create source");
    fs::create_dir_all(&compare_dir).expect("create compare");
    fs::create_dir_all(dest_dir.join("sub")).expect("create dest with sub");

    let source_file = source_dir.join("sub/file.txt");
    // For single-file copy, compare-dest looks for compare_dir/<filename>
    let compare_file = compare_dir.join("file.txt");

    fs::write(&source_file, b"content").expect("write source");
    fs::write(&compare_file, b"content").expect("write compare");

    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source_file, timestamp).expect("set source mtime");
    set_file_mtime(&compare_file, timestamp).expect("set compare mtime");

    let operands = vec![
        source_file.into_os_string(),
        dest_dir.join("sub/file.txt").into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .times(true)
        .push_reference_directory(ReferenceDirectory::new(
            ReferenceDirectoryKind::Compare,
            &compare_dir,
        ));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("execution succeeds");

    let dest_file = dest_dir.join("sub/file.txt");
    assert!(!dest_file.exists(), "file should not be created when identical to compare-dest");
    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_matched(), 1);
}

#[test]
fn compare_dest_mixed_with_copy_dest() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let compare_dir = temp.path().join("compare");
    let copy_dir = temp.path().join("copy");
    let dest_dir = temp.path().join("dest");

    fs::create_dir_all(&source_dir).expect("create source");
    fs::create_dir_all(&compare_dir).expect("create compare");
    fs::create_dir_all(&copy_dir).expect("create copy");
    fs::create_dir_all(&dest_dir).expect("create dest");

    let source_file = source_dir.join("file.txt");
    let compare_file = compare_dir.join("file.txt");
    let copy_file = copy_dir.join("file.txt");
    let dest_file = dest_dir.join("file.txt");

    fs::write(&source_file, b"content").expect("write source");
    fs::write(&compare_file, b"different1").expect("write compare");
    fs::write(&copy_file, b"content").expect("write copy");

    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source_file, timestamp).expect("set source mtime");
    set_file_mtime(&compare_file, timestamp).expect("set compare mtime");
    set_file_mtime(&copy_file, timestamp).expect("set copy mtime");

    let operands = vec![
        source_file.into_os_string(),
        dest_file.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // First compare-dest doesn't match, but copy-dest does
    let options = LocalCopyOptions::default()
        .times(true)
        .extend_reference_directories([
            ReferenceDirectory::new(ReferenceDirectoryKind::Compare, &compare_dir),
            ReferenceDirectory::new(ReferenceDirectoryKind::Copy, &copy_dir),
        ]);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("execution succeeds");

    assert!(dest_file.exists(), "file should be created via copy-dest when compare-dest doesn't match");
    assert_eq!(fs::read(&dest_file).expect("read dest"), b"content");
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.regular_files_matched(), 0);
}

// ============================================================================
// Compare-dest with empty reference directory
// ============================================================================

#[test]
fn compare_dest_empty_reference_directory_transfers_file() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let compare_dir = temp.path().join("compare");
    let dest_dir = temp.path().join("dest");

    fs::create_dir_all(&source_dir).expect("create source");
    fs::create_dir_all(&compare_dir).expect("create empty compare dir");
    fs::create_dir_all(&dest_dir).expect("create dest");

    let source_file = source_dir.join("file.txt");
    let dest_file = dest_dir.join("file.txt");

    fs::write(&source_file, b"empty ref content").expect("write source");

    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source_file, timestamp).expect("set source mtime");

    let operands = vec![
        source_file.into_os_string(),
        dest_file.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .times(true)
        .push_reference_directory(ReferenceDirectory::new(
            ReferenceDirectoryKind::Compare,
            &compare_dir,
        ));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("execution succeeds");

    assert!(dest_file.exists(), "file should be created when compare-dest is empty");
    assert_eq!(fs::read(&dest_file).expect("read dest"), b"empty ref content");
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.regular_files_matched(), 0);
}

// ============================================================================
// Compare-dest with --inplace mode
// ============================================================================

#[test]
fn compare_dest_with_inplace_mode() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let compare_dir = temp.path().join("compare");
    let dest_dir = temp.path().join("dest");

    fs::create_dir_all(&source_dir).expect("create source");
    fs::create_dir_all(&compare_dir).expect("create compare");
    fs::create_dir_all(&dest_dir).expect("create dest");

    let source_file = source_dir.join("file.txt");
    let compare_file = compare_dir.join("file.txt");
    let dest_file = dest_dir.join("file.txt");

    fs::write(&source_file, b"identical content").expect("write source");
    fs::write(&compare_file, b"identical content").expect("write compare");

    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source_file, timestamp).expect("set source mtime");
    set_file_mtime(&compare_file, timestamp).expect("set compare mtime");

    let operands = vec![
        source_file.into_os_string(),
        dest_file.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .times(true)
        .inplace(true)
        .push_reference_directory(ReferenceDirectory::new(
            ReferenceDirectoryKind::Compare,
            &compare_dir,
        ));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("execution succeeds");

    // With --inplace and compare-dest matching, the file should still be skipped
    assert!(!dest_file.exists(), "file should not be created when identical to compare-dest even with --inplace");
    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_matched(), 1);
}

// ============================================================================
// Compare-dest with nonexistent directory
// ============================================================================

#[test]
fn compare_dest_with_nonexistent_directory() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let compare_dir = temp.path().join("nonexistent_compare");
    let dest_dir = temp.path().join("dest");

    fs::create_dir_all(&source_dir).expect("create source");
    fs::create_dir_all(&dest_dir).expect("create dest");
    // Intentionally don't create compare_dir

    let source_file = source_dir.join("file.txt");
    let dest_file = dest_dir.join("file.txt");

    fs::write(&source_file, b"content").expect("write source");

    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source_file, timestamp).expect("set source mtime");

    let operands = vec![
        source_file.into_os_string(),
        dest_file.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .times(true)
        .push_reference_directory(ReferenceDirectory::new(
            ReferenceDirectoryKind::Compare,
            &compare_dir,
        ));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("execution succeeds");

    assert!(dest_file.exists(), "file should be created when compare-dest doesn't exist");
    assert_eq!(fs::read(&dest_file).expect("read dest"), b"content");
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.regular_files_matched(), 0);
}

// ============================================================================
// Compare-dest with zero-length file
// ============================================================================

#[test]
fn compare_dest_skips_identical_empty_file() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let compare_dir = temp.path().join("compare");
    let dest_dir = temp.path().join("dest");

    fs::create_dir_all(&source_dir).expect("create source");
    fs::create_dir_all(&compare_dir).expect("create compare");
    fs::create_dir_all(&dest_dir).expect("create dest");

    let source_file = source_dir.join("empty.txt");
    let compare_file = compare_dir.join("empty.txt");
    let dest_file = dest_dir.join("empty.txt");

    fs::write(&source_file, b"").expect("write source");
    fs::write(&compare_file, b"").expect("write compare");

    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source_file, timestamp).expect("set source mtime");
    set_file_mtime(&compare_file, timestamp).expect("set compare mtime");

    let operands = vec![
        source_file.into_os_string(),
        dest_file.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .times(true)
        .push_reference_directory(ReferenceDirectory::new(
            ReferenceDirectoryKind::Compare,
            &compare_dir,
        ));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("execution succeeds");

    assert!(!dest_file.exists(), "empty identical file should be skipped");
    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_matched(), 1);
}
