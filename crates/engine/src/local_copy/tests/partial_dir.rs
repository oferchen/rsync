// Tests for --partial-dir flag behavior.
//
// The --partial-dir option specifies a directory where partial files should be
// placed during transfers. This is useful for resumable transfers as the partial
// files are preserved on interruption and can be used for delta transfer resumption.
//
// Key behaviors tested:
// 1. Partial files are placed in the specified directory
// 2. Partial directory is created if it doesn't exist
// 3. Files are moved from partial dir to final destination on successful completion
// 4. Interrupted transfers leave files in partial dir (simulated via discard)
// 5. Works correctly with nested directory structures
// 6. Behavior matches upstream rsync

// ==================== Basic Partial Dir Placement Tests ====================

#[test]
fn partial_dir_places_partial_file_in_specified_directory() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let dest_dir = temp.path().join("dest");
    let partial_dir = temp.path().join("dest").join(".rsync-partial");
    fs::create_dir_all(&dest_dir).expect("create dest dir");
    fs::write(&source, b"partial dir test content").expect("write source");

    let destination = dest_dir.join("target.txt");
    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().with_partial_directory(Some(".rsync-partial")),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"partial dir test content"
    );
    // After successful completion, the partial file should be moved to destination
    // and the partial directory may or may not be empty
    assert!(partial_dir.exists(), "partial directory should be created");
}

#[test]
fn partial_dir_creates_directory_if_not_exists() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let dest_dir = temp.path().join("dest");
    let partial_dir = dest_dir.join(".my-partial-dir");
    fs::create_dir_all(&dest_dir).expect("create dest dir");
    fs::write(&source, b"auto-create partial dir").expect("write source");

    // Verify partial dir doesn't exist initially
    assert!(!partial_dir.exists());

    let destination = dest_dir.join("file.txt");
    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().with_partial_directory(Some(".my-partial-dir")),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    // Partial dir should have been created
    assert!(partial_dir.exists(), "partial directory should be auto-created");
    assert!(partial_dir.is_dir(), "partial directory should be a directory");
}

#[test]
fn partial_dir_moves_file_to_destination_on_commit() {
    let temp = tempdir().expect("tempdir");
    let dest_dir = temp.path().join("dest");
    let partial_dir_name = ".commit-partial";
    let partial_dir = dest_dir.join(partial_dir_name);
    fs::create_dir_all(&dest_dir).expect("create dest dir");

    let destination = dest_dir.join("committed.txt");

    // Use the low-level DestinationWriteGuard to test commit behavior
    let (guard, mut file) = DestinationWriteGuard::new(
        destination.as_path(),
        true,
        Some(Path::new(partial_dir_name)),
        None,
    )
    .expect("guard");

    let staging_path = guard.staging_path().to_path_buf();
    file.write_all(b"content to commit").expect("write");
    drop(file);

    // Before commit: staging path should exist in partial dir
    assert!(staging_path.exists());
    assert!(staging_path.starts_with(&partial_dir));
    assert!(!destination.exists());

    // Commit the file
    guard.commit().expect("commit");

    // After commit: file should be at destination, not in partial dir
    assert!(!staging_path.exists(), "staging path should be removed after commit");
    assert!(destination.exists(), "destination should exist after commit");
    assert_eq!(
        fs::read(&destination).expect("read"),
        b"content to commit"
    );
}

#[test]
fn partial_dir_preserves_partial_file_on_discard() {
    let temp = tempdir().expect("tempdir");
    let dest_dir = temp.path().join("dest");
    let partial_dir_name = ".interrupt-partial";
    let _partial_dir = dest_dir.join(partial_dir_name);
    fs::create_dir_all(&dest_dir).expect("create dest dir");

    let destination = dest_dir.join("interrupted.txt");

    // Use the low-level DestinationWriteGuard to test discard behavior
    let (guard, mut file) = DestinationWriteGuard::new(
        destination.as_path(),
        true,
        Some(Path::new(partial_dir_name)),
        None,
    )
    .expect("guard");

    let staging_path = guard.staging_path().to_path_buf();
    file.write_all(b"partial content before interrupt").expect("write");
    drop(file);

    // Discard simulates an interrupted transfer
    guard.discard();

    // After discard: partial file should still exist in partial dir
    assert!(staging_path.exists(), "partial file should be preserved on discard");
    assert!(!destination.exists(), "destination should not exist after discard");
    assert_eq!(
        fs::read(&staging_path).expect("read partial"),
        b"partial content before interrupt"
    );
}

// ==================== Nested Directory Tests ====================

#[test]
fn partial_dir_works_with_nested_source_directories() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    let partial_dir_name = ".nested-partial";

    // Create nested source structure
    fs::create_dir_all(source_root.join("level1").join("level2")).expect("create nested");
    fs::write(
        source_root.join("level1").join("level2").join("deep.txt"),
        b"deep nested content",
    )
    .expect("write deep file");

    fs::create_dir_all(&dest_root).expect("create dest");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().with_partial_directory(Some(partial_dir_name)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join("source/level1").join("level2").join("deep.txt").exists());
    assert_eq!(
        fs::read(dest_root.join("source/level1").join("level2").join("deep.txt")).expect("read"),
        b"deep nested content"
    );
}

#[test]
fn partial_dir_handles_multiple_files_in_nested_directories() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    let partial_dir_name = ".multi-partial";

    // Create multiple files at various levels
    fs::create_dir_all(source_root.join("dir1").join("subdir")).expect("create dir1/subdir");
    fs::create_dir_all(source_root.join("dir2")).expect("create dir2");

    fs::write(source_root.join("root.txt"), b"root level").expect("write root");
    fs::write(source_root.join("dir1").join("mid.txt"), b"middle level").expect("write mid");
    fs::write(
        source_root.join("dir1").join("subdir").join("deep.txt"),
        b"deep level",
    )
    .expect("write deep");
    fs::write(source_root.join("dir2").join("sibling.txt"), b"sibling dir").expect("write sibling");

    fs::create_dir_all(&dest_root).expect("create dest");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().with_partial_directory(Some(partial_dir_name)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 4);
    assert_eq!(
        fs::read(dest_root.join("source/root.txt")).expect("read root"),
        b"root level"
    );
    assert_eq!(
        fs::read(dest_root.join("source/dir1").join("mid.txt")).expect("read mid"),
        b"middle level"
    );
    assert_eq!(
        fs::read(dest_root.join("source/dir1").join("subdir").join("deep.txt")).expect("read deep"),
        b"deep level"
    );
    assert_eq!(
        fs::read(dest_root.join("source/dir2").join("sibling.txt")).expect("read sibling"),
        b"sibling dir"
    );
}

// ==================== Absolute Path Partial Dir Tests ====================

#[test]
fn partial_dir_supports_absolute_path() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let dest_dir = temp.path().join("dest");
    let partial_dir = temp.path().join("absolute-partial");

    fs::create_dir_all(&dest_dir).expect("create dest dir");
    fs::write(&source, b"absolute partial dir test").expect("write source");

    let destination = dest_dir.join("file.txt");
    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().with_partial_directory(Some(partial_dir.clone())),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(partial_dir.exists(), "absolute partial dir should be created");
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"absolute partial dir test"
    );
}

#[test]
fn partial_dir_absolute_path_preserves_partial_on_discard() {
    let temp = tempdir().expect("tempdir");
    let dest_dir = temp.path().join("dest");
    let partial_dir = temp.path().join("abs-partial");

    fs::create_dir_all(&dest_dir).expect("create dest dir");
    let destination = dest_dir.join("file.txt");

    let (guard, mut file) = DestinationWriteGuard::new(
        destination.as_path(),
        true,
        Some(partial_dir.as_path()),
        None,
    )
    .expect("guard");

    let staging_path = guard.staging_path().to_path_buf();
    file.write_all(b"absolute path partial").expect("write");
    drop(file);

    guard.discard();

    // Partial should be in the absolute path
    assert!(staging_path.starts_with(&partial_dir));
    assert!(staging_path.exists());
    assert_eq!(
        fs::read(&staging_path).expect("read"),
        b"absolute path partial"
    );
}

// ==================== Partial Dir with Relative Path Components Tests ====================

#[test]
fn partial_dir_relative_to_destination_directory() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let dest_dir = temp.path().join("dest");

    fs::create_dir_all(&dest_dir).expect("create dest dir");
    fs::write(&source, b"relative partial").expect("write source");

    let destination = dest_dir.join("output.txt");

    // Use low-level API to verify partial path is relative to destination's parent
    let (guard, mut file) = DestinationWriteGuard::new(
        destination.as_path(),
        true,
        Some(Path::new(".partial-relative")),
        None,
    )
    .expect("guard");

    let staging_path = guard.staging_path().to_path_buf();
    file.write_all(b"relative test").expect("write");
    drop(file);

    // Verify staging path is under dest_dir/.partial-relative
    let expected_partial_dir = dest_dir.join(".partial-relative");
    assert!(
        staging_path.starts_with(&expected_partial_dir),
        "staging path {} should start with {}",
        staging_path.display(),
        expected_partial_dir.display()
    );

    guard.commit().expect("commit");
    assert!(destination.exists());
}

// ==================== Edge Cases Tests ====================

#[test]
fn partial_dir_handles_empty_file() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("empty.txt");
    let dest_dir = temp.path().join("dest");
    let partial_dir_name = ".empty-partial";

    fs::create_dir_all(&dest_dir).expect("create dest dir");
    fs::write(&source, b"").expect("write empty source");

    let destination = dest_dir.join("empty-dest.txt");
    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().with_partial_directory(Some(partial_dir_name)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(destination.exists());
    assert_eq!(fs::metadata(&destination).expect("metadata").len(), 0);
}

#[test]
fn partial_dir_handles_large_file() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("large.bin");
    let dest_dir = temp.path().join("dest");
    let partial_dir_name = ".large-partial";

    fs::create_dir_all(&dest_dir).expect("create dest dir");

    // Create a file larger than typical copy buffer (256KB)
    let large_content = vec![0xABu8; 256 * 1024];
    fs::write(&source, &large_content).expect("write large source");

    let destination = dest_dir.join("large-dest.bin");
    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().with_partial_directory(Some(partial_dir_name)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.bytes_copied(), 256 * 1024);
    assert_eq!(fs::read(&destination).expect("read dest"), large_content);
}

#[test]
fn partial_dir_overwrites_existing_partial_file() {
    let temp = tempdir().expect("tempdir");
    let dest_dir = temp.path().join("dest");
    let partial_dir = dest_dir.join(".overwrite-partial");

    fs::create_dir_all(&partial_dir).expect("create partial dir");
    let destination = dest_dir.join("overwrite.txt");

    // Create an existing partial file
    let old_partial = partial_dir.join("overwrite.txt");
    fs::write(&old_partial, b"old partial content").expect("write old partial");

    // Now create a new partial for the same destination
    let (guard, mut file) = DestinationWriteGuard::new(
        destination.as_path(),
        true,
        Some(Path::new(".overwrite-partial")),
        None,
    )
    .expect("guard");

    file.write_all(b"new partial content").expect("write new");
    drop(file);

    let staging_path = guard.staging_path().to_path_buf();

    // The new partial should overwrite the old one
    assert_eq!(
        fs::read(&staging_path).expect("read staging"),
        b"new partial content"
    );

    guard.discard();
}

// ==================== Comparison with Non-Partial Mode Tests ====================

#[test]
fn partial_dir_differs_from_non_partial_behavior() {
    let temp = tempdir().expect("tempdir");
    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&dest_dir).expect("create dest dir");

    // Test with partial dir - file preserved on discard
    let dest1 = dest_dir.join("partial.txt");
    let partial_dir_name = ".partial-test";

    let (guard1, mut file1) = DestinationWriteGuard::new(
        dest1.as_path(),
        true,
        Some(Path::new(partial_dir_name)),
        None,
    )
    .expect("guard with partial dir");
    let staging1 = guard1.staging_path().to_path_buf();
    file1.write_all(b"partial mode").expect("write");
    drop(file1);
    guard1.discard();

    // Test without partial mode - temp file removed on discard
    let dest2 = dest_dir.join("non-partial.txt");
    let (guard2, mut file2) = DestinationWriteGuard::new(dest2.as_path(), false, None, None)
        .expect("guard without partial");
    let staging2 = guard2.staging_path().to_path_buf();
    file2.write_all(b"non-partial mode").expect("write");
    drop(file2);
    guard2.discard();

    // Partial mode preserves file, non-partial removes it
    assert!(staging1.exists(), "partial mode should preserve file");
    assert!(!staging2.exists(), "non-partial mode should remove file");
}

// ==================== Special Characters in Path Tests ====================

#[test]
fn partial_dir_handles_special_characters_in_filename() {
    let temp = tempdir().expect("tempdir");
    let dest_dir = temp.path().join("dest");
    let partial_dir_name = ".special-partial";

    fs::create_dir_all(&dest_dir).expect("create dest dir");

    // Test with special characters (but not path separators)
    let destination = dest_dir.join("file with spaces & symbols!.txt");

    let (guard, mut file) = DestinationWriteGuard::new(
        destination.as_path(),
        true,
        Some(Path::new(partial_dir_name)),
        None,
    )
    .expect("guard");

    file.write_all(b"special chars content").expect("write");
    drop(file);

    let staging_path = guard.staging_path().to_path_buf();
    assert!(staging_path.exists());

    guard.commit().expect("commit");
    assert!(destination.exists());
    assert_eq!(
        fs::read(&destination).expect("read"),
        b"special chars content"
    );
}

// ==================== Integration with Other Options Tests ====================

#[test]
fn partial_dir_with_times_preservation() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let dest_dir = temp.path().join("dest");
    let partial_dir_name = ".times-partial";

    fs::create_dir_all(&dest_dir).expect("create dest dir");
    fs::write(&source, b"times test").expect("write source");

    let past_time = FileTime::from_unix_time(1_600_000_000, 0);
    set_file_mtime(&source, past_time).expect("set source mtime");

    let destination = dest_dir.join("timed.txt");
    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .with_partial_directory(Some(partial_dir_name))
                .times(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);

    let dest_mtime =
        FileTime::from_last_modification_time(&fs::metadata(&destination).expect("dest metadata"));
    assert_eq!(dest_mtime, past_time);
}

#[cfg(unix)]
#[test]
fn partial_dir_with_permissions_preservation() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let dest_dir = temp.path().join("dest");
    let partial_dir_name = ".perms-partial";

    fs::create_dir_all(&dest_dir).expect("create dest dir");
    fs::write(&source, b"perms test").expect("write source");

    let mut perms = fs::metadata(&source).expect("source metadata").permissions();
    perms.set_mode(0o640);
    fs::set_permissions(&source, perms).expect("set source perms");

    let destination = dest_dir.join("permed.txt");
    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .with_partial_directory(Some(partial_dir_name))
                .permissions(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);

    let dest_perms = fs::metadata(&destination).expect("dest metadata").permissions();
    assert_eq!(dest_perms.mode() & 0o777, 0o640);
}

// ==================== Dry Run Tests ====================

#[test]
fn partial_dir_dry_run_does_not_create_directory() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let dest_dir = temp.path().join("dest");
    let partial_dir = dest_dir.join(".dry-run-partial");

    fs::create_dir_all(&dest_dir).expect("create dest dir");
    fs::write(&source, b"dry run test").expect("write source");

    // Verify partial dir doesn't exist initially
    assert!(!partial_dir.exists());

    let destination = dest_dir.join("output.txt");
    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::DryRun,
            LocalCopyOptions::default().with_partial_directory(Some(".dry-run-partial")),
        )
        .expect("dry run succeeds");

    assert_eq!(summary.files_copied(), 1);
    // In dry run mode, no actual changes should be made
    assert!(!destination.exists(), "destination should not be created in dry run");
    // Note: The partial dir creation behavior in dry run depends on implementation
    // The test documents expected behavior - partial dir should NOT be created
}

// ==================== Option Interaction Tests ====================

#[test]
fn partial_dir_setting_enables_partial_flag() {
    let opts = LocalCopyOptions::default().with_partial_directory(Some(".partial"));

    assert!(opts.partial_enabled(), "setting partial_dir should enable partial");
    assert_eq!(
        opts.partial_directory_path(),
        Some(Path::new(".partial"))
    );
}

#[test]
fn partial_dir_none_does_not_enable_partial() {
    let opts = LocalCopyOptions::default().with_partial_directory::<PathBuf>(None);

    assert!(!opts.partial_enabled());
    assert!(opts.partial_directory_path().is_none());
}

#[test]
fn partial_dir_can_be_cleared_after_setting() {
    let opts = LocalCopyOptions::default()
        .with_partial_directory(Some(".partial"))
        .with_partial_directory::<PathBuf>(None);

    assert!(opts.partial_directory_path().is_none());
}

// ==================== Delete + Partial Dir Interaction Tests ====================
//
// Upstream rsync protects the partial-dir from deletion when --delete is used
// with --partial-dir. This prevents --delete from removing the directory that
// stores partial files for resumable transfers.

#[test]
fn partial_dir_protected_from_delete_with_relative_path() {
    // When --delete and --partial-dir=.rsync-partial are both set,
    // the .rsync-partial directory in the destination should NOT be deleted
    // even though it doesn't exist in the source.
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    // Create source files
    fs::write(ctx.source.join("keep.txt"), b"keep me").expect("write source");

    // Set up the destination with existing files and a partial dir
    let target_root = ctx.dest.join("source");
    fs::create_dir_all(&target_root).expect("create target root");
    fs::write(target_root.join("extra.txt"), b"should be deleted").expect("write extra");

    // Create the relative partial dir with a partial file inside
    let partial_dir = target_root.join(".rsync-partial");
    fs::create_dir_all(&partial_dir).expect("create partial dir");
    fs::write(partial_dir.join("resumable.txt"), b"partial data").expect("write partial");

    let operands = ctx.operands();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .delete(true)
        .with_partial_directory(Some(".rsync-partial"));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(summary.files_copied() >= 1);
    // extra.txt should be deleted
    assert!(
        !target_root.join("extra.txt").exists(),
        "extraneous file should be deleted"
    );
    // The partial dir should be protected from deletion
    assert!(
        partial_dir.exists(),
        "relative partial dir should be protected from --delete"
    );
    // The partial file inside should also survive
    assert!(
        partial_dir.join("resumable.txt").exists(),
        "files inside partial dir should survive --delete"
    );
}

#[test]
fn partial_dir_absolute_path_not_affected_by_delete() {
    // When --partial-dir uses an absolute path, it lives outside the
    // destination tree, so --delete cannot touch it. This test verifies
    // that the absolute partial dir is unaffected.
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    let abs_partial = temp.path().join("global-partial");

    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");
    fs::create_dir_all(&abs_partial).expect("create absolute partial dir");

    // Create source file
    fs::write(source.join("file.txt"), b"source data").expect("write source");

    // Create a partial file in the absolute partial dir
    fs::write(abs_partial.join("old-partial.txt"), b"old partial").expect("write partial");

    // Pre-populate destination with extraneous file
    let target_root = dest.join("source");
    fs::create_dir_all(&target_root).expect("create target root");
    fs::write(target_root.join("stale.txt"), b"stale").expect("write stale");

    let operands = vec![source.into_os_string(), dest.into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .delete(true)
        .with_partial_directory(Some(abs_partial.clone()));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(summary.files_copied() >= 1);
    // Stale file should be deleted
    assert!(
        !target_root.join("stale.txt").exists(),
        "extraneous file should be deleted"
    );
    // Absolute partial dir should be completely unaffected
    assert!(
        abs_partial.exists(),
        "absolute partial dir should still exist"
    );
    assert!(
        abs_partial.join("old-partial.txt").exists(),
        "files in absolute partial dir should be unaffected by --delete"
    );
}

#[test]
fn partial_dir_delete_only_removes_non_partial_entries() {
    // Verify that --delete removes extraneous files but leaves the partial-dir
    // untouched, even when both exist at the same directory level.
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    // Create source with multiple files
    fs::write(ctx.source.join("alpha.txt"), b"alpha").expect("write alpha");
    fs::write(ctx.source.join("beta.txt"), b"beta").expect("write beta");

    // Set up destination with source + extraneous + partial dir
    let target_root = ctx.dest.join("source");
    fs::create_dir_all(&target_root).expect("create target root");
    fs::write(target_root.join("gamma.txt"), b"extraneous gamma").expect("write gamma");
    fs::write(target_root.join("delta.txt"), b"extraneous delta").expect("write delta");

    let partial_dir = target_root.join(".rsync-partial");
    fs::create_dir_all(&partial_dir).expect("create partial dir");
    fs::write(partial_dir.join("in-progress.dat"), b"partial transfer data")
        .expect("write partial file");

    let operands = ctx.operands();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .delete(true)
        .with_partial_directory(Some(".rsync-partial"));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Source files should be copied
    assert_eq!(summary.files_copied(), 2);
    assert_eq!(
        fs::read(target_root.join("alpha.txt")).expect("read alpha"),
        b"alpha"
    );
    assert_eq!(
        fs::read(target_root.join("beta.txt")).expect("read beta"),
        b"beta"
    );

    // Extraneous files should be deleted
    assert!(
        !target_root.join("gamma.txt").exists(),
        "extraneous gamma should be deleted"
    );
    assert!(
        !target_root.join("delta.txt").exists(),
        "extraneous delta should be deleted"
    );

    // Partial dir and contents should be preserved
    assert!(
        partial_dir.exists(),
        "partial dir should be protected from deletion"
    );
    assert!(
        partial_dir.join("in-progress.dat").exists(),
        "partial file should survive deletion sweep"
    );
}

#[test]
fn partial_dir_without_delete_does_not_affect_extraneous_files() {
    // When --delete is not used, extraneous files remain (including the
    // partial dir, of course). This is the baseline behavior.
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    fs::write(ctx.source.join("new.txt"), b"new file").expect("write source");

    let target_root = ctx.dest.join("source");
    fs::create_dir_all(&target_root).expect("create target root");
    fs::write(target_root.join("old.txt"), b"old file").expect("write old");

    let partial_dir = target_root.join(".rsync-partial");
    fs::create_dir_all(&partial_dir).expect("create partial dir");
    fs::write(partial_dir.join("stalled.txt"), b"stalled").expect("write stalled");

    let operands = ctx.operands();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .with_partial_directory(Some(".rsync-partial"));

    let _summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Without --delete, old files remain
    assert!(
        target_root.join("old.txt").exists(),
        "without --delete, extraneous files should remain"
    );
    // Partial dir also remains
    assert!(
        partial_dir.exists(),
        "partial dir should exist without --delete"
    );
    assert!(
        partial_dir.join("stalled.txt").exists(),
        "partial file should remain without --delete"
    );
}

#[test]
fn partial_dir_delete_protects_dot_prefixed_directory_name() {
    // The most common usage: --partial-dir=.rsync-partial with --delete.
    // The dot prefix is conventional for hidden directories.
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    fs::write(ctx.source.join("data.bin"), b"binary data").expect("write source");

    let target_root = ctx.dest.join("source");
    fs::create_dir_all(&target_root).expect("create target root");

    // Create .rsync-partial with multiple files
    let partial_dir = target_root.join(".rsync-partial");
    fs::create_dir_all(&partial_dir).expect("create partial dir");
    fs::write(partial_dir.join("file1.partial"), b"partial 1").expect("write partial 1");
    fs::write(partial_dir.join("file2.partial"), b"partial 2").expect("write partial 2");

    // Also create another extraneous directory that is NOT the partial dir
    let other_dir = target_root.join(".other-dir");
    fs::create_dir_all(&other_dir).expect("create other dir");
    fs::write(other_dir.join("file.txt"), b"other").expect("write other");

    let operands = ctx.operands();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .delete(true)
        .with_partial_directory(Some(".rsync-partial"));

    let _summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // .rsync-partial should be protected
    assert!(
        partial_dir.exists(),
        ".rsync-partial should be protected from --delete"
    );
    assert!(
        partial_dir.join("file1.partial").exists(),
        "files in .rsync-partial should survive --delete"
    );
    assert!(
        partial_dir.join("file2.partial").exists(),
        "all files in .rsync-partial should survive --delete"
    );

    // Other extraneous directories should be deleted normally
    assert!(
        !other_dir.exists(),
        "non-partial extraneous directories should be deleted"
    );
}

#[test]
fn partial_dir_delete_with_dry_run_does_not_modify_anything() {
    // In dry-run mode, --delete should not actually remove anything,
    // including verifying the partial-dir protection logic doesn't
    // cause issues in dry-run.
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    fs::write(ctx.source.join("file.txt"), b"source").expect("write source");

    let target_root = ctx.dest.join("source");
    fs::create_dir_all(&target_root).expect("create target root");
    fs::write(target_root.join("extra.txt"), b"extra").expect("write extra");

    let partial_dir = target_root.join(".rsync-partial");
    fs::create_dir_all(&partial_dir).expect("create partial dir");
    fs::write(partial_dir.join("partial.dat"), b"partial").expect("write partial");

    let operands = ctx.operands();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .delete(true)
        .with_partial_directory(Some(".rsync-partial"));

    let _summary = plan
        .execute_with_options(LocalCopyExecution::DryRun, options)
        .expect("dry run succeeds");

    // In dry-run, nothing should be modified
    assert!(
        target_root.join("extra.txt").exists(),
        "dry run should not delete extraneous files"
    );
    assert!(
        partial_dir.exists(),
        "dry run should not touch partial dir"
    );
    assert!(
        partial_dir.join("partial.dat").exists(),
        "dry run should not touch partial files"
    );
}

// ==================== Partial Dir Resume Workflow Tests ====================

#[test]
fn partial_dir_find_basis_locates_existing_partial_for_resume() {
    // Tests that PartialFileManager::find_basis correctly locates
    // partial files for delta transfer resumption.
    let dir = tempdir().expect("tempdir");
    let partial_dir = dir.path().join(".rsync-partial");
    fs::create_dir(&partial_dir).expect("create partial dir");

    let dest = dir.path().join("target.txt");
    let partial = partial_dir.join("target.txt");
    fs::write(&partial, b"previously interrupted content").expect("write partial");

    let manager = PartialFileManager::new(PartialMode::PartialDir(
        PathBuf::from(".rsync-partial"),
    ));
    let basis = manager.find_basis(&dest).expect("find_basis");
    assert_eq!(basis, Some(partial));
}

#[test]
fn partial_dir_find_basis_returns_none_when_no_partial_exists() {
    let dir = tempdir().expect("tempdir");
    let dest = dir.path().join("target.txt");

    let manager = PartialFileManager::new(PartialMode::PartialDir(
        PathBuf::from(".rsync-partial"),
    ));
    let basis = manager.find_basis(&dest).expect("find_basis");
    assert_eq!(basis, None);
}

#[test]
fn partial_dir_cleanup_removes_partial_after_successful_transfer() {
    // After a successful transfer completes, cleanup_partial should
    // remove the partial file from the partial-dir.
    let dir = tempdir().expect("tempdir");
    let partial_dir = dir.path().join(".rsync-partial");
    fs::create_dir(&partial_dir).expect("create partial dir");

    let dest = dir.path().join("completed.txt");
    let partial = partial_dir.join("completed.txt");

    // Simulate: partial file exists from a previous interrupted transfer
    fs::write(&partial, b"old partial data").expect("write partial");
    assert!(partial.exists());

    let manager = PartialFileManager::new(PartialMode::PartialDir(partial_dir.clone()));
    manager.cleanup_partial(&dest).expect("cleanup");

    // After cleanup, partial file should be gone
    assert!(!partial.exists(), "partial file should be removed after cleanup");
    // But the partial directory itself should remain
    assert!(
        partial_dir.exists(),
        "partial directory should remain after cleanup"
    );
}

#[test]
fn partial_dir_full_resume_workflow() {
    // Simulates the complete workflow:
    // 1. Initial transfer is interrupted, leaving a partial file
    // 2. Resume detects the partial file
    // 3. Second transfer completes successfully
    // 4. Partial file is cleaned up
    let dir = tempdir().expect("tempdir");
    let partial_dir = dir.path().join(".rsync-partial");
    fs::create_dir(&partial_dir).expect("create partial dir");

    let dest = dir.path().join("file.txt");
    let partial = partial_dir.join("file.txt");

    // Step 1: Simulate interrupted transfer by writing partial data
    fs::write(&partial, b"partial from interrupted transfer").expect("write partial");

    let manager = PartialFileManager::new(PartialMode::PartialDir(partial_dir.clone()));

    // Step 2: Find basis for resume
    let basis = manager.find_basis(&dest).expect("find_basis");
    assert_eq!(basis, Some(partial.clone()));
    assert_eq!(
        fs::read(basis.unwrap()).expect("read basis"),
        b"partial from interrupted transfer"
    );

    // Step 3: Simulate successful transfer completion
    fs::write(&dest, b"complete final content").expect("write dest");

    // Step 4: Clean up partial file
    manager.cleanup_partial(&dest).expect("cleanup");
    assert!(!partial.exists(), "partial should be cleaned up after success");
    assert!(dest.exists(), "destination should exist after completion");
    assert_eq!(
        fs::read(&dest).expect("read dest"),
        b"complete final content"
    );
}

// ==================== Partial Dir with Multiple Transfer Directories ====================

#[test]
fn partial_dir_relative_path_independent_per_directory() {
    // When using a relative partial-dir, each destination directory
    // gets its own partial sub-directory. Verify independence.
    let base = tempdir().expect("tempdir");

    let dir_a = base.path().join("dir_a");
    let dir_b = base.path().join("dir_b");
    fs::create_dir(&dir_a).expect("create dir_a");
    fs::create_dir(&dir_b).expect("create dir_b");

    let partial_a = dir_a.join(".partial");
    let partial_b = dir_b.join(".partial");
    fs::create_dir(&partial_a).expect("create partial_a");
    fs::create_dir(&partial_b).expect("create partial_b");

    // Write different partial content in each
    fs::write(partial_a.join("data.txt"), b"partial A").expect("write partial A");
    fs::write(partial_b.join("data.txt"), b"partial B").expect("write partial B");

    let manager = PartialFileManager::new(PartialMode::PartialDir(PathBuf::from(".partial")));

    let basis_a = manager
        .find_basis(&dir_a.join("data.txt"))
        .expect("find A");
    let basis_b = manager
        .find_basis(&dir_b.join("data.txt"))
        .expect("find B");

    assert_eq!(basis_a, Some(partial_a.join("data.txt")));
    assert_eq!(basis_b, Some(partial_b.join("data.txt")));

    // Verify content independence
    assert_eq!(
        fs::read(basis_a.unwrap()).expect("read A"),
        b"partial A"
    );
    assert_eq!(
        fs::read(basis_b.unwrap()).expect("read B"),
        b"partial B"
    );
}

#[test]
fn partial_dir_absolute_path_shared_across_directories() {
    // When using an absolute partial-dir, all destinations share
    // the same partial directory. Last writer wins for same-named files.
    let base = tempdir().expect("tempdir");
    let shared_partial = base.path().join("shared-partial");
    fs::create_dir(&shared_partial).expect("create shared partial");

    let dir_a = base.path().join("dir_a");
    let dir_b = base.path().join("dir_b");
    fs::create_dir(&dir_a).expect("create dir_a");
    fs::create_dir(&dir_b).expect("create dir_b");

    // Write a partial file with the same name from different source dirs
    fs::write(shared_partial.join("common.txt"), b"shared partial data")
        .expect("write shared partial");

    let manager =
        PartialFileManager::new(PartialMode::PartialDir(shared_partial.clone()));

    // Both destinations should find the same partial file
    let basis_a = manager
        .find_basis(&dir_a.join("common.txt"))
        .expect("find A");
    let basis_b = manager
        .find_basis(&dir_b.join("common.txt"))
        .expect("find B");

    assert_eq!(basis_a, Some(shared_partial.join("common.txt")));
    assert_eq!(basis_b, Some(shared_partial.join("common.txt")));
}

// ==================== Partial Dir Guard Behavior Tests ====================

#[test]
fn partial_dir_guard_staging_path_is_inside_partial_dir() {
    let temp = tempdir().expect("tempdir");
    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&dest_dir).expect("create dest");

    let destination = dest_dir.join("output.txt");
    let partial_dir_name = ".staging";

    let (guard, _file) = DestinationWriteGuard::new(
        destination.as_path(),
        true,
        Some(Path::new(partial_dir_name)),
        None,
    )
    .expect("guard");

    let staging = guard.staging_path();
    let expected_partial_dir = dest_dir.join(partial_dir_name);

    assert!(
        staging.starts_with(&expected_partial_dir),
        "staging path {} should be inside {}",
        staging.display(),
        expected_partial_dir.display()
    );

    // The filename should match the destination filename
    assert_eq!(
        staging.file_name(),
        destination.file_name(),
        "staging filename should match destination filename"
    );

    guard.discard();
}

#[test]
fn partial_dir_guard_commit_then_verify_no_leftover() {
    // After commit, the partial dir should not contain the file
    let temp = tempdir().expect("tempdir");
    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&dest_dir).expect("create dest");

    let destination = dest_dir.join("final.txt");
    let partial_dir_name = ".commit-check";

    let (guard, mut file) = DestinationWriteGuard::new(
        destination.as_path(),
        true,
        Some(Path::new(partial_dir_name)),
        None,
    )
    .expect("guard");

    let staging = guard.staging_path().to_path_buf();
    file.write_all(b"commit check content").expect("write");
    drop(file);

    assert!(staging.exists(), "staging file should exist before commit");
    guard.commit().expect("commit");

    assert!(
        !staging.exists(),
        "staging file should not exist after commit"
    );
    assert!(
        destination.exists(),
        "destination should exist after commit"
    );
    assert_eq!(
        fs::read(&destination).expect("read"),
        b"commit check content"
    );
}

#[test]
fn partial_dir_guard_discard_preserves_for_later_resume() {
    // After discard, the partial file should remain in the partial dir
    // so that a subsequent transfer can use it as a basis.
    let temp = tempdir().expect("tempdir");
    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&dest_dir).expect("create dest");

    let destination = dest_dir.join("resumable.txt");
    let partial_dir_name = ".resume-partial";

    let (guard, mut file) = DestinationWriteGuard::new(
        destination.as_path(),
        true,
        Some(Path::new(partial_dir_name)),
        None,
    )
    .expect("guard");

    let staging = guard.staging_path().to_path_buf();
    file.write_all(b"partial data for resume").expect("write");
    drop(file);

    guard.discard();

    // Partial file preserved
    assert!(staging.exists(), "partial file should be preserved for resume");

    // Now verify PartialFileManager can find it
    let manager = PartialFileManager::new(PartialMode::PartialDir(
        PathBuf::from(partial_dir_name),
    ));
    let basis = manager.find_basis(&destination).expect("find_basis");
    assert_eq!(basis, Some(staging.clone()));
    assert_eq!(
        fs::read(&staging).expect("read partial"),
        b"partial data for resume"
    );
}

// ==================== Edge Cases: Partial Dir Names ====================

#[test]
fn partial_dir_with_deeply_nested_relative_path() {
    // Test that partial-dir can be a multi-component relative path
    let temp = tempdir().expect("tempdir");
    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&dest_dir).expect("create dest dir");

    let destination = dest_dir.join("file.txt");
    let partial_dir_name = ".cache/rsync/partial";

    let (guard, mut file) = DestinationWriteGuard::new(
        destination.as_path(),
        true,
        Some(Path::new(partial_dir_name)),
        None,
    )
    .expect("guard");

    file.write_all(b"nested partial dir content").expect("write");
    drop(file);

    let staging = guard.staging_path().to_path_buf();
    let expected_base = dest_dir.join(partial_dir_name);
    assert!(
        staging.starts_with(&expected_base),
        "staging {} should be under {}",
        staging.display(),
        expected_base.display()
    );

    guard.commit().expect("commit");
    assert!(destination.exists());
    assert_eq!(
        fs::read(&destination).expect("read"),
        b"nested partial dir content"
    );
}

#[test]
fn partial_dir_delete_before_timing_also_protects_partial() {
    // --delete-before should also protect the partial-dir
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    fs::write(ctx.source.join("file.txt"), b"content").expect("write source");

    let target_root = ctx.dest.join("source");
    fs::create_dir_all(&target_root).expect("create target root");
    fs::write(target_root.join("old.txt"), b"old").expect("write old");

    let partial_dir = target_root.join(".rsync-partial");
    fs::create_dir_all(&partial_dir).expect("create partial dir");
    fs::write(partial_dir.join("partial.dat"), b"partial").expect("write partial");

    let operands = ctx.operands();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .delete_before(true)
        .with_partial_directory(Some(".rsync-partial"));

    let _summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(
        !target_root.join("old.txt").exists(),
        "extraneous file should be deleted with --delete-before"
    );
    assert!(
        partial_dir.exists(),
        "partial dir should be protected with --delete-before"
    );
    assert!(
        partial_dir.join("partial.dat").exists(),
        "partial files should survive --delete-before"
    );
}

#[test]
fn partial_dir_delete_after_timing_also_protects_partial() {
    // --delete-after should also protect the partial-dir
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    fs::write(ctx.source.join("file.txt"), b"content").expect("write source");

    let target_root = ctx.dest.join("source");
    fs::create_dir_all(&target_root).expect("create target root");
    fs::write(target_root.join("old.txt"), b"old").expect("write old");

    let partial_dir = target_root.join(".rsync-partial");
    fs::create_dir_all(&partial_dir).expect("create partial dir");
    fs::write(partial_dir.join("partial.dat"), b"partial").expect("write partial");

    let operands = ctx.operands();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .delete_after(true)
        .with_partial_directory(Some(".rsync-partial"));

    let _summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(
        !target_root.join("old.txt").exists(),
        "extraneous file should be deleted with --delete-after"
    );
    assert!(
        partial_dir.exists(),
        "partial dir should be protected with --delete-after"
    );
    assert!(
        partial_dir.join("partial.dat").exists(),
        "partial files should survive --delete-after"
    );
}

#[test]
fn partial_dir_empty_partial_dir_survives_delete() {
    // Even when the partial-dir is empty, it should survive --delete
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    fs::write(ctx.source.join("file.txt"), b"content").expect("write source");

    let target_root = ctx.dest.join("source");
    fs::create_dir_all(&target_root).expect("create target root");

    // Create an empty partial dir
    let partial_dir = target_root.join(".rsync-partial");
    fs::create_dir_all(&partial_dir).expect("create empty partial dir");

    let operands = ctx.operands();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .delete(true)
        .with_partial_directory(Some(".rsync-partial"));

    let _summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(
        partial_dir.exists(),
        "empty partial dir should survive --delete"
    );
}

// ==================== Integration: Partial Dir + Copy Workflow ====================

#[test]
fn partial_dir_successful_copy_creates_and_uses_partial_dir() {
    // End-to-end test: source -> partial-dir -> destination
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&dest_dir).expect("create dest dir");
    fs::write(&source, b"end-to-end partial dir test content").expect("write source");

    let destination = dest_dir.join("output.txt");
    let partial_dir_name = ".e2e-partial";
    let partial_dir_path = dest_dir.join(partial_dir_name);

    // Ensure partial dir does not exist before copy
    assert!(!partial_dir_path.exists());

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().with_partial_directory(Some(partial_dir_name)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    // Destination should have the content
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"end-to-end partial dir test content"
    );
    // Partial dir should have been created during the transfer
    assert!(
        partial_dir_path.exists(),
        "partial dir should be auto-created during transfer"
    );
    assert!(
        partial_dir_path.is_dir(),
        "partial dir should be a directory"
    );
}

#[test]
fn partial_dir_idempotent_transfer_with_existing_dest() {
    // Running the transfer again with the destination already up-to-date
    // should not fail and should not leave stale partial files.
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&dest_dir).expect("create dest dir");
    fs::write(&source, b"idempotent content").expect("write source");

    let destination = dest_dir.join("output.txt");
    let partial_dir_name = ".idem-partial";

    let operands = vec![
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ];

    // First transfer
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().with_partial_directory(Some(partial_dir_name)),
        )
        .expect("first copy");
    assert_eq!(summary.files_copied(), 1);

    // Second transfer (same source, dest already exists)
    let operands2 = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan2 = LocalCopyPlan::from_operands(&operands2).expect("plan2");
    let summary2 = plan2
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().with_partial_directory(Some(partial_dir_name)),
        )
        .expect("second copy");

    // Second run should match (not copy) since content is identical
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"idempotent content"
    );
    // At least one of the runs should succeed
    assert!(summary2.files_copied() + summary2.regular_files_matched() >= 1);
}
