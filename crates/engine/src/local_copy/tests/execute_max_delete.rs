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

// ==================== Basic Max-Delete Limit Tests ====================

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

// ==================== Edge Cases ====================

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

// ==================== Interaction with Delete Timing Modes ====================

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

// ==================== Dry-Run Behavior ====================

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

// ==================== Directory Deletion Tests ====================

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

// ==================== No Deletions Needed ====================

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

// ==================== Exit Code Tests ====================

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

// ==================== Option Builder Tests ====================

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
