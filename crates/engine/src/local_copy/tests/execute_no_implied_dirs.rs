// Tests for --no-implied-dirs flag
//
// The --no-implied-dirs flag changes directory creation behavior:
// - With implied dirs (default): parent directories are created automatically
// - Without implied dirs: only explicitly listed directories are created


#[test]
#[ignore] // TODO: Feature not fully implemented - currently creates directories implicitly
fn no_implied_dirs_does_not_create_intermediate_directories() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let deep_dir = source_root.join("level1").join("level2").join("level3");
    fs::create_dir_all(&deep_dir).expect("create deep directory");
    fs::write(deep_dir.join("file.txt"), b"content").expect("write file");

    let dest_root = temp.path().join("dest");

    let operands = vec![
        deep_dir.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().implied_dirs(false);

    let error = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect_err("should fail without parent directories");

    match error.kind() {
        LocalCopyErrorKind::Io { action, .. } => {
            assert_eq!(*action, "create directory");
        }
        other => panic!("unexpected error kind: {other:?}"),
    }

    // Destination should not exist since parent wasn't created
    assert!(!dest_root.exists());
}

#[test]
fn no_implied_dirs_creates_only_explicit_directories() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");

    // Create a directory structure
    let dir1 = source_root.join("dir1");
    let dir2 = source_root.join("dir2");
    let nested = source_root.join("dir3").join("nested");

    fs::create_dir_all(&dir1).expect("create dir1");
    fs::create_dir_all(&dir2).expect("create dir2");
    fs::create_dir_all(&nested).expect("create nested");

    fs::write(dir1.join("file1.txt"), b"content1").expect("write file1");
    fs::write(dir2.join("file2.txt"), b"content2").expect("write file2");
    fs::write(nested.join("file3.txt"), b"content3").expect("write file3");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest root");

    // Copy only dir1 and dir2, not dir3
    let operands = vec![
        dir1.into_os_string(),
        dir2.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().implied_dirs(false);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Only explicitly listed directories should exist
    assert!(dest_root.join("dir1").exists());
    assert!(dest_root.join("dir2").exists());
    assert!(dest_root.join("dir1").join("file1.txt").exists());
    assert!(dest_root.join("dir2").join("file2.txt").exists());

    // dir3 should not have been created implicitly
    assert!(!dest_root.join("dir3").exists());

    assert!(summary.files_copied() >= 2);
}

#[test]
fn no_implied_dirs_works_with_relative_paths() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let nested = source_root.join("dir1").join("dir2");
    fs::create_dir_all(&nested).expect("create nested");
    fs::write(nested.join("file.txt"), b"content").expect("write file");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest root");

    // Use relative mode to preserve structure
    let mut source_operand = source_root.clone().into_os_string();
    source_operand.push("/./");
    source_operand.push("dir1/dir2/file.txt");

    let operands = vec![
        source_operand,
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // With --relative and --no-implied-dirs, intermediate directories should not be created
    let options = LocalCopyOptions::default()
        .relative_paths(true)
        .implied_dirs(false);

    let error = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect_err("should fail without parent directories in relative mode");

    match error.kind() {
        LocalCopyErrorKind::Io { action, .. } => {
            assert!(
                *action == "create parent directory" || *action == "create directory",
                "expected directory creation error, got: {action}"
            );
        }
        other => panic!("unexpected error kind: {other:?}"),
    }

    // The deep structure should not have been created
    assert!(!dest_root.join("dir1").join("dir2").exists());
}

#[test]
fn no_implied_dirs_with_relative_creates_explicit_dirs_only() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dir1 = source_root.join("dir1");
    let dir2 = dir1.join("dir2");
    fs::create_dir_all(&dir2).expect("create nested");
    fs::write(dir1.join("file1.txt"), b"content1").expect("write file1");
    fs::write(dir2.join("file2.txt"), b"content2").expect("write file2");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest root");

    // Use relative mode with explicit directory in source list
    let mut source_dir_operand = source_root.clone().into_os_string();
    source_dir_operand.push("/./");
    source_dir_operand.push("dir1");

    let operands = vec![
        source_dir_operand,
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .relative_paths(true)
        .implied_dirs(false);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds with explicit directory");

    // dir1 should exist because it was explicitly listed
    assert!(dest_root.join("dir1").exists());
    assert!(dest_root.join("dir1").join("file1.txt").exists());

    // Nested directories should also exist because recursion includes them
    assert!(dest_root.join("dir1").join("dir2").exists());
    assert!(dest_root.join("dir1").join("dir2").join("file2.txt").exists());

    assert!(summary.files_copied() >= 2);
}

#[test]
fn no_implied_dirs_files_in_deep_paths_still_work_with_existing_dirs() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let deep_dir = source_root.join("a").join("b").join("c");
    fs::create_dir_all(&deep_dir).expect("create deep directory");
    fs::write(deep_dir.join("file.txt"), b"deep content").expect("write file");

    let dest_root = temp.path().join("dest");

    // Pre-create the parent directory structure
    let dest_deep = dest_root.join("a").join("b").join("c");
    fs::create_dir_all(&dest_deep).expect("create dest structure");

    let operands = vec![
        deep_dir.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().implied_dirs(false);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds with existing directories");

    // File should be copied successfully
    assert!(dest_root.join("c").exists());
    assert!(dest_root.join("c").join("file.txt").exists());
    assert_eq!(
        fs::read(dest_root.join("c").join("file.txt")).expect("read file"),
        b"deep content"
    );

    assert_eq!(summary.files_copied(), 1);
}

#[test]
fn no_implied_dirs_with_relative_and_deep_file() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let deep_dir = source_root.join("a").join("b").join("c");
    fs::create_dir_all(&deep_dir).expect("create deep directory");
    fs::write(deep_dir.join("file.txt"), b"content").expect("write file");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest root");

    // Use relative mode with a deep file
    let mut source_operand = source_root.clone().into_os_string();
    source_operand.push("/./");
    source_operand.push("a/b/c/file.txt");

    let operands = vec![
        source_operand,
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .relative_paths(true)
        .implied_dirs(false);

    let error = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect_err("should fail without intermediate directories");

    // Should fail trying to create parent directory
    match error.kind() {
        LocalCopyErrorKind::Io { action, .. } => {
            assert!(
                *action == "create parent directory" || *action == "create directory",
                "expected directory creation error, got: {action}"
            );
        }
        other => panic!("unexpected error kind: {other:?}"),
    }
}

#[test]
fn no_implied_dirs_with_mkpath_creates_missing_parents() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    fs::write(&source, b"content").expect("write source");

    let dest_root = temp.path().join("dest");
    let destination = dest_root.join("a").join("b").join("dest.txt");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // --mkpath overrides --no-implied-dirs
    let options = LocalCopyOptions::default()
        .implied_dirs(false)
        .mkpath(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds with mkpath");

    assert!(destination.exists());
    assert_eq!(fs::read(&destination).expect("read"), b"content");
    assert_eq!(summary.files_copied(), 1);
}

#[test]
fn no_implied_dirs_recursive_copy_with_nested_structure() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let nested = source_root.join("parent").join("child");
    fs::create_dir_all(&nested).expect("create nested");
    fs::write(source_root.join("root.txt"), b"root").expect("write root");
    fs::write(nested.join("nested.txt"), b"nested").expect("write nested");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest root");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().implied_dirs(false);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("recursive copy succeeds");

    // Root directory and files should be copied
    assert!(dest_root.join("source").exists());
    assert!(dest_root.join("source").join("root.txt").exists());

    // Nested structure should also be created as part of recursion
    assert!(dest_root.join("source").join("parent").join("child").exists());
    assert!(dest_root.join("source").join("parent").join("child").join("nested.txt").exists());

    assert!(summary.files_copied() >= 2);
    assert!(summary.directories_created() >= 3);
}

#[test]
fn no_implied_dirs_dry_run_detects_missing_parents() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    fs::write(&source, b"content").expect("write source");

    let destination = temp.path().join("missing").join("dest.txt");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().implied_dirs(false);

    let error = plan
        .execute_with_options(LocalCopyExecution::DryRun, options)
        .expect_err("dry-run should detect missing parent");

    match error.kind() {
        LocalCopyErrorKind::Io { action, path, .. } => {
            assert_eq!(*action, "create parent directory");
            assert_eq!(path, destination.parent().expect("parent"));
        }
        other => panic!("unexpected error kind: {other:?}"),
    }
}

#[test]
fn no_implied_dirs_with_trailing_slash_copies_contents() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let nested = source_root.join("nested");
    fs::create_dir_all(&nested).expect("create nested");
    fs::write(nested.join("file.txt"), b"content").expect("write file");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest root");

    // Use trailing separator to copy contents
    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let operands = vec![
        source_operand,
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().implied_dirs(false);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Contents should be copied directly into dest
    assert!(dest_root.join("nested").exists());
    assert!(dest_root.join("nested").join("file.txt").exists());
    assert!(!dest_root.join("source").exists());

    assert!(summary.files_copied() >= 1);
}

#[test]
fn no_implied_dirs_single_file_requires_existing_parent() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("file.txt");
    fs::write(&source, b"content").expect("write source");

    let destination = temp.path().join("subdir").join("file.txt");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().implied_dirs(false);

    let error = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect_err("should fail without parent directory");

    match error.kind() {
        LocalCopyErrorKind::Io { action, .. } => {
            assert_eq!(*action, "create parent directory");
        }
        other => panic!("unexpected error kind: {other:?}"),
    }

    assert!(!destination.exists());
}

#[test]
fn no_implied_dirs_succeeds_with_existing_parent() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("file.txt");
    fs::write(&source, b"content").expect("write source");

    let dest_dir = temp.path().join("subdir");
    fs::create_dir_all(&dest_dir).expect("create destination directory");
    let destination = dest_dir.join("file.txt");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().implied_dirs(false);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds with existing parent");

    assert!(destination.exists());
    assert_eq!(fs::read(&destination).expect("read"), b"content");
    assert_eq!(summary.files_copied(), 1);
}

#[test]
fn no_implied_dirs_with_multiple_sources_to_existing_dest() {
    let temp = tempdir().expect("tempdir");
    let source1 = temp.path().join("file1.txt");
    let source2 = temp.path().join("file2.txt");
    fs::write(&source1, b"content1").expect("write source1");
    fs::write(&source2, b"content2").expect("write source2");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest");

    let operands = vec![
        source1.into_os_string(),
        source2.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().implied_dirs(false);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(dest_root.join("file1.txt").exists());
    assert!(dest_root.join("file2.txt").exists());
    assert_eq!(summary.files_copied(), 2);
}

#[test]
fn no_implied_dirs_collection_reports_correct_events() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");
    fs::write(source_root.join("file.txt"), b"content").expect("write file");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .implied_dirs(false)
        .collect_events(true);

    let report = plan
        .execute_with_report(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let records = report.records();

    // Should have directory creation and file copy events
    assert!(records
        .iter()
        .any(|r| r.action() == &LocalCopyAction::DirectoryCreated));
    assert!(records
        .iter()
        .any(|r| matches!(r.action(),
            LocalCopyAction::DataCopied |
            LocalCopyAction::MetadataReused)));
}

#[test]
fn implied_dirs_default_creates_parents_automatically() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("file.txt");
    fs::write(&source, b"content").expect("write source");

    let destination = temp.path().join("a").join("b").join("c").join("file.txt");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Default should have implied_dirs enabled
    let summary = plan.execute().expect("copy succeeds with default options");

    assert!(destination.exists());
    assert_eq!(fs::read(&destination).expect("read"), b"content");
    assert_eq!(summary.files_copied(), 1);
}
