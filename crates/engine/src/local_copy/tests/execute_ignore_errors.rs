
// ==================== --ignore-errors tests ====================
//
// Upstream rsync behavior:
// - Without --ignore-errors: if I/O errors occurred during transfer,
//   --delete will NOT delete files (to prevent data loss when the sender
//   couldn't read all files)
// - With --ignore-errors: --delete proceeds with deletions even when
//   I/O errors occurred during the transfer

// ==================== Option Unit Tests ====================

#[test]
fn ignore_errors_option_defaults_to_false() {
    let opts = LocalCopyOptions::default();
    assert!(
        !opts.ignore_errors_enabled(),
        "ignore_errors should default to false"
    );
}

#[test]
fn ignore_errors_option_can_be_enabled() {
    let opts = LocalCopyOptions::default().ignore_errors(true);
    assert!(
        opts.ignore_errors_enabled(),
        "ignore_errors should be true after enabling"
    );
}

#[test]
fn ignore_errors_option_can_be_disabled_after_enabling() {
    let opts = LocalCopyOptions::default()
        .ignore_errors(true)
        .ignore_errors(false);
    assert!(
        !opts.ignore_errors_enabled(),
        "ignore_errors should be false after disabling"
    );
}

#[test]
fn ignore_errors_builder_defaults_to_false() {
    let opts = LocalCopyOptions::builder()
        .build()
        .expect("valid options");
    assert!(
        !opts.ignore_errors_enabled(),
        "builder should default ignore_errors to false"
    );
}

#[test]
fn ignore_errors_builder_can_be_enabled() {
    let opts = LocalCopyOptions::builder()
        .ignore_errors(true)
        .build()
        .expect("valid options");
    assert!(
        opts.ignore_errors_enabled(),
        "builder should enable ignore_errors"
    );
}

#[test]
fn ignore_errors_builder_can_be_disabled_after_enabling() {
    let opts = LocalCopyOptions::builder()
        .ignore_errors(true)
        .ignore_errors(false)
        .build()
        .expect("valid options");
    assert!(
        !opts.ignore_errors_enabled(),
        "builder should disable ignore_errors"
    );
}

#[test]
fn ignore_errors_compatible_with_delete() {
    let opts = LocalCopyOptions::builder()
        .delete(true)
        .ignore_errors(true)
        .build()
        .expect("valid options");
    assert!(opts.delete_extraneous());
    assert!(opts.ignore_errors_enabled());
}

#[test]
fn ignore_errors_compatible_with_delete_after() {
    let opts = LocalCopyOptions::builder()
        .delete_after(true)
        .ignore_errors(true)
        .build()
        .expect("valid options");
    assert!(opts.delete_after_enabled());
    assert!(opts.ignore_errors_enabled());
}

#[test]
fn ignore_errors_compatible_with_delete_before() {
    let opts = LocalCopyOptions::builder()
        .delete_before(true)
        .ignore_errors(true)
        .build()
        .expect("valid options");
    assert!(opts.delete_before_enabled());
    assert!(opts.ignore_errors_enabled());
}

#[test]
fn ignore_errors_compatible_with_delete_delay() {
    let opts = LocalCopyOptions::builder()
        .delete_delay(true)
        .ignore_errors(true)
        .build()
        .expect("valid options");
    assert!(opts.delete_delay_enabled());
    assert!(opts.ignore_errors_enabled());
}

#[test]
fn ignore_errors_without_delete_is_harmless() {
    // --ignore-errors without --delete should not cause issues
    let opts = LocalCopyOptions::default().ignore_errors(true);
    assert!(opts.ignore_errors_enabled());
    assert!(!opts.delete_extraneous());
}

// ==================== Functional Tests ====================
//
// These tests verify the actual interaction between --ignore-errors
// and the deletion behavior when I/O errors occur.

#[test]
fn delete_works_normally_without_io_errors() {
    // When no I/O errors occur, --delete should work normally
    // regardless of --ignore-errors setting
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    // Source has one file
    fs::write(source.join("keep.txt"), b"keep").expect("write keep");

    // Dest has an extra file that should be deleted
    fs::write(dest.join("keep.txt"), b"old keep").expect("write old keep");
    fs::write(dest.join("extra.txt"), b"extra").expect("write extra");

    let mut source_operand = source.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().delete(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(dest.join("keep.txt").exists(), "kept file should remain");
    assert!(!dest.join("extra.txt").exists(), "extra file should be deleted");
    assert!(summary.items_deleted() >= 1, "should report deletion");
}

#[test]
fn delete_with_ignore_errors_works_normally_without_io_errors() {
    // --ignore-errors with --delete should work the same as --delete alone
    // when no I/O errors occur
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    fs::write(source.join("keep.txt"), b"keep").expect("write keep");
    fs::write(dest.join("keep.txt"), b"old keep").expect("write old keep");
    fs::write(dest.join("extra.txt"), b"extra").expect("write extra");

    let mut source_operand = source.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .delete(true)
        .ignore_errors(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(dest.join("keep.txt").exists(), "kept file should remain");
    assert!(!dest.join("extra.txt").exists(), "extra file should be deleted");
    assert!(summary.items_deleted() >= 1, "should report deletion");
}

#[test]
fn ignore_errors_option_independent_of_delete_timing() {
    // Test that --ignore-errors works with all deletion timing variants
    for timing_setup in ["delete", "delete_after", "delete_before"] {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source");
        let dest = temp.path().join("dest");
        fs::create_dir_all(&source).expect("create source");
        fs::create_dir_all(&dest).expect("create dest");

        fs::write(source.join("keep.txt"), b"keep").expect("write keep");
        fs::write(dest.join("keep.txt"), b"old").expect("write old");
        fs::write(dest.join("extra.txt"), b"extra").expect("write extra");

        let mut source_operand = source.into_os_string();
        source_operand.push(std::path::MAIN_SEPARATOR.to_string());
        let operands = vec![source_operand, dest.clone().into_os_string()];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let options = match timing_setup {
            "delete" => LocalCopyOptions::default().delete(true).ignore_errors(true),
            "delete_after" => LocalCopyOptions::default().delete_after(true).ignore_errors(true),
            "delete_before" => LocalCopyOptions::default().delete_before(true).ignore_errors(true),
            _ => unreachable!(),
        };

        let summary = plan
            .execute_with_options(LocalCopyExecution::Apply, options)
            .expect("copy succeeds");

        assert!(
            dest.join("keep.txt").exists(),
            "kept file should remain with timing {timing_setup}"
        );
        assert!(
            !dest.join("extra.txt").exists(),
            "extra file should be deleted with timing {timing_setup}"
        );
        assert!(
            summary.items_deleted() >= 1,
            "should report deletion with timing {timing_setup}"
        );
    }
}

#[cfg(unix)]
#[test]
fn delete_suppressed_when_io_errors_and_ignore_errors_not_set() {
    // This test simulates what happens when a source file cannot be read:
    // - The transfer encounters an I/O error
    // - Without --ignore-errors, deletions should be suppressed
    //
    // We create a source file that is unreadable (permission denied) to
    // trigger the I/O error during transfer.
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    // Create a readable source file
    fs::write(source.join("good.txt"), b"good").expect("write good");
    // Create an unreadable source file to trigger I/O error
    fs::write(source.join("bad.txt"), b"bad").expect("write bad");
    fs::set_permissions(
        source.join("bad.txt"),
        fs::Permissions::from_mode(0o000),
    )
    .expect("make unreadable");

    // Dest has an extra file that would be deleted
    fs::write(dest.join("good.txt"), b"old good").expect("write old good");
    fs::write(dest.join("extra.txt"), b"should survive").expect("write extra");

    let mut source_operand = source.clone().into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // --delete WITHOUT --ignore-errors
    let options = LocalCopyOptions::default().delete(true);

    // The copy should fail due to the I/O error
    let result = plan.execute_with_options(LocalCopyExecution::Apply, options);

    // Restore permissions for cleanup
    let _ = fs::set_permissions(
        source.join("bad.txt"),
        fs::Permissions::from_mode(0o644),
    );

    // The extra file in dest should survive because deletions are suppressed
    // when I/O errors occur (unless --ignore-errors is set)
    assert!(
        dest.join("extra.txt").exists(),
        "extra.txt should survive when I/O errors occur without --ignore-errors"
    );

    // The operation should have reported an error
    assert!(result.is_err(), "copy should report I/O error");
}

#[cfg(unix)]
#[test]
fn delete_proceeds_when_io_errors_and_ignore_errors_set() {
    // This test is the counterpart: with --ignore-errors, deletions should
    // proceed even when I/O errors occurred during transfer.
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    // Create a readable source file
    fs::write(source.join("good.txt"), b"good").expect("write good");
    // Create an unreadable source file to trigger I/O error
    fs::write(source.join("bad.txt"), b"bad").expect("write bad");
    fs::set_permissions(
        source.join("bad.txt"),
        fs::Permissions::from_mode(0o000),
    )
    .expect("make unreadable");

    // Dest has extra files that should be deleted with --ignore-errors
    fs::write(dest.join("good.txt"), b"old good").expect("write old good");
    fs::write(dest.join("extra.txt"), b"should be deleted").expect("write extra");

    let mut source_operand = source.clone().into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // --delete WITH --ignore-errors
    let options = LocalCopyOptions::default()
        .delete(true)
        .ignore_errors(true);

    let result = plan.execute_with_options(LocalCopyExecution::Apply, options);

    // Restore permissions for cleanup
    let _ = fs::set_permissions(
        source.join("bad.txt"),
        fs::Permissions::from_mode(0o644),
    );

    // With --ignore-errors, the extra file should be deleted even though
    // the transfer had errors
    assert!(
        !dest.join("extra.txt").exists(),
        "extra.txt should be deleted when --ignore-errors is set"
    );

    // The operation should still report an error for the failed file
    assert!(result.is_err(), "copy should report I/O error");
}

#[cfg(unix)]
#[test]
fn ignore_errors_with_delete_after_timing() {
    // Test --ignore-errors with --delete-after timing
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    fs::write(source.join("good.txt"), b"good").expect("write good");
    fs::write(source.join("bad.txt"), b"bad").expect("write bad");
    fs::set_permissions(
        source.join("bad.txt"),
        fs::Permissions::from_mode(0o000),
    )
    .expect("make unreadable");

    fs::write(dest.join("good.txt"), b"old good").expect("write old good");
    fs::write(dest.join("extra.txt"), b"should be deleted").expect("write extra");

    let mut source_operand = source.clone().into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .delete_after(true)
        .ignore_errors(true);

    let result = plan.execute_with_options(LocalCopyExecution::Apply, options);

    let _ = fs::set_permissions(
        source.join("bad.txt"),
        fs::Permissions::from_mode(0o644),
    );

    assert!(
        !dest.join("extra.txt").exists(),
        "extra.txt should be deleted with --delete-after --ignore-errors"
    );
    assert!(result.is_err(), "copy should report I/O error");
}

#[cfg(unix)]
#[test]
fn no_ignore_errors_with_delete_after_suppresses_deletions() {
    // Test that --delete-after suppresses deletions on I/O error without --ignore-errors
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    fs::write(source.join("good.txt"), b"good").expect("write good");
    fs::write(source.join("bad.txt"), b"bad").expect("write bad");
    fs::set_permissions(
        source.join("bad.txt"),
        fs::Permissions::from_mode(0o000),
    )
    .expect("make unreadable");

    fs::write(dest.join("good.txt"), b"old good").expect("write old good");
    fs::write(dest.join("extra.txt"), b"should survive").expect("write extra");

    let mut source_operand = source.clone().into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().delete_after(true);

    let result = plan.execute_with_options(LocalCopyExecution::Apply, options);

    let _ = fs::set_permissions(
        source.join("bad.txt"),
        fs::Permissions::from_mode(0o644),
    );

    assert!(
        dest.join("extra.txt").exists(),
        "extra.txt should survive with --delete-after without --ignore-errors"
    );
    assert!(result.is_err(), "copy should report I/O error");
}

// ==================== Edge Case Tests ====================

#[test]
fn ignore_errors_with_dry_run_reports_deletions() {
    // --ignore-errors in dry-run mode should still report what would happen
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    fs::write(source.join("keep.txt"), b"keep").expect("write keep");
    fs::write(dest.join("keep.txt"), b"old keep").expect("write old keep");
    fs::write(dest.join("extra.txt"), b"extra").expect("write extra");

    let mut source_operand = source.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .delete(true)
        .ignore_errors(true)
        .collect_events(true);

    let report = plan
        .execute_with_report(LocalCopyExecution::DryRun, options)
        .expect("dry-run succeeds");

    let summary = report.summary();

    // In dry-run mode nothing should be modified on disk
    assert!(dest.join("extra.txt").exists(), "file should exist in dry-run");

    // But the summary should report what would happen
    assert_eq!(summary.items_deleted(), 1, "should report 1 deletion in dry-run");
}

#[test]
fn ignore_errors_preserves_good_files_during_transfer() {
    // Even with I/O errors on some files, successfully transferred files
    // should be present in the destination
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    // Create multiple source files - all readable
    fs::write(source.join("file1.txt"), b"content1").expect("write file1");
    fs::write(source.join("file2.txt"), b"content2").expect("write file2");
    fs::write(source.join("file3.txt"), b"content3").expect("write file3");

    let mut source_operand = source.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .delete(true)
        .ignore_errors(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // All files should be transferred
    assert!(dest.join("file1.txt").exists());
    assert!(dest.join("file2.txt").exists());
    assert!(dest.join("file3.txt").exists());
    assert!(summary.files_copied() >= 3, "all files should be copied");
}

#[test]
fn ignore_errors_with_nested_directories() {
    // Test --ignore-errors with nested directory structures
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(source.join("subdir")).expect("create source subdir");
    fs::create_dir_all(dest.join("subdir")).expect("create dest subdir");

    fs::write(source.join("root.txt"), b"root").expect("write root");
    fs::write(source.join("subdir/nested.txt"), b"nested").expect("write nested");

    fs::write(dest.join("root.txt"), b"old root").expect("write old root");
    fs::write(dest.join("subdir/nested.txt"), b"old nested").expect("write old nested");
    fs::write(dest.join("subdir/extra.txt"), b"extra nested").expect("write extra nested");
    fs::write(dest.join("root_extra.txt"), b"extra root").expect("write extra root");

    let mut source_operand = source.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .delete(true)
        .ignore_errors(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(dest.join("root.txt").exists());
    assert!(dest.join("subdir/nested.txt").exists());
    assert!(!dest.join("subdir/extra.txt").exists(), "nested extra should be deleted");
    assert!(!dest.join("root_extra.txt").exists(), "root extra should be deleted");
    assert!(summary.items_deleted() >= 2);
}

#[test]
fn ignore_errors_combined_with_max_delete() {
    // --ignore-errors should work alongside --max-delete
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    fs::write(source.join("keep.txt"), b"keep").expect("write keep");
    fs::write(dest.join("keep.txt"), b"old").expect("write old");
    fs::write(dest.join("extra1.txt"), b"extra1").expect("write extra1");

    let mut source_operand = source.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .delete(true)
        .ignore_errors(true)
        .max_deletions(Some(1));

    // Should work - max_delete=1 and only 1 file to delete
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(!dest.join("extra1.txt").exists());
    assert_eq!(summary.items_deleted(), 1);
}

#[test]
fn ignore_errors_combined_with_delete_excluded() {
    // --ignore-errors + --delete-excluded should work together
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    fs::write(source.join("keep.txt"), b"keep").expect("write keep");
    fs::write(source.join("skip.tmp"), b"skip").expect("write skip");
    fs::write(dest.join("keep.txt"), b"old").expect("write old");
    fs::write(dest.join("skip.tmp"), b"dest skip").expect("write dest skip");

    let mut source_operand = source.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let filter_set = FilterSet::from_rules([FilterRule::exclude("*.tmp")])
        .expect("compile filters");
    let options = LocalCopyOptions::default()
        .delete(true)
        .delete_excluded(true)
        .ignore_errors(true)
        .filters(Some(filter_set));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(dest.join("keep.txt").exists());
    assert!(!dest.join("skip.tmp").exists(), "excluded file should be deleted");
    assert_eq!(summary.items_deleted(), 1);
}

#[test]
fn ignore_errors_without_delete_no_deletions() {
    // --ignore-errors without --delete should not cause any deletions
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    fs::write(source.join("keep.txt"), b"keep").expect("write keep");
    fs::write(dest.join("keep.txt"), b"old").expect("write old");
    fs::write(dest.join("extra.txt"), b"extra").expect("write extra");

    let mut source_operand = source.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().ignore_errors(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Extra file should remain since --delete is not enabled
    assert!(dest.join("extra.txt").exists(), "extra file should remain without --delete");
    assert_eq!(summary.items_deleted(), 0, "no deletions should occur without --delete");
}

#[test]
fn ignore_errors_build_unchecked_also_works() {
    let opts = LocalCopyOptions::builder()
        .delete(true)
        .ignore_errors(true)
        .build_unchecked();
    assert!(opts.delete_extraneous());
    assert!(opts.ignore_errors_enabled());
}
