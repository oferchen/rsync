// FFV-5/6/7: Tests for --files-from vanished file handling.
//
// Verifies that when source files listed in the plan vanish before
// transfer, the engine produces the correct exit codes and behavior
// for --ignore-missing-args and --delete-missing-args.

// FFV-5: Vanished source file produces exit code 24 and warning.
//
// When a source file disappears between plan creation and execution,
// upstream rsync emits "file has vanished" and exits with
// RERR_VANISHED (24). The remaining files in the transfer must still
// be copied.
#[test]
fn vanished_source_exits_with_vanished_code_and_copies_remaining() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    fs::write(source_root.join("keep.txt"), b"persistent content").expect("write keep");
    fs::write(source_root.join("vanish.txt"), b"ephemeral content").expect("write vanish");
    fs::write(source_root.join("also_keep.txt"), b"also persistent").expect("write also_keep");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest");

    let mut source_operand = source_root.clone().into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Delete one source file after the plan is built.
    fs::remove_file(source_root.join("vanish.txt")).expect("delete vanish");

    let result = plan.execute();

    // Must fail with exit code 24 (RERR_VANISHED).
    let err = result.expect_err("vanished file should produce an error");
    assert_eq!(
        err.exit_code(),
        24,
        "expected RERR_VANISHED (24), got {}",
        err.exit_code()
    );
    assert!(
        err.is_vanished_error(),
        "error should be classified as vanished"
    );

    // Surviving files must still be copied.
    assert!(
        dest_root.join("keep.txt").exists(),
        "keep.txt should be copied despite vanished sibling"
    );
    assert!(
        dest_root.join("also_keep.txt").exists(),
        "also_keep.txt should be copied despite vanished sibling"
    );
    assert!(
        !dest_root.join("vanish.txt").exists(),
        "vanished file should not appear in destination"
    );
}

// FFV-5 (single file variant): When the sole source file vanishes,
// the error should still be RERR_VANISHED (24).
#[test]
fn sole_vanished_source_exits_with_vanished_code() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    let source_file = source_root.join("only.txt");
    fs::write(&source_file, b"sole file").expect("write only");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest");

    let operands = vec![
        source_file.clone().into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    fs::remove_file(&source_file).expect("delete source");

    let err = plan.execute().expect_err("vanished source should fail");
    assert_eq!(err.exit_code(), 24, "sole vanished file: expected 24");
}

// FFV-6: --ignore-missing-args suppresses warning and exits 0.
//
// With this flag, vanished sources are silently skipped.
// The transfer succeeds (exit 0) and remaining files are copied.
#[test]
fn ignore_missing_args_suppresses_vanished_error() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    fs::write(source_root.join("present.txt"), b"present content").expect("write present");
    fs::write(source_root.join("gone.txt"), b"will vanish").expect("write gone");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest");

    let mut source_operand = source_root.clone().into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    fs::remove_file(source_root.join("gone.txt")).expect("delete gone");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().ignore_missing_args(true),
        )
        .expect("ignore-missing-args should succeed (exit 0)");

    // The surviving file must be copied.
    assert!(
        dest_root.join("present.txt").exists(),
        "present.txt should be copied"
    );
    assert!(
        !dest_root.join("gone.txt").exists(),
        "vanished file should not appear in destination"
    );
    assert!(
        summary.files_copied() >= 1,
        "at least one file should be transferred"
    );
}

// FFV-6 (all files vanished): Even when every source file has vanished,
// --ignore-missing-args should succeed with exit 0.
#[test]
fn ignore_missing_args_all_vanished_exits_zero() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    fs::write(source_root.join("a.txt"), b"aaa").expect("write a");
    fs::write(source_root.join("b.txt"), b"bbb").expect("write b");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest");

    let mut source_operand = source_root.clone().into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Remove all source files.
    fs::remove_file(source_root.join("a.txt")).expect("delete a");
    fs::remove_file(source_root.join("b.txt")).expect("delete b");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().ignore_missing_args(true),
        )
        .expect("ignore-missing-args with all vanished should still exit 0");

    assert_eq!(summary.files_copied(), 0, "no files should be copied");
}

// FFV-7: --delete-missing-args removes destination file for vanished source.
//
// When a source file listed as an operand vanishes between plan creation
// and execution, --delete-missing-args deletes the corresponding destination
// entry. The transfer succeeds (exit 0).
#[test]
fn delete_missing_args_removes_destination_for_vanished_source() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    let stay_src = source_root.join("stay.txt");
    let disappear_src = source_root.join("disappear.txt");
    fs::write(&stay_src, b"stays").expect("write stay");
    fs::write(&disappear_src, b"will vanish").expect("write disappear");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest");

    // Pre-populate destination with the file that should be deleted.
    fs::write(dest_root.join("disappear.txt"), b"old destination copy").expect("write dest disappear");
    // Also pre-populate a file that should remain.
    fs::write(dest_root.join("stay.txt"), b"old stay").expect("write dest stay");

    // Each source file is an individual operand - this is how --delete-missing-args
    // works: it applies to top-level args that vanish, not files discovered during
    // directory traversal.
    let operands = vec![
        stay_src.into_os_string(),
        disappear_src.clone().into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Remove source - the corresponding destination entry should be deleted.
    fs::remove_file(&disappear_src).expect("delete disappear from source");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().delete_missing_args(true),
        )
        .expect("delete-missing-args should succeed");

    // The surviving source should be transferred to the destination.
    assert!(
        dest_root.join("stay.txt").exists(),
        "stay.txt should be present in destination"
    );
    // The vanished source's destination entry should be removed.
    assert!(
        !dest_root.join("disappear.txt").exists(),
        "disappear.txt should be deleted from destination"
    );
    assert!(
        summary.items_deleted() >= 1,
        "at least one item should be recorded as deleted"
    );
}

// FFV-7 (no destination to delete): When the source vanishes but
// no corresponding destination entry exists, --delete-missing-args
// should still succeed without error.
#[test]
fn delete_missing_args_no_destination_entry_succeeds() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    let present_src = source_root.join("present.txt");
    let phantom_src = source_root.join("phantom.txt");
    fs::write(&present_src, b"present").expect("write present");
    fs::write(&phantom_src, b"phantom").expect("write phantom");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest");

    // Each source file is an individual operand.
    let operands = vec![
        present_src.into_os_string(),
        phantom_src.clone().into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Remove source - but no destination entry exists for it.
    fs::remove_file(&phantom_src).expect("delete phantom from source");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().delete_missing_args(true),
        )
        .expect("delete-missing-args should succeed even without destination entry");

    assert!(
        dest_root.join("present.txt").exists(),
        "present.txt should be copied"
    );
    assert_eq!(
        summary.items_deleted(),
        0,
        "no deletions expected when destination entry does not exist"
    );
}

// FFV-7 (directory vanished): When a directory source vanishes and
// --delete-missing-args is active, the corresponding destination
// directory should be removed.
#[test]
fn delete_missing_args_removes_destination_directory_for_vanished_source() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    let vanish_dir = source_root.join("vanish_dir");
    fs::create_dir_all(&vanish_dir).expect("create vanish_dir");
    fs::write(vanish_dir.join("inner.txt"), b"inner").expect("write inner");

    fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest");

    // Pre-populate destination with the directory that should be deleted.
    let dest_vanish_dir = dest_root.join("vanish_dir");
    fs::create_dir_all(&dest_vanish_dir).expect("create dest vanish_dir");
    fs::write(dest_vanish_dir.join("inner.txt"), b"old inner").expect("write dest inner");

    // Use individual operands (not trailing separator) so each source
    // is a separate operand - the directory is one, keep.txt is another.
    let operands = vec![
        vanish_dir.clone().into_os_string(),
        source_root.join("keep.txt").into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Remove the entire source directory after the plan is built.
    fs::remove_dir_all(&vanish_dir).expect("delete vanish_dir from source");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().delete_missing_args(true),
        )
        .expect("delete-missing-args for directory should succeed");

    // The vanished directory should be removed from the destination.
    assert!(
        !dest_root.join("vanish_dir").exists(),
        "vanish_dir should be deleted from destination"
    );
    // The file source should still be transferred.
    assert!(
        dest_root.join("keep.txt").exists(),
        "keep.txt should be present in destination"
    );
    assert!(
        summary.items_deleted() >= 1,
        "at least one item should be recorded as deleted"
    );
}
