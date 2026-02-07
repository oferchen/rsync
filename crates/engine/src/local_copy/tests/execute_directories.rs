
#[test]
fn execute_with_trailing_separator_copies_contents() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let nested = source_root.join("nested");
    fs::create_dir_all(&nested).expect("create nested");
    fs::write(nested.join("file.txt"), b"contents").expect("write file");

    let dest_root = temp.path().join("dest");
    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan.execute().expect("copy succeeds");
    assert!(dest_root.join("nested").exists());
    assert!(!dest_root.join("source").exists());
    assert!(summary.files_copied() >= 1);
}

#[test]
fn execute_skips_directories_when_recursion_disabled_without_dirs() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let nested_dir = source_root.join("child");
    fs::create_dir_all(&nested_dir).expect("create nested dir");
    fs::write(nested_dir.join("file.txt"), b"payload").expect("write payload");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest root");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .recursive(false)
        .dirs(false)
        .collect_events(true);
    let report = plan
        .execute_with_report(LocalCopyExecution::Apply, options)
        .expect("execution succeeds");
    let summary = report.summary();

    assert_eq!(summary.directories_total(), 1);
    assert_eq!(summary.directories_created(), 0);
    assert_eq!(summary.files_copied(), 0);

    let records = report.records();
    assert!(
        records
            .iter()
            .any(|record| record.action() == &LocalCopyAction::SkippedDirectory),
        "expected skipped directory record, got {records:?}"
    );
    assert!(
        !dest_root.join("source").exists(),
        "destination should not contain skipped directory"
    );
}

#[test]
fn execute_creates_directories_when_dirs_enabled_without_recursion() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let nested_dir = source_root.join("child");
    fs::create_dir_all(&nested_dir).expect("create nested dir");
    fs::write(nested_dir.join("file.txt"), b"payload").expect("write payload");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest root");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .recursive(false)
        .dirs(true)
        .collect_events(true);
    let report = plan
        .execute_with_report(LocalCopyExecution::Apply, options)
        .expect("execution succeeds");
    let summary = report.summary();

    assert_eq!(summary.directories_total(), 1);
    assert_eq!(summary.directories_created(), 1);
    assert_eq!(summary.files_copied(), 0);

    let records = report.records();
    assert!(
        records.iter().any(|record| {
            record.action() == &LocalCopyAction::DirectoryCreated
                && record.relative_path() == std::path::Path::new("source")
        }),
        "expected directory creation record, got {records:?}"
    );
    assert!(dest_root.join("source").is_dir());
    assert!(
        !dest_root.join("source").join("child").exists(),
        "dirs flag should not create nested directories"
    );
}

#[test]
fn execute_into_child_directory_succeeds_without_recursing() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let nested_dir = source_root.join("dir");
    fs::create_dir_all(&nested_dir).expect("create nested dir");
    fs::write(source_root.join("root.txt"), b"root").expect("write root");
    fs::write(nested_dir.join("child.txt"), b"child").expect("write nested");

    let dest_root = source_root.join("child");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan.execute().expect("copy into child succeeds");

    assert_eq!(
        fs::read(dest_root.join("root.txt")).expect("read root copy"),
        b"root"
    );
    assert_eq!(
        fs::read(dest_root.join("dir").join("child.txt")).expect("read nested copy"),
        b"child"
    );
    assert!(
        !dest_root.join("child").exists(),
        "destination recursion detected at {}",
        dest_root.join("child").display()
    );
    assert!(summary.files_copied() >= 2);
}

#[test]
fn execute_with_delete_removes_extraneous_entries() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source root");
    fs::write(source_root.join("keep.txt"), b"fresh").expect("write keep");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest root");
    fs::write(dest_root.join("keep.txt"), b"stale").expect("write stale");
    fs::write(dest_root.join("extra.txt"), b"extra").expect("write extra");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().delete(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert_eq!(
        fs::read(dest_root.join("keep.txt")).expect("read keep"),
        b"fresh"
    );
    assert!(!dest_root.join("extra.txt").exists());
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.items_deleted(), 1);
}

#[test]
fn execute_with_delete_after_removes_extraneous_entries() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source root");
    fs::write(source_root.join("keep.txt"), b"fresh").expect("write keep");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest root");
    fs::write(dest_root.join("keep.txt"), b"stale").expect("write stale");
    fs::write(dest_root.join("extra.txt"), b"extra").expect("write extra");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().delete_after(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert_eq!(
        fs::read(dest_root.join("keep.txt")).expect("read keep"),
        b"fresh"
    );
    assert!(!dest_root.join("extra.txt").exists());
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.items_deleted(), 1);
}

#[test]
fn execute_with_delete_delay_removes_extraneous_entries() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source root");
    fs::write(source_root.join("keep.txt"), b"fresh").expect("write keep");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest root");
    fs::write(dest_root.join("keep.txt"), b"stale").expect("write stale");
    fs::write(dest_root.join("extra.txt"), b"extra").expect("write extra");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().delete_delay(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert_eq!(
        fs::read(dest_root.join("keep.txt")).expect("read keep"),
        b"fresh"
    );
    assert!(!dest_root.join("extra.txt").exists());
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.items_deleted(), 1);
}

#[test]
fn execute_with_delete_before_removes_conflicting_entries() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source root");
    fs::write(source_root.join("file"), b"fresh").expect("write source file");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(dest_root.join("file")).expect("create conflicting directory");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().delete_before(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds with delete-before");

    let target = dest_root.join("file");
    assert_eq!(fs::read(&target).expect("read copied file"), b"fresh");
    assert!(target.is_file());
    assert_eq!(summary.files_copied(), 1);
    assert!(summary.items_deleted() >= 1);
}

#[test]
fn execute_with_max_delete_limit_enforced() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source root");
    fs::write(source_root.join("keep.txt"), b"fresh").expect("write source file");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create destination root");
    fs::write(dest_root.join("keep.txt"), b"stale").expect("write stale");
    fs::write(dest_root.join("extra-1.txt"), b"extra").expect("write extra 1");
    fs::write(dest_root.join("extra-2.txt"), b"extra").expect("write extra 2");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .delete(true)
        .max_deletions(Some(1));

    let error = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect_err("max-delete should stop deletions");

    match error.kind() {
        LocalCopyErrorKind::DeleteLimitExceeded { skipped } => assert_eq!(*skipped, 1),
        other => panic!("unexpected error kind: {other:?}"),
    }

    let remaining = [
        dest_root.join("extra-1.txt").exists(),
        dest_root.join("extra-2.txt").exists(),
    ];
    assert!(remaining.iter().copied().any(|exists| exists));
    assert!(remaining.iter().copied().any(|exists| !exists));
}

#[test]
fn execute_with_max_delete_limit_in_dry_run_reports_error() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source root");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest root");
    fs::write(dest_root.join("obsolete.txt"), b"extra").expect("write extra");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .delete(true)
        .max_deletions(Some(0));

    let error = plan
        .execute_with_options(LocalCopyExecution::DryRun, options)
        .expect_err("dry-run should stop deletions when limit is zero");

    match error.kind() {
        LocalCopyErrorKind::DeleteLimitExceeded { skipped } => assert_eq!(*skipped, 1),
        other => panic!("unexpected error kind: {other:?}"),
    }

    assert!(dest_root.join("obsolete.txt").exists());
}

#[test]
fn execute_with_delete_respects_dry_run() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source root");
    fs::write(source_root.join("keep.txt"), b"fresh").expect("write keep");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest root");
    fs::write(dest_root.join("keep.txt"), b"stale").expect("write stale");
    fs::write(dest_root.join("extra.txt"), b"extra").expect("write extra");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().delete(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::DryRun, options)
        .expect("dry-run succeeds");

    assert_eq!(
        fs::read(dest_root.join("keep.txt")).expect("read keep"),
        b"stale"
    );
    assert!(dest_root.join("extra.txt").exists());
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.items_deleted(), 1);
}

#[test]
fn execute_with_dry_run_leaves_destination_absent() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"preview").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with(LocalCopyExecution::DryRun)
        .expect("dry-run succeeds");

    assert!(!destination.exists());
}

#[test]
fn execute_without_implied_dirs_requires_existing_parent() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("missing").join("dest.txt");
    fs::write(&source, b"data").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().implied_dirs(false);

    let error = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect_err("missing parent should error");

    match error.kind() {
        LocalCopyErrorKind::Io { action, path, .. } => {
            assert_eq!(*action, "create parent directory");
            assert_eq!(path, destination.parent().expect("parent"));
        }
        other => panic!("unexpected error kind: {other:?}"),
    }
    assert!(!destination.exists());
}

#[test]
fn execute_dry_run_without_implied_dirs_requires_existing_parent() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("missing").join("dest.txt");
    fs::write(&source, b"data").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().implied_dirs(false);

    let error = plan
        .execute_with_options(LocalCopyExecution::DryRun, options)
        .expect_err("dry-run should error");

    match error.kind() {
        LocalCopyErrorKind::Io { action, path, .. } => {
            assert_eq!(*action, "create parent directory");
            assert_eq!(path, destination.parent().expect("parent"));
        }
        other => panic!("unexpected error kind: {other:?}"),
    }
    assert!(!destination.exists());
}

#[test]
fn execute_with_implied_dirs_creates_missing_parents() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("missing").join("dest.txt");
    fs::write(&source, b"data").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute().expect("copy succeeds");

    assert!(destination.exists());
    assert_eq!(fs::read(&destination).expect("read dest"), b"data");
}

#[test]
fn execute_with_mkpath_creates_missing_parents_without_implied_dirs() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("missing").join("dest.txt");
    fs::write(&source, b"data").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().implied_dirs(false).mkpath(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(destination.exists());
    assert_eq!(fs::read(&destination).expect("read dest"), b"data");
}

#[test]
fn execute_with_dry_run_detects_directory_conflict() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    fs::write(&source, b"data").expect("write source");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest root");
    let conflict_dir = dest_root.join("source.txt");
    fs::create_dir_all(&conflict_dir).expect("create conflicting directory");

    let operands = vec![source.into_os_string(), dest_root.into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let error = plan
        .execute_with(LocalCopyExecution::DryRun)
        .expect_err("dry-run should detect conflict");

    match error.into_kind() {
        LocalCopyErrorKind::InvalidArgument(reason) => {
            assert_eq!(reason, LocalCopyArgumentError::ReplaceDirectoryWithFile);
        }
        other => panic!("unexpected error kind: {other:?}"),
    }
}

#[test]
fn execute_directory_replaces_file_when_force_enabled() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source-dir");
    fs::create_dir_all(&source_root).expect("create source directory");
    fs::write(source_root.join("file.txt"), b"payload").expect("write source file");

    let destination = temp.path().join("dest");
    fs::write(&destination, b"old").expect("write existing file");

    let operands = vec![
        source_root.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().force_replacements(true),
        )
        .expect("forced replacement succeeds");

    assert!(destination.is_dir(), "file should be replaced by directory");
    assert_eq!(
        fs::read(destination.join("file.txt")).expect("read copied file"),
        b"payload"
    );
    assert!(summary.directories_created() >= 1);
}

#[cfg(unix)]
#[test]
fn execute_preserves_hard_links() {
    use std::os::unix::fs::MetadataExt;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source root");
    let file_a = source_root.join("file-a");
    let file_b = source_root.join("file-b");
    fs::write(&file_a, b"shared").expect("write source file");
    fs::hard_link(&file_a, &file_b).expect("create hard link");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().hard_links(true);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let dest_a = dest_root.join("file-a");
    let dest_b = dest_root.join("file-b");
    let metadata_a = fs::metadata(&dest_a).expect("metadata a");
    let metadata_b = fs::metadata(&dest_b).expect("metadata b");

    assert_eq!(metadata_a.ino(), metadata_b.ino());
    assert_eq!(metadata_a.nlink(), 2);
    assert_eq!(metadata_b.nlink(), 2);
    assert_eq!(fs::read(&dest_a).expect("read dest a"), b"shared");
    assert_eq!(fs::read(&dest_b).expect("read dest b"), b"shared");
    assert!(summary.hard_links_created() >= 1);
}

#[cfg(unix)]
#[test]
fn execute_without_hard_links_materialises_independent_files() {
    use std::os::unix::fs::MetadataExt;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source root");
    let file_a = source_root.join("file-a");
    let file_b = source_root.join("file-b");
    fs::write(&file_a, b"shared").expect("write source file");
    fs::hard_link(&file_a, &file_b).expect("create hard link");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan.execute().expect("copy succeeds");

    let dest_a = dest_root.join("file-a");
    let dest_b = dest_root.join("file-b");
    let metadata_a = fs::metadata(&dest_a).expect("metadata a");
    let metadata_b = fs::metadata(&dest_b).expect("metadata b");

    assert_ne!(metadata_a.ino(), metadata_b.ino());
    assert_eq!(metadata_a.nlink(), 1);
    assert_eq!(metadata_b.nlink(), 1);
    assert_eq!(fs::read(&dest_a).expect("read dest a"), b"shared");
    assert_eq!(fs::read(&dest_b).expect("read dest b"), b"shared");
    assert_eq!(summary.hard_links_created(), 0);
}

// ============================================================================
// Directory Creation with Various Permissions Tests
// ============================================================================

#[cfg(unix)]
#[test]
fn execute_directory_preserves_mode_777() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source root");
    fs::set_permissions(&source_root, PermissionsExt::from_mode(0o777)).expect("set perms");
    fs::write(source_root.join("file.txt"), b"content").expect("write file");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().permissions(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let dest_metadata = fs::metadata(&dest_root).expect("dest metadata");
    assert_eq!(dest_metadata.permissions().mode() & 0o777, 0o777);
}

#[cfg(unix)]
#[test]
fn execute_directory_preserves_mode_755() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source root");
    fs::set_permissions(&source_root, PermissionsExt::from_mode(0o755)).expect("set perms");
    fs::write(source_root.join("file.txt"), b"content").expect("write file");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().permissions(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let dest_metadata = fs::metadata(&dest_root).expect("dest metadata");
    assert_eq!(dest_metadata.permissions().mode() & 0o777, 0o755);
}

#[cfg(unix)]
#[test]
fn execute_directory_preserves_mode_700() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source root");
    fs::set_permissions(&source_root, PermissionsExt::from_mode(0o700)).expect("set perms");
    fs::write(source_root.join("file.txt"), b"content").expect("write file");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().permissions(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let dest_metadata = fs::metadata(&dest_root).expect("dest metadata");
    assert_eq!(dest_metadata.permissions().mode() & 0o777, 0o700);
}

#[cfg(unix)]
#[test]
fn execute_directory_preserves_mode_750() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source root");
    fs::set_permissions(&source_root, PermissionsExt::from_mode(0o750)).expect("set perms");
    fs::write(source_root.join("file.txt"), b"content").expect("write file");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().permissions(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let dest_metadata = fs::metadata(&dest_root).expect("dest metadata");
    assert_eq!(dest_metadata.permissions().mode() & 0o777, 0o750);
}

#[cfg(unix)]
#[test]
fn execute_nested_directory_preserves_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let nested = source_root.join("nested");
    let deeply_nested = nested.join("deep");
    fs::create_dir_all(&deeply_nested).expect("create deeply nested");

    fs::set_permissions(&source_root, PermissionsExt::from_mode(0o755)).expect("set root perms");
    fs::set_permissions(&nested, PermissionsExt::from_mode(0o750)).expect("set nested perms");
    fs::set_permissions(&deeply_nested, PermissionsExt::from_mode(0o700)).expect("set deep perms");
    fs::write(deeply_nested.join("file.txt"), b"content").expect("write file");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().permissions(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let dest_nested = dest_root.join("nested");
    let dest_deep = dest_nested.join("deep");

    assert_eq!(fs::metadata(&dest_root).expect("root").permissions().mode() & 0o777, 0o755);
    assert_eq!(fs::metadata(&dest_nested).expect("nested").permissions().mode() & 0o777, 0o750);
    assert_eq!(fs::metadata(&dest_deep).expect("deep").permissions().mode() & 0o777, 0o700);
}

#[cfg(unix)]
#[test]
fn execute_directory_without_preserve_permissions_uses_default() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source root");
    fs::set_permissions(&source_root, PermissionsExt::from_mode(0o700)).expect("set restrictive perms");
    fs::write(source_root.join("file.txt"), b"content").expect("write file");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    // No permissions(true) - should not preserve exact mode
    let options = LocalCopyOptions::default();

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let dest_metadata = fs::metadata(&dest_root).expect("dest metadata");
    // Without preserve permissions, the mode should NOT be exactly 0o700
    // (it will be modified by umask or use a default)
    let dest_mode = dest_metadata.permissions().mode() & 0o777;
    // The test verifies that when permissions are NOT preserved,
    // the restrictive 0o700 is not necessarily maintained
    assert!(dest_root.is_dir());
    // We don't assert the exact mode since it depends on umask,
    // but we verify the directory was created successfully
    assert!(dest_mode > 0);
}

// ============================================================================
// Recursive Directory Handling Tests
// ============================================================================

#[test]
fn execute_recursive_copies_deep_hierarchy() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");

    // Create a deep hierarchy: source/a/b/c/d/e
    let deep_path = source_root.join("a").join("b").join("c").join("d").join("e");
    fs::create_dir_all(&deep_path).expect("create deep hierarchy");
    fs::write(deep_path.join("deep.txt"), b"deep content").expect("write deep file");
    fs::write(source_root.join("a").join("shallow.txt"), b"shallow").expect("write shallow");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan.execute().expect("copy succeeds");

    assert!(dest_root.join("a").join("b").join("c").join("d").join("e").join("deep.txt").exists());
    assert!(dest_root.join("a").join("shallow.txt").exists());
    assert_eq!(
        fs::read(dest_root.join("a").join("b").join("c").join("d").join("e").join("deep.txt"))
            .expect("read deep"),
        b"deep content"
    );
    assert!(summary.directories_created() >= 5);
}

#[test]
fn execute_recursive_copies_wide_hierarchy() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source root");

    // Create many sibling directories
    for i in 0..10 {
        let dir = source_root.join(format!("dir{i:02}"));
        fs::create_dir_all(&dir).expect("create sibling dir");
        fs::write(dir.join("file.txt"), format!("content{i}").as_bytes()).expect("write file");
    }

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan.execute().expect("copy succeeds");

    for i in 0..10 {
        let dest_file = dest_root.join(format!("dir{i:02}")).join("file.txt");
        assert!(dest_file.exists(), "file in dir{i:02} should exist");
        assert_eq!(
            fs::read(&dest_file).expect("read file"),
            format!("content{i}").as_bytes()
        );
    }
    assert!(summary.directories_created() >= 10);
    assert_eq!(summary.files_copied(), 10);
}

#[test]
fn execute_recursive_handles_empty_directories() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let empty_dir = source_root.join("empty");
    fs::create_dir_all(&empty_dir).expect("create empty dir");
    // Also create a non-empty sibling to ensure the copy actually runs
    let nonempty = source_root.join("nonempty");
    fs::create_dir_all(&nonempty).expect("create nonempty");
    fs::write(nonempty.join("file.txt"), b"content").expect("write file");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan.execute().expect("copy succeeds");

    assert!(dest_root.join("empty").is_dir());
    assert!(dest_root.join("nonempty").join("file.txt").exists());
    assert!(summary.directories_created() >= 2);
}

#[test]
fn execute_recursive_with_prune_empty_dirs_removes_empty_directories() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let empty_dir = source_root.join("empty");
    fs::create_dir_all(&empty_dir).expect("create empty dir");
    let nonempty = source_root.join("nonempty");
    fs::create_dir_all(&nonempty).expect("create nonempty");
    fs::write(nonempty.join("file.txt"), b"content").expect("write file");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().prune_empty_dirs(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(!dest_root.join("empty").exists(), "empty dir should be pruned");
    assert!(dest_root.join("nonempty").join("file.txt").exists());
}

#[test]
fn execute_recursive_disabled_only_copies_single_level() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let nested = source_root.join("nested");
    fs::create_dir_all(&nested).expect("create nested");
    fs::write(source_root.join("root.txt"), b"root").expect("write root file");
    fs::write(nested.join("nested.txt"), b"nested").expect("write nested file");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().recursive(false).dirs(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(dest_root.join("source").is_dir());
    assert!(!dest_root.join("source").join("nested").exists(), "nested dir should not be copied");
    assert!(!dest_root.join("source").join("root.txt").exists(), "files should not be copied without recursion");
}

// ============================================================================
// Error Case Tests
// ============================================================================

#[cfg(unix)]
#[test]
fn execute_directory_fails_on_permission_denied_when_creating() {
    use std::os::unix::fs::PermissionsExt;

    // Skip if running as root (root can write anywhere)
    if rustix::process::geteuid().as_raw() == 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");
    fs::write(source_root.join("file.txt"), b"content").expect("write file");

    // Create destination with no write permission
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest");
    fs::set_permissions(&dest_root, PermissionsExt::from_mode(0o555)).expect("make readonly");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let result = plan.execute();

    // Restore permissions for cleanup
    fs::set_permissions(&dest_root, PermissionsExt::from_mode(0o755)).expect("restore perms");

    let error = result.expect_err("should fail with permission denied");
    match error.kind() {
        LocalCopyErrorKind::Io { source, .. } => {
            assert_eq!(source.kind(), io::ErrorKind::PermissionDenied);
        }
        other => panic!("unexpected error kind: {other:?}"),
    }
}

#[test]
fn execute_directory_already_exists_succeeds() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");
    fs::write(source_root.join("file.txt"), b"content").expect("write file");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("pre-create dest");

    // Use trailing separator to copy contents directly into dest
    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Copy should succeed even if destination already exists
    let summary = plan.execute().expect("copy succeeds");

    assert!(dest_root.join("file.txt").exists());
    assert!(summary.files_copied() >= 1);
}

#[test]
fn execute_directory_nested_already_exists_merges_content() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let source_nested = source_root.join("nested");
    fs::create_dir_all(&source_nested).expect("create source nested");
    fs::write(source_nested.join("new.txt"), b"new content").expect("write new");

    let dest_root = temp.path().join("dest");
    let dest_nested = dest_root.join("nested");
    fs::create_dir_all(&dest_nested).expect("pre-create dest nested");
    fs::write(dest_nested.join("existing.txt"), b"existing content").expect("write existing");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute().expect("copy succeeds");

    // Both files should exist after merge
    assert!(dest_nested.join("new.txt").exists());
    assert!(dest_nested.join("existing.txt").exists());
    assert_eq!(fs::read(dest_nested.join("new.txt")).expect("read new"), b"new content");
    assert_eq!(fs::read(dest_nested.join("existing.txt")).expect("read existing"), b"existing content");
}

#[test]
fn execute_directory_errors_when_destination_is_file_without_force() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source dir");
    fs::write(source_root.join("child.txt"), b"content").expect("write child");

    // Destination is a file, not a directory
    let destination = temp.path().join("dest");
    fs::write(&destination, b"existing file").expect("write existing file");

    let operands = vec![
        source_root.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let error = plan.execute().expect_err("should fail to replace file with directory");

    match error.kind() {
        LocalCopyErrorKind::InvalidArgument(reason) => {
            assert_eq!(*reason, LocalCopyArgumentError::ReplaceNonDirectoryWithDirectory);
        }
        other => panic!("unexpected error kind: {other:?}"),
    }
}

// ============================================================================
// Dry-Run Mode Tests
// ============================================================================

#[test]
fn execute_dry_run_does_not_create_directory() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");
    fs::write(source_root.join("file.txt"), b"content").expect("write file");

    let dest_root = temp.path().join("dest");
    // Destination does not exist

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with(LocalCopyExecution::DryRun)
        .expect("dry-run succeeds");

    assert!(!dest_root.exists(), "destination should not be created in dry-run");
    assert!(summary.directories_created() >= 1, "should report would-be created directories");
}

#[test]
fn execute_dry_run_reports_correct_directory_counts() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let nested = source_root.join("a").join("b").join("c");
    fs::create_dir_all(&nested).expect("create nested");
    fs::write(nested.join("file.txt"), b"content").expect("write file");

    let dest_root = temp.path().join("dest");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with(LocalCopyExecution::DryRun)
        .expect("dry-run succeeds");

    assert!(!dest_root.exists());
    // source + a + b + c = 4 directories
    assert!(summary.directories_total() >= 4);
    assert!(summary.directories_created() >= 4);
}

#[test]
fn execute_dry_run_with_existing_destination_reports_no_creation() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");
    fs::write(source_root.join("file.txt"), b"new content").expect("write source");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("pre-create dest");
    fs::write(dest_root.join("file.txt"), b"old content").expect("write existing");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with(LocalCopyExecution::DryRun)
        .expect("dry-run succeeds");

    // Existing file should NOT be modified
    assert_eq!(fs::read(dest_root.join("file.txt")).expect("read"), b"old content");
    // Should report would-be copied files
    assert_eq!(summary.files_copied(), 1);
}

#[test]
fn execute_dry_run_with_collect_events_records_directory_creation() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let nested = source_root.join("nested");
    fs::create_dir_all(&nested).expect("create nested");
    fs::write(nested.join("file.txt"), b"content").expect("write file");

    let dest_root = temp.path().join("dest");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().collect_events(true);

    let report = plan
        .execute_with_report(LocalCopyExecution::DryRun, options)
        .expect("dry-run succeeds");

    let records = report.records();
    let dir_created_count = records
        .iter()
        .filter(|r| r.action() == &LocalCopyAction::DirectoryCreated)
        .count();

    assert!(dir_created_count >= 2, "should record directory creations, got {dir_created_count}");
    assert!(!dest_root.exists());
}

// ============================================================================
// Preserve Permissions Behavior Tests
// ============================================================================

#[cfg(unix)]
#[test]
fn execute_directory_preserve_times_sets_mtime() {
    use filetime::{FileTime, set_file_mtime};

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    // Write file first, then set directory mtime
    // (writing files modifies parent directory mtime)
    fs::write(source_root.join("file.txt"), b"content").expect("write file");

    let fixed_mtime = FileTime::from_unix_time(1_600_000_000, 0);
    set_file_mtime(&source_root, fixed_mtime).expect("set source mtime");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().times(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let dest_metadata = fs::metadata(&dest_root).expect("dest metadata");
    let dest_mtime = FileTime::from_last_modification_time(&dest_metadata);
    assert_eq!(dest_mtime, fixed_mtime);
}

#[cfg(unix)]
#[test]
fn execute_directory_omit_dir_times_does_not_set_mtime() {
    use filetime::{FileTime, set_file_mtime};

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    // Write file first, then set directory mtime
    fs::write(source_root.join("file.txt"), b"content").expect("write file");

    let fixed_mtime = FileTime::from_unix_time(1_600_000_000, 0);
    set_file_mtime(&source_root, fixed_mtime).expect("set source mtime");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().times(true).omit_dir_times(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let dest_metadata = fs::metadata(&dest_root).expect("dest metadata");
    let dest_mtime = FileTime::from_last_modification_time(&dest_metadata);
    // With omit_dir_times, the directory mtime should NOT be preserved
    assert_ne!(dest_mtime, fixed_mtime);
}

#[cfg(unix)]
#[test]
fn execute_directory_with_chmod_applies_modifiers() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");
    fs::set_permissions(&source_root, PermissionsExt::from_mode(0o777)).expect("set full perms");
    fs::write(source_root.join("file.txt"), b"content").expect("write file");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Apply chmod modifier to remove write from group and other
    let modifiers = ChmodModifiers::parse("Dgo-w").expect("parse chmod");
    let options = LocalCopyOptions::default()
        .permissions(true)
        .with_chmod(Some(modifiers));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let dest_metadata = fs::metadata(&dest_root).expect("dest metadata");
    // 0o777 with go-w applied to directories = 0o755
    assert_eq!(dest_metadata.permissions().mode() & 0o777, 0o755);
}

// ============================================================================
// Edge Case Tests
// ============================================================================

#[test]
fn execute_directory_with_special_characters_in_name() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let special_dir = source_root.join("dir with spaces & special-chars_123");
    fs::create_dir_all(&special_dir).expect("create special dir");
    fs::write(special_dir.join("file.txt"), b"content").expect("write file");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute().expect("copy succeeds");

    assert!(dest_root.join("dir with spaces & special-chars_123").join("file.txt").exists());
}

#[cfg(unix)]
#[test]
fn execute_directory_with_unicode_name() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let unicode_dir = source_root.join("dir_with_unicode_\u{1F600}_\u{4E2D}\u{6587}");
    fs::create_dir_all(&unicode_dir).expect("create unicode dir");
    fs::write(unicode_dir.join("file.txt"), b"content").expect("write file");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute().expect("copy succeeds");

    assert!(dest_root.join("dir_with_unicode_\u{1F600}_\u{4E2D}\u{6587}").join("file.txt").exists());
}

#[test]
fn execute_directory_one_file_system_stays_on_same_device() {
    // This test verifies that the one_file_system option is accepted
    // (actual cross-device testing would require mount points)
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let nested = source_root.join("nested");
    fs::create_dir_all(&nested).expect("create nested");
    fs::write(nested.join("file.txt"), b"content").expect("write file");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().one_file_system(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(dest_root.join("nested").join("file.txt").exists());
}

#[test]
fn execute_directory_ignore_existing_skips_existing_dirs() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let nested = source_root.join("nested");
    fs::create_dir_all(&nested).expect("create nested");
    fs::write(nested.join("new.txt"), b"new content").expect("write new");

    let dest_root = temp.path().join("dest");
    let dest_nested = dest_root.join("nested");
    fs::create_dir_all(&dest_nested).expect("pre-create dest nested");
    fs::write(dest_nested.join("existing.txt"), b"existing").expect("write existing");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().ignore_existing(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // The existing file should still be there, the new file should be added
    assert!(dest_nested.join("existing.txt").exists());
    // Note: ignore_existing affects files, not directories
    // The new.txt should be created since it didn't exist
    assert!(dest_nested.join("new.txt").exists());
}

#[test]
fn execute_directory_existing_only_skips_new_dirs() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let new_dir = source_root.join("new_dir");
    let existing_dir = source_root.join("existing_dir");
    fs::create_dir_all(&new_dir).expect("create new_dir");
    fs::create_dir_all(&existing_dir).expect("create existing_dir");
    fs::write(new_dir.join("file.txt"), b"new").expect("write new");
    fs::write(existing_dir.join("file.txt"), b"existing_src").expect("write existing");

    let dest_root = temp.path().join("dest");
    let dest_existing = dest_root.join("existing_dir");
    fs::create_dir_all(&dest_existing).expect("pre-create existing_dir at dest");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().existing_only(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // new_dir should NOT be created because it doesn't exist at destination
    assert!(!dest_root.join("new_dir").exists());
    // existing_dir exists but file.txt does NOT exist in dest, so it's skipped
    assert!(!dest_existing.join("file.txt").exists());
    // No files should have been copied since none existed at destination
    assert_eq!(summary.files_copied(), 0);
    // The existing_dir was present at destination so it should be counted
    assert!(dest_existing.is_dir());
}

#[test]
fn execute_multiple_source_directories_to_destination() {
    let temp = tempdir().expect("tempdir");
    let source_a = temp.path().join("source_a");
    let source_b = temp.path().join("source_b");
    fs::create_dir_all(&source_a).expect("create source_a");
    fs::create_dir_all(&source_b).expect("create source_b");
    fs::write(source_a.join("a.txt"), b"from a").expect("write a");
    fs::write(source_b.join("b.txt"), b"from b").expect("write b");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest");

    let operands = vec![
        source_a.into_os_string(),
        source_b.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute().expect("copy succeeds");

    assert!(dest_root.join("source_a").join("a.txt").exists());
    assert!(dest_root.join("source_b").join("b.txt").exists());
}

#[test]
fn execute_directory_update_only_copies_newer() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");
    fs::write(source_root.join("file.txt"), b"new content").expect("write source");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest");
    fs::write(dest_root.join("file.txt"), b"old content").expect("write dest");

    // Make source file newer
    let source_file = source_root.join("file.txt");
    let new_time = filetime::FileTime::from_unix_time(2_000_000_000, 0);
    filetime::set_file_mtime(&source_file, new_time).expect("set source mtime");

    // Make dest file older
    let dest_file = dest_root.join("file.txt");
    let old_time = filetime::FileTime::from_unix_time(1_000_000_000, 0);
    filetime::set_file_mtime(&dest_file, old_time).expect("set dest mtime");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().update(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert_eq!(fs::read(dest_root.join("file.txt")).expect("read"), b"new content");
    assert_eq!(summary.files_copied(), 1);
}

#[test]
fn execute_directory_update_skips_older_files() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");
    fs::write(source_root.join("file.txt"), b"old content").expect("write source");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest");
    fs::write(dest_root.join("file.txt"), b"new content").expect("write dest");

    // Make source file older
    let source_file = source_root.join("file.txt");
    let old_time = filetime::FileTime::from_unix_time(1_000_000_000, 0);
    filetime::set_file_mtime(&source_file, old_time).expect("set source mtime");

    // Make dest file newer
    let dest_file = dest_root.join("file.txt");
    let new_time = filetime::FileTime::from_unix_time(2_000_000_000, 0);
    filetime::set_file_mtime(&dest_file, new_time).expect("set dest mtime");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().update(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Dest should keep its newer content
    assert_eq!(fs::read(dest_root.join("file.txt")).expect("read"), b"new content");
    assert_eq!(summary.files_copied(), 0);
}

// ============================================================================
// Combined Options Tests
// ============================================================================

#[cfg(unix)]
#[test]
fn execute_directory_archive_mode_preserves_all() {
    use std::os::unix::fs::PermissionsExt;
    use filetime::{FileTime, set_file_mtime};

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let nested = source_root.join("nested");
    fs::create_dir_all(&nested).expect("create nested");

    // Write files first, then set directory mtimes
    // (writing files modifies parent directory mtime)
    fs::write(nested.join("file.txt"), b"content").expect("write file");

    fs::set_permissions(&source_root, PermissionsExt::from_mode(0o755)).expect("set root perms");
    fs::set_permissions(&nested, PermissionsExt::from_mode(0o750)).expect("set nested perms");

    let fixed_mtime = FileTime::from_unix_time(1_600_000_000, 0);
    // Set mtimes after all file operations are complete
    set_file_mtime(&nested, fixed_mtime).expect("set nested mtime");
    set_file_mtime(&source_root, fixed_mtime).expect("set root mtime");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Archive mode: recursive + preserve permissions + preserve times + preserve links
    let options = LocalCopyOptions::default()
        .recursive(true)
        .permissions(true)
        .times(true)
        .links(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let dest_nested = dest_root.join("nested");

    // Check permissions
    assert_eq!(fs::metadata(&dest_root).expect("root").permissions().mode() & 0o777, 0o755);
    assert_eq!(fs::metadata(&dest_nested).expect("nested").permissions().mode() & 0o777, 0o750);

    // Check times
    let root_mtime = FileTime::from_last_modification_time(&fs::metadata(&dest_root).expect("root"));
    let nested_mtime = FileTime::from_last_modification_time(&fs::metadata(&dest_nested).expect("nested"));
    assert_eq!(root_mtime, fixed_mtime);
    assert_eq!(nested_mtime, fixed_mtime);
}

#[test]
fn execute_dry_run_with_delete_and_force_reports_all_actions() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let nested = source_root.join("nested");
    fs::create_dir_all(&nested).expect("create nested");
    fs::write(nested.join("keep.txt"), b"keep").expect("write keep");

    let dest_root = temp.path().join("dest");
    let dest_nested = dest_root.join("nested");
    fs::create_dir_all(&dest_nested).expect("create dest nested");
    fs::write(dest_nested.join("keep.txt"), b"old").expect("write old");
    fs::write(dest_nested.join("extra.txt"), b"extra").expect("write extra");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .delete(true)
        .force_replacements(true)
        .collect_events(true);

    let report = plan
        .execute_with_report(LocalCopyExecution::DryRun, options)
        .expect("dry-run succeeds");

    let summary = report.summary();

    // Verify nothing was actually modified
    assert_eq!(fs::read(dest_nested.join("keep.txt")).expect("read keep"), b"old");
    assert!(dest_nested.join("extra.txt").exists());

    // Verify dry-run reported would-be actions
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.items_deleted(), 1);
}

// ============================================================================
// Recursive Directory Creation Tests
// ============================================================================

#[test]
fn execute_recursive_creates_mixed_content_hierarchy() {
    // A hierarchy with files at various depths, empty dirs, and deep nesting
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(source_root.join("a").join("b").join("c")).expect("create deep");
    fs::create_dir_all(source_root.join("a").join("empty_sibling")).expect("create empty sibling");
    fs::create_dir_all(source_root.join("x").join("y")).expect("create xy");
    fs::write(source_root.join("root.txt"), b"root").expect("write root");
    fs::write(source_root.join("a").join("mid.txt"), b"mid").expect("write mid");
    fs::write(
        source_root.join("a").join("b").join("c").join("deep.txt"),
        b"deep",
    )
    .expect("write deep");
    fs::write(source_root.join("x").join("y").join("leaf.txt"), b"leaf").expect("write leaf");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan.execute().expect("copy succeeds");

    assert_eq!(
        fs::read(dest_root.join("root.txt")).expect("read root"),
        b"root"
    );
    assert_eq!(
        fs::read(dest_root.join("a").join("mid.txt")).expect("read mid"),
        b"mid"
    );
    assert_eq!(
        fs::read(
            dest_root
                .join("a")
                .join("b")
                .join("c")
                .join("deep.txt")
        )
        .expect("read deep"),
        b"deep"
    );
    assert_eq!(
        fs::read(dest_root.join("x").join("y").join("leaf.txt")).expect("read leaf"),
        b"leaf"
    );
    assert!(dest_root.join("a").join("empty_sibling").is_dir());
    assert_eq!(summary.files_copied(), 4);
    // source + a + b + c + empty_sibling + x + y = 7
    assert!(summary.directories_created() >= 7);
}

#[test]
fn execute_recursive_creates_only_subdirs_no_files() {
    // Source has a tree of directories with no files at all
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(source_root.join("a").join("b")).expect("create a/b");
    fs::create_dir_all(source_root.join("c")).expect("create c");
    fs::create_dir_all(source_root.join("d").join("e").join("f")).expect("create d/e/f");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan.execute().expect("copy succeeds");

    assert!(dest_root.join("a").join("b").is_dir());
    assert!(dest_root.join("c").is_dir());
    assert!(dest_root.join("d").join("e").join("f").is_dir());
    assert_eq!(summary.files_copied(), 0);
    // source + a + b + c + d + e + f = 7
    assert!(summary.directories_created() >= 7);
}

#[test]
fn execute_recursive_files_at_every_level() {
    // Files at each nesting level to ensure all are copied
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(source_root.join("l1").join("l2").join("l3")).expect("create dirs");
    fs::write(source_root.join("f0.txt"), b"level0").expect("write f0");
    fs::write(source_root.join("l1").join("f1.txt"), b"level1").expect("write f1");
    fs::write(
        source_root.join("l1").join("l2").join("f2.txt"),
        b"level2",
    )
    .expect("write f2");
    fs::write(
        source_root.join("l1").join("l2").join("l3").join("f3.txt"),
        b"level3",
    )
    .expect("write f3");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan.execute().expect("copy succeeds");

    assert_eq!(
        fs::read(dest_root.join("f0.txt")).expect("read f0"),
        b"level0"
    );
    assert_eq!(
        fs::read(dest_root.join("l1").join("f1.txt")).expect("read f1"),
        b"level1"
    );
    assert_eq!(
        fs::read(dest_root.join("l1").join("l2").join("f2.txt")).expect("read f2"),
        b"level2"
    );
    assert_eq!(
        fs::read(
            dest_root
                .join("l1")
                .join("l2")
                .join("l3")
                .join("f3.txt")
        )
        .expect("read f3"),
        b"level3"
    );
    assert_eq!(summary.files_copied(), 4);
}

#[test]
fn execute_recursive_idempotent_second_run() {
    // Running the same copy twice should succeed; second run copies nothing new
    let ctx = test_helpers::setup_copy_test();
    test_helpers::create_file_with_mtime(
        &ctx.source.join("sub").join("file.txt"),
        b"content",
        test_helpers::TEST_TIMESTAMP,
    );

    let operands = ctx.operands_with_trailing_separator();
    let options = LocalCopyOptions::default().times(true);

    // First run: copies the file and creates directories
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan1");
    let summary1 = plan
        .execute_with_options(LocalCopyExecution::Apply, options.clone())
        .expect("first copy succeeds");
    assert_eq!(summary1.files_copied(), 1);
    assert!(summary1.directories_created() >= 1);

    // Second run: file is unchanged (same size + mtime) so should not be re-copied
    let plan2 = LocalCopyPlan::from_operands(&operands).expect("plan2");
    let summary2 = plan2
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("second copy succeeds");

    assert_eq!(summary2.files_copied(), 0);
    assert_eq!(summary2.directories_created(), 0);
    assert_eq!(ctx.read_dest("sub/file.txt"), b"content");
}

#[test]
fn execute_trailing_separator_with_nested_empty_dirs() {
    // Trailing separator + nested empty directories
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(source_root.join("empty1")).expect("create empty1");
    fs::create_dir_all(source_root.join("empty2").join("nested_empty")).expect("create nested empty");
    fs::write(source_root.join("file.txt"), b"root file").expect("write root file");

    let dest_root = temp.path().join("dest");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan.execute().expect("copy succeeds");

    assert!(dest_root.join("empty1").is_dir());
    assert!(dest_root.join("empty2").join("nested_empty").is_dir());
    assert_eq!(
        fs::read(dest_root.join("file.txt")).expect("read"),
        b"root file"
    );
    assert!(!dest_root.join("source").exists(), "should not contain source dir itself");
    assert_eq!(summary.files_copied(), 1);
}

// ============================================================================
// --mkpath Behavior Tests
// ============================================================================

#[test]
fn execute_mkpath_creates_deeply_nested_missing_parents() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    fs::write(&source, b"mkpath deep").expect("write source");

    let destination = temp
        .path()
        .join("a")
        .join("b")
        .join("c")
        .join("d")
        .join("dest.txt");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().implied_dirs(false).mkpath(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(destination.exists());
    assert_eq!(fs::read(&destination).expect("read dest"), b"mkpath deep");
}

#[test]
fn execute_mkpath_with_directory_source_creates_parents() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");
    fs::write(source_root.join("file.txt"), b"content").expect("write file");

    let dest_root = temp.path().join("nested").join("deep").join("dest");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().implied_dirs(false).mkpath(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(dest_root.is_dir());
    assert!(dest_root.join("file.txt").exists());
    assert_eq!(
        fs::read(dest_root.join("file.txt")).expect("read"),
        b"content"
    );
}

#[test]
fn execute_mkpath_dry_run_does_not_create_parents() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    fs::write(&source, b"mkpath dry").expect("write source");

    let destination = temp.path().join("missing").join("deep").join("dest.txt");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().implied_dirs(false).mkpath(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::DryRun, options)
        .expect("dry-run succeeds");

    assert!(!destination.exists());
    assert!(!temp.path().join("missing").exists());
    assert_eq!(summary.files_copied(), 1);
}

#[test]
fn execute_mkpath_with_existing_parents_succeeds() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    fs::write(&source, b"data").expect("write source");

    let parent = temp.path().join("already").join("exists");
    fs::create_dir_all(&parent).expect("create parent");
    let destination = parent.join("dest.txt");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().implied_dirs(false).mkpath(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert_eq!(fs::read(&destination).expect("read"), b"data");
}

// ============================================================================
// --prune-empty-dirs Behavior Tests (directory-specific scenarios)
// ============================================================================

#[test]
fn execute_prune_empty_dirs_nested_hierarchy_file_at_bottom() {
    // Deep nesting: only the very deepest directory has a file
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let deep = source_root.join("a").join("b").join("c").join("d");
    fs::create_dir_all(&deep).expect("create deep");
    fs::write(deep.join("bottom.txt"), b"bottom").expect("write bottom");
    // Create a parallel empty branch
    fs::create_dir_all(source_root.join("a").join("empty_branch")).expect("create empty branch");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().prune_empty_dirs(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Full path to the bottom file should exist
    assert!(dest_root.join("a").join("b").join("c").join("d").join("bottom.txt").exists());
    // Empty branch should be pruned
    assert!(
        !dest_root.join("a").join("empty_branch").exists(),
        "empty branch should be pruned"
    );
}

#[test]
fn execute_prune_empty_dirs_with_trailing_separator_and_nested() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(source_root.join("populated").join("sub")).expect("create populated");
    fs::create_dir_all(source_root.join("barren").join("sub")).expect("create barren");
    fs::write(
        source_root.join("populated").join("sub").join("data.txt"),
        b"data",
    )
    .expect("write data");

    let dest_root = temp.path().join("dest");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().prune_empty_dirs(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(dest_root.join("populated").join("sub").join("data.txt").exists());
    assert!(
        !dest_root.join("barren").exists(),
        "barren directory tree should be pruned"
    );
}

#[test]
fn execute_prune_empty_dirs_preserves_dir_with_only_subdirs_containing_files() {
    // Parent dir has no files of its own, but grandchild does
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(source_root.join("parent").join("child")).expect("create dirs");
    fs::write(
        source_root.join("parent").join("child").join("file.txt"),
        b"deep",
    )
    .expect("write file");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().prune_empty_dirs(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(
        dest_root.join("parent").is_dir(),
        "parent should be kept because grandchild has files"
    );
    assert!(dest_root.join("parent").join("child").join("file.txt").exists());
}

// ============================================================================
// --omit-dir-times Behavior Tests (directory-specific scenarios)
// ============================================================================

#[cfg(unix)]
#[test]
fn execute_omit_dir_times_nested_dirs_preserves_file_times_at_all_levels() {
    use filetime::{FileTime, set_file_mtime};

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let nested = source_root.join("sub1").join("sub2");
    fs::create_dir_all(&nested).expect("create nested");

    let file_root = source_root.join("f_root.txt");
    let file_sub1 = source_root.join("sub1").join("f_sub1.txt");
    let file_sub2 = nested.join("f_sub2.txt");
    fs::write(&file_root, b"root").expect("write root file");
    fs::write(&file_sub1, b"sub1").expect("write sub1 file");
    fs::write(&file_sub2, b"sub2").expect("write sub2 file");

    let root_file_mtime = FileTime::from_unix_time(1_400_000_000, 0);
    let sub1_file_mtime = FileTime::from_unix_time(1_450_000_000, 0);
    let sub2_file_mtime = FileTime::from_unix_time(1_500_000_000, 0);
    let dir_mtime = FileTime::from_unix_time(1_600_000_000, 0);

    set_file_mtime(&file_root, root_file_mtime).expect("set root file mtime");
    set_file_mtime(&file_sub1, sub1_file_mtime).expect("set sub1 file mtime");
    set_file_mtime(&file_sub2, sub2_file_mtime).expect("set sub2 file mtime");
    set_file_mtime(&nested, dir_mtime).expect("set sub2 dir mtime");
    set_file_mtime(nested.parent().unwrap(), dir_mtime).expect("set sub1 dir mtime");
    set_file_mtime(&source_root, dir_mtime).expect("set root dir mtime");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().times(true).omit_dir_times(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // File times should be preserved
    let dest_root_file_mtime = FileTime::from_last_modification_time(
        &fs::metadata(dest_root.join("f_root.txt")).expect("root file metadata"),
    );
    let dest_sub1_file_mtime = FileTime::from_last_modification_time(
        &fs::metadata(dest_root.join("sub1").join("f_sub1.txt")).expect("sub1 file metadata"),
    );
    let dest_sub2_file_mtime = FileTime::from_last_modification_time(
        &fs::metadata(dest_root.join("sub1").join("sub2").join("f_sub2.txt"))
            .expect("sub2 file metadata"),
    );
    assert_eq!(dest_root_file_mtime, root_file_mtime);
    assert_eq!(dest_sub1_file_mtime, sub1_file_mtime);
    assert_eq!(dest_sub2_file_mtime, sub2_file_mtime);

    // Directory times should NOT be preserved
    let dest_root_dir_mtime = FileTime::from_last_modification_time(
        &fs::metadata(&dest_root).expect("root dir metadata"),
    );
    let dest_sub1_dir_mtime = FileTime::from_last_modification_time(
        &fs::metadata(dest_root.join("sub1")).expect("sub1 dir metadata"),
    );
    let dest_sub2_dir_mtime = FileTime::from_last_modification_time(
        &fs::metadata(dest_root.join("sub1").join("sub2")).expect("sub2 dir metadata"),
    );
    assert_ne!(dest_root_dir_mtime, dir_mtime);
    assert_ne!(dest_sub1_dir_mtime, dir_mtime);
    assert_ne!(dest_sub2_dir_mtime, dir_mtime);
}

#[cfg(unix)]
#[test]
fn execute_omit_dir_times_with_trailing_separator() {
    use filetime::{FileTime, set_file_mtime};

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let nested = source_root.join("sub");
    fs::create_dir_all(&nested).expect("create sub");
    fs::write(nested.join("file.txt"), b"content").expect("write file");

    let file_mtime = FileTime::from_unix_time(1_500_000_000, 0);
    let dir_mtime = FileTime::from_unix_time(1_600_000_000, 0);
    set_file_mtime(&nested.join("file.txt"), file_mtime).expect("set file mtime");
    set_file_mtime(&nested, dir_mtime).expect("set sub dir mtime");
    set_file_mtime(&source_root, dir_mtime).expect("set root dir mtime");

    let dest_root = temp.path().join("dest");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().times(true).omit_dir_times(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // File time preserved
    let dest_file_mtime = FileTime::from_last_modification_time(
        &fs::metadata(dest_root.join("sub").join("file.txt")).expect("file metadata"),
    );
    assert_eq!(dest_file_mtime, file_mtime);

    // Directory time NOT preserved
    let dest_sub_dir_mtime = FileTime::from_last_modification_time(
        &fs::metadata(dest_root.join("sub")).expect("sub metadata"),
    );
    assert_ne!(dest_sub_dir_mtime, dir_mtime);
}

// ============================================================================
// Directory Permission Handling Tests
// ============================================================================

#[cfg(unix)]
#[test]
fn execute_directory_permissions_with_chmod_nested_applies_to_all_dirs() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let nested = source_root.join("a").join("b");
    fs::create_dir_all(&nested).expect("create nested");
    fs::set_permissions(&source_root, PermissionsExt::from_mode(0o777)).expect("set root perms");
    fs::set_permissions(source_root.join("a").as_path(), PermissionsExt::from_mode(0o777))
        .expect("set a perms");
    fs::set_permissions(&nested, PermissionsExt::from_mode(0o777)).expect("set b perms");
    fs::write(nested.join("file.txt"), b"content").expect("write file");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Remove write from group and other on directories: 0o777 -> 0o755
    let modifiers = ChmodModifiers::parse("Dgo-w").expect("parse chmod");
    let options = LocalCopyOptions::default()
        .permissions(true)
        .with_chmod(Some(modifiers));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert_eq!(
        fs::metadata(&dest_root).expect("root").permissions().mode() & 0o777,
        0o755
    );
    assert_eq!(
        fs::metadata(dest_root.join("a")).expect("a").permissions().mode() & 0o777,
        0o755
    );
    assert_eq!(
        fs::metadata(dest_root.join("a").join("b"))
            .expect("b")
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
}

#[cfg(unix)]
#[test]
fn execute_directory_preserves_mixed_permissions_in_hierarchy() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let public_dir = source_root.join("public");
    let private_dir = source_root.join("private");
    let shared_dir = source_root.join("shared");
    fs::create_dir_all(&public_dir).expect("create public");
    fs::create_dir_all(&private_dir).expect("create private");
    fs::create_dir_all(&shared_dir).expect("create shared");

    fs::set_permissions(&source_root, PermissionsExt::from_mode(0o755)).expect("set root");
    fs::set_permissions(&public_dir, PermissionsExt::from_mode(0o755)).expect("set public");
    fs::set_permissions(&private_dir, PermissionsExt::from_mode(0o700)).expect("set private");
    fs::set_permissions(&shared_dir, PermissionsExt::from_mode(0o750)).expect("set shared");

    fs::write(public_dir.join("pub.txt"), b"pub").expect("write pub");
    fs::write(private_dir.join("priv.txt"), b"priv").expect("write priv");
    fs::write(shared_dir.join("sh.txt"), b"sh").expect("write sh");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().permissions(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert_eq!(
        fs::metadata(&dest_root).expect("root").permissions().mode() & 0o777,
        0o755
    );
    assert_eq!(
        fs::metadata(dest_root.join("public"))
            .expect("public")
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
    assert_eq!(
        fs::metadata(dest_root.join("private"))
            .expect("private")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(dest_root.join("shared"))
            .expect("shared")
            .permissions()
            .mode()
            & 0o777,
        0o750
    );
}

#[cfg(unix)]
#[test]
fn execute_directory_permissions_with_dry_run_does_not_set() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");
    fs::set_permissions(&source_root, PermissionsExt::from_mode(0o700)).expect("set perms");
    fs::write(source_root.join("file.txt"), b"content").expect("write file");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().permissions(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::DryRun, options)
        .expect("dry-run succeeds");

    assert!(!dest_root.exists(), "destination should not be created in dry-run");
    assert!(summary.directories_created() >= 1);
}

// ============================================================================
// Nested Directory Hierarchy Tests
// ============================================================================

#[test]
fn execute_nested_hierarchy_with_overlapping_names() {
    // Test directories with similar names at different levels
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(source_root.join("dir").join("dir").join("dir")).expect("create same-name dirs");
    fs::write(source_root.join("dir").join("file.txt"), b"level1").expect("write level1");
    fs::write(
        source_root.join("dir").join("dir").join("file.txt"),
        b"level2",
    )
    .expect("write level2");
    fs::write(
        source_root
            .join("dir")
            .join("dir")
            .join("dir")
            .join("file.txt"),
        b"level3",
    )
    .expect("write level3");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan.execute().expect("copy succeeds");

    assert_eq!(
        fs::read(dest_root.join("dir").join("file.txt")).expect("l1"),
        b"level1"
    );
    assert_eq!(
        fs::read(dest_root.join("dir").join("dir").join("file.txt")).expect("l2"),
        b"level2"
    );
    assert_eq!(
        fs::read(
            dest_root
                .join("dir")
                .join("dir")
                .join("dir")
                .join("file.txt")
        )
        .expect("l3"),
        b"level3"
    );
    assert_eq!(summary.files_copied(), 3);
}

#[test]
fn execute_nested_hierarchy_mixed_empty_and_populated() {
    // Verify that a hierarchy with intermixed empty and populated directories works
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    // populated -> empty -> populated
    fs::create_dir_all(source_root.join("top").join("empty").join("bottom")).expect("create dirs");
    fs::write(source_root.join("top").join("top.txt"), b"top").expect("write top");
    fs::write(
        source_root.join("top").join("empty").join("bottom").join("bottom.txt"),
        b"bottom",
    )
    .expect("write bottom");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan.execute().expect("copy succeeds");

    assert_eq!(
        fs::read(dest_root.join("top").join("top.txt")).expect("read top"),
        b"top"
    );
    assert!(
        dest_root.join("top").join("empty").is_dir(),
        "intermediate empty dir should be created"
    );
    assert_eq!(
        fs::read(
            dest_root
                .join("top")
                .join("empty")
                .join("bottom")
                .join("bottom.txt")
        )
        .expect("read bottom"),
        b"bottom"
    );
    assert_eq!(summary.files_copied(), 2);
}

#[test]
fn execute_directory_with_many_siblings() {
    // Stress test with many sibling directories
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    let count = 50;
    for i in 0..count {
        let dir = source_root.join(format!("dir_{i:04}"));
        fs::create_dir_all(&dir).expect("create dir");
        fs::write(dir.join("f.txt"), format!("content_{i}").as_bytes()).expect("write file");
    }

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan.execute().expect("copy succeeds");

    for i in 0..count {
        let dest_file = dest_root.join(format!("dir_{i:04}")).join("f.txt");
        assert!(dest_file.exists(), "dir_{i:04}/f.txt should exist");
        assert_eq!(
            fs::read(&dest_file).expect("read"),
            format!("content_{i}").as_bytes()
        );
    }
    assert_eq!(summary.files_copied(), count);
    assert!(summary.directories_created() >= count + 1); // source + N dirs
}

// ============================================================================
// Combined Directory Options Tests
// ============================================================================

#[cfg(unix)]
#[test]
fn execute_directory_prune_empty_dirs_with_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let kept = source_root.join("kept");
    let pruned = source_root.join("pruned");
    fs::create_dir_all(&kept).expect("create kept");
    fs::create_dir_all(&pruned).expect("create pruned");
    fs::set_permissions(&kept, PermissionsExt::from_mode(0o750)).expect("set kept perms");
    fs::set_permissions(&pruned, PermissionsExt::from_mode(0o700)).expect("set pruned perms");
    fs::write(kept.join("file.txt"), b"data").expect("write file");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .prune_empty_dirs(true)
        .permissions(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(dest_root.join("kept").is_dir());
    assert_eq!(
        fs::metadata(dest_root.join("kept"))
            .expect("kept")
            .permissions()
            .mode()
            & 0o777,
        0o750
    );
    assert!(
        !dest_root.join("pruned").exists(),
        "empty pruned dir should not exist"
    );
}

#[cfg(unix)]
#[test]
fn execute_directory_prune_empty_dirs_with_omit_dir_times() {
    use filetime::{FileTime, set_file_mtime};

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let kept = source_root.join("kept");
    let pruned = source_root.join("pruned");
    fs::create_dir_all(&kept).expect("create kept");
    fs::create_dir_all(&pruned).expect("create pruned");
    fs::write(kept.join("file.txt"), b"data").expect("write file");

    let dir_mtime = FileTime::from_unix_time(1_600_000_000, 0);
    set_file_mtime(&kept, dir_mtime).expect("set kept mtime");
    set_file_mtime(&source_root, dir_mtime).expect("set root mtime");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .prune_empty_dirs(true)
        .times(true)
        .omit_dir_times(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(dest_root.join("kept").is_dir());
    assert!(!dest_root.join("pruned").exists());

    // Directory time should NOT be preserved because of omit_dir_times
    let dest_kept_mtime = FileTime::from_last_modification_time(
        &fs::metadata(dest_root.join("kept")).expect("kept metadata"),
    );
    assert_ne!(dest_kept_mtime, dir_mtime);
}

#[test]
fn execute_directory_mkpath_with_prune_empty_dirs() {
    // mkpath creates missing parents, prune removes empty dirs from source
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(source_root.join("kept")).expect("create kept");
    fs::create_dir_all(source_root.join("empty")).expect("create empty");
    fs::write(source_root.join("kept").join("file.txt"), b"data").expect("write file");

    let dest_root = temp.path().join("missing_parent").join("dest");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .mkpath(true)
        .prune_empty_dirs(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(dest_root.join("kept").join("file.txt").exists());
    assert!(
        !dest_root.join("empty").exists(),
        "empty dir should be pruned even with mkpath"
    );
}

#[test]
fn execute_directory_collect_events_records_all_directory_operations() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let nested = source_root.join("a").join("b");
    fs::create_dir_all(&nested).expect("create nested");
    fs::write(nested.join("file.txt"), b"content").expect("write file");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().collect_events(true);

    let report = plan
        .execute_with_report(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let records = report.records();
    let dir_records: Vec<_> = records
        .iter()
        .filter(|r| r.action() == &LocalCopyAction::DirectoryCreated)
        .collect();

    // Should have created: source, a, b (3 directories)
    assert!(
        dir_records.len() >= 3,
        "expected at least 3 directory creation records, got {}",
        dir_records.len()
    );

    // Verify that file copy event is also present
    let file_records: Vec<_> = records
        .iter()
        .filter(|r| matches!(r.action(), LocalCopyAction::DataCopied))
        .collect();
    assert_eq!(file_records.len(), 1);
}

#[test]
fn execute_directory_delete_removes_extraneous_subdirs() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(source_root.join("keep")).expect("create keep");
    fs::write(source_root.join("keep").join("file.txt"), b"data").expect("write file");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(dest_root.join("keep")).expect("pre-create keep");
    fs::write(dest_root.join("keep").join("file.txt"), b"old").expect("write old");
    fs::create_dir_all(dest_root.join("extra_dir")).expect("create extraneous dir");
    fs::write(dest_root.join("extra_dir").join("old.txt"), b"extra").expect("write extra");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().delete(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(dest_root.join("keep").join("file.txt").exists());
    assert!(
        !dest_root.join("extra_dir").exists(),
        "extraneous subdir should be deleted"
    );
    assert!(summary.items_deleted() >= 1);
}

#[cfg(unix)]
#[test]
fn execute_directory_archive_preserves_permissions_and_times_nested() {
    use filetime::{FileTime, set_file_mtime};
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let sub_a = source_root.join("a");
    let sub_b = sub_a.join("b");
    fs::create_dir_all(&sub_b).expect("create nested");
    fs::write(sub_b.join("file.txt"), b"deep").expect("write file");

    fs::set_permissions(&source_root, PermissionsExt::from_mode(0o755)).expect("set root");
    fs::set_permissions(&sub_a, PermissionsExt::from_mode(0o750)).expect("set a");
    fs::set_permissions(&sub_b, PermissionsExt::from_mode(0o700)).expect("set b");

    let root_mtime = FileTime::from_unix_time(1_600_000_000, 0);
    let a_mtime = FileTime::from_unix_time(1_650_000_000, 0);
    let b_mtime = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&sub_b, b_mtime).expect("set b mtime");
    set_file_mtime(&sub_a, a_mtime).expect("set a mtime");
    set_file_mtime(&source_root, root_mtime).expect("set root mtime");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .recursive(true)
        .permissions(true)
        .times(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Permissions
    assert_eq!(
        fs::metadata(&dest_root).expect("root").permissions().mode() & 0o777,
        0o755
    );
    assert_eq!(
        fs::metadata(dest_root.join("a"))
            .expect("a")
            .permissions()
            .mode()
            & 0o777,
        0o750
    );
    assert_eq!(
        fs::metadata(dest_root.join("a").join("b"))
            .expect("b")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );

    // Times
    let dest_root_mtime =
        FileTime::from_last_modification_time(&fs::metadata(&dest_root).expect("root"));
    let dest_a_mtime =
        FileTime::from_last_modification_time(&fs::metadata(dest_root.join("a")).expect("a"));
    let dest_b_mtime = FileTime::from_last_modification_time(
        &fs::metadata(dest_root.join("a").join("b")).expect("b"),
    );
    assert_eq!(dest_root_mtime, root_mtime);
    assert_eq!(dest_a_mtime, a_mtime);
    assert_eq!(dest_b_mtime, b_mtime);
}
