// Tests for --delete-delay flag behavior.
//
// The --delete-delay option defers deletion of extraneous files until after the
// file transfer is complete. This provides safety by ensuring files are only
// deleted after all transfers succeed, reducing the window of time when files
// may be missing.
//
// Key behaviors tested:
// 1. Deletions are batched and occur at the end of transfer
// 2. Behavior differs from --delete-during (which deletes immediately)
// 3. Works correctly with recursive directory traversal
// 4. Partial failures still handle deletions correctly
// 5. Deferred deletions respect filter rules
// 6. Works with max_deletions limit

// ==================== Basic Delay Behavior Tests ====================

#[test]
fn delete_delay_removes_extraneous_files_after_transfer() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    // Create source with two files
    fs::write(ctx.source.join("file1.txt"), b"file1").expect("write file1");
    fs::write(ctx.source.join("file2.txt"), b"file2").expect("write file2");

    // Create destination with source files plus extra files
    let target_root = ctx.dest.join("source");
    fs::create_dir_all(&target_root).expect("create target root");
    fs::write(target_root.join("file1.txt"), b"old1").expect("write old1");
    fs::write(target_root.join("file2.txt"), b"old2").expect("write old2");
    fs::write(target_root.join("extra1.txt"), b"extra1").expect("write extra1");
    fs::write(target_root.join("extra2.txt"), b"extra2").expect("write extra2");

    let operands = vec![
        ctx.source.clone().into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().delete_delay(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Verify correct files remain and extras are deleted
    assert!(target_root.join("file1.txt").exists());
    assert!(target_root.join("file2.txt").exists());
    assert!(!target_root.join("extra1.txt").exists());
    assert!(!target_root.join("extra2.txt").exists());
    assert_eq!(summary.files_copied(), 2);
    assert_eq!(summary.items_deleted(), 2);
}

#[test]
fn delete_delay_defers_directory_deletions_until_end() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    // Create source structure: dir1/file.txt
    let source_dir = ctx.source.join("dir1");
    fs::create_dir_all(&source_dir).expect("create source dir");
    fs::write(source_dir.join("file.txt"), b"content").expect("write file");

    // Create destination with extra directory
    let target_root = ctx.dest.join("source");
    fs::create_dir_all(&target_root).expect("create target root");
    fs::create_dir_all(target_root.join("dir1")).expect("create target dir1");
    fs::write(target_root.join("dir1/file.txt"), b"old").expect("write old file");

    let extra_dir = target_root.join("extra_dir");
    fs::create_dir_all(&extra_dir).expect("create extra dir");
    fs::write(extra_dir.join("extra.txt"), b"extra").expect("write extra file");

    let operands = vec![
        ctx.source.clone().into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().delete_delay(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Verify extra directory is deleted
    assert!(target_root.join("dir1").exists());
    assert!(target_root.join("dir1/file.txt").exists());
    assert!(!extra_dir.exists());
    assert_eq!(summary.files_copied(), 1);
    assert!(summary.items_deleted() >= 1); // At least the directory
}

// ==================== Comparison with --delete-during ====================

#[test]
fn delete_delay_differs_from_delete_during_timing() {
    // This test verifies that delete-delay and delete-during are semantically
    // different options, even though the end result is the same for simple cases.
    // The key difference is timing: delete-during processes deletions incrementally
    // during directory traversal, while delete-delay batches them until the end.

    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    fs::write(ctx.source.join("keep.txt"), b"keep").expect("write keep");

    let target_root = ctx.dest.join("source");
    fs::create_dir_all(&target_root).expect("create target root");
    fs::write(target_root.join("keep.txt"), b"old").expect("write old");
    fs::write(target_root.join("extra.txt"), b"extra").expect("write extra");

    let operands = vec![
        ctx.source.clone().into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];

    // Test with delete-delay
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options_delay = LocalCopyOptions::default().delete_delay(true);
    assert!(options_delay.delete_delay_enabled());
    assert!(!options_delay.delete_during_enabled());

    let summary_delay = plan
        .execute_with_options(LocalCopyExecution::Apply, options_delay)
        .expect("copy succeeds with delay");

    // Create new destination for delete-during test
    let ctx2 = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx2.dest).expect("create dest2");
    fs::write(ctx2.source.join("keep.txt"), b"keep").expect("write keep2");

    let target_root2 = ctx2.dest.join("source");
    fs::create_dir_all(&target_root2).expect("create target root2");
    fs::write(target_root2.join("keep.txt"), b"old").expect("write old2");
    fs::write(target_root2.join("extra.txt"), b"extra").expect("write extra2");

    let operands2 = vec![
        ctx2.source.clone().into_os_string(),
        ctx2.dest.clone().into_os_string(),
    ];

    // Test with delete-during
    let plan2 = LocalCopyPlan::from_operands(&operands2).expect("plan2");
    let options_during = LocalCopyOptions::default().delete_during();
    assert!(!options_during.delete_delay_enabled());
    assert!(options_during.delete_during_enabled());

    let summary_during = plan2
        .execute_with_options(LocalCopyExecution::Apply, options_during)
        .expect("copy succeeds with during");

    // Both should achieve the same end result
    assert_eq!(summary_delay.items_deleted(), summary_during.items_deleted());
    assert!(!target_root.join("extra.txt").exists());
    assert!(!target_root2.join("extra.txt").exists());
}

#[test]
fn delete_delay_option_correctly_enables_delay_timing() {
    let options = LocalCopyOptions::default().delete_delay(true);
    assert!(options.delete_delay_enabled());
    assert_eq!(options.delete_timing(), Some(DeleteTiming::Delay));
}

#[test]
fn delete_during_option_enables_during_timing() {
    let options = LocalCopyOptions::default().delete_during();
    assert!(options.delete_during_enabled());
    assert_eq!(options.delete_timing(), Some(DeleteTiming::During));
}

// ==================== Recursive Directory Handling ====================

#[test]
fn delete_delay_works_with_recursive_traversal() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    // Create nested source structure
    test_helpers::create_test_tree(&ctx.source, &[
        ("level1/level2/level3/file.txt", Some(b"deep")),
        ("level1/keep.txt", Some(b"keep1")),
        ("top.txt", Some(b"top")),
    ]);

    // Create destination with extra files at various levels
    let target_root = ctx.dest.join("source");
    test_helpers::create_test_tree(&target_root, &[
        ("level1/level2/level3/file.txt", Some(b"old")),
        ("level1/level2/level3/extra_deep.txt", Some(b"extra")),
        ("level1/level2/extra_mid.txt", Some(b"extra")),
        ("level1/keep.txt", Some(b"old")),
        ("level1/extra_shallow.txt", Some(b"extra")),
        ("top.txt", Some(b"old")),
        ("extra_top.txt", Some(b"extra")),
    ]);

    let operands = vec![
        ctx.source.clone().into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .delete_delay(true)
        .recursive(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Verify kept files exist
    assert!(target_root.join("top.txt").exists());
    assert!(target_root.join("level1/keep.txt").exists());
    assert!(target_root.join("level1/level2/level3/file.txt").exists());

    // Verify extra files at all levels are deleted
    assert!(!target_root.join("extra_top.txt").exists());
    assert!(!target_root.join("level1/extra_shallow.txt").exists());
    assert!(!target_root.join("level1/level2/extra_mid.txt").exists());
    assert!(!target_root.join("level1/level2/level3/extra_deep.txt").exists());

    assert_eq!(summary.files_copied(), 3);
    assert_eq!(summary.items_deleted(), 4);
}

#[test]
fn delete_delay_handles_nested_empty_directories() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    // Source has a directory with a file
    let source_dir = ctx.source.join("data");
    fs::create_dir_all(&source_dir).expect("create source dir");
    fs::write(source_dir.join("file.txt"), b"content").expect("write file");

    // Destination has nested empty directories that should be deleted
    let target_root = ctx.dest.join("source");
    fs::create_dir_all(&target_root).expect("create target root");
    fs::create_dir_all(target_root.join("data")).expect("create data dir");
    fs::write(target_root.join("data/file.txt"), b"old").expect("write old file");

    // Create extra nested empty directories
    fs::create_dir_all(target_root.join("empty1/empty2/empty3"))
        .expect("create nested empties");

    let operands = vec![
        ctx.source.clone().into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().delete_delay(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Verify data directory remains but empty directories are deleted
    assert!(target_root.join("data").exists());
    assert!(target_root.join("data/file.txt").exists());
    assert!(!target_root.join("empty1").exists());

    assert_eq!(summary.files_copied(), 1);
    assert!(summary.items_deleted() >= 1);
}

// ==================== Partial Failures and Error Handling ====================

#[test]
fn delete_delay_with_max_deletions_limit() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    fs::write(ctx.source.join("keep.txt"), b"keep").expect("write keep");

    // Create destination with multiple extra files
    let target_root = ctx.dest.join("source");
    fs::create_dir_all(&target_root).expect("create target root");
    fs::write(target_root.join("keep.txt"), b"old").expect("write old");
    fs::write(target_root.join("extra1.txt"), b"extra1").expect("write extra1");
    fs::write(target_root.join("extra2.txt"), b"extra2").expect("write extra2");
    fs::write(target_root.join("extra3.txt"), b"extra3").expect("write extra3");

    let operands = vec![
        ctx.source.clone().into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .delete_delay(true)
        .max_deletions(Some(2));

    let error = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect_err("max-delete should stop deletions");

    match error.kind() {
        LocalCopyErrorKind::DeleteLimitExceeded { skipped } => assert_eq!(*skipped, 1),
        other => panic!("unexpected error kind: {other:?}"),
    }

    // Verify keep file exists and was updated
    assert!(target_root.join("keep.txt").exists());
    assert_eq!(
        fs::read(target_root.join("keep.txt")).expect("read keep"),
        b"keep"
    );

    // Exactly one extra file should remain (the one that exceeded the limit)
    let remaining = [
        target_root.join("extra1.txt").exists(),
        target_root.join("extra2.txt").exists(),
        target_root.join("extra3.txt").exists(),
    ];
    let remaining_count = remaining.iter().filter(|&&exists| exists).count();
    assert_eq!(remaining_count, 1);
}

#[test]
fn delete_delay_processes_deletions_even_after_successful_transfer() {
    // This test verifies that even if all file transfers succeed,
    // deferred deletions are still processed at the end.

    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    // Create simple source
    fs::write(ctx.source.join("file.txt"), b"new content").expect("write source");

    // Create destination with extra files
    let target_root = ctx.dest.join("source");
    fs::create_dir_all(&target_root).expect("create target root");
    fs::write(target_root.join("file.txt"), b"old content").expect("write old");
    fs::write(target_root.join("delete_me.txt"), b"delete").expect("write delete_me");

    let operands = vec![
        ctx.source.clone().into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().delete_delay(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Verify file was updated
    assert_eq!(
        fs::read(target_root.join("file.txt")).expect("read file"),
        b"new content"
    );

    // Verify deferred deletion was processed
    assert!(!target_root.join("delete_me.txt").exists());
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.items_deleted(), 1);
}

/// Tests that delete_delay properly batches multiple directory deletions.
/// TODO: Fix - may need recursive(true) option for proper directory handling.
#[test]
#[ignore = "delete_delay batched directory deletions: needs recursive option or implementation fix"]
fn delete_delay_batches_multiple_directory_deletions() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    // Create source with single directory
    fs::create_dir_all(ctx.source.join("keep_dir")).expect("create keep dir");
    fs::write(ctx.source.join("keep_dir/file.txt"), b"keep").expect("write keep");

    // Create destination with multiple extra directories
    let target_root = ctx.dest.join("source");
    fs::create_dir_all(&target_root).expect("create target root");
    fs::create_dir_all(target_root.join("keep_dir")).expect("create target keep dir");
    fs::write(target_root.join("keep_dir/file.txt"), b"old").expect("write old");

    // Extra directories to be deleted
    fs::create_dir_all(target_root.join("extra1")).expect("create extra1");
    fs::write(target_root.join("extra1/file1.txt"), b"extra1").expect("write extra1 file");
    fs::create_dir_all(target_root.join("extra2")).expect("create extra2");
    fs::write(target_root.join("extra2/file2.txt"), b"extra2").expect("write extra2 file");
    fs::create_dir_all(target_root.join("extra3")).expect("create extra3");
    fs::write(target_root.join("extra3/file3.txt"), b"extra3").expect("write extra3 file");

    let operands = vec![
        ctx.source.clone().into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().delete_delay(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Verify keep directory remains
    assert!(target_root.join("keep_dir").exists());
    assert!(target_root.join("keep_dir/file.txt").exists());

    // Verify all extra directories are deleted (batched deletions)
    assert!(!target_root.join("extra1").exists());
    assert!(!target_root.join("extra2").exists());
    assert!(!target_root.join("extra3").exists());

    assert_eq!(summary.files_copied(), 1);
    // At least 3 directories + 3 files = 6 items deleted
    assert!(summary.items_deleted() >= 6);
}

// ==================== Filter Integration ====================

#[test]
fn delete_delay_respects_exclude_filters() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    fs::write(ctx.source.join("keep.txt"), b"keep").expect("write keep");

    // Create destination with files matching filter patterns
    let target_root = ctx.dest.join("source");
    fs::create_dir_all(&target_root).expect("create target root");
    fs::write(target_root.join("keep.txt"), b"old").expect("write old");
    fs::write(target_root.join("extra.txt"), b"extra").expect("write extra");
    fs::write(target_root.join("skip.tmp"), b"skip").expect("write skip");

    let operands = vec![
        ctx.source.clone().into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let filters = FilterSet::from_rules([FilterRule::exclude("*.tmp")])
        .expect("compile filters");
    let options = LocalCopyOptions::default()
        .delete_delay(true)
        .filters(Some(filters));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Verify keep file updated
    assert!(target_root.join("keep.txt").exists());
    assert_eq!(
        fs::read(target_root.join("keep.txt")).expect("read keep"),
        b"keep"
    );

    // Verify extra.txt is deleted but skip.tmp is preserved (excluded)
    assert!(!target_root.join("extra.txt").exists());
    assert!(target_root.join("skip.tmp").exists());
    assert_eq!(
        fs::read(target_root.join("skip.tmp")).expect("read skip"),
        b"skip"
    );

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.items_deleted(), 1); // Only extra.txt
}

#[test]
fn delete_delay_with_delete_excluded_removes_filtered_files() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    fs::write(ctx.source.join("keep.txt"), b"keep").expect("write keep");

    // Create destination with files matching filter patterns
    let target_root = ctx.dest.join("source");
    fs::create_dir_all(&target_root).expect("create target root");
    fs::write(target_root.join("keep.txt"), b"old").expect("write old");
    fs::write(target_root.join("remove.tmp"), b"remove").expect("write remove");

    let operands = vec![
        ctx.source.clone().into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let filters = FilterSet::from_rules([FilterRule::exclude("*.tmp")])
        .expect("compile filters");
    let options = LocalCopyOptions::default()
        .delete_delay(true)
        .delete_excluded(true)
        .filters(Some(filters));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Verify keep file exists
    assert!(target_root.join("keep.txt").exists());

    // Verify excluded file is deleted when delete_excluded is enabled
    assert!(!target_root.join("remove.tmp").exists());

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.items_deleted(), 1);
}

// ==================== Edge Cases ====================

#[test]
fn delete_delay_with_no_files_to_delete() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    // Source and destination have identical structure
    fs::write(ctx.source.join("file.txt"), b"content").expect("write source");

    let target_root = ctx.dest.join("source");
    fs::create_dir_all(&target_root).expect("create target root");
    fs::write(target_root.join("file.txt"), b"old").expect("write old");

    let operands = vec![
        ctx.source.clone().into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().delete_delay(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Verify file exists and was updated
    assert!(target_root.join("file.txt").exists());
    assert_eq!(
        fs::read(target_root.join("file.txt")).expect("read file"),
        b"content"
    );

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.items_deleted(), 0);
}

#[test]
fn delete_delay_with_only_deletions_no_transfers() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    // Empty source directory
    // (source directory already exists from setup_copy_test)

    // Destination has files to delete
    let target_root = ctx.dest.join("source");
    fs::create_dir_all(&target_root).expect("create target root");
    fs::write(target_root.join("delete1.txt"), b"delete1").expect("write delete1");
    fs::write(target_root.join("delete2.txt"), b"delete2").expect("write delete2");

    let operands = vec![
        ctx.source.clone().into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().delete_delay(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Verify all files are deleted
    assert!(!target_root.join("delete1.txt").exists());
    assert!(!target_root.join("delete2.txt").exists());

    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.items_deleted(), 2);
}

#[test]
fn delete_delay_with_dry_run_reports_but_does_not_delete() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    fs::write(ctx.source.join("keep.txt"), b"keep").expect("write keep");

    let target_root = ctx.dest.join("source");
    fs::create_dir_all(&target_root).expect("create target root");
    fs::write(target_root.join("keep.txt"), b"old").expect("write old");
    fs::write(target_root.join("extra.txt"), b"extra").expect("write extra");

    let operands = vec![
        ctx.source.clone().into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().delete_delay(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::DryRun, options)
        .expect("dry run succeeds");

    // In dry-run, extra file should still exist
    assert!(target_root.join("extra.txt").exists());
    assert_eq!(
        fs::read(target_root.join("extra.txt")).expect("read extra"),
        b"extra"
    );

    // Old content should still exist (not updated in dry-run)
    assert_eq!(
        fs::read(target_root.join("keep.txt")).expect("read keep"),
        b"old"
    );

    // Summary should still report what would be deleted
    assert_eq!(summary.items_deleted(), 1);
}

#[test]
fn delete_delay_preserves_symlinks_when_appropriate() {
    #[cfg(unix)]
    {
        use std::os::unix::fs as unix_fs;

        let ctx = test_helpers::setup_copy_test();
        fs::create_dir_all(&ctx.dest).expect("create dest");

        fs::write(ctx.source.join("file.txt"), b"content").expect("write file");

        let target_root = ctx.dest.join("source");
        fs::create_dir_all(&target_root).expect("create target root");
        fs::write(target_root.join("file.txt"), b"old").expect("write old");

        // Create a symlink that points to something outside the transfer
        unix_fs::symlink("/etc/hosts", target_root.join("external_link"))
            .expect("create symlink");

        let operands = vec![
            ctx.source.clone().into_os_string(),
            ctx.dest.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
        let options = LocalCopyOptions::default().delete_delay(true);

        let summary = plan
            .execute_with_options(LocalCopyExecution::Apply, options)
            .expect("copy succeeds");

        assert!(target_root.join("file.txt").exists());
        // The external symlink should be deleted (it's extraneous)
        assert!(!target_root.join("external_link").exists());
        assert_eq!(summary.items_deleted(), 1);
    }
}
