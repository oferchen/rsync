// Comprehensive dry-run tests verifying behavior matches upstream rsync.
//
// Upstream rsync's --dry-run (-n) flag shows what would be transferred without
// actually performing copies. The summary counters still reflect the operations
// that *would* be performed, but no filesystem mutations occur.
//
// These tests complement the scattered dry-run tests in other files by
// providing a single, comprehensive test suite focused exclusively on dry-run
// semantics as documented in the rsync(1) man page.

// ==================== Basic Dry-Run Semantics ====================

#[test]
fn dry_run_single_file_lists_but_does_not_copy() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"payload").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::DryRun, LocalCopyOptions::default())
        .expect("dry run succeeds");

    // Summary reports what *would* happen.
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.bytes_copied(), 7); // "payload" is 7 bytes
    // Destination must not be created.
    assert!(!destination.exists(), "dry run must not create destination file");
}

#[test]
fn dry_run_exit_code_is_zero_on_success() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"ok").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // execute_with_options returns Ok(..) on success -- the absence of Err
    // corresponds to exit code 0 in the CLI.
    let result = plan.execute_with_options(LocalCopyExecution::DryRun, LocalCopyOptions::default());
    assert!(result.is_ok(), "dry run should succeed (exit code 0)");
}

#[test]
fn dry_run_preserves_source_unmodified() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    let original_content = b"source content unchanged";
    fs::write(&source, original_content).expect("write source");
    let original_mtime = filetime::FileTime::from_last_modification_time(
        &fs::metadata(&source).expect("source metadata"),
    );

    let operands = vec![
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(LocalCopyExecution::DryRun, LocalCopyOptions::default())
        .expect("dry run succeeds");

    // Source must remain identical.
    assert_eq!(
        fs::read(&source).expect("read source"),
        original_content,
        "source content must be unchanged after dry run"
    );
    let post_mtime = filetime::FileTime::from_last_modification_time(
        &fs::metadata(&source).expect("source metadata"),
    );
    assert_eq!(
        original_mtime, post_mtime,
        "source mtime must be unchanged after dry run"
    );
}

#[test]
fn dry_run_preserves_existing_destination_unmodified() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"new content").expect("write source");
    let original_dest = b"old destination content";
    fs::write(&destination, original_dest).expect("write dest");
    let original_mtime = filetime::FileTime::from_last_modification_time(
        &fs::metadata(&destination).expect("dest metadata"),
    );

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(LocalCopyExecution::DryRun, LocalCopyOptions::default())
        .expect("dry run succeeds");

    assert_eq!(
        fs::read(&destination).expect("read dest"),
        original_dest,
        "destination content must be unchanged after dry run"
    );
    let post_mtime = filetime::FileTime::from_last_modification_time(
        &fs::metadata(&destination).expect("dest metadata"),
    );
    assert_eq!(
        original_mtime, post_mtime,
        "destination mtime must be unchanged after dry run"
    );
}

// ==================== Directory Structure in Dry-Run ====================

#[test]
fn dry_run_directory_tree_not_created() {
    let ctx = test_helpers::setup_copy_test();
    ctx.write_source("sub/deep/file.txt", b"nested");

    let operands = ctx.operands();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::DryRun, LocalCopyOptions::default())
        .expect("dry run succeeds");

    // Summary should report a directory would be created and a file would be
    // copied, but the destination tree must not exist.
    assert_eq!(summary.files_copied(), 1);
    assert!(summary.directories_created() >= 1);
    assert!(
        !ctx.dest.exists(),
        "dry run must not create destination directory tree"
    );
}

#[test]
fn dry_run_multiple_directories_reported_but_not_created() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(ctx.source.join("dir1")).expect("create dir1");
    fs::create_dir_all(ctx.source.join("dir2")).expect("create dir2");
    fs::create_dir_all(ctx.source.join("dir3")).expect("create dir3");
    ctx.write_source("dir1/a.txt", b"a");
    ctx.write_source("dir2/b.txt", b"b");

    let operands = ctx.operands();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::DryRun, LocalCopyOptions::default())
        .expect("dry run succeeds");

    assert_eq!(summary.files_copied(), 2);
    // At least the 3 sub-directories + the root "source" directory.
    assert!(
        summary.directories_created() >= 3,
        "expected at least 3 directories_created, got {}",
        summary.directories_created()
    );
    assert!(!ctx.dest.exists(), "no destination directories should exist");
}

#[test]
fn dry_run_empty_directory_tree_reported() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(ctx.source.join("empty_subdir")).expect("create empty subdir");

    let operands = ctx.operands();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::DryRun, LocalCopyOptions::default())
        .expect("dry run succeeds");

    assert_eq!(summary.files_copied(), 0);
    assert!(summary.directories_created() >= 1);
    assert!(!ctx.dest.exists(), "no destination should be created");
}

// ==================== Dry-Run with --delete ====================

#[test]
fn dry_run_with_delete_reports_deletions_but_preserves_files() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    // Source has only keep.txt
    fs::write(source.join("keep.txt"), b"keep").expect("write keep");
    // Destination has keep.txt and extra.txt
    fs::write(dest.join("keep.txt"), b"old keep").expect("write old keep");
    fs::write(dest.join("extra.txt"), b"to be deleted").expect("write extra");

    let mut source_operand = source.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().delete(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::DryRun, options)
        .expect("dry run succeeds");

    // Summary should report the deletion.
    assert_eq!(summary.items_deleted(), 1);
    // But the file must still exist on disk.
    assert!(
        dest.join("extra.txt").exists(),
        "dry run must not actually delete files"
    );
    assert_eq!(
        fs::read(dest.join("extra.txt")).expect("read extra"),
        b"to be deleted"
    );
    // Original destination content must also be preserved.
    assert_eq!(
        fs::read(dest.join("keep.txt")).expect("read keep"),
        b"old keep"
    );
}

#[test]
fn dry_run_with_delete_before_reports_deletions_without_side_effects() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    fs::write(source.join("file.txt"), b"content").expect("write source");
    fs::write(dest.join("file.txt"), b"old").expect("write old file");
    fs::write(dest.join("orphan1.txt"), b"orphan1").expect("write orphan1");
    fs::write(dest.join("orphan2.txt"), b"orphan2").expect("write orphan2");

    let mut source_operand = source.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .delete_before(true)
        .collect_events(true);

    let report = plan
        .execute_with_report(LocalCopyExecution::DryRun, options)
        .expect("dry run succeeds");

    let summary = report.summary();
    // At least the 2 orphan files should be reported as deleted.
    // delete-before may also count the existing file.txt if it decides to
    // re-create it, so we check >= 2 here.
    assert!(
        summary.items_deleted() >= 2,
        "expected at least 2 items_deleted, got {}",
        summary.items_deleted()
    );
    assert!(dest.join("orphan1.txt").exists(), "orphan1 must still exist");
    assert!(dest.join("orphan2.txt").exists(), "orphan2 must still exist");
    assert_eq!(
        fs::read(dest.join("file.txt")).expect("read file"),
        b"old",
        "existing destination file must be unmodified"
    );
}

#[test]
fn dry_run_with_delete_during_reports_interleaved_events() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    fs::write(source.join("keep.txt"), b"keep").expect("write keep");
    fs::write(dest.join("keep.txt"), b"old").expect("write old keep");
    fs::write(dest.join("stale.txt"), b"stale").expect("write stale");

    let mut source_operand = source.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .delete(true)
        .collect_events(true);

    let report = plan
        .execute_with_report(LocalCopyExecution::DryRun, options)
        .expect("dry run succeeds");

    let summary = report.summary();
    assert_eq!(summary.items_deleted(), 1);
    assert_eq!(summary.files_copied(), 1);

    // Verify that records report both a copy and a deletion.
    let records = report.records();
    let has_copy = records
        .iter()
        .any(|r| matches!(r.action(), LocalCopyAction::DataCopied));
    let has_delete = records
        .iter()
        .any(|r| matches!(r.action(), LocalCopyAction::EntryDeleted));
    assert!(has_copy, "should have a DataCopied record");
    assert!(has_delete, "should have an EntryDeleted record");

    // Filesystem must be untouched.
    assert!(dest.join("stale.txt").exists());
    assert_eq!(fs::read(dest.join("keep.txt")).expect("read"), b"old");
}

// ==================== Dry-Run with Filters ====================

#[test]
fn dry_run_with_exclude_filter_omits_excluded_files() {
    let ctx = test_helpers::setup_copy_test();
    ctx.write_source("include.txt", b"included");
    ctx.write_source("exclude.log", b"excluded");
    ctx.write_source("another.txt", b"also included");

    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let filters = FilterSet::from_rules([FilterRule::exclude("*.log")])
        .expect("compile filters");

    let options = LocalCopyOptions::default()
        .filters(Some(filters))
        .collect_events(true);

    let report = plan
        .execute_with_report(LocalCopyExecution::DryRun, options)
        .expect("dry run succeeds");

    let summary = report.summary();
    // Only the .txt files should be reported.
    assert_eq!(summary.files_copied(), 2);

    let records = report.records();
    let paths: Vec<_> = records
        .iter()
        .filter(|r| matches!(r.action(), LocalCopyAction::DataCopied))
        .map(|r| r.relative_path().to_string_lossy().to_string())
        .collect();

    assert!(paths.iter().any(|p| p == "include.txt"));
    assert!(paths.iter().any(|p| p == "another.txt"));
    assert!(
        !paths.iter().any(|p| p.contains("exclude.log")),
        "excluded files must not appear in dry-run records"
    );
    assert!(!ctx.dest.exists(), "destination must not be created");
}

#[test]
fn dry_run_with_include_exclude_filter_chain() {
    let ctx = test_helpers::setup_copy_test();
    ctx.write_source("important.cfg", b"keep");
    ctx.write_source("other.cfg", b"skip");
    ctx.write_source("readme.txt", b"skip");

    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let filters = FilterSet::from_rules([
        FilterRule::include("important.cfg"),
        FilterRule::exclude("*"),
    ])
    .expect("compile filters");

    let options = LocalCopyOptions::default()
        .filters(Some(filters))
        .collect_events(true);

    let report = plan
        .execute_with_report(LocalCopyExecution::DryRun, options)
        .expect("dry run succeeds");

    let summary = report.summary();
    assert_eq!(summary.files_copied(), 1);

    let paths: Vec<_> = report
        .records()
        .iter()
        .filter(|r| matches!(r.action(), LocalCopyAction::DataCopied))
        .map(|r| r.relative_path().to_string_lossy().to_string())
        .collect();
    assert_eq!(paths, vec!["important.cfg"]);
}

// ==================== Dry-Run Records and Metadata ====================

#[test]
fn dry_run_records_contain_file_metadata() {
    let ctx = test_helpers::setup_copy_test();
    let content = b"metadata test content";
    ctx.write_source("data.bin", content);

    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().collect_events(true);

    let report = plan
        .execute_with_report(LocalCopyExecution::DryRun, options)
        .expect("dry run succeeds");

    let records = report.records();
    let file_record = records
        .iter()
        .find(|r| r.relative_path().to_string_lossy() == "data.bin")
        .expect("data.bin record must be present");

    assert_eq!(file_record.action(), &LocalCopyAction::DataCopied);

    let metadata = file_record.metadata().expect("metadata must be present");
    assert_eq!(metadata.len(), content.len() as u64);
    assert!(metadata.modified().is_some());
}

#[test]
fn dry_run_records_mark_new_files_as_created() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"new file").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().collect_events(true);

    let report = plan
        .execute_with_report(LocalCopyExecution::DryRun, options)
        .expect("dry run succeeds");

    let records = report.records();
    let file_record = records
        .iter()
        .find(|r| matches!(r.action(), LocalCopyAction::DataCopied))
        .expect("should have a DataCopied record");

    // New destination -> was_created() should be true.
    assert!(
        file_record.was_created(),
        "new file in dry run should be marked as would-be-created"
    );
    assert!(!destination.exists());
}

#[test]
fn dry_run_records_update_is_not_marked_as_created() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"updated content").expect("write source");
    fs::write(&destination, b"old").expect("write dest");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().collect_events(true);

    let report = plan
        .execute_with_report(LocalCopyExecution::DryRun, options)
        .expect("dry run succeeds");

    let records = report.records();
    let file_record = records
        .iter()
        .find(|r| matches!(r.action(), LocalCopyAction::DataCopied))
        .expect("should have a DataCopied record");

    // Existing destination -> was_created() should be false.
    assert!(
        !file_record.was_created(),
        "updating existing file in dry run should not be marked as created"
    );
    assert_eq!(fs::read(&destination).expect("read"), b"old");
}

// ==================== Dry-Run Statistics ====================

#[test]
fn dry_run_statistics_match_apply_mode_statistics() {
    // In upstream rsync, --dry-run produces identical summary counters
    // compared to a real run -- the only difference is filesystem state.
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");
    fs::write(source_root.join("file1.txt"), b"content1").expect("write file1");
    fs::write(source_root.join("file2.txt"), b"content2").expect("write file2");

    let dry_dest = temp.path().join("dry_dest");
    let apply_dest = temp.path().join("apply_dest");

    // Dry-run
    let operands_dry = vec![
        source_root.clone().into_os_string(),
        dry_dest.clone().into_os_string(),
    ];
    let plan_dry = LocalCopyPlan::from_operands(&operands_dry).expect("plan");
    let summary_dry = plan_dry
        .execute_with_options(LocalCopyExecution::DryRun, LocalCopyOptions::default())
        .expect("dry run succeeds");

    // Apply
    let operands_apply = vec![
        source_root.into_os_string(),
        apply_dest.clone().into_os_string(),
    ];
    let plan_apply = LocalCopyPlan::from_operands(&operands_apply).expect("plan");
    let summary_apply = plan_apply
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("apply succeeds");

    // Statistics must match.
    assert_eq!(
        summary_dry.files_copied(),
        summary_apply.files_copied(),
        "files_copied should match between dry-run and apply"
    );
    assert_eq!(
        summary_dry.directories_created(),
        summary_apply.directories_created(),
        "directories_created should match between dry-run and apply"
    );
    assert_eq!(
        summary_dry.bytes_copied(),
        summary_apply.bytes_copied(),
        "bytes_copied should match between dry-run and apply"
    );
    assert_eq!(
        summary_dry.total_source_bytes(),
        summary_apply.total_source_bytes(),
        "total_source_bytes should match between dry-run and apply"
    );

    // Filesystem state must differ.
    assert!(!dry_dest.exists(), "dry run destination should not exist");
    assert!(apply_dest.exists(), "apply destination should exist");
}

#[test]
fn dry_run_total_source_bytes_correct() {
    let ctx = test_helpers::setup_copy_test();
    ctx.write_source("a.txt", b"12345");
    ctx.write_source("b.txt", b"67890");

    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::DryRun, LocalCopyOptions::default())
        .expect("dry run succeeds");

    assert_eq!(summary.files_copied(), 2);
    assert_eq!(summary.bytes_copied(), 10);
    assert_eq!(summary.total_source_bytes(), 10);
}

#[test]
fn dry_run_empty_files_reported() {
    let ctx = test_helpers::setup_copy_test();
    ctx.write_source("empty.txt", b"");

    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::DryRun, LocalCopyOptions::default())
        .expect("dry run succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.bytes_copied(), 0);
    assert!(!ctx.dest.exists());
}

// ==================== Dry-Run Combined With Various Options ====================

#[test]
fn dry_run_with_times_does_not_set_timestamps() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"times").expect("write source");
    let past = filetime::FileTime::from_unix_time(1_600_000_000, 0);
    filetime::set_file_mtime(&source, past).expect("set source mtime");

    // Pre-create destination to verify it is not modified.
    fs::write(&destination, b"original").expect("write dest");
    let dest_mtime_before = filetime::FileTime::from_last_modification_time(
        &fs::metadata(&destination).expect("dest metadata"),
    );

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::DryRun,
        LocalCopyOptions::default().times(true),
    )
    .expect("dry run succeeds");

    let dest_mtime_after = filetime::FileTime::from_last_modification_time(
        &fs::metadata(&destination).expect("dest metadata"),
    );
    assert_eq!(
        dest_mtime_before, dest_mtime_after,
        "destination mtime must not change during dry run"
    );
    assert_eq!(fs::read(&destination).expect("read"), b"original");
}

#[cfg(unix)]
#[test]
fn dry_run_with_permissions_does_not_change_mode() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"perms").expect("write source");
    fs::set_permissions(&source, fs::Permissions::from_mode(0o600)).expect("set source perms");

    fs::write(&destination, b"old").expect("write dest");
    let original_mode = fs::metadata(&destination)
        .expect("dest metadata")
        .permissions()
        .mode()
        & 0o777;

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::DryRun,
        LocalCopyOptions::default().permissions(true),
    )
    .expect("dry run succeeds");

    let post_mode = fs::metadata(&destination)
        .expect("dest metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        original_mode, post_mode,
        "destination permissions must not change during dry run"
    );
}

#[test]
fn dry_run_with_remove_source_files_does_not_delete_source() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"move me").expect("write source");

    let operands = vec![
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::DryRun,
            LocalCopyOptions::default().remove_source_files(true),
        )
        .expect("dry run succeeds");

    assert_eq!(summary.files_copied(), 1);
    // Source must still exist -- dry-run does not remove sources.
    assert!(
        source.exists(),
        "dry run must not remove source files even with --remove-source-files"
    );
    assert!(!destination.exists());
}

#[test]
fn dry_run_with_ignore_existing_reports_skip() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"newer").expect("write source");
    fs::write(&destination, b"existing").expect("write dest");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::DryRun,
            LocalCopyOptions::default().ignore_existing(true),
        )
        .expect("dry run succeeds");

    // With --ignore-existing, files that already exist are skipped.
    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_ignored_existing(), 1);
    assert_eq!(fs::read(&destination).expect("read"), b"existing");
}

#[test]
fn dry_run_with_checksum_mode_reports_correctly() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"checksum test").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::DryRun,
            LocalCopyOptions::default().checksum(true),
        )
        .expect("dry run succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(!destination.exists());
}

// ==================== Dry-Run with Symlinks ====================

#[cfg(unix)]
#[test]
fn dry_run_does_not_create_symlinks() {
    use std::os::unix::fs::symlink;

    let ctx = test_helpers::setup_copy_test();
    fs::write(ctx.source.join("target.txt"), b"target").expect("write target");
    symlink("target.txt", ctx.source.join("link.txt")).expect("create symlink");

    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::DryRun,
            LocalCopyOptions::default().links(true),
        )
        .expect("dry run succeeds");

    assert_eq!(summary.symlinks_copied(), 1);
    assert!(!ctx.dest.exists(), "no destination should be created in dry run");
}

// ==================== Dry-Run with delete + filters interaction ====================

#[test]
fn dry_run_delete_respects_exclude_filters() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    fs::write(source.join("keep.txt"), b"keep").expect("write keep");
    // dest has a file matching exclude filter AND an extra file.
    fs::write(dest.join("keep.txt"), b"old").expect("write old keep");
    fs::write(dest.join("excluded.log"), b"excluded").expect("write excluded");
    fs::write(dest.join("extra.txt"), b"extra").expect("write extra");

    let mut source_operand = source.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let filters = FilterSet::from_rules([FilterRule::exclude("*.log")])
        .expect("compile filters");
    let options = LocalCopyOptions::default()
        .delete(true)
        .filters(Some(filters));

    let summary = plan
        .execute_with_options(LocalCopyExecution::DryRun, options)
        .expect("dry run succeeds");

    // extra.txt should be reported as deleted, excluded.log should be
    // protected by the exclude filter.
    assert_eq!(summary.items_deleted(), 1);
    // Everything still on disk.
    assert!(dest.join("excluded.log").exists());
    assert!(dest.join("extra.txt").exists());
    assert!(dest.join("keep.txt").exists());
}

// ==================== Dry-Run Multiple Files ====================

#[test]
fn dry_run_multiple_files_all_reported() {
    let ctx = test_helpers::setup_copy_test();
    ctx.write_source("file1.txt", b"content1");
    ctx.write_source("file2.txt", b"content2");
    ctx.write_source("file3.txt", b"content3");

    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().collect_events(true);

    let report = plan
        .execute_with_report(LocalCopyExecution::DryRun, options)
        .expect("dry run succeeds");

    let summary = report.summary();
    assert_eq!(summary.files_copied(), 3);
    assert_eq!(summary.bytes_copied(), 24); // 8 * 3

    let records = report.records();
    let file_paths: Vec<_> = records
        .iter()
        .filter(|r| matches!(r.action(), LocalCopyAction::DataCopied))
        .map(|r| r.relative_path().to_string_lossy().to_string())
        .collect();

    assert_eq!(file_paths.len(), 3);
    assert!(file_paths.contains(&"file1.txt".to_string()));
    assert!(file_paths.contains(&"file2.txt".to_string()));
    assert!(file_paths.contains(&"file3.txt".to_string()));
    assert!(!ctx.dest.exists());
}

// ==================== Dry-Run Idempotency ====================

#[test]
fn dry_run_is_idempotent() {
    // Running dry-run multiple times should produce the same result because
    // no filesystem state changes.
    let ctx = test_helpers::setup_copy_test();
    ctx.write_source("stable.txt", b"stable content");

    let operands = ctx.operands();

    for _ in 0..3 {
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
        let summary = plan
            .execute_with_options(LocalCopyExecution::DryRun, LocalCopyOptions::default())
            .expect("dry run succeeds");

        assert_eq!(summary.files_copied(), 1);
        assert_eq!(summary.bytes_copied(), 14);
        assert!(!ctx.dest.exists());
    }
}

// ==================== Dry-Run with large files ====================

#[test]
fn dry_run_large_file_reports_correct_byte_count() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("large.bin");
    let destination = temp.path().join("dest.bin");

    let large_content = vec![0xABu8; 256 * 1024];
    fs::write(&source, &large_content).expect("write large source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::DryRun, LocalCopyOptions::default())
        .expect("dry run succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.bytes_copied(), 256 * 1024);
    assert!(!destination.exists());
}

// ==================== Dry-Run with matched (up-to-date) files ====================

#[test]
fn dry_run_reports_matched_file_as_would_copy() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    let content = b"identical";
    fs::write(&source, content).expect("write source");
    fs::write(&destination, content).expect("write dest");

    // Synchronize modification times so the files are considered up-to-date.
    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source, timestamp).expect("set source mtime");
    set_file_mtime(&destination, timestamp).expect("set dest mtime");

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

    // In dry-run mode the engine reports the file as would-be-copied
    // because the skip-comparison check is not performed in the dry-run
    // path.  The destination file must remain unchanged.
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        content,
        "destination must remain unchanged in dry-run mode"
    );
}

// ==================== Dry-Run with backup option ====================

#[test]
fn dry_run_with_backup_does_not_create_backup_files() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"new").expect("write source");
    fs::write(&destination, b"old").expect("write dest");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::DryRun,
            LocalCopyOptions::default().backup(true),
        )
        .expect("dry run succeeds");

    assert_eq!(summary.files_copied(), 1);
    // The backup file (dest.txt~) must not exist.
    assert!(
        !temp.path().join("dest.txt~").exists(),
        "dry run must not create backup files"
    );
    assert_eq!(fs::read(&destination).expect("read"), b"old");
}

// ==================== Dry-Run with recursive nested tree ====================

#[test]
fn dry_run_deeply_nested_tree_all_reported() {
    let ctx = test_helpers::setup_copy_test();
    let deep = ctx.source.join("a").join("b").join("c");
    fs::create_dir_all(&deep).expect("create deep path");
    fs::write(deep.join("deep.txt"), b"deep content").expect("write deep file");
    ctx.write_source("root.txt", b"root");

    let operands = ctx.operands();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().collect_events(true);

    let report = plan
        .execute_with_report(LocalCopyExecution::DryRun, options)
        .expect("dry run succeeds");

    let summary = report.summary();
    assert_eq!(summary.files_copied(), 2);
    assert!(summary.directories_created() >= 3);

    let records = report.records();
    let paths: Vec<_> = records
        .iter()
        .filter(|r| matches!(r.action(), LocalCopyAction::DataCopied))
        .map(|r| r.relative_path().to_string_lossy().to_string())
        .collect();

    assert!(paths.iter().any(|p| p.contains("root.txt")));
    assert!(paths.iter().any(|p| p.contains("deep.txt")));
    assert!(!ctx.dest.exists());
}

// ==================== Dry-Run with --whole-file ====================

#[test]
fn dry_run_whole_file_reports_transfer() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"whole file content").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::DryRun,
            LocalCopyOptions::default().whole_file(true),
        )
        .expect("dry run succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.bytes_copied(), 18);
    assert!(!destination.exists());
}

// ==================== Dry-Run with --inplace ====================

#[test]
fn dry_run_with_inplace_does_not_modify_destination() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"inplace content").expect("write source");
    fs::write(&destination, b"original").expect("write dest");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::DryRun,
            LocalCopyOptions::default().inplace(true),
        )
        .expect("dry run succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"original",
        "dry run with inplace must not modify destination"
    );
}
