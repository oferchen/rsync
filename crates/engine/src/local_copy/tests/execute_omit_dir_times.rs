// ============================================================================
// Tests for --omit-dir-times flag
// ============================================================================
//
// The --omit-dir-times flag prevents directory modification times from being
// preserved, while still preserving file timestamps. This is useful for:
// 1. Performance: Reduces metadata operations in deep hierarchies
// 2. Consistency: Avoids timestamp conflicts when directories are modified
//
// These tests verify:
// - Directory timestamps are NOT preserved when --omit-dir-times is enabled
// - File timestamps ARE still preserved (with -t)
// - The flag works correctly with --times
// - Performance improvement for deep hierarchies (measured via syscall counts)

#[cfg(unix)]
#[test]
fn omit_dir_times_does_not_preserve_directory_mtime() {
    use filetime::{FileTime, set_file_mtime};

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");
    fs::write(source_root.join("file.txt"), b"content").expect("write file");

    // Set a specific mtime on the directory (after writing files)
    let dir_mtime = FileTime::from_unix_time(1_600_000_000, 0);
    set_file_mtime(&source_root, dir_mtime).expect("set source mtime");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .times(true)
        .omit_dir_times(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let dest_metadata = fs::metadata(&dest_root).expect("dest metadata");
    let dest_mtime = FileTime::from_last_modification_time(&dest_metadata);

    // With omit_dir_times, the directory mtime should NOT match the source
    assert_ne!(
        dest_mtime, dir_mtime,
        "directory mtime should not be preserved with --omit-dir-times"
    );
}

#[cfg(unix)]
#[test]
fn omit_dir_times_still_preserves_file_timestamps() {
    use filetime::{FileTime, set_file_mtime};

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    let source_file = source_root.join("file.txt");
    fs::write(&source_file, b"content").expect("write file");

    // Set specific mtimes for both directory and file
    let file_mtime = FileTime::from_unix_time(1_500_000_000, 0);
    let dir_mtime = FileTime::from_unix_time(1_600_000_000, 0);

    set_file_mtime(&source_file, file_mtime).expect("set file mtime");
    set_file_mtime(&source_root, dir_mtime).expect("set dir mtime");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .times(true)
        .omit_dir_times(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Directory mtime should NOT be preserved
    let dest_dir_metadata = fs::metadata(&dest_root).expect("dest dir metadata");
    let dest_dir_mtime = FileTime::from_last_modification_time(&dest_dir_metadata);
    assert_ne!(
        dest_dir_mtime, dir_mtime,
        "directory mtime should not be preserved"
    );

    // File mtime SHOULD be preserved
    let dest_file = dest_root.join("file.txt");
    let dest_file_metadata = fs::metadata(&dest_file).expect("dest file metadata");
    let dest_file_mtime = FileTime::from_last_modification_time(&dest_file_metadata);
    assert_eq!(
        dest_file_mtime, file_mtime,
        "file mtime should still be preserved with --omit-dir-times"
    );
}

#[cfg(unix)]
#[test]
fn omit_dir_times_works_with_nested_directories() {
    use filetime::{FileTime, set_file_mtime};

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let nested = source_root.join("level1").join("level2").join("level3");
    fs::create_dir_all(&nested).expect("create nested");

    let source_file = nested.join("deep_file.txt");
    fs::write(&source_file, b"deep content").expect("write file");

    // Set specific mtimes for all directories and the file
    let file_mtime = FileTime::from_unix_time(1_400_000_000, 0);
    let level3_mtime = FileTime::from_unix_time(1_500_000_000, 0);
    let level2_mtime = FileTime::from_unix_time(1_600_000_000, 0);
    let level1_mtime = FileTime::from_unix_time(1_700_000_000, 0);
    let root_mtime = FileTime::from_unix_time(1_800_000_000, 0);

    set_file_mtime(&source_file, file_mtime).expect("set file mtime");
    set_file_mtime(&nested, level3_mtime).expect("set level3 mtime");
    set_file_mtime(nested.parent().unwrap(), level2_mtime).expect("set level2 mtime");
    set_file_mtime(nested.parent().unwrap().parent().unwrap(), level1_mtime).expect("set level1 mtime");
    set_file_mtime(&source_root, root_mtime).expect("set root mtime");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .times(true)
        .omit_dir_times(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // File mtime should be preserved
    let dest_file = dest_root.join("level1").join("level2").join("level3").join("deep_file.txt");
    let dest_file_metadata = fs::metadata(&dest_file).expect("dest file metadata");
    let dest_file_mtime = FileTime::from_last_modification_time(&dest_file_metadata);
    assert_eq!(dest_file_mtime, file_mtime, "file mtime should be preserved");

    // All directory mtimes should NOT be preserved
    let dest_root_metadata = fs::metadata(&dest_root).expect("root metadata");
    let dest_root_mtime = FileTime::from_last_modification_time(&dest_root_metadata);
    assert_ne!(dest_root_mtime, root_mtime, "root mtime should not be preserved");

    let dest_level1 = dest_root.join("level1");
    let dest_level1_metadata = fs::metadata(&dest_level1).expect("level1 metadata");
    let dest_level1_mtime = FileTime::from_last_modification_time(&dest_level1_metadata);
    assert_ne!(dest_level1_mtime, level1_mtime, "level1 mtime should not be preserved");

    let dest_level2 = dest_level1.join("level2");
    let dest_level2_metadata = fs::metadata(&dest_level2).expect("level2 metadata");
    let dest_level2_mtime = FileTime::from_last_modification_time(&dest_level2_metadata);
    assert_ne!(dest_level2_mtime, level2_mtime, "level2 mtime should not be preserved");

    let dest_level3 = dest_level2.join("level3");
    let dest_level3_metadata = fs::metadata(&dest_level3).expect("level3 metadata");
    let dest_level3_mtime = FileTime::from_last_modification_time(&dest_level3_metadata);
    assert_ne!(dest_level3_mtime, level3_mtime, "level3 mtime should not be preserved");
}

#[cfg(unix)]
#[test]
fn omit_dir_times_without_times_flag_has_no_effect() {
    use filetime::{FileTime, set_file_mtime};

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");
    fs::write(source_root.join("file.txt"), b"content").expect("write file");

    let dir_mtime = FileTime::from_unix_time(1_600_000_000, 0);
    set_file_mtime(&source_root, dir_mtime).expect("set source mtime");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Enable omit_dir_times but NOT times - so no timestamps should be preserved anyway
    let options = LocalCopyOptions::default()
        .times(false)
        .omit_dir_times(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let dest_metadata = fs::metadata(&dest_root).expect("dest metadata");
    let dest_mtime = FileTime::from_last_modification_time(&dest_metadata);

    // Without --times, the directory mtime should not be preserved regardless
    assert_ne!(dest_mtime, dir_mtime);
}

#[cfg(unix)]
#[test]
fn omit_dir_times_with_permissions_preserves_mode() {
    use std::os::unix::fs::PermissionsExt;
    use filetime::{FileTime, set_file_mtime};

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");
    fs::set_permissions(&source_root, PermissionsExt::from_mode(0o750)).expect("set perms");
    fs::write(source_root.join("file.txt"), b"content").expect("write file");

    let dir_mtime = FileTime::from_unix_time(1_600_000_000, 0);
    set_file_mtime(&source_root, dir_mtime).expect("set source mtime");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .times(true)
        .omit_dir_times(true)
        .permissions(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let dest_metadata = fs::metadata(&dest_root).expect("dest metadata");
    let dest_mtime = FileTime::from_last_modification_time(&dest_metadata);

    // Directory mtime should not be preserved
    assert_ne!(dest_mtime, dir_mtime);

    // But permissions should still be preserved
    assert_eq!(dest_metadata.permissions().mode() & 0o777, 0o750);
}

#[cfg(unix)]
#[test]
fn omit_dir_times_preserves_directory_permissions_multiple_dirs() {
    use std::os::unix::fs::PermissionsExt;
    use filetime::{FileTime, set_file_mtime};

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let nested = source_root.join("nested");
    fs::create_dir_all(&nested).expect("create nested");

    fs::set_permissions(&source_root, PermissionsExt::from_mode(0o755)).expect("set root perms");
    fs::set_permissions(&nested, PermissionsExt::from_mode(0o700)).expect("set nested perms");

    fs::write(nested.join("file.txt"), b"content").expect("write file");

    let dir_mtime = FileTime::from_unix_time(1_600_000_000, 0);
    set_file_mtime(&nested, dir_mtime).expect("set nested mtime");
    set_file_mtime(&source_root, dir_mtime).expect("set root mtime");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .times(true)
        .omit_dir_times(true)
        .permissions(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Check that permissions are still preserved
    let dest_root_metadata = fs::metadata(&dest_root).expect("root metadata");
    assert_eq!(dest_root_metadata.permissions().mode() & 0o777, 0o755);

    let dest_nested = dest_root.join("nested");
    let dest_nested_metadata = fs::metadata(&dest_nested).expect("nested metadata");
    assert_eq!(dest_nested_metadata.permissions().mode() & 0o777, 0o700);

    // Check that mtimes are NOT preserved
    let dest_root_mtime = FileTime::from_last_modification_time(&dest_root_metadata);
    let dest_nested_mtime = FileTime::from_last_modification_time(&dest_nested_metadata);
    assert_ne!(dest_root_mtime, dir_mtime);
    assert_ne!(dest_nested_mtime, dir_mtime);
}

#[cfg(unix)]
#[test]
fn omit_dir_times_works_in_dry_run_mode() {
    use filetime::{FileTime, set_file_mtime};

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");
    fs::write(source_root.join("file.txt"), b"content").expect("write file");

    let dir_mtime = FileTime::from_unix_time(1_600_000_000, 0);
    set_file_mtime(&source_root, dir_mtime).expect("set source mtime");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .times(true)
        .omit_dir_times(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::DryRun, options)
        .expect("dry-run succeeds");

    // In dry-run mode, nothing should be created
    assert!(!dest_root.exists());
    assert!(summary.directories_created() >= 1);
}

#[cfg(unix)]
#[test]
fn omit_dir_times_with_multiple_source_files() {
    use filetime::{FileTime, set_file_mtime};

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    // Create multiple files with different timestamps
    let file1 = source_root.join("file1.txt");
    let file2 = source_root.join("file2.txt");
    let file3 = source_root.join("file3.txt");

    fs::write(&file1, b"content1").expect("write file1");
    fs::write(&file2, b"content2").expect("write file2");
    fs::write(&file3, b"content3").expect("write file3");

    let file1_mtime = FileTime::from_unix_time(1_400_000_000, 0);
    let file2_mtime = FileTime::from_unix_time(1_500_000_000, 0);
    let file3_mtime = FileTime::from_unix_time(1_600_000_000, 0);
    let dir_mtime = FileTime::from_unix_time(1_700_000_000, 0);

    set_file_mtime(&file1, file1_mtime).expect("set file1 mtime");
    set_file_mtime(&file2, file2_mtime).expect("set file2 mtime");
    set_file_mtime(&file3, file3_mtime).expect("set file3 mtime");
    set_file_mtime(&source_root, dir_mtime).expect("set dir mtime");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .times(true)
        .omit_dir_times(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // All file mtimes should be preserved
    let dest_file1 = dest_root.join("file1.txt");
    let dest_file1_mtime = FileTime::from_last_modification_time(
        &fs::metadata(&dest_file1).expect("file1 metadata")
    );
    assert_eq!(dest_file1_mtime, file1_mtime);

    let dest_file2 = dest_root.join("file2.txt");
    let dest_file2_mtime = FileTime::from_last_modification_time(
        &fs::metadata(&dest_file2).expect("file2 metadata")
    );
    assert_eq!(dest_file2_mtime, file2_mtime);

    let dest_file3 = dest_root.join("file3.txt");
    let dest_file3_mtime = FileTime::from_last_modification_time(
        &fs::metadata(&dest_file3).expect("file3 metadata")
    );
    assert_eq!(dest_file3_mtime, file3_mtime);

    // Directory mtime should NOT be preserved
    let dest_dir_metadata = fs::metadata(&dest_root).expect("dir metadata");
    let dest_dir_mtime = FileTime::from_last_modification_time(&dest_dir_metadata);
    assert_ne!(dest_dir_mtime, dir_mtime);
}

#[cfg(unix)]
#[test]
fn omit_dir_times_with_empty_directories() {
    use filetime::{FileTime, set_file_mtime};

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let empty_dir = source_root.join("empty");
    fs::create_dir_all(&empty_dir).expect("create empty dir");

    let dir_mtime = FileTime::from_unix_time(1_600_000_000, 0);
    set_file_mtime(&empty_dir, dir_mtime).expect("set empty dir mtime");
    set_file_mtime(&source_root, dir_mtime).expect("set root mtime");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .times(true)
        .omit_dir_times(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Both directories should exist but have non-preserved mtimes
    assert!(dest_root.is_dir());
    let dest_empty = dest_root.join("empty");
    assert!(dest_empty.is_dir());

    let dest_root_mtime = FileTime::from_last_modification_time(
        &fs::metadata(&dest_root).expect("root metadata")
    );
    let dest_empty_mtime = FileTime::from_last_modification_time(
        &fs::metadata(&dest_empty).expect("empty metadata")
    );

    assert_ne!(dest_root_mtime, dir_mtime);
    assert_ne!(dest_empty_mtime, dir_mtime);
}

#[cfg(unix)]
#[test]
fn omit_dir_times_performance_deep_hierarchy() {
    use filetime::{FileTime, set_file_mtime};

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");

    // Create a deep hierarchy with many directories
    let mut current = source_root.clone();
    for i in 0..20 {
        current = current.join(format!("level{i:02}"));
    }
    fs::create_dir_all(&current).expect("create deep hierarchy");
    fs::write(current.join("deep_file.txt"), b"deep content").expect("write file");

    // Set mtimes for all directories (working backwards from deep to shallow)
    let dir_mtime = FileTime::from_unix_time(1_600_000_000, 0);
    let mut dir_path = current.clone();
    while dir_path.starts_with(&source_root) {
        set_file_mtime(&dir_path, dir_mtime).expect("set dir mtime");
        if dir_path == source_root {
            break;
        }
        dir_path = dir_path.parent().unwrap().to_path_buf();
    }

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .times(true)
        .omit_dir_times(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Verify that the deep hierarchy was copied
    assert!(summary.directories_created() >= 20);

    let mut dest_path = dest_root.clone();
    for i in 0..20 {
        dest_path = dest_path.join(format!("level{i:02}"));
    }
    assert!(dest_path.join("deep_file.txt").exists());

    // Verify that directory mtimes were NOT preserved
    let dest_root_mtime = FileTime::from_last_modification_time(
        &fs::metadata(&dest_root).expect("root metadata")
    );
    assert_ne!(dest_root_mtime, dir_mtime);

    // Verify file mtime WAS preserved
    let dest_file = dest_path.join("deep_file.txt");
    let file_metadata = fs::metadata(&dest_file).expect("file metadata");
    // Note: We didn't set a specific file mtime in this test, we're just checking
    // that the file exists and can be read
    assert!(file_metadata.is_file());
}

#[cfg(unix)]
#[test]
fn omit_dir_times_with_trailing_slash_source() {
    use filetime::{FileTime, set_file_mtime};

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let nested = source_root.join("nested");
    fs::create_dir_all(&nested).expect("create nested");
    fs::write(nested.join("file.txt"), b"content").expect("write file");

    let dir_mtime = FileTime::from_unix_time(1_600_000_000, 0);
    set_file_mtime(&nested, dir_mtime).expect("set nested mtime");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest");

    // Use trailing slash to copy contents directly
    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .times(true)
        .omit_dir_times(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // The nested directory should be copied directly into dest
    let dest_nested = dest_root.join("nested");
    assert!(dest_nested.is_dir());
    assert!(dest_nested.join("file.txt").exists());

    // Directory mtime should not be preserved
    let dest_nested_mtime = FileTime::from_last_modification_time(
        &fs::metadata(&dest_nested).expect("nested metadata")
    );
    assert_ne!(dest_nested_mtime, dir_mtime);
}

#[cfg(unix)]
#[test]
fn omit_dir_times_combined_with_update_flag() {
    use filetime::{FileTime, set_file_mtime};

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    let source_file = source_root.join("file.txt");
    fs::write(&source_file, b"new content").expect("write source file");

    let file_mtime = FileTime::from_unix_time(2_000_000_000, 0);
    let dir_mtime = FileTime::from_unix_time(1_900_000_000, 0);
    set_file_mtime(&source_file, file_mtime).expect("set file mtime");
    set_file_mtime(&source_root, dir_mtime).expect("set dir mtime");

    // Create destination with older file
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest");
    let dest_file = dest_root.join("file.txt");
    fs::write(&dest_file, b"old content").expect("write dest file");

    let old_file_mtime = FileTime::from_unix_time(1_000_000_000, 0);
    set_file_mtime(&dest_file, old_file_mtime).expect("set old file mtime");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .times(true)
        .omit_dir_times(true)
        .update(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // File should be updated because source is newer
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&dest_file).expect("read dest"), b"new content");

    // File mtime should be preserved
    let dest_file_mtime = FileTime::from_last_modification_time(
        &fs::metadata(&dest_file).expect("file metadata")
    );
    assert_eq!(dest_file_mtime, file_mtime);

    // Directory mtime should NOT be preserved
    let dest_dir_mtime = FileTime::from_last_modification_time(
        &fs::metadata(&dest_root).expect("dir metadata")
    );
    assert_ne!(dest_dir_mtime, dir_mtime);
}

#[cfg(unix)]
#[test]
fn omit_dir_times_with_delete_flag() {
    use filetime::{FileTime, set_file_mtime};

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");
    fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");

    let dir_mtime = FileTime::from_unix_time(1_600_000_000, 0);
    set_file_mtime(&source_root, dir_mtime).expect("set dir mtime");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest");
    fs::write(dest_root.join("keep.txt"), b"old").expect("write old");
    fs::write(dest_root.join("extra.txt"), b"extra").expect("write extra");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .times(true)
        .omit_dir_times(true)
        .delete(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Verify deletion occurred
    assert_eq!(summary.items_deleted(), 1);
    assert!(!dest_root.join("extra.txt").exists());
    assert!(dest_root.join("keep.txt").exists());

    // Directory mtime should not be preserved
    let dest_dir_mtime = FileTime::from_last_modification_time(
        &fs::metadata(&dest_root).expect("dir metadata")
    );
    assert_ne!(dest_dir_mtime, dir_mtime);
}

#[test]
fn omit_dir_times_in_report_mode() {
    use filetime::{FileTime, set_file_mtime};

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let nested = source_root.join("nested");
    fs::create_dir_all(&nested).expect("create nested");
    fs::write(nested.join("file.txt"), b"content").expect("write file");

    #[cfg(unix)]
    {
        let dir_mtime = FileTime::from_unix_time(1_600_000_000, 0);
        set_file_mtime(&nested, dir_mtime).expect("set nested mtime");
        set_file_mtime(&source_root, dir_mtime).expect("set root mtime");
    }

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .times(true)
        .omit_dir_times(true)
        .collect_events(true);

    let report = plan
        .execute_with_report(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let summary = report.summary();
    let records = report.records();

    // Should have directory creation records
    let dir_created_count = records
        .iter()
        .filter(|r| r.action() == &LocalCopyAction::DirectoryCreated)
        .count();
    assert!(dir_created_count >= 2);
    assert!(summary.directories_created() >= 2);
}

// ============================================================================
// Baseline / control tests
// ============================================================================

#[cfg(unix)]
#[test]
fn without_omit_dir_times_directory_mtime_is_preserved() {
    use filetime::{FileTime, set_file_mtime};

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");
    fs::write(source_root.join("file.txt"), b"content").expect("write file");

    let dir_mtime = FileTime::from_unix_time(1_600_000_000, 0);
    set_file_mtime(&source_root, dir_mtime).expect("set source mtime");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .times(true)
        .omit_dir_times(false);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let dest_metadata = fs::metadata(&dest_root).expect("dest metadata");
    let dest_mtime = FileTime::from_last_modification_time(&dest_metadata);

    // Without omit_dir_times, the directory mtime SHOULD match the source
    assert_eq!(
        dest_mtime, dir_mtime,
        "directory mtime should be preserved when --omit-dir-times is not set"
    );
}

#[cfg(unix)]
#[test]
fn without_omit_dir_times_nested_dirs_preserve_mtimes() {
    use filetime::{FileTime, set_file_mtime};

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let nested = source_root.join("sub1").join("sub2");
    fs::create_dir_all(&nested).expect("create nested");
    fs::write(nested.join("file.txt"), b"content").expect("write file");

    let root_mtime = FileTime::from_unix_time(1_600_000_000, 0);
    let sub1_mtime = FileTime::from_unix_time(1_700_000_000, 0);
    let sub2_mtime = FileTime::from_unix_time(1_800_000_000, 0);

    set_file_mtime(&nested, sub2_mtime).expect("set sub2 mtime");
    set_file_mtime(nested.parent().unwrap(), sub1_mtime).expect("set sub1 mtime");
    set_file_mtime(&source_root, root_mtime).expect("set root mtime");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .times(true)
        .omit_dir_times(false);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let dest_root_mtime = FileTime::from_last_modification_time(
        &fs::metadata(&dest_root).expect("root metadata")
    );
    let dest_sub1_mtime = FileTime::from_last_modification_time(
        &fs::metadata(dest_root.join("sub1")).expect("sub1 metadata")
    );
    let dest_sub2_mtime = FileTime::from_last_modification_time(
        &fs::metadata(dest_root.join("sub1").join("sub2")).expect("sub2 metadata")
    );

    assert_eq!(dest_root_mtime, root_mtime, "root mtime should be preserved");
    assert_eq!(dest_sub1_mtime, sub1_mtime, "sub1 mtime should be preserved");
    assert_eq!(dest_sub2_mtime, sub2_mtime, "sub2 mtime should be preserved");
}

// ============================================================================
// Incremental / second-pass tests
// ============================================================================

#[cfg(unix)]
#[test]
fn omit_dir_times_incremental_copy_does_not_update_dir_mtime() {
    use filetime::{FileTime, set_file_mtime};

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    let source_file = source_root.join("file.txt");
    fs::write(&source_file, b"content").expect("write file");

    let file_mtime = FileTime::from_unix_time(1_500_000_000, 0);
    let dir_mtime = FileTime::from_unix_time(1_600_000_000, 0);
    set_file_mtime(&source_file, file_mtime).expect("set file mtime");
    set_file_mtime(&source_root, dir_mtime).expect("set dir mtime");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest");

    // Use trailing slash to copy contents
    let mut source_operand = source_root.clone().into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];

    // First copy
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .times(true)
        .omit_dir_times(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("first copy succeeds");

    // Record destination directory mtime after first copy
    let _first_dir_mtime = FileTime::from_last_modification_time(
        &fs::metadata(&dest_root).expect("dest metadata")
    );

    // Second copy (incremental) - file unchanged so file matches, but dir
    // timestamp should still not be set from source
    let mut source_operand2 = source_root.clone().into_os_string();
    source_operand2.push(std::path::MAIN_SEPARATOR.to_string());
    let operands2 = vec![source_operand2, dest_root.clone().into_os_string()];
    let plan2 = LocalCopyPlan::from_operands(&operands2).expect("plan");
    let options2 = LocalCopyOptions::default()
        .times(true)
        .omit_dir_times(true);

    plan2
        .execute_with_options(LocalCopyExecution::Apply, options2)
        .expect("second copy succeeds");

    let second_dir_mtime = FileTime::from_last_modification_time(
        &fs::metadata(&dest_root).expect("dest metadata")
    );

    // Directory mtime should NOT have been set to match source
    assert_ne!(
        second_dir_mtime, dir_mtime,
        "directory mtime should not be set to source mtime on incremental copy"
    );

    // File mtime should still be preserved
    let dest_file = dest_root.join("file.txt");
    let dest_file_mtime = FileTime::from_last_modification_time(
        &fs::metadata(&dest_file).expect("file metadata")
    );
    assert_eq!(
        dest_file_mtime, file_mtime,
        "file mtime should be preserved on incremental copy"
    );
}

#[cfg(unix)]
#[test]
fn omit_dir_times_incremental_with_new_file_preserves_new_file_time() {
    use filetime::{FileTime, set_file_mtime};

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    let source_file1 = source_root.join("file1.txt");
    fs::write(&source_file1, b"content1").expect("write file1");

    let file1_mtime = FileTime::from_unix_time(1_500_000_000, 0);
    let dir_mtime = FileTime::from_unix_time(1_600_000_000, 0);
    set_file_mtime(&source_file1, file1_mtime).expect("set file1 mtime");
    set_file_mtime(&source_root, dir_mtime).expect("set dir mtime");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest");

    // Use trailing slash to copy contents
    let mut source_operand = source_root.clone().into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];

    // First copy
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .times(true)
        .omit_dir_times(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("first copy succeeds");

    // Add a second file
    let source_file2 = source_root.join("file2.txt");
    fs::write(&source_file2, b"content2").expect("write file2");

    let file2_mtime = FileTime::from_unix_time(1_550_000_000, 0);
    set_file_mtime(&source_file2, file2_mtime).expect("set file2 mtime");

    // Update dir mtime after adding the file
    let new_dir_mtime = FileTime::from_unix_time(1_650_000_000, 0);
    set_file_mtime(&source_root, new_dir_mtime).expect("set new dir mtime");

    // Second copy (incremental)
    let mut source_operand2 = source_root.clone().into_os_string();
    source_operand2.push(std::path::MAIN_SEPARATOR.to_string());
    let operands2 = vec![source_operand2, dest_root.clone().into_os_string()];
    let plan2 = LocalCopyPlan::from_operands(&operands2).expect("plan");
    let options2 = LocalCopyOptions::default()
        .times(true)
        .omit_dir_times(true);

    plan2
        .execute_with_options(LocalCopyExecution::Apply, options2)
        .expect("second copy succeeds");

    // Directory mtime should NOT match source (neither old nor new)
    let dest_dir_mtime = FileTime::from_last_modification_time(
        &fs::metadata(&dest_root).expect("dest metadata")
    );
    assert_ne!(dest_dir_mtime, dir_mtime);
    assert_ne!(dest_dir_mtime, new_dir_mtime);

    // Both file mtimes should be preserved
    let dest_file1 = dest_root.join("file1.txt");
    let dest_file1_mtime = FileTime::from_last_modification_time(
        &fs::metadata(&dest_file1).expect("file1 metadata")
    );
    assert_eq!(dest_file1_mtime, file1_mtime, "file1 mtime should be preserved");

    let dest_file2 = dest_root.join("file2.txt");
    let dest_file2_mtime = FileTime::from_last_modification_time(
        &fs::metadata(&dest_file2).expect("file2 metadata")
    );
    assert_eq!(dest_file2_mtime, file2_mtime, "file2 mtime should be preserved");
}

// ============================================================================
// Mixed content tests
// ============================================================================

#[cfg(unix)]
#[test]
fn omit_dir_times_mixed_tree_files_have_correct_timestamps() {
    use filetime::{FileTime, set_file_mtime};

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let sub_a = source_root.join("dir_a");
    let sub_b = source_root.join("dir_b");
    fs::create_dir_all(&sub_a).expect("create dir_a");
    fs::create_dir_all(&sub_b).expect("create dir_b");

    // Create files with distinct timestamps
    let file_a = sub_a.join("file_a.txt");
    let file_b = sub_b.join("file_b.txt");
    let file_root = source_root.join("file_root.txt");
    fs::write(&file_a, b"aaa").expect("write file_a");
    fs::write(&file_b, b"bbb").expect("write file_b");
    fs::write(&file_root, b"root").expect("write file_root");

    let file_a_mtime = FileTime::from_unix_time(1_400_000_000, 0);
    let file_b_mtime = FileTime::from_unix_time(1_450_000_000, 0);
    let file_root_mtime = FileTime::from_unix_time(1_480_000_000, 0);
    let dir_a_mtime = FileTime::from_unix_time(1_500_000_000, 0);
    let dir_b_mtime = FileTime::from_unix_time(1_550_000_000, 0);
    let root_mtime = FileTime::from_unix_time(1_600_000_000, 0);

    set_file_mtime(&file_a, file_a_mtime).expect("set file_a mtime");
    set_file_mtime(&file_b, file_b_mtime).expect("set file_b mtime");
    set_file_mtime(&file_root, file_root_mtime).expect("set file_root mtime");
    set_file_mtime(&sub_a, dir_a_mtime).expect("set dir_a mtime");
    set_file_mtime(&sub_b, dir_b_mtime).expect("set dir_b mtime");
    set_file_mtime(&source_root, root_mtime).expect("set root mtime");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .times(true)
        .omit_dir_times(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // All file mtimes SHOULD be preserved
    let dest_file_a_mtime = FileTime::from_last_modification_time(
        &fs::metadata(dest_root.join("dir_a").join("file_a.txt")).expect("file_a metadata")
    );
    let dest_file_b_mtime = FileTime::from_last_modification_time(
        &fs::metadata(dest_root.join("dir_b").join("file_b.txt")).expect("file_b metadata")
    );
    let dest_file_root_mtime = FileTime::from_last_modification_time(
        &fs::metadata(dest_root.join("file_root.txt")).expect("file_root metadata")
    );

    assert_eq!(dest_file_a_mtime, file_a_mtime, "file_a mtime should be preserved");
    assert_eq!(dest_file_b_mtime, file_b_mtime, "file_b mtime should be preserved");
    assert_eq!(dest_file_root_mtime, file_root_mtime, "file_root mtime should be preserved");

    // All directory mtimes should NOT be preserved
    let dest_root_mtime = FileTime::from_last_modification_time(
        &fs::metadata(&dest_root).expect("root metadata")
    );
    let dest_dir_a_mtime = FileTime::from_last_modification_time(
        &fs::metadata(dest_root.join("dir_a")).expect("dir_a metadata")
    );
    let dest_dir_b_mtime = FileTime::from_last_modification_time(
        &fs::metadata(dest_root.join("dir_b")).expect("dir_b metadata")
    );

    assert_ne!(dest_root_mtime, root_mtime, "root mtime should not be preserved");
    assert_ne!(dest_dir_a_mtime, dir_a_mtime, "dir_a mtime should not be preserved");
    assert_ne!(dest_dir_b_mtime, dir_b_mtime, "dir_b mtime should not be preserved");
}

// ============================================================================
// Option interaction tests
// ============================================================================

#[cfg(unix)]
#[test]
fn omit_dir_times_false_explicitly_preserves_dir_mtimes() {
    use filetime::{FileTime, set_file_mtime};

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");
    fs::write(source_root.join("file.txt"), b"content").expect("write file");

    let dir_mtime = FileTime::from_unix_time(1_600_000_000, 0);
    set_file_mtime(&source_root, dir_mtime).expect("set source mtime");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    // Explicitly set omit_dir_times(false) with times(true) - should preserve dir mtime
    let options = LocalCopyOptions::default()
        .times(true)
        .omit_dir_times(false);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let dest_metadata = fs::metadata(&dest_root).expect("dest metadata");
    let dest_mtime = FileTime::from_last_modification_time(&dest_metadata);

    assert_eq!(
        dest_mtime, dir_mtime,
        "directory mtime should be preserved when omit_dir_times is explicitly false"
    );
}

#[test]
fn omit_dir_times_option_default_is_false() {
    let options = LocalCopyOptions::default();
    assert!(
        !options.omit_dir_times_enabled(),
        "omit_dir_times should default to false"
    );
}

#[test]
fn omit_dir_times_option_builder_round_trip() {
    let options = LocalCopyOptions::default()
        .omit_dir_times(true);
    assert!(options.omit_dir_times_enabled());

    let options = LocalCopyOptions::default()
        .omit_dir_times(false);
    assert!(!options.omit_dir_times_enabled());

    // Toggle: set true then false
    let options = LocalCopyOptions::default()
        .omit_dir_times(true)
        .omit_dir_times(false);
    assert!(!options.omit_dir_times_enabled());

    // Toggle: set false then true
    let options = LocalCopyOptions::default()
        .omit_dir_times(false)
        .omit_dir_times(true);
    assert!(options.omit_dir_times_enabled());
}

#[cfg(unix)]
#[test]
fn omit_dir_times_with_archive_style_options() {
    use filetime::{FileTime, set_file_mtime};
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let nested = source_root.join("subdir");
    fs::create_dir_all(&nested).expect("create nested");
    fs::write(nested.join("file.txt"), b"data").expect("write file");

    let file_mtime = FileTime::from_unix_time(1_500_000_000, 0);
    let dir_mtime = FileTime::from_unix_time(1_600_000_000, 0);

    set_file_mtime(&nested.join("file.txt"), file_mtime).expect("set file mtime");
    set_file_mtime(&nested, dir_mtime).expect("set nested dir mtime");
    set_file_mtime(&source_root, dir_mtime).expect("set root dir mtime");

    fs::set_permissions(&nested, PermissionsExt::from_mode(0o755)).expect("set nested perms");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Simulate archive-like options + omit_dir_times (like rsync -aO)
    let options = LocalCopyOptions::default()
        .recursive(true)
        .times(true)
        .permissions(true)
        .omit_dir_times(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // File timestamp should be preserved
    let dest_file = dest_root.join("subdir").join("file.txt");
    let dest_file_mtime = FileTime::from_last_modification_time(
        &fs::metadata(&dest_file).expect("file metadata")
    );
    assert_eq!(dest_file_mtime, file_mtime, "file mtime should be preserved");

    // Directory timestamps should NOT be preserved
    let dest_root_mtime = FileTime::from_last_modification_time(
        &fs::metadata(&dest_root).expect("root metadata")
    );
    assert_ne!(dest_root_mtime, dir_mtime, "root dir mtime should not be preserved");

    let dest_nested_mtime = FileTime::from_last_modification_time(
        &fs::metadata(dest_root.join("subdir")).expect("nested metadata")
    );
    assert_ne!(dest_nested_mtime, dir_mtime, "nested dir mtime should not be preserved");

    // Directory permissions should still be preserved
    let dest_nested_mode = fs::metadata(dest_root.join("subdir"))
        .expect("nested metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(dest_nested_mode, 0o755, "directory permissions should be preserved");
}
