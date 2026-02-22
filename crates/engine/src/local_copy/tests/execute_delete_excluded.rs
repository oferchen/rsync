// Integration tests for `--delete-excluded` with filter rules.
//
// The key distinction being tested:
// - `--delete` alone preserves excluded destination files even when absent from source.
// - `--delete-excluded` deletes destination files that match exclude rules.

/// Verifies that `--delete` with `--exclude=*.log` preserves all `.log` files
/// at the destination, including those not present in the source, because excluded
/// files are protected from deletion by default.
///
/// Source: {a.txt, b.log}
/// Dest:   {a.txt, b.log, c.log}
/// Filter: exclude *.log
///
/// Expected after `--delete`: dest keeps a.txt, b.log, c.log (b.log and c.log protected)
#[test]
fn delete_preserves_excluded_log_files_at_dest() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    // Source has a.txt and b.log.
    fs::write(source.join("a.txt"), b"content a").expect("write a.txt");
    fs::write(source.join("b.log"), b"log b").expect("write b.log");

    // Dest has a.txt, b.log, and an extra c.log not in source.
    // Use different content sizes to avoid quick-check mtime+size skips.
    fs::write(dest.join("a.txt"), b"old a content with extra bytes").expect("write old a.txt");
    fs::write(dest.join("b.log"), b"old b log with extra bytes here").expect("write old b.log");
    fs::write(dest.join("c.log"), b"only at dest log").expect("write c.log");

    let mut source_operand = source.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let filters =
        FilterSet::from_rules([FilterRule::exclude("*.log")]).expect("compile filters");
    let options = LocalCopyOptions::default()
        .delete(true)
        .filters(Some(filters));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // a.txt transferred from source, must remain.
    assert!(dest.join("a.txt").exists(), "a.txt must exist at dest");
    // b.log excluded from transfer but preserved at dest (--delete respects excludes).
    assert!(
        dest.join("b.log").exists(),
        "b.log must be preserved by --delete (excluded files not deleted)"
    );
    // c.log not in source and excluded, so also preserved by --delete.
    assert!(
        dest.join("c.log").exists(),
        "c.log must be preserved by --delete (excluded files not deleted)"
    );
    // No deletions: excluded files are protected, no other extraneous files.
    assert_eq!(summary.items_deleted(), 0, "no deletions expected");
}

/// Verifies that `--delete-excluded` with `--exclude=*.log` deletes ALL `.log`
/// files from the destination, including those that also exist in the source,
/// because `--delete-excluded` overrides the protection for excluded files.
///
/// Source: {a.txt, b.log}
/// Dest:   {a.txt, b.log, c.log}
/// Filter: exclude *.log
///
/// Expected after `--delete --delete-excluded`: dest keeps only a.txt; b.log and
/// c.log are deleted because they match the exclude pattern.
#[test]
fn delete_excluded_removes_log_files_matching_exclude_pattern() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    // Source has a.txt and b.log.
    fs::write(source.join("a.txt"), b"content a").expect("write a.txt");
    fs::write(source.join("b.log"), b"log b source").expect("write b.log");

    // Dest has a.txt, b.log, and an extra c.log not in source.
    // Use different content sizes to avoid quick-check mtime+size skips.
    fs::write(dest.join("a.txt"), b"old a content with extra bytes here").expect("write old a.txt");
    fs::write(dest.join("b.log"), b"old b log content with extra bytes").expect("write old b.log");
    fs::write(dest.join("c.log"), b"only at dest log content").expect("write c.log");

    let mut source_operand = source.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let filters =
        FilterSet::from_rules([FilterRule::exclude("*.log")]).expect("compile filters");
    let options = LocalCopyOptions::default()
        .delete(true)
        .delete_excluded(true)
        .filters(Some(filters));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // a.txt transferred from source, must remain.
    assert!(dest.join("a.txt").exists(), "a.txt must exist at dest");
    // b.log is excluded and --delete-excluded is active: must be deleted even though
    // it exists in source, because the exclude rule takes priority.
    assert!(
        !dest.join("b.log").exists(),
        "b.log must be deleted by --delete-excluded"
    );
    // c.log is excluded and not in source: must also be deleted.
    assert!(
        !dest.join("c.log").exists(),
        "c.log must be deleted by --delete-excluded"
    );
    // Both b.log and c.log are deleted.
    assert_eq!(
        summary.items_deleted(),
        2,
        "both excluded .log files must be deleted"
    );
}

/// Verifies that an explicit `--filter='exclude *.tmp'` rule combined with
/// `--delete-excluded` causes `.tmp` files present at the destination to be
/// deleted, mirroring upstream rsync's `--filter='exclude *.tmp'` CLI behaviour.
#[test]
fn delete_excluded_with_filter_exclude_rule_removes_tmp_files() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    // Source has only regular files; .tmp files are absent from source.
    fs::write(source.join("data.txt"), b"important data").expect("write data.txt");
    fs::write(source.join("notes.txt"), b"notes content").expect("write notes.txt");

    // Dest has the same files plus .tmp files that should be swept away.
    // Use different content sizes to avoid quick-check mtime+size skips.
    fs::write(dest.join("data.txt"), b"old data with more bytes here").expect("write old data.txt");
    fs::write(dest.join("notes.txt"), b"old notes with more bytes here").expect("write old notes.txt");
    fs::write(dest.join("cache.tmp"), b"temporary cache data").expect("write cache.tmp");
    fs::write(dest.join("scratch.tmp"), b"scratch file data").expect("write scratch.tmp");

    let mut source_operand = source.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Simulate --filter='exclude *.tmp' via FilterSet.
    let filters =
        FilterSet::from_rules([FilterRule::exclude("*.tmp")]).expect("compile filters");
    let options = LocalCopyOptions::default()
        .delete(true)
        .delete_excluded(true)
        .filters(Some(filters));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Regular files must survive.
    assert!(dest.join("data.txt").exists(), "data.txt must exist");
    assert!(dest.join("notes.txt").exists(), "notes.txt must exist");
    // Both .tmp files must be removed by --delete-excluded.
    assert!(
        !dest.join("cache.tmp").exists(),
        "cache.tmp must be deleted by --delete-excluded"
    );
    assert!(
        !dest.join("scratch.tmp").exists(),
        "scratch.tmp must be deleted by --delete-excluded"
    );
    assert_eq!(
        summary.items_deleted(),
        2,
        "both .tmp files must be deleted"
    );
}

/// Verifies that `--delete` without `--delete-excluded` preserves `.tmp` files
/// at the destination that match the exclude rule, even when those files are
/// absent from the source entirely. This is the contrast case to
/// `delete_excluded_with_filter_exclude_rule_removes_tmp_files`.
#[test]
fn delete_without_delete_excluded_preserves_filtered_tmp_files() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    // Source has only regular files; .tmp files are absent from source.
    fs::write(source.join("data.txt"), b"important data").expect("write data.txt");

    // Dest has both regular and .tmp files.
    // Use different content sizes to avoid quick-check mtime+size skips.
    fs::write(dest.join("data.txt"), b"old data with extra bytes here").expect("write old data.txt");
    fs::write(dest.join("extra.txt"), b"extraneous non-excluded file").expect("write extra.txt");
    fs::write(dest.join("keep.tmp"), b"preserved tmp file content").expect("write keep.tmp");

    let mut source_operand = source.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let filters =
        FilterSet::from_rules([FilterRule::exclude("*.tmp")]).expect("compile filters");
    // No delete_excluded: excluded files are protected from deletion.
    let options = LocalCopyOptions::default()
        .delete(true)
        .filters(Some(filters));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // data.txt copied from source; extra.txt is extraneous and not excluded, so deleted.
    assert!(dest.join("data.txt").exists(), "data.txt must exist");
    assert!(!dest.join("extra.txt").exists(), "extra.txt must be deleted");
    // keep.tmp matches exclude rule and --delete-excluded is not set: must be preserved.
    assert!(
        dest.join("keep.tmp").exists(),
        "keep.tmp must be preserved by --delete (no --delete-excluded)"
    );
    assert_eq!(
        summary.items_deleted(),
        1,
        "only the non-excluded extraneous file is deleted"
    );
}
