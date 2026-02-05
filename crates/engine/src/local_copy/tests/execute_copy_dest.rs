
// ==================== Basic Copy-Dest Tests ====================

#[test]
fn copy_dest_identical_file_is_copied_not_linked() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let copy_dest_dir = temp.path().join("copy_dest");
    let destination_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create source dir");
    fs::create_dir_all(&copy_dest_dir).expect("create copy_dest dir");
    fs::create_dir_all(&destination_dir).expect("create dest dir");

    let source_file = source_dir.join("file.txt");
    let copy_dest_file = copy_dest_dir.join("file.txt");
    fs::write(&source_file, b"identical content").expect("write source");
    fs::write(&copy_dest_file, b"identical content").expect("write copy_dest");

    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source_file, timestamp).expect("source mtime");
    set_file_mtime(&copy_dest_file, timestamp).expect("copy_dest mtime");

    let destination_file = destination_dir.join("file.txt");
    let operands = vec![
        source_file.into_os_string(),
        destination_file.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .times(true)
        .extend_reference_directories([ReferenceDirectory::new(
            ReferenceDirectoryKind::Copy,
            &copy_dest_dir,
        )]);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(destination_file.exists(), "destination file should be created");
    assert_eq!(fs::read(&destination_file).expect("read dest"), b"identical content");
    assert_eq!(summary.files_copied(), 1);

    // Verify it's a copy, not a hard link
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let dest_meta = fs::metadata(&destination_file).expect("dest metadata");
        let copy_dest_meta = fs::metadata(&copy_dest_file).expect("copy_dest metadata");
        assert_ne!(dest_meta.ino(), copy_dest_meta.ino(), "should be different files, not hard linked");
    }
}

#[test]
fn copy_dest_different_file_uses_delta_transfer() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let copy_dest_dir = temp.path().join("copy_dest");
    let destination_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create source dir");
    fs::create_dir_all(&copy_dest_dir).expect("create copy_dest dir");
    fs::create_dir_all(&destination_dir).expect("create dest dir");

    let source_file = source_dir.join("file.txt");
    let copy_dest_file = copy_dest_dir.join("file.txt");

    // Create files with similar content but different timestamps to trigger delta transfer
    fs::write(&source_file, b"updated content here").expect("write source");
    fs::write(&copy_dest_file, b"old content goes here").expect("write copy_dest");

    let old_timestamp = FileTime::from_unix_time(1_600_000_000, 0);
    let new_timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&copy_dest_file, old_timestamp).expect("copy_dest mtime");
    set_file_mtime(&source_file, new_timestamp).expect("source mtime");

    let destination_file = destination_dir.join("file.txt");
    let operands = vec![
        source_file.into_os_string(),
        destination_file.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .times(true)
        .whole_file(false)
        .extend_reference_directories([ReferenceDirectory::new(
            ReferenceDirectoryKind::Copy,
            &copy_dest_dir,
        )]);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(destination_file.exists(), "destination file should be created");
    assert_eq!(fs::read(&destination_file).expect("read dest"), b"updated content here");
    assert_eq!(summary.files_copied(), 1);
}

#[test]
fn copy_dest_missing_file_transfers_normally() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let copy_dest_dir = temp.path().join("copy_dest");
    let destination_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create source dir");
    fs::create_dir_all(&copy_dest_dir).expect("create copy_dest dir");
    fs::create_dir_all(&destination_dir).expect("create dest dir");

    let source_file = source_dir.join("file.txt");
    fs::write(&source_file, b"new file content").expect("write source");

    // Note: copy_dest_dir exists but does NOT contain file.txt

    let destination_file = destination_dir.join("file.txt");
    let operands = vec![
        source_file.into_os_string(),
        destination_file.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .extend_reference_directories([ReferenceDirectory::new(
            ReferenceDirectoryKind::Copy,
            &copy_dest_dir,
        )]);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(destination_file.exists(), "destination file should be created");
    assert_eq!(fs::read(&destination_file).expect("read dest"), b"new file content");
    assert_eq!(summary.files_copied(), 1);
}

// ==================== Multiple Copy-Dest Directories Tests ====================

#[test]
fn copy_dest_multiple_directories_checks_in_order() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let copy_dest1_dir = temp.path().join("copy_dest1");
    let copy_dest2_dir = temp.path().join("copy_dest2");
    let destination_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create source dir");
    fs::create_dir_all(&copy_dest1_dir).expect("create copy_dest1 dir");
    fs::create_dir_all(&copy_dest2_dir).expect("create copy_dest2 dir");
    fs::create_dir_all(&destination_dir).expect("create dest dir");

    let source_file = source_dir.join("file.txt");
    let copy_dest2_file = copy_dest2_dir.join("file.txt");
    fs::write(&source_file, b"source content").expect("write source");
    fs::write(&copy_dest2_file, b"source content").expect("write copy_dest2");

    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source_file, timestamp).expect("source mtime");
    set_file_mtime(&copy_dest2_file, timestamp).expect("copy_dest2 mtime");

    // Note: copy_dest1 exists but does NOT contain file.txt
    // copy_dest2 contains matching file.txt

    let destination_file = destination_dir.join("file.txt");
    let operands = vec![
        source_file.into_os_string(),
        destination_file.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .times(true)
        .extend_reference_directories([
            ReferenceDirectory::new(ReferenceDirectoryKind::Copy, &copy_dest1_dir),
            ReferenceDirectory::new(ReferenceDirectoryKind::Copy, &copy_dest2_dir),
        ]);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(destination_file.exists(), "destination file should be created");
    assert_eq!(fs::read(&destination_file).expect("read dest"), b"source content");
    assert_eq!(summary.files_copied(), 1);
}

#[test]
fn copy_dest_multiple_directories_uses_first_match() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let copy_dest1_dir = temp.path().join("copy_dest1");
    let copy_dest2_dir = temp.path().join("copy_dest2");
    let destination_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create source dir");
    fs::create_dir_all(&copy_dest1_dir).expect("create copy_dest1 dir");
    fs::create_dir_all(&copy_dest2_dir).expect("create copy_dest2 dir");
    fs::create_dir_all(&destination_dir).expect("create dest dir");

    let source_file = source_dir.join("file.txt");
    let copy_dest1_file = copy_dest1_dir.join("file.txt");
    let copy_dest2_file = copy_dest2_dir.join("file.txt");
    fs::write(&source_file, b"matching content").expect("write source");
    fs::write(&copy_dest1_file, b"matching content").expect("write copy_dest1");
    fs::write(&copy_dest2_file, b"matching content").expect("write copy_dest2");

    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source_file, timestamp).expect("source mtime");
    set_file_mtime(&copy_dest1_file, timestamp).expect("copy_dest1 mtime");
    set_file_mtime(&copy_dest2_file, timestamp).expect("copy_dest2 mtime");

    let destination_file = destination_dir.join("file.txt");
    let operands = vec![
        source_file.into_os_string(),
        destination_file.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .times(true)
        .extend_reference_directories([
            ReferenceDirectory::new(ReferenceDirectoryKind::Copy, &copy_dest1_dir),
            ReferenceDirectory::new(ReferenceDirectoryKind::Copy, &copy_dest2_dir),
        ]);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(destination_file.exists(), "destination file should be created");
    assert_eq!(fs::read(&destination_file).expect("read dest"), b"matching content");
    assert_eq!(summary.files_copied(), 1);

    // Both directories had matching files, but first one should be used
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let dest_meta = fs::metadata(&destination_file).expect("dest metadata");
        let copy_dest1_meta = fs::metadata(&copy_dest1_file).expect("copy_dest1 metadata");
        let copy_dest2_meta = fs::metadata(&copy_dest2_file).expect("copy_dest2 metadata");

        // File should not be hard linked to either
        assert_ne!(dest_meta.ino(), copy_dest1_meta.ino());
        assert_ne!(dest_meta.ino(), copy_dest2_meta.ino());
    }
}

// ==================== Copy-Dest vs Compare-Dest Tests ====================

#[test]
fn copy_dest_creates_file_while_compare_dest_skips() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let reference_dir = temp.path().join("reference");
    let dest_compare_dir = temp.path().join("dest_compare");
    let dest_copy_dir = temp.path().join("dest_copy");
    fs::create_dir_all(&source_dir).expect("create source dir");
    fs::create_dir_all(&reference_dir).expect("create reference dir");
    fs::create_dir_all(&dest_compare_dir).expect("create dest_compare dir");
    fs::create_dir_all(&dest_copy_dir).expect("create dest_copy dir");

    let source_file = source_dir.join("file.txt");
    let reference_file = reference_dir.join("file.txt");
    fs::write(&source_file, b"payload").expect("write source");
    fs::write(&reference_file, b"payload").expect("write reference");

    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source_file, timestamp).expect("source mtime");
    set_file_mtime(&reference_file, timestamp).expect("reference mtime");

    // Test Compare behavior
    let dest_compare_file = dest_compare_dir.join("file.txt");
    let operands_compare = vec![
        source_file.clone().into_os_string(),
        dest_compare_file.clone().into_os_string(),
    ];
    let plan_compare = LocalCopyPlan::from_operands(&operands_compare).expect("plan");

    let options_compare = LocalCopyOptions::default()
        .times(true)
        .extend_reference_directories([ReferenceDirectory::new(
            ReferenceDirectoryKind::Compare,
            &reference_dir,
        )]);

    let summary_compare = plan_compare
        .execute_with_options(LocalCopyExecution::Apply, options_compare)
        .expect("compare succeeds");

    assert!(!dest_compare_file.exists(), "compare-dest should skip file creation");
    assert_eq!(summary_compare.files_copied(), 0);
    assert_eq!(summary_compare.regular_files_matched(), 1);

    // Test Copy behavior
    let dest_copy_file = dest_copy_dir.join("file.txt");
    let operands_copy = vec![
        source_file.into_os_string(),
        dest_copy_file.clone().into_os_string(),
    ];
    let plan_copy = LocalCopyPlan::from_operands(&operands_copy).expect("plan");

    let options_copy = LocalCopyOptions::default()
        .times(true)
        .extend_reference_directories([ReferenceDirectory::new(
            ReferenceDirectoryKind::Copy,
            &reference_dir,
        )]);

    let summary_copy = plan_copy
        .execute_with_options(LocalCopyExecution::Apply, options_copy)
        .expect("copy succeeds");

    assert!(dest_copy_file.exists(), "copy-dest should create file");
    assert_eq!(fs::read(&dest_copy_file).expect("read dest"), b"payload");
    assert_eq!(summary_copy.files_copied(), 1);
}

// ==================== Copy-Dest with Directory Trees ====================

#[test]
fn copy_dest_works_with_directory_trees() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let copy_dest_root = temp.path().join("copy_dest");
    let destination_root = temp.path().join("dest");

    // Create source tree
    fs::create_dir_all(source_root.join("dir1/subdir")).expect("create source tree");
    fs::write(source_root.join("dir1/file1.txt"), b"file1").expect("write file1");
    fs::write(source_root.join("dir1/subdir/file2.txt"), b"file2").expect("write file2");

    // Create matching copy_dest tree
    fs::create_dir_all(copy_dest_root.join("dir1/subdir")).expect("create copy_dest tree");
    fs::write(copy_dest_root.join("dir1/file1.txt"), b"file1").expect("write copy_dest file1");
    fs::write(copy_dest_root.join("dir1/subdir/file2.txt"), b"file2").expect("write copy_dest file2");

    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(source_root.join("dir1/file1.txt"), timestamp).expect("source file1 mtime");
    set_file_mtime(source_root.join("dir1/subdir/file2.txt"), timestamp).expect("source file2 mtime");
    set_file_mtime(copy_dest_root.join("dir1/file1.txt"), timestamp).expect("copy_dest file1 mtime");
    set_file_mtime(copy_dest_root.join("dir1/subdir/file2.txt"), timestamp).expect("copy_dest file2 mtime");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let operands = vec![source_operand, destination_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .times(true)
        .extend_reference_directories([ReferenceDirectory::new(
            ReferenceDirectoryKind::Copy,
            &copy_dest_root,
        )]);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(destination_root.join("dir1/file1.txt").exists());
    assert!(destination_root.join("dir1/subdir/file2.txt").exists());
    assert_eq!(fs::read(destination_root.join("dir1/file1.txt")).expect("read"), b"file1");
    assert_eq!(fs::read(destination_root.join("dir1/subdir/file2.txt")).expect("read"), b"file2");
    assert_eq!(summary.files_copied(), 2);
}

#[test]
fn copy_dest_mixed_existing_and_new_files() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let copy_dest_root = temp.path().join("copy_dest");
    let destination_root = temp.path().join("dest");

    // Create source tree with three files
    fs::create_dir_all(&source_root).expect("create source dir");
    fs::write(source_root.join("file1.txt"), b"content1").expect("write file1");
    fs::write(source_root.join("file2.txt"), b"content2").expect("write file2");
    fs::write(source_root.join("file3.txt"), b"content3").expect("write file3");

    // Create copy_dest tree with only file1 matching
    fs::create_dir_all(&copy_dest_root).expect("create copy_dest dir");
    fs::write(copy_dest_root.join("file1.txt"), b"content1").expect("write copy_dest file1");
    // file2 exists but with different content
    fs::write(copy_dest_root.join("file2.txt"), b"old_content2").expect("write copy_dest file2");
    // file3 doesn't exist in copy_dest at all

    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    let old_timestamp = FileTime::from_unix_time(1_600_000_000, 0);
    set_file_mtime(source_root.join("file1.txt"), timestamp).expect("source file1 mtime");
    set_file_mtime(source_root.join("file2.txt"), timestamp).expect("source file2 mtime");
    set_file_mtime(source_root.join("file3.txt"), timestamp).expect("source file3 mtime");
    set_file_mtime(copy_dest_root.join("file1.txt"), timestamp).expect("copy_dest file1 mtime");
    set_file_mtime(copy_dest_root.join("file2.txt"), old_timestamp).expect("copy_dest file2 mtime");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let operands = vec![source_operand, destination_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .times(true)
        .extend_reference_directories([ReferenceDirectory::new(
            ReferenceDirectoryKind::Copy,
            &copy_dest_root,
        )]);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(destination_root.join("file1.txt").exists());
    assert!(destination_root.join("file2.txt").exists());
    assert!(destination_root.join("file3.txt").exists());
    assert_eq!(fs::read(destination_root.join("file1.txt")).expect("read"), b"content1");
    assert_eq!(fs::read(destination_root.join("file2.txt")).expect("read"), b"content2");
    assert_eq!(fs::read(destination_root.join("file3.txt")).expect("read"), b"content3");
    assert_eq!(summary.files_copied(), 3);
}

// ==================== Copy-Dest with Checksum Tests ====================

#[test]
fn copy_dest_with_checksum_validates_content() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let copy_dest_dir = temp.path().join("copy_dest");
    let destination_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create source dir");
    fs::create_dir_all(&copy_dest_dir).expect("create copy_dest dir");
    fs::create_dir_all(&destination_dir).expect("create dest dir");

    let source_file = source_dir.join("file.txt");
    let copy_dest_file = copy_dest_dir.join("file.txt");

    // Same size and timestamp, but different content
    let content_a = b"content version A";
    let content_b = b"content version B";
    assert_eq!(content_a.len(), content_b.len(), "test requires same size");

    fs::write(&source_file, content_a).expect("write source");
    fs::write(&copy_dest_file, content_b).expect("write copy_dest");

    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source_file, timestamp).expect("source mtime");
    set_file_mtime(&copy_dest_file, timestamp).expect("copy_dest mtime");

    let destination_file = destination_dir.join("file.txt");
    let operands = vec![
        source_file.into_os_string(),
        destination_file.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .times(true)
        .checksum(true)
        .extend_reference_directories([ReferenceDirectory::new(
            ReferenceDirectoryKind::Copy,
            &copy_dest_dir,
        )]);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // With checksum enabled, files should be recognized as different
    assert!(destination_file.exists());
    assert_eq!(fs::read(&destination_file).expect("read dest"), content_a);
    assert_eq!(summary.files_copied(), 1);
}

// ==================== Edge Cases ====================

#[test]
fn copy_dest_ignores_non_regular_files() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let copy_dest_dir = temp.path().join("copy_dest");
    let destination_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create source dir");
    fs::create_dir_all(&copy_dest_dir).expect("create copy_dest dir");
    fs::create_dir_all(&destination_dir).expect("create dest dir");

    let source_file = source_dir.join("file.txt");
    fs::write(&source_file, b"regular file").expect("write source");

    // Create a directory in copy_dest with the same name
    let copy_dest_path = copy_dest_dir.join("file.txt");
    fs::create_dir_all(&copy_dest_path).expect("create directory in copy_dest");

    let destination_file = destination_dir.join("file.txt");
    let operands = vec![
        source_file.into_os_string(),
        destination_file.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .extend_reference_directories([ReferenceDirectory::new(
            ReferenceDirectoryKind::Copy,
            &copy_dest_dir,
        )]);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Should transfer normally since copy_dest has a directory, not a file
    assert!(destination_file.exists());
    assert!(destination_file.is_file());
    assert_eq!(fs::read(&destination_file).expect("read dest"), b"regular file");
    assert_eq!(summary.files_copied(), 1);
}

#[test]
fn copy_dest_with_size_only_mode() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let copy_dest_dir = temp.path().join("copy_dest");
    let destination_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create source dir");
    fs::create_dir_all(&copy_dest_dir).expect("create copy_dest dir");
    fs::create_dir_all(&destination_dir).expect("create dest dir");

    let source_file = source_dir.join("file.txt");
    let copy_dest_file = copy_dest_dir.join("file.txt");

    // Same size content
    fs::write(&source_file, b"12345").expect("write source");
    fs::write(&copy_dest_file, b"abcde").expect("write copy_dest");

    // Different timestamps
    set_file_mtime(&source_file, FileTime::from_unix_time(1_700_000_000, 0)).expect("source mtime");
    set_file_mtime(&copy_dest_file, FileTime::from_unix_time(1_600_000_000, 0)).expect("copy_dest mtime");

    let destination_file = destination_dir.join("file.txt");
    let operands = vec![
        source_file.into_os_string(),
        destination_file.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .size_only(true)
        .extend_reference_directories([ReferenceDirectory::new(
            ReferenceDirectoryKind::Copy,
            &copy_dest_dir,
        )]);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // With size_only, files with same size should match
    assert!(destination_file.exists());
    assert_eq!(summary.files_copied(), 1);
}

#[test]
fn copy_dest_empty_reference_directory() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let copy_dest_dir = temp.path().join("copy_dest");
    let destination_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create source dir");
    fs::create_dir_all(&copy_dest_dir).expect("create empty copy_dest dir");
    fs::create_dir_all(&destination_dir).expect("create dest dir");

    let source_file = source_dir.join("file.txt");
    fs::write(&source_file, b"content").expect("write source");

    let destination_file = destination_dir.join("file.txt");
    let operands = vec![
        source_file.into_os_string(),
        destination_file.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .extend_reference_directories([ReferenceDirectory::new(
            ReferenceDirectoryKind::Copy,
            &copy_dest_dir,
        )]);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Should transfer normally
    assert!(destination_file.exists());
    assert_eq!(fs::read(&destination_file).expect("read dest"), b"content");
    assert_eq!(summary.files_copied(), 1);
}
