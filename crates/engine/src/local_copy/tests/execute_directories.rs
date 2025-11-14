
#[test]
fn execute_with_trailing_separator_copies_contents() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let nested = source_root.join("nested");
    fs::create_dir_all(&nested).expect("create nested");
    fs::write(nested.join("file.txt"), b"contents").expect("write file");

    let dest_root = temp.path().join("dest");
    let mut source_operand = source_root.clone().into_os_string();
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
        source_root.clone().into_os_string(),
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
        source_root.clone().into_os_string(),
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
        source_root.clone().into_os_string(),
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

    let mut source_operand = source_root.clone().into_os_string();
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

    let mut source_operand = source_root.clone().into_os_string();
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

    let mut source_operand = source_root.clone().into_os_string();
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

    let mut source_operand = source_root.clone().into_os_string();
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

    let mut source_operand = source_root.clone().into_os_string();
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

    let mut source_operand = source_root.clone().into_os_string();
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
        source_root.clone().into_os_string(),
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
