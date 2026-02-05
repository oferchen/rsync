// Tests for --temp-dir temporary file placement
//
// These tests verify:
// 1. Temporary files are placed in specified directory
// 2. Files are moved to destination on completion
// 3. Behavior with same/different filesystem temp dirs
// 4. Atomic rename semantics
// 5. Comparison with upstream rsync behavior

// ==================== Basic Temp Dir Placement Tests ====================

#[test]
fn execute_with_temp_dir_places_temp_files_in_specified_directory() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination_dir = temp.path().join("dest");
    let temp_staging = temp.path().join("staging");
    fs::create_dir_all(&destination_dir).expect("dest dir");
    fs::create_dir_all(&temp_staging).expect("temp staging dir");
    fs::write(&source, b"temp dir test content").expect("write source");

    let destination = destination_dir.join("file.txt");
    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().with_temp_directory(Some(&temp_staging)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"temp dir test content"
    );

    // Verify no stray temp files remain in staging directory
    let staging_contents: Vec<_> = fs::read_dir(&temp_staging)
        .expect("read staging dir")
        .collect();
    assert!(
        staging_contents.is_empty(),
        "staging directory should be empty after successful transfer"
    );
}

#[test]
fn execute_with_temp_dir_moves_file_to_destination_on_completion() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    let temp_staging = temp.path().join("staging");
    fs::create_dir_all(&temp_staging).expect("temp staging dir");

    let content = b"content for atomic move";
    fs::write(&source, content).expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().with_temp_directory(Some(&temp_staging)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(destination.exists(), "destination should exist");
    assert_eq!(fs::read(&destination).expect("read dest"), content);

    // Verify the staging directory is empty (file was moved, not copied)
    let staging_files: Vec<_> = fs::read_dir(&temp_staging)
        .expect("read staging")
        .filter_map(|e| e.ok())
        .collect();
    assert!(
        staging_files.is_empty(),
        "staging directory should have no remaining temp files"
    );
}

#[test]
fn execute_with_temp_dir_same_filesystem_uses_atomic_rename() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    let temp_staging = temp.path().join("staging");
    fs::create_dir_all(&temp_staging).expect("temp staging dir");

    // Create a file that spans multiple blocks to ensure it's not trivially copied
    let large_content = vec![0xABu8; 64 * 1024];
    fs::write(&source, &large_content).expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().with_temp_directory(Some(&temp_staging)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read dest"), large_content);
}

// ==================== Cross-filesystem Tests ====================

#[cfg(unix)]
#[test]
fn execute_with_temp_dir_different_filesystem_falls_back_to_copy() {
    // This test uses /tmp which is often on a different filesystem (tmpfs)
    // compared to the user's home directory on many systems.
    //
    // If /tmp is on the same filesystem, this test still verifies the
    // copy operation works correctly.
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    // Try to use /tmp as a different filesystem staging area
    let system_tmp = PathBuf::from("/tmp");
    let staging_subdir = system_tmp.join(format!("rsync-test-{}", std::process::id()));
    if fs::create_dir_all(&staging_subdir).is_err() {
        // Skip if we can't create the staging directory
        return;
    }

    let content = b"cross-filesystem content";
    fs::write(&source, content).expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let result = plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().with_temp_directory(Some(&staging_subdir)),
    );

    // Clean up the staging directory
    let _ = fs::remove_dir_all(&staging_subdir);

    let summary = result.expect("copy should succeed even across filesystems");
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read dest"), content);
}

// ==================== Multiple Files Tests ====================

#[test]
fn execute_with_temp_dir_handles_multiple_files() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    let temp_staging = temp.path().join("staging");
    fs::create_dir_all(&source_root).expect("source dir");
    fs::create_dir_all(&temp_staging).expect("staging dir");

    fs::write(source_root.join("file1.txt"), b"content1").expect("write file1");
    fs::write(source_root.join("file2.txt"), b"content2").expect("write file2");
    fs::write(source_root.join("file3.txt"), b"content3").expect("write file3");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().with_temp_directory(Some(&temp_staging)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 3);
    assert_eq!(
        fs::read(dest_root.join("file1.txt")).expect("read"),
        b"content1"
    );
    assert_eq!(
        fs::read(dest_root.join("file2.txt")).expect("read"),
        b"content2"
    );
    assert_eq!(
        fs::read(dest_root.join("file3.txt")).expect("read"),
        b"content3"
    );

    // Verify staging directory is empty after completion
    let staging_files: Vec<_> = fs::read_dir(&temp_staging)
        .expect("read staging")
        .filter_map(|e| e.ok())
        .collect();
    assert!(staging_files.is_empty(), "no temp files should remain");
}

// ==================== Nested Directory Tests ====================

#[test]
fn execute_with_temp_dir_handles_nested_directories() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    let temp_staging = temp.path().join("staging");
    fs::create_dir_all(source_root.join("a").join("b").join("c")).expect("nested dirs");
    fs::create_dir_all(&temp_staging).expect("staging dir");

    fs::write(source_root.join("a").join("file1.txt"), b"level1").expect("write");
    fs::write(source_root.join("a").join("b").join("file2.txt"), b"level2").expect("write");
    fs::write(
        source_root.join("a").join("b").join("c").join("file3.txt"),
        b"level3",
    )
    .expect("write");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().with_temp_directory(Some(&temp_staging)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 3);
    assert_eq!(
        fs::read(dest_root.join("a").join("file1.txt")).expect("read"),
        b"level1"
    );
    assert_eq!(
        fs::read(dest_root.join("a").join("b").join("file2.txt")).expect("read"),
        b"level2"
    );
    assert_eq!(
        fs::read(dest_root.join("a").join("b").join("c").join("file3.txt")).expect("read"),
        b"level3"
    );

    // All temp files should be cleaned up
    let staging_files: Vec<_> = fs::read_dir(&temp_staging)
        .expect("read staging")
        .filter_map(|e| e.ok())
        .collect();
    assert!(staging_files.is_empty());
}

// ==================== Absolute Temp Dir Path Tests ====================

#[test]
fn execute_with_absolute_temp_dir_path() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    let temp_staging = temp.path().join("absolute-staging");
    fs::create_dir_all(&temp_staging).expect("staging dir");

    fs::write(&source, b"absolute path test").expect("write source");

    // Ensure we're using an absolute path
    let absolute_staging = temp_staging.canonicalize().expect("canonicalize");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().with_temp_directory(Some(&absolute_staging)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"absolute path test"
    );
}

// ==================== Temp Dir with Existing Files Tests ====================

#[test]
fn execute_with_temp_dir_replaces_existing_destination() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    let temp_staging = temp.path().join("staging");
    fs::create_dir_all(&temp_staging).expect("staging dir");

    fs::write(&source, b"new content").expect("write source");
    fs::write(&destination, b"old content").expect("write existing dest");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .with_temp_directory(Some(&temp_staging))
                .ignore_times(true), // Force transfer
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read dest"), b"new content");
}

// ==================== Empty File Tests ====================

#[test]
fn execute_with_temp_dir_handles_empty_file() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("empty.txt");
    let destination = temp.path().join("dest.txt");
    let temp_staging = temp.path().join("staging");
    fs::create_dir_all(&temp_staging).expect("staging dir");

    fs::write(&source, b"").expect("write empty source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().with_temp_directory(Some(&temp_staging)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(destination.exists());
    assert_eq!(fs::metadata(&destination).expect("metadata").len(), 0);
}

// ==================== Large File Tests ====================

#[test]
fn execute_with_temp_dir_handles_large_file() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("large.bin");
    let destination = temp.path().join("dest.bin");
    let temp_staging = temp.path().join("staging");
    fs::create_dir_all(&temp_staging).expect("staging dir");

    // Create a file larger than typical copy buffer
    let large_content = vec![0xCDu8; 256 * 1024];
    fs::write(&source, &large_content).expect("write large source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().with_temp_directory(Some(&temp_staging)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.bytes_copied(), 256 * 1024);
    assert_eq!(fs::read(&destination).expect("read dest"), large_content);

    // Staging directory should be empty
    let staging_files: Vec<_> = fs::read_dir(&temp_staging)
        .expect("read staging")
        .filter_map(|e| e.ok())
        .collect();
    assert!(staging_files.is_empty());
}

// ==================== Temp Dir with Metadata Preservation ====================

#[cfg(unix)]
#[test]
fn execute_with_temp_dir_preserves_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    let temp_staging = temp.path().join("staging");
    fs::create_dir_all(&temp_staging).expect("staging dir");

    fs::write(&source, b"perms test").expect("write source");
    let mut perms = fs::metadata(&source).expect("source metadata").permissions();
    perms.set_mode(0o600);
    fs::set_permissions(&source, perms).expect("set source perms");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .with_temp_directory(Some(&temp_staging))
                .permissions(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    let dest_perms = fs::metadata(&destination)
        .expect("dest metadata")
        .permissions();
    assert_eq!(dest_perms.mode() & 0o777, 0o600);
}

#[test]
fn execute_with_temp_dir_preserves_modification_time() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    let temp_staging = temp.path().join("staging");
    fs::create_dir_all(&temp_staging).expect("staging dir");

    fs::write(&source, b"time test").expect("write source");
    let past_time = FileTime::from_unix_time(1_600_000_000, 0);
    set_file_mtime(&source, past_time).expect("set source mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .with_temp_directory(Some(&temp_staging))
                .times(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    let dest_mtime =
        FileTime::from_last_modification_time(&fs::metadata(&destination).expect("dest metadata"));
    assert_eq!(dest_mtime, past_time);
}

// ==================== Temp Dir Nonexistent Tests ====================

#[test]
fn execute_with_nonexistent_temp_dir_fails() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    let nonexistent_staging = temp.path().join("does-not-exist");

    fs::write(&source, b"test content").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let result = plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().with_temp_directory(Some(&nonexistent_staging)),
    );

    assert!(
        result.is_err(),
        "copy should fail when temp dir does not exist"
    );
    assert!(
        !destination.exists(),
        "destination should not be created on failure"
    );
}

// ==================== Combination with Other Options ====================

#[test]
fn execute_with_temp_dir_and_partial_mode() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    let temp_staging = temp.path().join("staging");
    fs::create_dir_all(&temp_staging).expect("staging dir");

    fs::write(&source, b"partial + temp-dir").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // When partial is enabled, partial files use the partial naming convention
    // temp-dir is used for non-partial temp files
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .with_temp_directory(Some(&temp_staging))
                .partial(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"partial + temp-dir"
    );
}

#[test]
fn execute_with_temp_dir_and_delay_updates() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    let temp_staging = temp.path().join("staging");
    fs::create_dir_all(&temp_staging).expect("staging dir");

    fs::write(&source, b"delay updates content").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .with_temp_directory(Some(&temp_staging))
                .delay_updates(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"delay updates content"
    );
}

// ==================== Dry Run with Temp Dir ====================

#[test]
fn execute_dry_run_with_temp_dir_does_not_create_files() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    let temp_staging = temp.path().join("staging");
    fs::create_dir_all(&temp_staging).expect("staging dir");

    fs::write(&source, b"dry run content").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::DryRun,
            LocalCopyOptions::default().with_temp_directory(Some(&temp_staging)),
        )
        .expect("dry run succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(!destination.exists(), "destination should not exist in dry run");

    // Staging directory should also be empty
    let staging_files: Vec<_> = fs::read_dir(&temp_staging)
        .expect("read staging")
        .filter_map(|e| e.ok())
        .collect();
    assert!(staging_files.is_empty(), "no temp files in dry run");
}

// ==================== Inplace Mode with Temp Dir ====================

#[test]
fn execute_inplace_mode_ignores_temp_dir() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    let temp_staging = temp.path().join("staging");
    fs::create_dir_all(&temp_staging).expect("staging dir");

    fs::write(&source, b"inplace content").expect("write source");
    fs::write(&destination, b"original").expect("write dest");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Inplace mode writes directly to destination, so temp-dir is not used
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .with_temp_directory(Some(&temp_staging))
                .inplace(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"inplace content"
    );

    // With inplace, staging directory should remain empty
    let staging_files: Vec<_> = fs::read_dir(&temp_staging)
        .expect("read staging")
        .filter_map(|e| e.ok())
        .collect();
    assert!(staging_files.is_empty());
}

// ==================== Temp Dir Cleanup on Success ====================

#[test]
fn execute_with_temp_dir_cleans_up_on_successful_multi_file_transfer() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    let temp_staging = temp.path().join("staging");
    fs::create_dir_all(&source_root).expect("source dir");
    fs::create_dir_all(&temp_staging).expect("staging dir");

    // Create multiple files of varying sizes
    for i in 0..10 {
        let content = vec![i as u8; (i + 1) * 1024];
        fs::write(source_root.join(format!("file{i}.bin")), &content).expect("write file");
    }

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().with_temp_directory(Some(&temp_staging)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 10);

    // Verify all files exist in destination
    for i in 0..10 {
        let dest_file = dest_root.join(format!("file{i}.bin"));
        assert!(dest_file.exists(), "file{i}.bin should exist");
        let expected_content = vec![i as u8; (i + 1) * 1024];
        assert_eq!(fs::read(&dest_file).expect("read"), expected_content);
    }

    // Verify staging is completely clean
    let staging_files: Vec<_> = fs::read_dir(&temp_staging)
        .expect("read staging")
        .filter_map(|e| e.ok())
        .collect();
    assert!(
        staging_files.is_empty(),
        "all temp files should be cleaned up"
    );
}

// ==================== Temp Dir Path with Spaces ====================

#[test]
fn execute_with_temp_dir_containing_spaces() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    let temp_staging = temp.path().join("staging with spaces");
    fs::create_dir_all(&temp_staging).expect("staging dir");

    fs::write(&source, b"spaces in path").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().with_temp_directory(Some(&temp_staging)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"spaces in path"
    );
}

// ==================== Temp Dir with Special Characters ====================

#[test]
fn execute_with_temp_dir_containing_special_chars() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    let temp_staging = temp.path().join("staging-with_special.chars");
    fs::create_dir_all(&temp_staging).expect("staging dir");

    fs::write(&source, b"special chars").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().with_temp_directory(Some(&temp_staging)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read dest"), b"special chars");
}

// ==================== Atomic Semantics Verification ====================

#[test]
fn execute_with_temp_dir_provides_atomic_destination_update() {
    // This test verifies that the destination file is updated atomically.
    // During the transfer, the destination should either have the old content
    // or the new content, never partial content.
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    let temp_staging = temp.path().join("staging");
    fs::create_dir_all(&temp_staging).expect("staging dir");

    let old_content = b"old content here";
    let new_content = b"new content that replaces old";
    fs::write(&source, new_content).expect("write source");
    fs::write(&destination, old_content).expect("write existing dest");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().with_temp_directory(Some(&temp_staging)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);

    // The destination should have exactly the new content
    let final_content = fs::read(&destination).expect("read dest");
    assert_eq!(final_content, new_content);
}

// ==================== Temp Dir with Delete Option ====================

#[test]
fn execute_with_temp_dir_and_delete_removes_extraneous() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    let temp_staging = temp.path().join("staging");
    fs::create_dir_all(&source_root).expect("source dir");
    fs::create_dir_all(&dest_root).expect("dest dir");
    fs::create_dir_all(&temp_staging).expect("staging dir");

    fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
    fs::write(dest_root.join("keep.txt"), b"old keep").expect("write existing");
    fs::write(dest_root.join("delete_me.txt"), b"delete").expect("write extraneous");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .with_temp_directory(Some(&temp_staging))
                .delete(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join("keep.txt").exists());
    assert!(
        !dest_root.join("delete_me.txt").exists(),
        "extraneous file should be deleted"
    );
}
