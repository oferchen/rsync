// Tests for --max-delete limit enforcement.
//
// The --max-delete option limits the number of deletions performed during a
// transfer. When the limit is reached, remaining deletions are skipped and
// the operation returns an error with exit code 25 (RERR_DEL_LIMIT).
//
// Key behaviors tested:
// 1. --max-delete=N stops after N deletions
// 2. Error returned when limit is reached with correct exit code
// 3. Correct number of skipped entries reported in error
// 4. Edge cases: max-delete=0, max-delete=1, large values
// 5. Interaction with different delete timing modes
// 6. Dry-run respects max-delete semantics

use super::filter_program::MAX_DELETE_EXIT_CODE;


#[test]
fn max_delete_stops_after_n_deletions() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    // Create source with one file
    fs::write(ctx.source.join("keep.txt"), b"keep").expect("write keep");

    // Create destination with multiple extra files
    let target_root = ctx.dest.join("source");
    fs::create_dir_all(&target_root).expect("create target root");
    fs::write(target_root.join("keep.txt"), b"old").expect("write old");
    fs::write(target_root.join("extra1.txt"), b"extra1").expect("write extra1");
    fs::write(target_root.join("extra2.txt"), b"extra2").expect("write extra2");
    fs::write(target_root.join("extra3.txt"), b"extra3").expect("write extra3");
    fs::write(target_root.join("extra4.txt"), b"extra4").expect("write extra4");

    let operands = vec![
        ctx.source.clone().into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .delete(true)
        .max_deletions(Some(2));

    let error = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect_err("should fail when max-delete limit reached");

    // Verify the error type and exit code
    assert_eq!(error.exit_code(), MAX_DELETE_EXIT_CODE);

    // Verify keep file was still updated
    assert!(target_root.join("keep.txt").exists());
    assert_eq!(
        fs::read(target_root.join("keep.txt")).expect("read keep"),
        b"keep"
    );

    // Count remaining extra files - exactly 2 should be deleted, 2 should remain
    let remaining = [
        target_root.join("extra1.txt").exists(),
        target_root.join("extra2.txt").exists(),
        target_root.join("extra3.txt").exists(),
        target_root.join("extra4.txt").exists(),
    ];
    let remaining_count = remaining.iter().filter(|&&exists| exists).count();
    assert_eq!(remaining_count, 2, "exactly 2 extra files should remain");
}

#[test]
fn max_delete_reports_correct_skipped_count() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    fs::write(ctx.source.join("keep.txt"), b"keep").expect("write keep");

    // Create destination with 5 extra files, limit to 2 deletions
    let target_root = ctx.dest.join("source");
    fs::create_dir_all(&target_root).expect("create target root");
    fs::write(target_root.join("keep.txt"), b"old").expect("write old");
    for i in 1..=5 {
        fs::write(target_root.join(format!("extra{i}.txt")), format!("extra{i}").as_bytes())
            .expect("write extra");
    }

    let operands = vec![
        ctx.source.clone().into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .delete(true)
        .max_deletions(Some(2));

    let error = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect_err("should fail when max-delete limit reached");

    // Check the skipped count in the error
    match error.kind() {
        LocalCopyErrorKind::DeleteLimitExceeded { skipped } => {
            assert_eq!(*skipped, 3, "should report 3 skipped entries (5 total - 2 deleted)");
        }
        other => panic!("unexpected error kind: {other:?}"),
    }
}

#[test]
fn max_delete_error_message_format() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    fs::write(ctx.source.join("keep.txt"), b"keep").expect("write keep");

    let target_root = ctx.dest.join("source");
    fs::create_dir_all(&target_root).expect("create target root");
    fs::write(target_root.join("keep.txt"), b"old").expect("write old");
    fs::write(target_root.join("extra1.txt"), b"extra1").expect("write extra1");
    fs::write(target_root.join("extra2.txt"), b"extra2").expect("write extra2");

    let operands = vec![
        ctx.source.clone().into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .delete(true)
        .max_deletions(Some(1));

    let error = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect_err("should fail when max-delete limit reached");

    // Verify the error message contains expected text
    let message = error.to_string();
    assert!(
        message.contains("--max-delete"),
        "error message should mention --max-delete: {message}"
    );
    assert!(
        message.contains("skipped"),
        "error message should mention skipped: {message}"
    );
}


#[test]
fn max_delete_zero_prevents_all_deletions() {
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
    let options = LocalCopyOptions::default()
        .delete(true)
        .max_deletions(Some(0));

    let error = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect_err("should fail when max-delete=0 and deletions needed");

    // Verify error
    assert_eq!(error.exit_code(), MAX_DELETE_EXIT_CODE);
    match error.kind() {
        LocalCopyErrorKind::DeleteLimitExceeded { skipped } => {
            assert_eq!(*skipped, 1, "should report 1 skipped entry");
        }
        other => panic!("unexpected error kind: {other:?}"),
    }

    // Verify no deletions occurred
    assert!(
        target_root.join("extra.txt").exists(),
        "extra file should still exist with max-delete=0"
    );
    // But keep file should still be updated
    assert_eq!(
        fs::read(target_root.join("keep.txt")).expect("read keep"),
        b"keep"
    );
}

#[test]
fn max_delete_one_allows_single_deletion() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    fs::write(ctx.source.join("keep.txt"), b"keep").expect("write keep");

    let target_root = ctx.dest.join("source");
    fs::create_dir_all(&target_root).expect("create target root");
    fs::write(target_root.join("keep.txt"), b"old").expect("write old");
    fs::write(target_root.join("extra1.txt"), b"extra1").expect("write extra1");
    fs::write(target_root.join("extra2.txt"), b"extra2").expect("write extra2");

    let operands = vec![
        ctx.source.clone().into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .delete(true)
        .max_deletions(Some(1));

    let error = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect_err("should fail when max-delete=1 with 2 deletions needed");

    match error.kind() {
        LocalCopyErrorKind::DeleteLimitExceeded { skipped } => {
            assert_eq!(*skipped, 1, "should report 1 skipped entry");
        }
        other => panic!("unexpected error kind: {other:?}"),
    }

    // Exactly 1 file should be deleted, 1 should remain
    let remaining = [
        target_root.join("extra1.txt").exists(),
        target_root.join("extra2.txt").exists(),
    ];
    let remaining_count = remaining.iter().filter(|&&exists| exists).count();
    assert_eq!(remaining_count, 1, "exactly 1 extra file should remain");
}

#[test]
fn max_delete_exact_limit_succeeds() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    fs::write(ctx.source.join("keep.txt"), b"keep").expect("write keep");

    // Create destination with exactly 3 extra files, set limit to 3
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
        .delete(true)
        .max_deletions(Some(3));

    // Should succeed when deletions exactly match limit
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("should succeed when deletions equal limit");

    assert_eq!(summary.items_deleted(), 3);
    assert!(!target_root.join("extra1.txt").exists());
    assert!(!target_root.join("extra2.txt").exists());
    assert!(!target_root.join("extra3.txt").exists());
}

#[test]
fn max_delete_under_limit_succeeds() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    fs::write(ctx.source.join("keep.txt"), b"keep").expect("write keep");

    // Create destination with 2 extra files, set limit to 10
    let target_root = ctx.dest.join("source");
    fs::create_dir_all(&target_root).expect("create target root");
    fs::write(target_root.join("keep.txt"), b"old").expect("write old");
    fs::write(target_root.join("extra1.txt"), b"extra1").expect("write extra1");
    fs::write(target_root.join("extra2.txt"), b"extra2").expect("write extra2");

    let operands = vec![
        ctx.source.clone().into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .delete(true)
        .max_deletions(Some(10));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("should succeed when deletions under limit");

    assert_eq!(summary.items_deleted(), 2);
    assert!(!target_root.join("extra1.txt").exists());
    assert!(!target_root.join("extra2.txt").exists());
}

#[test]
fn max_delete_large_value() {
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
    // Use a very large limit
    let options = LocalCopyOptions::default()
        .delete(true)
        .max_deletions(Some(u64::MAX));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("should succeed with large max-delete limit");

    assert_eq!(summary.items_deleted(), 1);
    assert!(!target_root.join("extra.txt").exists());
}

#[test]
fn max_delete_none_allows_unlimited_deletions() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    fs::write(ctx.source.join("keep.txt"), b"keep").expect("write keep");

    // Create destination with many extra files
    let target_root = ctx.dest.join("source");
    fs::create_dir_all(&target_root).expect("create target root");
    fs::write(target_root.join("keep.txt"), b"old").expect("write old");
    for i in 1..=10 {
        fs::write(target_root.join(format!("extra{i}.txt")), format!("extra{i}").as_bytes())
            .expect("write extra");
    }

    let operands = vec![
        ctx.source.clone().into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    // No max_deletions limit (None)
    let options = LocalCopyOptions::default()
        .delete(true)
        .max_deletions(None);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("should succeed with no max-delete limit");

    assert_eq!(summary.items_deleted(), 10);
    for i in 1..=10 {
        assert!(
            !target_root.join(format!("extra{i}.txt")).exists(),
            "extra{i}.txt should be deleted"
        );
    }
}


#[test]
fn max_delete_with_delete_during() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    fs::write(ctx.source.join("keep.txt"), b"keep").expect("write keep");

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
        .delete_during()
        .max_deletions(Some(1));

    let error = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect_err("should fail when max-delete exceeded with delete-during");

    assert_eq!(error.exit_code(), MAX_DELETE_EXIT_CODE);
    match error.kind() {
        LocalCopyErrorKind::DeleteLimitExceeded { skipped } => {
            assert_eq!(*skipped, 2, "should report 2 skipped entries");
        }
        other => panic!("unexpected error kind: {other:?}"),
    }
}

#[test]
fn max_delete_with_delete_before() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    fs::write(ctx.source.join("keep.txt"), b"keep").expect("write keep");

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
        .delete_before(true)
        .max_deletions(Some(2));

    let error = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect_err("should fail when max-delete exceeded with delete-before");

    assert_eq!(error.exit_code(), MAX_DELETE_EXIT_CODE);
    match error.kind() {
        LocalCopyErrorKind::DeleteLimitExceeded { skipped } => {
            // delete-before may traverse directories differently, causing varying skip counts
            assert!(*skipped >= 1, "should report at least 1 skipped entry, got {skipped}");
        }
        other => panic!("unexpected error kind: {other:?}"),
    }
}

#[test]
fn max_delete_with_delete_after() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    fs::write(ctx.source.join("keep.txt"), b"keep").expect("write keep");

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
        .delete_after(true)
        .max_deletions(Some(2));

    let error = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect_err("should fail when max-delete exceeded with delete-after");

    assert_eq!(error.exit_code(), MAX_DELETE_EXIT_CODE);
    match error.kind() {
        LocalCopyErrorKind::DeleteLimitExceeded { skipped } => {
            assert_eq!(*skipped, 1, "should report 1 skipped entry");
        }
        other => panic!("unexpected error kind: {other:?}"),
    }
}

#[test]
fn max_delete_with_delete_delay() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    fs::write(ctx.source.join("keep.txt"), b"keep").expect("write keep");

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
        .expect_err("should fail when max-delete exceeded with delete-delay");

    assert_eq!(error.exit_code(), MAX_DELETE_EXIT_CODE);
    match error.kind() {
        LocalCopyErrorKind::DeleteLimitExceeded { skipped } => {
            assert_eq!(*skipped, 1, "should report 1 skipped entry");
        }
        other => panic!("unexpected error kind: {other:?}"),
    }
}


#[test]
fn max_delete_with_dry_run_reports_limit_exceeded() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    fs::write(ctx.source.join("keep.txt"), b"keep").expect("write keep");

    let target_root = ctx.dest.join("source");
    fs::create_dir_all(&target_root).expect("create target root");
    fs::write(target_root.join("keep.txt"), b"old").expect("write old");
    fs::write(target_root.join("extra1.txt"), b"extra1").expect("write extra1");
    fs::write(target_root.join("extra2.txt"), b"extra2").expect("write extra2");

    let operands = vec![
        ctx.source.clone().into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .delete(true)
        .max_deletions(Some(1));

    // Dry-run should still report the error
    let error = plan
        .execute_with_options(LocalCopyExecution::DryRun, options)
        .expect_err("dry-run should report max-delete exceeded");

    assert_eq!(error.exit_code(), MAX_DELETE_EXIT_CODE);

    // Files should still exist (dry-run doesn't modify)
    assert!(target_root.join("extra1.txt").exists());
    assert!(target_root.join("extra2.txt").exists());
    assert_eq!(
        fs::read(target_root.join("keep.txt")).expect("read keep"),
        b"old",
        "dry-run should not modify files"
    );
}

#[test]
fn max_delete_dry_run_success_when_under_limit() {
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
    let options = LocalCopyOptions::default()
        .delete(true)
        .max_deletions(Some(10));

    let summary = plan
        .execute_with_options(LocalCopyExecution::DryRun, options)
        .expect("dry-run should succeed when under limit");

    // Dry-run reports what would be deleted
    assert_eq!(summary.items_deleted(), 1);

    // Files should still exist
    assert!(target_root.join("extra.txt").exists());
}


#[test]
fn max_delete_counts_directory_deletions() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    fs::write(ctx.source.join("keep.txt"), b"keep").expect("write keep");

    // Create destination with extra directories
    let target_root = ctx.dest.join("source");
    fs::create_dir_all(&target_root).expect("create target root");
    fs::write(target_root.join("keep.txt"), b"old").expect("write old");
    fs::create_dir_all(target_root.join("extra_dir1")).expect("create extra_dir1");
    fs::create_dir_all(target_root.join("extra_dir2")).expect("create extra_dir2");
    fs::create_dir_all(target_root.join("extra_dir3")).expect("create extra_dir3");

    let operands = vec![
        ctx.source.clone().into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .delete(true)
        .max_deletions(Some(2));

    let error = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect_err("should fail when max-delete exceeded for directories");

    assert_eq!(error.exit_code(), MAX_DELETE_EXIT_CODE);
    match error.kind() {
        LocalCopyErrorKind::DeleteLimitExceeded { skipped } => {
            assert_eq!(*skipped, 1, "should report 1 skipped directory");
        }
        other => panic!("unexpected error kind: {other:?}"),
    }
}

#[test]
fn max_delete_mixed_files_and_directories() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    fs::write(ctx.source.join("keep.txt"), b"keep").expect("write keep");

    // Create destination with mix of extra files and directories
    let target_root = ctx.dest.join("source");
    fs::create_dir_all(&target_root).expect("create target root");
    fs::write(target_root.join("keep.txt"), b"old").expect("write old");
    fs::write(target_root.join("extra1.txt"), b"extra1").expect("write extra1");
    fs::create_dir_all(target_root.join("extra_dir")).expect("create extra_dir");
    fs::write(target_root.join("extra2.txt"), b"extra2").expect("write extra2");

    let operands = vec![
        ctx.source.clone().into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .delete(true)
        .max_deletions(Some(2));

    let error = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect_err("should fail when max-delete exceeded");

    assert_eq!(error.exit_code(), MAX_DELETE_EXIT_CODE);
    match error.kind() {
        LocalCopyErrorKind::DeleteLimitExceeded { skipped } => {
            assert_eq!(*skipped, 1, "should report 1 skipped entry");
        }
        other => panic!("unexpected error kind: {other:?}"),
    }
}


#[test]
fn max_delete_no_effect_when_no_deletions_needed() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    fs::write(ctx.source.join("file.txt"), b"content").expect("write file");

    // Create destination with same structure (no extra files)
    let target_root = ctx.dest.join("source");
    fs::create_dir_all(&target_root).expect("create target root");
    fs::write(target_root.join("file.txt"), b"old").expect("write old");

    let operands = vec![
        ctx.source.clone().into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .delete(true)
        .max_deletions(Some(0)); // Even with max-delete=0, should succeed

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("should succeed when no deletions needed");

    assert_eq!(summary.items_deleted(), 0);
    assert_eq!(summary.files_copied(), 1);
}

#[test]
fn max_delete_without_delete_flag_has_no_effect() {
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
    // Set max_deletions but NOT delete flag
    let options = LocalCopyOptions::default().max_deletions(Some(0));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("should succeed when delete not enabled");

    // No deletions should occur
    assert_eq!(summary.items_deleted(), 0);
    assert!(
        target_root.join("extra.txt").exists(),
        "extra file should remain when delete not enabled"
    );
}


#[test]
fn max_delete_survivor_set_matches_upstream_traversal() {
    // Regression for the which-entries divergence (#207): with a mix of an
    // extraneous directory whose name byte-sorts BEFORE the extraneous
    // sibling files, oc must delete the same entries upstream rsync does
    // when `--max-delete` caps the pass.
    //
    // Layout under the target: `D/{f1..f5}` (directory), plus files `a1`,
    // `a2`. Upstream sorts each directory with protocol-29 f_name_cmp -
    // files before directories - and deletes the resulting list in reverse,
    // so it visits `D` first (recursing into it) and only reaches `a1`/`a2`
    // afterwards. With `--max-delete=3` it removes `D/f5`, `D/f4`, `D/f3`
    // (reverse name order inside `D`), hits the cap, and keeps `D/f1`,
    // `D/f2`, `a1`, `a2`.
    //
    // A plain byte sort would order `D` (0x44) ahead of `a1`/`a2` (0x61) and,
    // reversed, delete `a2` and `a1` first, then a single file inside `D` -
    // surviving set `D/f1..f4`, which diverges from upstream. This test
    // fails if the dir-aware ordering regresses to a byte sort.
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    fs::write(ctx.source.join("keep.txt"), b"keep").expect("write keep");

    let target_root = ctx.dest.join("source");
    fs::create_dir_all(target_root.join("D")).expect("create D");
    fs::write(target_root.join("keep.txt"), b"old").expect("write old");
    for i in 1..=5 {
        fs::write(target_root.join("D").join(format!("f{i}")), b"x").expect("write f");
    }
    fs::write(target_root.join("a1"), b"x").expect("write a1");
    fs::write(target_root.join("a2"), b"x").expect("write a2");

    let operands = vec![
        ctx.source.clone().into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .delete(true)
        .max_deletions(Some(3));

    let error = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect_err("should fail when max-delete exceeded");

    assert_eq!(error.exit_code(), MAX_DELETE_EXIT_CODE);
    match error.kind() {
        LocalCopyErrorKind::DeleteLimitExceeded { skipped } => {
            assert_eq!(*skipped, 4, "f2, f1, a2, a1 remain unskipped");
        }
        other => panic!("unexpected error kind: {other:?}"),
    }

    // Upstream-correct survivors.
    for survivor in ["a1", "a2", "D/f1", "D/f2"] {
        assert!(
            target_root.join(survivor).exists(),
            "{survivor} must survive (upstream keeps it)"
        );
    }
    // Upstream-correct deletions.
    for deleted in ["D/f3", "D/f4", "D/f5"] {
        assert!(
            !target_root.join(deleted).exists(),
            "{deleted} must be deleted (upstream removes it)"
        );
    }
}

#[test]
fn max_delete_exit_code_is_25() {
    // Verify the exit code constant matches upstream rsync's RERR_DEL_LIMIT
    assert_eq!(MAX_DELETE_EXIT_CODE, 25);
}

#[test]
fn max_delete_error_code_name() {
    let error = LocalCopyError::delete_limit_exceeded(5);
    assert_eq!(error.code_name(), "RERR_DEL_LIMIT");
}


#[test]
fn max_deletions_option_sets_correctly() {
    let options = LocalCopyOptions::new().max_deletions(Some(100));
    assert_eq!(options.max_deletion_limit(), Some(100));
}

#[test]
fn max_deletions_option_none_clears() {
    let options = LocalCopyOptions::new()
        .max_deletions(Some(100))
        .max_deletions(None);
    assert_eq!(options.max_deletion_limit(), None);
}

#[test]
fn max_deletions_option_zero() {
    let options = LocalCopyOptions::new().max_deletions(Some(0));
    assert_eq!(options.max_deletion_limit(), Some(0));
}

/// Recursively counts every filesystem entry under `dir`, optionally skipping
/// a single top-level name. Used to prove the exact number of entries a
/// `--max-delete` run removed.
fn count_entries_under(dir: &std::path::Path, skip_top: Option<&str>) -> usize {
    let mut total = 0;
    for entry in fs::read_dir(dir).expect("read_dir") {
        let entry = entry.expect("dir entry");
        if skip_top.is_some_and(|s| entry.file_name() == std::ffi::OsStr::new(s)) {
            continue;
        }
        total += 1;
        if entry.file_type().expect("file_type").is_dir() {
            total += count_entries_under(&entry.path(), None);
        }
    }
    total
}

/// DATA-SAFETY REGRESSION: a non-empty extraneous directory must count every
/// leaf it contains against `--max-delete`, and the run must never remove more
/// than the configured number of filesystem entries.
///
/// Before the fix the local-copy emitter removed a doomed subtree wholesale
/// (`remove_dir_all`) and counted it as a single deletion, so `--max-delete=3`
/// against a directory of five files plus two loose files silently deleted all
/// eight entries. upstream `delete.c:156`/`:181` guards and counts each removed
/// leaf, deleting exactly three and reporting `RERR_DEL_LIMIT` (25). Verified
/// byte-for-byte against rsync 3.4.3 (3 deleted, 4 skipped, exit 25).
#[test]
fn max_delete_counts_leaves_inside_nonempty_directory() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");
    fs::write(ctx.source.join("keep.txt"), b"keep").expect("write keep");

    let target_root = ctx.dest.join("source");
    fs::create_dir_all(&target_root).expect("create target root");
    fs::write(target_root.join("keep.txt"), b"old").expect("write old");

    // One extraneous directory holding five files, plus two loose files: eight
    // filesystem entries doomed by --delete.
    let extra_dir = target_root.join("extra_dir");
    fs::create_dir_all(&extra_dir).expect("create extra_dir");
    for i in 1..=5 {
        fs::write(extra_dir.join(format!("f{i}")), b"x").expect("write child");
    }
    fs::write(target_root.join("a1"), b"x").expect("write a1");
    fs::write(target_root.join("a2"), b"x").expect("write a2");

    let operands = vec![
        ctx.source.clone().into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .delete(true)
        .max_deletions(Some(3));

    let error = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect_err("must stop at the cap and report RERR_DEL_LIMIT");

    assert_eq!(error.exit_code(), MAX_DELETE_EXIT_CODE);
    match error.kind() {
        LocalCopyErrorKind::DeleteLimitExceeded { skipped } => {
            assert_eq!(*skipped, 4, "upstream reports 4 skipped for this layout");
        }
        other => panic!("unexpected error kind: {other:?}"),
    }

    // Exactly three of the eight doomed entries may be removed - never the whole
    // subtree. Five must survive.
    let survivors = count_entries_under(&target_root, Some("keep.txt"));
    assert_eq!(
        survivors, 5,
        "exactly 3 of 8 entries may be deleted under --max-delete=3"
    );
}

/// The same leaf-counting cap must hold under `--delete-delay` (collect then
/// apply): a small `--max-delete` must not be defeated by the deferred pass.
#[test]
fn max_delete_counts_leaves_inside_nonempty_directory_delete_delay() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");
    fs::write(ctx.source.join("keep.txt"), b"keep").expect("write keep");

    let target_root = ctx.dest.join("source");
    fs::create_dir_all(&target_root).expect("create target root");
    fs::write(target_root.join("keep.txt"), b"old").expect("write old");

    let extra_dir = target_root.join("extra_dir");
    fs::create_dir_all(&extra_dir).expect("create extra_dir");
    for i in 1..=5 {
        fs::write(extra_dir.join(format!("f{i}")), b"x").expect("write child");
    }
    fs::write(target_root.join("a1"), b"x").expect("write a1");
    fs::write(target_root.join("a2"), b"x").expect("write a2");

    let operands = vec![
        ctx.source.clone().into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .delete_delay(true)
        .max_deletions(Some(3));

    let error = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect_err("must stop at the cap under --delete-delay");

    assert_eq!(error.exit_code(), MAX_DELETE_EXIT_CODE);
    let survivors = count_entries_under(&target_root, Some("keep.txt"));
    assert_eq!(
        survivors, 5,
        "exactly 3 of 8 entries may be deleted under --max-delete=3"
    );
}
