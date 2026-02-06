// Tests for --force behavior: replacing directories with non-directories and vice versa.
//
// From the rsync man page: "This option tells rsync to delete a non-empty directory
// when it is to be replaced by a non-directory. This is only relevant if deletions
// are not active."

// ============================================================================
// File replaces directory
// ============================================================================

#[test]
fn force_file_replaces_non_empty_directory() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("item");
    fs::write(&source, b"file-content").expect("write source file");

    let destination = temp.path().join("dest");
    fs::create_dir_all(destination.join("subdir")).expect("create nested dir");
    fs::write(destination.join("subdir/child.txt"), b"child").expect("write child");
    fs::write(destination.join("existing.txt"), b"existing").expect("write existing");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().force_replacements(true),
        )
        .expect("forced replacement succeeds");

    assert!(destination.is_file(), "directory should be replaced by file");
    assert_eq!(
        fs::read(&destination).expect("read destination"),
        b"file-content"
    );
    assert_eq!(summary.files_copied(), 1);
}

#[test]
fn force_disabled_file_cannot_replace_directory_in_recursive_copy() {
    // When copying source/ -> dest/, if source has "item" as a file but dest
    // has "item" as a directory, rsync must fail without --force.
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source root");
    fs::write(source_root.join("item"), b"replacement").expect("write source file");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(dest_root.join("item")).expect("create conflicting directory");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let error = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().force_replacements(false),
        )
        .expect_err("should fail without force");

    match error.kind() {
        LocalCopyErrorKind::InvalidArgument(reason) => {
            assert_eq!(*reason, LocalCopyArgumentError::ReplaceDirectoryWithFile);
        }
        other => panic!("unexpected error kind: {other:?}"),
    }
    assert!(dest_root.join("item").is_dir(), "directory should remain untouched");
}

#[test]
fn force_file_replaces_deeply_nested_directory() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("item");
    fs::write(&source, b"flat").expect("write source");

    let destination = temp.path().join("dest");
    fs::create_dir_all(destination.join("a/b/c/d")).expect("create deep tree");
    fs::write(destination.join("a/b/c/d/leaf.txt"), b"deep").expect("write leaf");
    fs::write(destination.join("a/top.txt"), b"top").expect("write top");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().force_replacements(true),
    )
    .expect("forced replacement succeeds");

    assert!(destination.is_file(), "deeply nested directory replaced by file");
    assert_eq!(fs::read(&destination).expect("read"), b"flat");
}

// ============================================================================
// Directory replaces non-directory
// ============================================================================

#[test]
fn force_directory_replaces_file() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("srcdir");
    fs::create_dir_all(&source_root).expect("create source dir");
    fs::write(source_root.join("inner.txt"), b"inner").expect("write inner");

    let destination = temp.path().join("dest");
    fs::write(&destination, b"old-file").expect("write existing file");

    let operands = vec![
        source_root.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().force_replacements(true),
    )
    .expect("forced replacement succeeds");

    assert!(destination.is_dir(), "file should be replaced by directory");
    assert_eq!(
        fs::read(destination.join("inner.txt")).expect("read inner"),
        b"inner"
    );
}

#[test]
fn force_disabled_directory_cannot_replace_file() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("srcdir");
    fs::create_dir_all(&source_root).expect("create source dir");
    fs::write(source_root.join("file.txt"), b"content").expect("write file");

    let destination = temp.path().join("dest");
    fs::write(&destination, b"existing file").expect("write existing file");

    let operands = vec![
        source_root.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let error = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().force_replacements(false),
        )
        .expect_err("should fail without force");

    match error.kind() {
        LocalCopyErrorKind::InvalidArgument(reason) => {
            assert_eq!(
                *reason,
                LocalCopyArgumentError::ReplaceNonDirectoryWithDirectory
            );
        }
        other => panic!("unexpected error kind: {other:?}"),
    }
    assert!(destination.is_file(), "file should remain");
}

// ============================================================================
// Recursive directory copies with conflicts
// ============================================================================

#[test]
fn force_replaces_directory_entry_with_file_during_recursive_copy() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source root");
    // Source has "item" as a regular file
    fs::write(source_root.join("item"), b"file-data").expect("write source file");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest root");
    // Destination has "item" as a directory with contents
    fs::create_dir_all(dest_root.join("item/subdir")).expect("create conflicting directory");
    fs::write(dest_root.join("item/existing.txt"), b"old").expect("write old");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().force_replacements(true),
    )
    .expect("forced replacement succeeds");

    let item_path = dest_root.join("item");
    assert!(item_path.is_file(), "directory entry should be replaced by file");
    assert_eq!(fs::read(&item_path).expect("read"), b"file-data");
}

#[test]
fn force_replaces_file_entry_with_directory_during_recursive_copy() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source root");
    // Source has "item" as a directory
    fs::create_dir_all(source_root.join("item")).expect("create source dir");
    fs::write(source_root.join("item/child.txt"), b"child").expect("write child");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest root");
    // Destination has "item" as a regular file
    fs::write(dest_root.join("item"), b"old-file").expect("write conflicting file");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().force_replacements(true),
    )
    .expect("forced replacement succeeds");

    let item_path = dest_root.join("item");
    assert!(item_path.is_dir(), "file entry should be replaced by directory");
    assert_eq!(
        fs::read(item_path.join("child.txt")).expect("read child"),
        b"child"
    );
}

#[test]
fn force_disabled_recursive_copy_fails_on_type_conflict() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source root");
    // Source has "item" as a regular file
    fs::write(source_root.join("item"), b"file-data").expect("write source file");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest root");
    // Destination has "item" as a directory
    fs::create_dir_all(dest_root.join("item")).expect("create conflicting directory");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let error = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().force_replacements(false),
        )
        .expect_err("should fail without force");

    match error.kind() {
        LocalCopyErrorKind::InvalidArgument(reason) => {
            assert_eq!(*reason, LocalCopyArgumentError::ReplaceDirectoryWithFile);
        }
        other => panic!("unexpected error kind: {other:?}"),
    }
    // The conflicting directory should still be intact
    assert!(dest_root.join("item").is_dir());
}

// ============================================================================
// Dry-run mode
// ============================================================================

#[test]
fn force_dry_run_does_not_modify_directory() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("item");
    fs::write(&source, b"replacement").expect("write source");

    let destination = temp.path().join("dest");
    fs::create_dir_all(destination.join("inner")).expect("create dest dir structure");
    fs::write(destination.join("inner/keep.txt"), b"keep").expect("write keep");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::DryRun,
        LocalCopyOptions::default().force_replacements(true),
    )
    .expect("dry-run succeeds");

    // Destination should remain a directory in dry-run mode
    assert!(destination.is_dir(), "directory should not be modified in dry-run");
    assert_eq!(
        fs::read(destination.join("inner/keep.txt")).expect("read"),
        b"keep"
    );
}

#[test]
fn force_dry_run_reports_deletion_for_replaced_directory() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("item");
    fs::write(&source, b"replacement").expect("write source");

    let destination = temp.path().join("dest");
    fs::create_dir_all(&destination).expect("create dest dir");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .force_replacements(true)
        .collect_events(true);
    let report = plan
        .execute_with_report(LocalCopyExecution::DryRun, options)
        .expect("dry-run succeeds");

    let summary = report.summary();
    assert_eq!(summary.items_deleted(), 1, "should report one deletion");
    assert_eq!(summary.files_copied(), 1, "should report one file copy");
}

// ============================================================================
// Symlink replaces directory with force (Unix only)
// ============================================================================

#[cfg(unix)]
#[test]
fn force_symlink_replaces_non_empty_directory() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let link_target = temp.path().join("target.txt");
    fs::write(&link_target, b"target").expect("write target");

    let source_link = temp.path().join("link");
    symlink(&link_target, &source_link).expect("create symlink");

    let destination = temp.path().join("dest");
    fs::create_dir_all(destination.join("contents")).expect("create dest dir");
    fs::write(destination.join("contents/file.txt"), b"old").expect("write file");

    let operands = vec![
        source_link.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default()
            .links(true)
            .force_replacements(true),
    )
    .expect("forced replacement succeeds");

    let metadata = fs::symlink_metadata(&destination).expect("dest metadata");
    assert!(
        metadata.file_type().is_symlink(),
        "directory should be replaced by symlink"
    );
    assert_eq!(
        fs::read_link(&destination).expect("read link"),
        link_target
    );
}

#[cfg(unix)]
#[test]
fn force_disabled_symlink_cannot_replace_directory_in_recursive_copy() {
    // When copying source/ -> dest/, if source has "link" as a symlink but dest
    // has "link" as a directory, rsync must fail without --force.
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source root");
    let link_target = temp.path().join("target.txt");
    fs::write(&link_target, b"target").expect("write target");
    symlink(&link_target, source_root.join("link")).expect("create symlink");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(dest_root.join("link")).expect("create conflicting directory");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let error = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .links(true)
                .force_replacements(false),
        )
        .expect_err("should fail without force");

    match error.kind() {
        LocalCopyErrorKind::InvalidArgument(reason) => {
            assert_eq!(
                *reason,
                LocalCopyArgumentError::ReplaceDirectoryWithSymlink
            );
        }
        other => panic!("unexpected error kind: {other:?}"),
    }
    assert!(dest_root.join("link").is_dir(), "directory should remain");
}

// ============================================================================
// FIFO replaces directory with force (Unix only)
// ============================================================================

#[cfg(all(
    unix,
    not(any(
        target_os = "ios",
        target_os = "macos",
        target_os = "tvos",
        target_os = "watchos"
    ))
))]
#[test]
fn force_fifo_replaces_non_empty_directory() {
    use std::os::unix::fs::FileTypeExt;

    let temp = tempdir().expect("tempdir");
    let source_fifo = temp.path().join("source.pipe");
    mkfifo_for_tests(&source_fifo, 0o600).expect("mkfifo");

    let destination = temp.path().join("dest");
    fs::create_dir_all(destination.join("subdir")).expect("create dest dir");
    fs::write(destination.join("subdir/file.txt"), b"old").expect("write file");

    let operands = vec![
        source_fifo.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default()
            .specials(true)
            .force_replacements(true),
    )
    .expect("forced replacement succeeds");

    let metadata = fs::symlink_metadata(&destination).expect("dest metadata");
    assert!(
        metadata.file_type().is_fifo(),
        "directory should be replaced by FIFO"
    );
}

// ============================================================================
// Multiple type conflicts in a single recursive copy
// ============================================================================

#[test]
fn force_handles_multiple_type_conflicts_in_one_copy() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source root");

    // Source: "alpha" is a file, "beta" is a directory
    fs::write(source_root.join("alpha"), b"alpha-file").expect("write alpha");
    fs::create_dir_all(source_root.join("beta")).expect("create beta dir");
    fs::write(source_root.join("beta/inside.txt"), b"inside").expect("write inside");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest root");

    // Destination: "alpha" is a directory, "beta" is a file (opposite types)
    fs::create_dir_all(dest_root.join("alpha/child")).expect("create alpha dir");
    fs::write(dest_root.join("alpha/child/deep.txt"), b"deep").expect("write deep");
    fs::write(dest_root.join("beta"), b"beta-file").expect("write beta");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().force_replacements(true),
    )
    .expect("forced replacement with multiple conflicts succeeds");

    // "alpha" should now be a file
    assert!(
        dest_root.join("alpha").is_file(),
        "alpha: directory should become file"
    );
    assert_eq!(
        fs::read(dest_root.join("alpha")).expect("read alpha"),
        b"alpha-file"
    );

    // "beta" should now be a directory
    assert!(
        dest_root.join("beta").is_dir(),
        "beta: file should become directory"
    );
    assert_eq!(
        fs::read(dest_root.join("beta/inside.txt")).expect("read inside"),
        b"inside"
    );
}

// ============================================================================
// Parent directory force replacement
// ============================================================================

#[test]
fn force_replaces_file_in_parent_path_with_directory() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(source_root.join("parent/child")).expect("create source tree");
    fs::write(source_root.join("parent/child/file.txt"), b"nested").expect("write nested");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest root");
    // Create "parent" as a file at the destination, blocking directory creation
    fs::write(dest_root.join("parent"), b"blocker").expect("write blocker file");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().force_replacements(true),
    )
    .expect("forced replacement of parent path succeeds");

    assert!(
        dest_root.join("parent").is_dir(),
        "parent file should become directory"
    );
    assert_eq!(
        fs::read(dest_root.join("parent/child/file.txt")).expect("read nested"),
        b"nested"
    );
}

// ============================================================================
// Summary tracking
// ============================================================================

#[test]
fn force_replacement_counts_deletion() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("item");
    fs::write(&source, b"new-content").expect("write source");

    let destination = temp.path().join("dest");
    fs::create_dir_all(destination.join("old-contents")).expect("create dest dir");
    fs::write(destination.join("old-contents/file.txt"), b"old").expect("write old");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .force_replacements(true)
        .collect_events(true);
    let report = plan
        .execute_with_report(LocalCopyExecution::Apply, options)
        .expect("forced replacement succeeds");

    let summary = report.summary();
    assert!(summary.items_deleted() >= 1, "should count at least one deletion for the replaced directory");
    assert_eq!(summary.files_copied(), 1, "should count the file copy");
}

// ============================================================================
// Idempotent behavior: force with no conflict
// ============================================================================

#[test]
fn force_with_no_type_conflict_copies_normally() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source root");
    fs::write(source_root.join("file.txt"), b"content").expect("write file");

    let dest_root = temp.path().join("dest");
    // No conflicting entries at destination

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().force_replacements(true),
        )
        .expect("copy succeeds");

    assert!(dest_root.join("file.txt").is_file());
    assert_eq!(
        fs::read(dest_root.join("file.txt")).expect("read"),
        b"content"
    );
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.items_deleted(), 0, "no deletions when no conflict");
}

#[test]
fn force_overwrite_file_with_file_is_normal_copy() {
    // When source and destination are both files, --force should not trigger
    // any directory removal. Use --ignore-times to ensure the copy happens
    // even if timestamps match.
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("file.txt");
    fs::write(&source, b"new-content").expect("write source");

    let destination = temp.path().join("dest.txt");
    fs::write(&destination, b"old-content").expect("write destination");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .force_replacements(true)
                .ignore_times(true),
        )
        .expect("copy succeeds");

    assert_eq!(
        fs::read(&destination).expect("read"),
        b"new-content"
    );
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.items_deleted(), 0, "no force deletion needed for same-type overwrite");
}
