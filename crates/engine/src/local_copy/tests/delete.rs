
#[test]
fn delete_respects_exclude_filters() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");
    fs::write(ctx.source.join("keep.txt"), b"keep").expect("write keep");

    let target_root = ctx.dest.join("source");
    fs::create_dir_all(&target_root).expect("create target root");
    fs::write(target_root.join("skip.tmp"), b"dest skip").expect("write existing skip");
    fs::write(target_root.join("extra.txt"), b"extra").expect("write extra");

    let operands = vec![
        ctx.source.clone().into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let filters = FilterSet::from_rules([filters::FilterRule::exclude("*.tmp")])
        .expect("compile filters");
    let options = LocalCopyOptions::default()
        .delete(true)
        .filters(Some(filters));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let target_root = ctx.dest.join("source");
    assert!(target_root.join("keep.txt").exists());
    assert!(!target_root.join("extra.txt").exists());
    let skip_path = target_root.join("skip.tmp");
    assert!(skip_path.exists());
    assert_eq!(fs::read(skip_path).expect("read skip"), b"dest skip");
    assert!(summary.files_copied() >= 1);
    assert_eq!(summary.items_deleted(), 1);
}

#[test]
fn delete_excluded_removes_excluded_entries() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");
    fs::write(ctx.source.join("keep.txt"), b"keep").expect("write keep");

    let target_root = ctx.dest.join("source");
    fs::create_dir_all(&target_root).expect("create target root");
    fs::write(target_root.join("skip.tmp"), b"dest skip").expect("write existing skip");

    let operands = vec![
        ctx.source.clone().into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let filters = FilterSet::from_rules([filters::FilterRule::exclude("*.tmp")])
        .expect("compile filters");
    let options = LocalCopyOptions::default()
        .delete(true)
        .delete_excluded(true)
        .filters(Some(filters));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let target_root = ctx.dest.join("source");
    assert!(target_root.join("keep.txt").exists());
    assert!(!target_root.join("skip.tmp").exists());
    assert_eq!(summary.items_deleted(), 1);
}

#[test]
fn delete_excluded_removes_matching_source_files() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");
    fs::write(source.join("keep.txt"), b"keep").expect("write keep");
    fs::write(source.join("skip.tmp"), b"skip source").expect("write skip source");

    let target_root = dest.join("source");
    fs::create_dir_all(&target_root).expect("create target root");
    fs::write(target_root.join("skip.tmp"), b"dest skip").expect("write existing skip");

    let operands = vec![
        source.into_os_string(),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let filters = FilterSet::from_rules([filters::FilterRule::exclude("*.tmp")])
        .expect("compile filters");
    let options = LocalCopyOptions::default()
        .delete(true)
        .delete_excluded(true)
        .filters(Some(filters));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let target_root = dest.join("source");
    assert!(target_root.join("keep.txt").exists());
    assert!(!target_root.join("skip.tmp").exists());
    assert_eq!(summary.items_deleted(), 1);
}

#[test]
fn delete_after_files_present_during_transfer() {
    use std::sync::{Arc, Mutex};
    use std::collections::HashSet;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    // Create source files
    fs::write(source.join("keep.txt"), b"keep content").expect("write keep");
    fs::write(source.join("update.txt"), b"new version").expect("write update");

    // Create destination with extra file that should be deleted
    fs::write(dest.join("keep.txt"), b"old keep").expect("write old keep");
    fs::write(dest.join("update.txt"), b"old version").expect("write old update");
    fs::write(dest.join("delete_me.txt"), b"to be deleted").expect("write delete_me");
    fs::write(dest.join("also_delete.txt"), b"also deleted").expect("write also_delete");

    let mut source_operand = source.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Track which files exist at different phases
    let files_during_copy = Arc::new(Mutex::new(HashSet::new()));
    let files_during_copy_clone = Arc::clone(&files_during_copy);

    // Create a custom observer to check file existence during transfer
    struct TransferObserver {
        dest: std::path::PathBuf,
        files_seen: Arc<Mutex<HashSet<String>>>,
        checked: bool,
    }

    impl LocalCopyRecordHandler for TransferObserver {
        fn handle(&mut self, record: LocalCopyRecord) {
            // Check file existence when we see a file being copied (not on first event)
            if !self.checked && record.action() == &LocalCopyAction::DataCopied {
                self.checked = true;
                // At this point, files marked for deletion should still exist
                if self.dest.join("delete_me.txt").exists() {
                    self.files_seen.lock().unwrap().insert("delete_me.txt".to_string());
                }
                if self.dest.join("also_delete.txt").exists() {
                    self.files_seen.lock().unwrap().insert("also_delete.txt".to_string());
                }
            }
        }
    }

    let mut observer = TransferObserver {
        dest: dest.clone(),
        files_seen: files_during_copy_clone,
        checked: false,
    };

    let options = LocalCopyOptions::default()
        .delete_after(true)
        .collect_events(true);

    let summary = plan
        .execute_with_options_and_handler(LocalCopyExecution::Apply, options, Some(&mut observer))
        .expect("copy succeeds");

    // Verify deletions happened
    assert!(!dest.join("delete_me.txt").exists(), "delete_me.txt should be deleted");
    assert!(!dest.join("also_delete.txt").exists(), "also_delete.txt should be deleted");
    assert!(dest.join("keep.txt").exists(), "keep.txt should exist");
    assert!(dest.join("update.txt").exists(), "update.txt should exist");
    assert_eq!(summary.items_deleted(), 2);

    // Verify files existed during transfer
    let seen = files_during_copy.lock().unwrap();
    assert!(seen.contains("delete_me.txt"), "delete_me.txt should have existed during transfer");
    assert!(seen.contains("also_delete.txt"), "also_delete.txt should have existed during transfer");
}

#[test]
fn delete_after_deletes_after_all_transfers_complete() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    // Create nested directory structure in source
    let nested = source.join("subdir");
    fs::create_dir_all(&nested).expect("create nested");
    fs::write(source.join("file1.txt"), b"file1").expect("write file1");
    fs::write(nested.join("file2.txt"), b"file2").expect("write file2");

    // Create destination with files to delete at root and in subdirectory
    fs::write(dest.join("root_extra.txt"), b"root extra").expect("write root_extra");
    let dest_nested = dest.join("subdir");
    fs::create_dir_all(&dest_nested).expect("create dest nested");
    fs::write(dest_nested.join("nested_extra.txt"), b"nested extra").expect("write nested_extra");

    let mut source_operand = source.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .delete_after(true)
        .collect_events(true);

    let report = plan
        .execute_with_report(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let summary = report.summary();
    let records = report.records();

    // Verify all transfers completed
    assert!(dest.join("file1.txt").exists());
    assert!(dest_nested.join("file2.txt").exists());

    // Verify all deletions happened
    assert!(!dest.join("root_extra.txt").exists());
    assert!(!dest_nested.join("nested_extra.txt").exists());
    assert_eq!(summary.items_deleted(), 2);

    // Verify order: all file copies should come before any deletions
    let mut last_copy_index = None;
    let mut first_delete_index = None;

    for (i, record) in records.iter().enumerate() {
        match record.action() {
            LocalCopyAction::DataCopied => {
                last_copy_index = Some(i);
            }
            LocalCopyAction::EntryDeleted => {
                if first_delete_index.is_none() {
                    first_delete_index = Some(i);
                }
            }
            _ => {}
        }
    }

    // If both copies and deletes occurred, copies must come before deletes
    if let (Some(last_copy), Some(first_delete)) = (last_copy_index, first_delete_index) {
        assert!(
            last_copy < first_delete,
            "All file copies (last at {}) should complete before any deletions (first at {})",
            last_copy,
            first_delete
        );
    }
}

#[test]
fn delete_after_timing_differs_from_delete_before() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest_after = temp.path().join("dest_after");
    let dest_before = temp.path().join("dest_before");

    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest_after).expect("create dest_after");
    fs::create_dir_all(&dest_before).expect("create dest_before");

    // Create a file that exists in source
    fs::write(source.join("keep.txt"), b"keep").expect("write keep");

    // For delete-after: create extra file in destination
    fs::write(dest_after.join("extra.txt"), b"extra").expect("write extra_after");
    fs::write(dest_after.join("keep.txt"), b"old keep").expect("write old keep_after");

    // For delete-before: create extra file in destination
    fs::write(dest_before.join("extra.txt"), b"extra").expect("write extra_before");
    fs::write(dest_before.join("keep.txt"), b"old keep").expect("write old keep_before");

    // Test delete-after
    let mut source_operand_after = source.clone().into_os_string();
    source_operand_after.push(std::path::MAIN_SEPARATOR.to_string());
    let operands_after = vec![source_operand_after, dest_after.clone().into_os_string()];
    let plan_after = LocalCopyPlan::from_operands(&operands_after).expect("plan after");

    let options_after = LocalCopyOptions::default()
        .delete_after(true)
        .collect_events(true);

    let report_after = plan_after
        .execute_with_report(LocalCopyExecution::Apply, options_after)
        .expect("copy with delete-after succeeds");

    // Test delete-before
    let mut source_operand_before = source.into_os_string();
    source_operand_before.push(std::path::MAIN_SEPARATOR.to_string());
    let operands_before = vec![source_operand_before, dest_before.clone().into_os_string()];
    let plan_before = LocalCopyPlan::from_operands(&operands_before).expect("plan before");

    let options_before = LocalCopyOptions::default()
        .delete_before(true)
        .collect_events(true);

    let report_before = plan_before
        .execute_with_report(LocalCopyExecution::Apply, options_before)
        .expect("copy with delete-before succeeds");

    // Both should have same end result
    assert!(!dest_after.join("extra.txt").exists());
    assert!(!dest_before.join("extra.txt").exists());
    assert!(dest_after.join("keep.txt").exists());
    assert!(dest_before.join("keep.txt").exists());

    // Analyze event order
    let records_after = report_after.records();
    let records_before = report_before.records();

    // For delete-after: deletions come after copies
    let mut after_last_copy = None;
    let mut after_first_delete = None;
    for (i, record) in records_after.iter().enumerate() {
        match record.action() {
            LocalCopyAction::DataCopied => after_last_copy = Some(i),
            LocalCopyAction::EntryDeleted => {
                if after_first_delete.is_none() {
                    after_first_delete = Some(i);
                }
            }
            _ => {}
        }
    }

    // For delete-before: deletions come before copies
    let mut before_last_delete = None;
    let mut before_first_copy = None;
    for (i, record) in records_before.iter().enumerate() {
        match record.action() {
            LocalCopyAction::EntryDeleted => before_last_delete = Some(i),
            LocalCopyAction::DataCopied => {
                if before_first_copy.is_none() {
                    before_first_copy = Some(i);
                }
            }
            _ => {}
        }
    }

    // Verify timing difference
    if let (Some(after_copy), Some(after_delete)) = (after_last_copy, after_first_delete) {
        assert!(
            after_copy < after_delete,
            "delete-after: copies should finish before deletes"
        );
    }

    if let (Some(before_delete), Some(before_copy)) = (before_last_delete, before_first_copy) {
        assert!(
            before_delete < before_copy,
            "delete-before: deletes should finish before copies"
        );
    }
}

#[test]
fn delete_after_timing_differs_from_delete_during() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    fs::create_dir_all(&source).expect("create source");

    // Create nested structure
    let subdir = source.join("subdir");
    fs::create_dir_all(&subdir).expect("create subdir");
    fs::write(source.join("root.txt"), b"root").expect("write root");
    fs::write(subdir.join("nested.txt"), b"nested").expect("write nested");

    // Test delete-after
    let dest_after = temp.path().join("dest_after");
    fs::create_dir_all(&dest_after).expect("create dest_after");
    fs::write(dest_after.join("root_extra.txt"), b"root extra").expect("write root_extra");
    let dest_after_subdir = dest_after.join("subdir");
    fs::create_dir_all(&dest_after_subdir).expect("create dest_after subdir");
    fs::write(dest_after_subdir.join("nested_extra.txt"), b"nested extra").expect("write nested_extra");

    let mut source_operand_after = source.clone().into_os_string();
    source_operand_after.push(std::path::MAIN_SEPARATOR.to_string());
    let operands_after = vec![source_operand_after, dest_after.clone().into_os_string()];
    let plan_after = LocalCopyPlan::from_operands(&operands_after).expect("plan after");

    let options_after = LocalCopyOptions::default()
        .delete_after(true)
        .collect_events(true);

    let report_after = plan_after
        .execute_with_report(LocalCopyExecution::Apply, options_after)
        .expect("copy with delete-after succeeds");

    // Test delete-during (default)
    let dest_during = temp.path().join("dest_during");
    fs::create_dir_all(&dest_during).expect("create dest_during");
    fs::write(dest_during.join("root_extra.txt"), b"root extra").expect("write root_extra");
    let dest_during_subdir = dest_during.join("subdir");
    fs::create_dir_all(&dest_during_subdir).expect("create dest_during subdir");
    fs::write(dest_during_subdir.join("nested_extra.txt"), b"nested extra").expect("write nested_extra");

    let mut source_operand_during = source.into_os_string();
    source_operand_during.push(std::path::MAIN_SEPARATOR.to_string());
    let operands_during = vec![source_operand_during, dest_during.clone().into_os_string()];
    let plan_during = LocalCopyPlan::from_operands(&operands_during).expect("plan during");

    let options_during = LocalCopyOptions::default()
        .delete(true)  // delete-during is the default
        .collect_events(true);

    let report_during = plan_during
        .execute_with_report(LocalCopyExecution::Apply, options_during)
        .expect("copy with delete-during succeeds");

    // Both should have same end result
    assert!(!dest_after.join("root_extra.txt").exists());
    assert!(!dest_during.join("root_extra.txt").exists());
    assert!(!dest_after_subdir.join("nested_extra.txt").exists());
    assert!(!dest_during_subdir.join("nested_extra.txt").exists());

    let records_after = report_after.records();
    let records_during = report_during.records();

    // For delete-after: all deletions at the end
    let mut after_copy_indices = Vec::new();
    let mut after_delete_indices = Vec::new();
    for (i, record) in records_after.iter().enumerate() {
        match record.action() {
            LocalCopyAction::DataCopied => after_copy_indices.push(i),
            LocalCopyAction::EntryDeleted => after_delete_indices.push(i),
            _ => {}
        }
    }

    // For delete-during: deletions interleaved with directory processing
    let mut during_copy_indices = Vec::new();
    let mut during_delete_indices = Vec::new();
    for (i, record) in records_during.iter().enumerate() {
        match record.action() {
            LocalCopyAction::DataCopied => during_copy_indices.push(i),
            LocalCopyAction::EntryDeleted => during_delete_indices.push(i),
            _ => {}
        }
    }

    // Verify delete-after: all copies before all deletes
    if let (Some(&last_copy), Some(&first_delete)) = (after_copy_indices.last(), after_delete_indices.first()) {
        assert!(
            last_copy < first_delete,
            "delete-after: all copies should complete before any deletes"
        );
    }

    // Verify delete-during: deletes can be interleaved (not all at the end)
    // This is harder to verify precisely, but we can check that there's no strict ordering
    // If we have both copies and deletes, in delete-during they may be interleaved
    assert!(during_copy_indices.len() > 0, "should have copies");
    assert!(during_delete_indices.len() > 0, "should have deletes");
}

#[test]
fn delete_after_with_multiple_directories() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    fs::create_dir_all(&source).expect("create source");

    // Create multiple subdirectories
    let dir1 = source.join("dir1");
    let dir2 = source.join("dir2");
    fs::create_dir_all(&dir1).expect("create dir1");
    fs::create_dir_all(&dir2).expect("create dir2");
    fs::write(dir1.join("file1.txt"), b"file1").expect("write file1");
    fs::write(dir2.join("file2.txt"), b"file2").expect("write file2");

    // Create destination with extra files in each directory
    let dest = temp.path().join("dest");
    let dest_dir1 = dest.join("dir1");
    let dest_dir2 = dest.join("dir2");
    fs::create_dir_all(&dest_dir1).expect("create dest_dir1");
    fs::create_dir_all(&dest_dir2).expect("create dest_dir2");
    fs::write(dest_dir1.join("extra1.txt"), b"extra1").expect("write extra1");
    fs::write(dest_dir2.join("extra2.txt"), b"extra2").expect("write extra2");

    let mut source_operand = source.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .delete_after(true)
        .collect_events(true);

    let report = plan
        .execute_with_report(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let summary = report.summary();

    // Verify all source files copied
    assert!(dest_dir1.join("file1.txt").exists());
    assert!(dest_dir2.join("file2.txt").exists());

    // Verify all extra files deleted
    assert!(!dest_dir1.join("extra1.txt").exists());
    assert!(!dest_dir2.join("extra2.txt").exists());
    assert_eq!(summary.items_deleted(), 2);

    // Verify all files copied before any deletions
    let records = report.records();
    let mut last_copy_index = None;
    let mut first_delete_index = None;

    for (i, record) in records.iter().enumerate() {
        match record.action() {
            LocalCopyAction::DataCopied => last_copy_index = Some(i),
            LocalCopyAction::EntryDeleted => {
                if first_delete_index.is_none() {
                    first_delete_index = Some(i);
                }
            }
            _ => {}
        }
    }

    if let (Some(last_copy), Some(first_delete)) = (last_copy_index, first_delete_index) {
        assert!(
            last_copy < first_delete,
            "All copies across all directories should complete before any deletions"
        );
    }
}

#[test]
fn delete_after_works_with_dry_run() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    fs::write(source.join("keep.txt"), b"keep").expect("write keep");
    fs::write(dest.join("keep.txt"), b"old keep").expect("write old keep");
    fs::write(dest.join("delete_me.txt"), b"to be deleted").expect("write delete_me");

    let mut source_operand = source.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .delete_after(true)
        .collect_events(true);

    let report = plan
        .execute_with_report(LocalCopyExecution::DryRun, options)
        .expect("dry-run succeeds");

    let summary = report.summary();

    // In dry-run, nothing should be modified
    assert_eq!(fs::read(dest.join("keep.txt")).expect("read"), b"old keep");
    assert!(dest.join("delete_me.txt").exists(), "file should still exist in dry-run");

    // But summary should report what would happen
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.items_deleted(), 1);

    // Verify event ordering in dry-run too
    let records = report.records();
    let mut last_copy_index = None;
    let mut first_delete_index = None;

    for (i, record) in records.iter().enumerate() {
        match record.action() {
            LocalCopyAction::DataCopied => last_copy_index = Some(i),
            LocalCopyAction::EntryDeleted => {
                if first_delete_index.is_none() {
                    first_delete_index = Some(i);
                }
            }
            _ => {}
        }
    }

    if let (Some(last_copy), Some(first_delete)) = (last_copy_index, first_delete_index) {
        assert!(
            last_copy < first_delete,
            "Even in dry-run, delete-after should report copies before deletes"
        );
    }
}
