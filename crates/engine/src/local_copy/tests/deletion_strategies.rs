//! Integration tests for deletion strategies.
//!
//! This module tests the complete deletion behavior at the integration level,
//! verifying that delete-before, delete-during, delete-after, and delete-delay
//! all work correctly with various options.

use super::*;

/// Tests that --delete-before removes extraneous files before transfer begins.
#[test]
fn delete_before_removes_files_before_transfer() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    // Create source files
    fs::write(source.join("keep.txt"), b"keep").expect("write keep");

    // Create destination with extra files (no overlap to avoid confusion about deletions)
    fs::write(dest.join("delete1.txt"), b"delete").expect("write delete1");
    fs::write(dest.join("delete2.txt"), b"delete").expect("write delete2");

    let mut source_operand = source.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().delete_before(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Verify extraneous files were deleted
    assert!(!dest.join("delete1.txt").exists());
    assert!(!dest.join("delete2.txt").exists());
    assert!(dest.join("keep.txt").exists());
    assert_eq!(summary.items_deleted(), 2);
}

/// Tests that --delete-during removes files as directories are processed.
#[test]
fn delete_during_removes_files_incrementally() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    // Create nested directory structure
    fs::create_dir_all(source.join("subdir")).expect("create subdir");
    fs::write(source.join("keep.txt"), b"keep").expect("write keep");
    fs::write(source.join("subdir").join("nested.txt"), b"nested").expect("write nested");

    // Create destination with extra files in root and subdirectory
    fs::create_dir_all(dest.join("subdir")).expect("create dest subdir");
    fs::write(dest.join("keep.txt"), b"old").expect("write old keep");
    fs::write(dest.join("delete_root.txt"), b"delete").expect("write delete_root");
    fs::write(dest.join("subdir").join("nested.txt"), b"old").expect("write old nested");
    fs::write(dest.join("subdir").join("delete_nested.txt"), b"delete")
        .expect("write delete_nested");

    let mut source_operand = source.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().delete_during();

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Verify files were deleted in both root and subdirectory
    assert!(!dest.join("delete_root.txt").exists());
    assert!(!dest.join("subdir").join("delete_nested.txt").exists());
    assert!(dest.join("keep.txt").exists());
    assert!(dest.join("subdir").join("nested.txt").exists());
    assert_eq!(summary.items_deleted(), 2);
}

/// Tests that --delete-after preserves files during transfer.
#[test]
fn delete_after_preserves_files_during_transfer() {
    use std::collections::HashSet;
    use std::sync::{Arc, Mutex};

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    fs::write(source.join("keep.txt"), b"keep").expect("write keep");
    fs::write(dest.join("keep.txt"), b"old").expect("write old keep");
    fs::write(dest.join("delete_me.txt"), b"delete").expect("write delete");

    let files_during_copy = Arc::new(Mutex::new(HashSet::new()));
    let files_during_copy_clone = Arc::clone(&files_during_copy);

    struct TransferObserver {
        dest: PathBuf,
        files_seen: Arc<Mutex<HashSet<String>>>,
        checked: bool,
    }

    impl LocalCopyRecordHandler for TransferObserver {
        fn handle(&mut self, record: LocalCopyRecord) {
            if !self.checked && record.action() == &LocalCopyAction::DataCopied {
                self.checked = true;
                if self.dest.join("delete_me.txt").exists() {
                    self.files_seen
                        .lock()
                        .unwrap()
                        .insert("delete_me.txt".to_string());
                }
            }
        }
    }

    let mut observer = TransferObserver {
        dest: dest.clone(),
        files_seen: files_during_copy_clone,
        checked: false,
    };

    let mut source_operand = source.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .delete_after(true)
        .collect_events(true);

    let summary = plan
        .execute_with_options_and_handler(LocalCopyExecution::Apply, options, Some(&mut observer))
        .expect("copy succeeds");

    // Verify file existed during transfer
    let seen = files_during_copy.lock().unwrap();
    assert!(
        seen.contains("delete_me.txt"),
        "delete_me.txt should have existed during transfer"
    );

    // But was deleted after
    assert!(!dest.join("delete_me.txt").exists());
    assert_eq!(summary.items_deleted(), 1);
}

/// Tests that --delete-delay accumulates deletions and applies them after transfer.
#[test]
fn delete_delay_defers_deletion_until_end() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    fs::create_dir_all(source.join("dir1")).expect("create dir1");
    fs::create_dir_all(source.join("dir2")).expect("create dir2");
    fs::write(source.join("dir1").join("keep1.txt"), b"keep1").expect("write keep1");
    fs::write(source.join("dir2").join("keep2.txt"), b"keep2").expect("write keep2");

    fs::create_dir_all(dest.join("dir1")).expect("create dest dir1");
    fs::create_dir_all(dest.join("dir2")).expect("create dest dir2");
    fs::write(dest.join("dir1").join("keep1.txt"), b"old1").expect("write old1");
    fs::write(dest.join("dir1").join("delete1.txt"), b"delete1").expect("write delete1");
    fs::write(dest.join("dir2").join("keep2.txt"), b"old2").expect("write old2");
    fs::write(dest.join("dir2").join("delete2.txt"), b"delete2").expect("write delete2");

    let mut source_operand = source.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().delete_delay(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Verify all deletions happened
    assert!(!dest.join("dir1").join("delete1.txt").exists());
    assert!(!dest.join("dir2").join("delete2.txt").exists());
    assert!(dest.join("dir1").join("keep1.txt").exists());
    assert!(dest.join("dir2").join("keep2.txt").exists());
    assert_eq!(summary.items_deleted(), 2);
}

/// Tests that deletion respects filter rules by default.
#[test]
fn deletion_respects_filter_rules() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    fs::write(source.join("keep.txt"), b"keep").expect("write keep");

    // Destination has files that would be deleted, plus excluded files
    fs::write(dest.join("keep.txt"), b"old").expect("write old");
    fs::write(dest.join("delete_me.txt"), b"delete").expect("write delete");
    fs::write(dest.join("skip.tmp"), b"excluded").expect("write excluded");

    let mut source_operand = source.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let filters = FilterSet::from_rules([FilterRule::exclude("*.tmp")]).expect("filters");
    let options = LocalCopyOptions::default()
        .delete(true)
        .filters(Some(filters));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Extraneous file deleted
    assert!(!dest.join("delete_me.txt").exists());
    // Excluded file preserved (not deleted unless --delete-excluded)
    assert!(dest.join("skip.tmp").exists());
    assert_eq!(summary.items_deleted(), 1);
}

/// Tests that --delete-excluded removes filtered files.
#[test]
fn delete_excluded_removes_filtered_files() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    fs::write(source.join("keep.txt"), b"keep").expect("write keep");
    // Source also has excluded file (should not be copied)
    fs::write(source.join("source.tmp"), b"source excluded").expect("write source excluded");

    fs::write(dest.join("keep.txt"), b"old").expect("write old");
    fs::write(dest.join("delete.txt"), b"delete").expect("write delete");
    fs::write(dest.join("dest.tmp"), b"dest excluded").expect("write dest excluded");

    let mut source_operand = source.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let filters = FilterSet::from_rules([FilterRule::exclude("*.tmp")]).expect("filters");
    let options = LocalCopyOptions::default()
        .delete(true)
        .delete_excluded(true)
        .filters(Some(filters));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Both extraneous and excluded files deleted
    assert!(!dest.join("delete.txt").exists());
    assert!(!dest.join("dest.tmp").exists());
    assert!(dest.join("keep.txt").exists());
    // 2 files deleted: delete.txt and dest.tmp
    assert_eq!(summary.items_deleted(), 2);
}

/// Tests that --max-delete limits the number of deletions.
#[test]
fn max_delete_enforces_limit() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    fs::write(source.join("keep.txt"), b"keep").expect("write keep");

    // Create many files to delete
    fs::write(dest.join("keep.txt"), b"old").expect("write old");
    for i in 1..=10 {
        fs::write(dest.join(format!("delete{i}.txt")), b"delete").expect("write delete file");
    }

    let mut source_operand = source.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .delete(true)
        .max_deletions(Some(5));

    let result = plan.execute_with_options(LocalCopyExecution::Apply, options);

    // Should fail because limit was exceeded
    assert!(
        result.is_err(),
        "Expected error when deletion limit exceeded"
    );
    let err = result.unwrap_err();
    let err_str = format!("{err:?}");
    // Check for deletion/limit related error message
    let has_delete_error = err_str.to_lowercase().contains("delet")
        || err_str.to_lowercase().contains("limit")
        || err_str.to_lowercase().contains("exceed");
    assert!(
        has_delete_error,
        "Error should mention deletion limit: {err_str}"
    );
}

/// Tests that --max-delete=0 prevents all deletions.
#[test]
fn max_delete_zero_prevents_deletion() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    fs::write(source.join("keep.txt"), b"keep").expect("write keep");
    fs::write(dest.join("keep.txt"), b"old").expect("write old");
    fs::write(dest.join("delete.txt"), b"delete").expect("write delete");

    let mut source_operand = source.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .delete(true)
        .max_deletions(Some(0));

    let result = plan.execute_with_options(LocalCopyExecution::Apply, options);

    // Should fail because even one deletion exceeds limit of 0
    assert!(result.is_err());
}

/// Tests that deletion works in dry-run mode.
#[test]
fn deletion_in_dry_run_mode() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    fs::write(source.join("keep.txt"), b"keep").expect("write keep");
    fs::write(dest.join("keep.txt"), b"old").expect("write old");
    fs::write(dest.join("delete_me.txt"), b"delete").expect("write delete");

    let mut source_operand = source.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().delete(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::DryRun, options)
        .expect("dry run succeeds");

    // File should still exist (dry-run doesn't delete)
    assert!(dest.join("delete_me.txt").exists());
    // But summary should report it would be deleted
    assert_eq!(summary.items_deleted(), 1);
}

/// Tests that directories are deleted recursively.
#[test]
fn deletion_removes_directories_recursively() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    fs::write(source.join("keep.txt"), b"keep").expect("write keep");

    // Create a directory with nested content to delete
    fs::create_dir_all(dest.join("delete_dir").join("nested")).expect("create nested");
    fs::write(dest.join("delete_dir").join("file.txt"), b"file").expect("write file");
    fs::write(
        dest.join("delete_dir").join("nested").join("deep.txt"),
        b"deep",
    )
    .expect("write deep");
    fs::write(dest.join("keep.txt"), b"old").expect("write old");

    let mut source_operand = source.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().delete(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Directory and all contents should be deleted
    assert!(!dest.join("delete_dir").exists());
    assert!(dest.join("keep.txt").exists());
    assert_eq!(summary.items_deleted(), 1);
}

/// Tests deletion with multiple sources.
#[test]
fn deletion_with_multiple_sources() {
    let temp = tempdir().expect("tempdir");
    let source1 = temp.path().join("source1");
    let source2 = temp.path().join("source2");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source1).expect("create source1");
    fs::create_dir_all(&source2).expect("create source2");
    fs::create_dir_all(&dest).expect("create dest");

    fs::write(source1.join("file1.txt"), b"file1").expect("write file1");
    fs::write(source2.join("file2.txt"), b"file2").expect("write file2");

    // Destination should merge both sources
    fs::create_dir_all(dest.join("source1")).expect("create dest source1");
    fs::create_dir_all(dest.join("source2")).expect("create dest source2");
    fs::write(dest.join("source1").join("file1.txt"), b"old1").expect("write old1");
    fs::write(dest.join("source1").join("delete1.txt"), b"delete1").expect("write delete1");
    fs::write(dest.join("source2").join("file2.txt"), b"old2").expect("write old2");
    fs::write(dest.join("source2").join("delete2.txt"), b"delete2").expect("write delete2");

    let operands = vec![
        source1.into_os_string(),
        source2.into_os_string(),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().delete(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Extraneous files in both source directories should be deleted
    assert!(!dest.join("source1").join("delete1.txt").exists());
    assert!(!dest.join("source2").join("delete2.txt").exists());
    assert!(dest.join("source1").join("file1.txt").exists());
    assert!(dest.join("source2").join("file2.txt").exists());
    assert_eq!(summary.items_deleted(), 2);
}
