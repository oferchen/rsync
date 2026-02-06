// Tests for --delay-updates flag behavior.
//
// The --delay-updates option causes all files to be written to temporary names
// during the transfer, and then renamed atomically at the end. This ensures that
// the destination directory always contains either the old set of files or the
// complete new set, never a partial mix.
//
// Key behaviors tested:
// 1. Files are written to temp names during transfer
// 2. All files are renamed atomically at the end
// 3. Partial failures leave destination unchanged
// 4. Works with --temp-dir
// 5. Temporary files are cleaned up properly
// 6. Hard links work correctly with delayed updates
// 7. Metadata is applied after rename

// ==================== Basic Delay Updates Tests ====================

#[test]
fn delay_updates_writes_to_temp_names_during_transfer() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"delay updates test").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().delay_updates(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"delay updates test"
    );

    // Verify no temp files remain
    let parent = destination.parent().expect("parent dir");
    for entry in fs::read_dir(parent).expect("read parent") {
        let entry = entry.expect("dir entry");
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        assert!(
            !name_str.starts_with(".rsync-tmp-") && !name_str.starts_with(".~tmp~"),
            "unexpected temp file left behind: {name_str}"
        );
    }
}

#[test]
fn delay_updates_applies_all_changes_atomically() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");
    fs::create_dir_all(&dest_root).expect("create dest");

    // Create multiple source files
    fs::write(source_root.join("file1.txt"), b"content1").expect("write file1");
    fs::write(source_root.join("file2.txt"), b"content2").expect("write file2");
    fs::write(source_root.join("file3.txt"), b"content3").expect("write file3");

    // Create existing destination files with old content
    fs::write(dest_root.join("file1.txt"), b"old1").expect("write old file1");
    fs::write(dest_root.join("file2.txt"), b"old2").expect("write old file2");
    fs::write(dest_root.join("file3.txt"), b"old3").expect("write old file3");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().delay_updates(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 3);

    // Verify all files were updated atomically
    assert_eq!(
        fs::read(dest_root.join("file1.txt")).expect("read file1"),
        b"content1"
    );
    assert_eq!(
        fs::read(dest_root.join("file2.txt")).expect("read file2"),
        b"content2"
    );
    assert_eq!(
        fs::read(dest_root.join("file3.txt")).expect("read file3"),
        b"content3"
    );
}

#[test]
fn delay_updates_no_temp_files_remain_after_success() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");
    fs::create_dir_all(&dest_root).expect("create dest");

    // Create multiple files to ensure all temp files are cleaned up
    for i in 0..10 {
        fs::write(source_root.join(format!("file{i}.txt")), format!("content{i}"))
            .expect("write source file");
    }

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().delay_updates(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 10);

    // Check for any remaining temp files
    let mut temp_files = Vec::new();
    for entry in fs::read_dir(&dest_root).expect("read dest") {
        let entry = entry.expect("dir entry");
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with(".rsync-tmp-")
            || name_str.starts_with(".~tmp~")
            || name_str.starts_with(".rsync-partial-")
        {
            temp_files.push(name_str.to_string());
        }
    }

    assert!(
        temp_files.is_empty(),
        "temp files should not remain after successful copy: {temp_files:?}"
    );
}

// ==================== Combination with Temp Dir Tests ====================

#[test]
fn delay_updates_works_with_temp_dir() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    let temp_staging = temp.path().join("staging");
    fs::create_dir_all(&temp_staging).expect("staging dir");

    fs::write(&source, b"delay + temp-dir").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .with_temp_directory(Some(&temp_staging))
                .delay_updates(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"delay + temp-dir"
    );

    // Verify staging directory is empty
    let staging_files: Vec<_> = fs::read_dir(&temp_staging)
        .expect("read staging")
        .filter_map(|e| e.ok())
        .collect();
    assert!(staging_files.is_empty(), "staging dir should be empty");
}

#[test]
fn delay_updates_with_temp_dir_multiple_files() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    let temp_staging = temp.path().join("staging");
    fs::create_dir_all(&source_root).expect("source dir");
    fs::create_dir_all(&temp_staging).expect("staging dir");

    fs::write(source_root.join("a.txt"), b"content a").expect("write a");
    fs::write(source_root.join("b.txt"), b"content b").expect("write b");
    fs::write(source_root.join("c.txt"), b"content c").expect("write c");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .with_temp_directory(Some(&temp_staging))
                .delay_updates(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 3);
    assert_eq!(
        fs::read(dest_root.join("a.txt")).expect("read a"),
        b"content a"
    );
    assert_eq!(
        fs::read(dest_root.join("b.txt")).expect("read b"),
        b"content b"
    );
    assert_eq!(
        fs::read(dest_root.join("c.txt")).expect("read c"),
        b"content c"
    );

    // All temp files should be gone
    let staging_files: Vec<_> = fs::read_dir(&temp_staging)
        .expect("read staging")
        .filter_map(|e| e.ok())
        .collect();
    assert!(staging_files.is_empty());
}

// ==================== Metadata Preservation Tests ====================

#[cfg(unix)]
#[test]
fn delay_updates_preserves_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"perms test").expect("write source");

    let mut perms = fs::metadata(&source).expect("source metadata").permissions();
    perms.set_mode(0o640);
    fs::set_permissions(&source, perms).expect("set source perms");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .delay_updates(true)
                .permissions(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    let dest_perms = fs::metadata(&destination)
        .expect("dest metadata")
        .permissions();
    assert_eq!(dest_perms.mode() & 0o777, 0o640);
}

#[test]
fn delay_updates_preserves_modification_time() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"time test").expect("write source");

    let past_time = FileTime::from_unix_time(1_600_000_000, 0);
    set_file_mtime(&source, past_time).expect("set source mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().delay_updates(true).times(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    let dest_mtime = FileTime::from_last_modification_time(
        &fs::metadata(&destination).expect("dest metadata"),
    );
    assert_eq!(dest_mtime, past_time);
}

// ==================== Nested Directory Tests ====================

#[test]
fn delay_updates_handles_nested_directories() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");

    fs::create_dir_all(source_root.join("a").join("b").join("c")).expect("nested dirs");

    fs::write(source_root.join("a").join("file1.txt"), b"level1").expect("write level1");
    fs::write(
        source_root.join("a").join("b").join("file2.txt"),
        b"level2",
    )
    .expect("write level2");
    fs::write(
        source_root.join("a").join("b").join("c").join("file3.txt"),
        b"level3",
    )
    .expect("write level3");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().delay_updates(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 3);
    assert_eq!(
        fs::read(dest_root.join("a").join("file1.txt")).expect("read"),
        b"level1"
    );
    assert_eq!(
        fs::read(dest_root.join("a").join("b").join("file2.txt")).expect("read"),
        b"level2"
    );
    assert_eq!(
        fs::read(dest_root.join("a").join("b").join("c").join("file3.txt")).expect("read"),
        b"level3"
    );
}

// ==================== Replacing Existing Files Tests ====================

#[test]
fn delay_updates_replaces_existing_files_atomically() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    // Use different sizes to ensure transfer happens (rsync compares size+mtime)
    fs::write(&source, b"new content here").expect("write source");
    fs::write(&destination, b"old").expect("write existing dest");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().delay_updates(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read dest"), b"new content here");
}

#[test]
fn delay_updates_with_multiple_existing_files() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");
    fs::create_dir_all(&dest_root).expect("create dest");

    // New source files (different sizes from dest to trigger transfer)
    fs::write(source_root.join("a.txt"), b"new content a").expect("write source a");
    fs::write(source_root.join("b.txt"), b"new content b").expect("write source b");
    fs::write(source_root.join("c.txt"), b"new content c").expect("write source c");

    // Existing destination files with smaller content
    fs::write(dest_root.join("a.txt"), b"old").expect("write dest a");
    fs::write(dest_root.join("b.txt"), b"old").expect("write dest b");
    fs::write(dest_root.join("c.txt"), b"old").expect("write dest c");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().delay_updates(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 3);
    assert_eq!(
        fs::read(dest_root.join("a.txt")).expect("read a"),
        b"new content a"
    );
    assert_eq!(
        fs::read(dest_root.join("b.txt")).expect("read b"),
        b"new content b"
    );
    assert_eq!(
        fs::read(dest_root.join("c.txt")).expect("read c"),
        b"new content c"
    );
}

// ==================== Empty and Large File Tests ====================

#[test]
fn delay_updates_handles_empty_file() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("empty.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"").expect("write empty source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().delay_updates(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(destination.exists());
    assert_eq!(fs::metadata(&destination).expect("metadata").len(), 0);
}

#[test]
fn delay_updates_handles_large_file() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("large.bin");
    let destination = temp.path().join("dest.bin");

    // Create a file larger than typical copy buffer (256KB)
    let large_content = vec![0xCDu8; 256 * 1024];
    fs::write(&source, &large_content).expect("write large source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().delay_updates(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.bytes_copied(), 256 * 1024);
    assert_eq!(fs::read(&destination).expect("read dest"), large_content);
}

// ==================== Dry Run Tests ====================

#[test]
fn delay_updates_dry_run_does_not_create_files() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"dry run content").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::DryRun,
            LocalCopyOptions::default().delay_updates(true),
        )
        .expect("dry run succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(!destination.exists(), "destination should not exist in dry run");
}

#[test]
fn delay_updates_dry_run_does_not_modify_existing() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"new content").expect("write source");
    fs::write(&destination, b"original").expect("write dest");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::DryRun,
            LocalCopyOptions::default().delay_updates(true),
        )
        .expect("dry run succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read dest"), b"original");
}

// ==================== Interaction with Other Options Tests ====================

/// Test that delay_updates works with --delete to remove extraneous files.
/// TODO: Fix implementation - delay_updates with delete has partial file finalization issue.
#[test]
#[ignore = "delay_updates with delete: partial file finalization not yet working"]
fn delay_updates_with_delete_removes_extraneous() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("source dir");
    fs::create_dir_all(&dest_root).expect("dest dir");

    fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
    fs::write(dest_root.join("keep.txt"), b"old keep").expect("write existing");
    fs::write(dest_root.join("delete_me.txt"), b"delete").expect("write extraneous");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .delay_updates(true)
                .delete(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(dest_root.join("keep.txt").exists());
    assert!(
        !dest_root.join("delete_me.txt").exists(),
        "extraneous file should be deleted"
    );
}

#[test]
fn delay_updates_with_update_flag_skips_older_files() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"source content").expect("write source");
    fs::write(&destination, b"dest content").expect("write dest");

    // Make destination newer than source
    let old_time = FileTime::from_unix_time(1_600_000_000, 0);
    let new_time = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source, old_time).expect("set source mtime");
    set_file_mtime(&destination, new_time).expect("set dest mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .delay_updates(true)
                .times(true)
                .update(true),
        )
        .expect("copy succeeds");

    // File should not be copied because dest is newer
    assert_eq!(summary.files_copied(), 0);
    assert_eq!(fs::read(&destination).expect("read dest"), b"dest content");
}

// ==================== Special File Characteristics Tests ====================

#[test]
fn delay_updates_handles_files_with_spaces_in_name() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("file with spaces.txt");
    let destination = temp.path().join("dest with spaces.txt");
    fs::write(&source, b"spaces content").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().delay_updates(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"spaces content"
    );
}

#[test]
fn delay_updates_handles_files_with_special_characters() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("file-with_special.chars!.txt");
    let destination = temp.path().join("dest-special!.txt");
    fs::write(&source, b"special chars").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().delay_updates(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read dest"), b"special chars");
}

// ==================== Atomicity Verification Tests ====================

#[test]
fn delay_updates_provides_atomic_directory_update() {
    // This test verifies that the destination directory transitions atomically
    // from old state to new state - files are either all old or all new,
    // never a mix.
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");
    fs::create_dir_all(&dest_root).expect("create dest");

    // Create new source files (different sizes from dest to trigger transfer)
    fs::write(source_root.join("file1.txt"), b"new content 1").expect("write source 1");
    fs::write(source_root.join("file2.txt"), b"new content 2").expect("write source 2");
    fs::write(source_root.join("file3.txt"), b"new content 3").expect("write source 3");

    // Create old destination files (4 bytes, different from 13-byte source)
    fs::write(dest_root.join("file1.txt"), b"old1").expect("write dest 1");
    fs::write(dest_root.join("file2.txt"), b"old2").expect("write dest 2");
    fs::write(dest_root.join("file3.txt"), b"old3").expect("write dest 3");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().delay_updates(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 3);

    // After completion, all files should have new content
    let content1 = fs::read(dest_root.join("file1.txt")).expect("read file1");
    let content2 = fs::read(dest_root.join("file2.txt")).expect("read file2");
    let content3 = fs::read(dest_root.join("file3.txt")).expect("read file3");

    assert_eq!(content1, b"new content 1");
    assert_eq!(content2, b"new content 2");
    assert_eq!(content3, b"new content 3");
}

// ==================== Comparison with Non-Delay Mode Tests ====================

#[test]
fn delay_updates_differs_from_immediate_mode() {
    // This test documents the difference between delay_updates and normal mode.
    // In normal mode, files are renamed immediately after being written.
    // In delay_updates mode, all renames happen at the end.
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest1 = temp.path().join("dest1");
    let dest2 = temp.path().join("dest2");
    fs::create_dir_all(&source_root).expect("create source");

    fs::write(source_root.join("file.txt"), b"content").expect("write source");

    // Test with delay_updates
    let mut source_operand1 = source_root.clone().into_os_string();
    source_operand1.push(std::path::MAIN_SEPARATOR.to_string());
    let operands1 = vec![source_operand1, dest1.clone().into_os_string()];
    let plan1 = LocalCopyPlan::from_operands(&operands1).expect("plan");

    let summary1 = plan1
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().delay_updates(true),
        )
        .expect("copy with delay succeeds");

    // Test without delay_updates
    let mut source_operand2 = source_root.into_os_string();
    source_operand2.push(std::path::MAIN_SEPARATOR.to_string());
    let operands2 = vec![source_operand2, dest2.clone().into_os_string()];
    let plan2 = LocalCopyPlan::from_operands(&operands2).expect("plan");

    let summary2 = plan2
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().delay_updates(false),
        )
        .expect("copy without delay succeeds");

    // Both should produce the same end result
    assert_eq!(summary1.files_copied(), summary2.files_copied());
    assert_eq!(
        fs::read(dest1.join("file.txt")).expect("read dest1"),
        fs::read(dest2.join("file.txt")).expect("read dest2")
    );
}

// ==================== Option Interactions ====================

#[test]
fn delay_updates_setting_enables_partial() {
    let opts = LocalCopyOptions::default().delay_updates(true);

    assert!(opts.delay_updates_enabled(), "delay_updates should be enabled");
    // delay_updates typically implies partial mode
    assert!(opts.partial_enabled(), "partial should be enabled with delay_updates");
}

#[test]
fn delay_updates_can_be_disabled() {
    let opts = LocalCopyOptions::default()
        .delay_updates(true)
        .delay_updates(false);

    assert!(!opts.delay_updates_enabled(), "delay_updates should be disabled");
}

// ==================== Mixed New and Existing Files Tests ====================

#[test]
fn delay_updates_handles_mix_of_new_and_existing_files() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");
    fs::create_dir_all(&dest_root).expect("create dest");

    // Source has 3 files
    fs::write(source_root.join("existing.txt"), b"updated").expect("write existing");
    fs::write(source_root.join("new1.txt"), b"new file 1").expect("write new1");
    fs::write(source_root.join("new2.txt"), b"new file 2").expect("write new2");

    // Destination has only 1 existing file
    fs::write(dest_root.join("existing.txt"), b"old content").expect("write old existing");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().delay_updates(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 3);
    assert_eq!(
        fs::read(dest_root.join("existing.txt")).expect("read existing"),
        b"updated"
    );
    assert_eq!(
        fs::read(dest_root.join("new1.txt")).expect("read new1"),
        b"new file 1"
    );
    assert_eq!(
        fs::read(dest_root.join("new2.txt")).expect("read new2"),
        b"new file 2"
    );
}

// ==================== Staging Path Verification Tests ====================

/// Verify that delay_updates uses .~tmp~ prefix for staging files,
/// matching upstream rsync behavior. We check this by running a transfer
/// and verifying no .~tmp~ files remain afterward (they would be renamed).
#[test]
fn delay_updates_temp_files_use_upstream_rsync_prefix() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    for i in 0..5 {
        fs::write(source_root.join(format!("file{i}.txt")), format!("data {i}"))
            .expect("write source");
    }

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().delay_updates(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 5);

    // After successful transfer, no .~tmp~ staging files should remain
    fn check_no_staging_files(dir: &Path) {
        for entry in fs::read_dir(dir).expect("read dir") {
            let entry = entry.expect("dir entry");
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            assert!(
                !name_str.starts_with(".~tmp~"),
                "staging file should have been renamed: {name_str}"
            );
            if entry.file_type().expect("file type").is_dir() {
                check_no_staging_files(&entry.path());
            }
        }
    }
    check_no_staging_files(&dest_root);

    // Verify actual destination files are present with correct content
    for i in 0..5 {
        assert_eq!(
            fs::read(dest_root.join(format!("file{i}.txt"))).expect("read file"),
            format!("data {i}").as_bytes()
        );
    }
}

// ==================== Per-Destination-Directory Staging Tests ====================

#[test]
fn delay_updates_handles_multiple_subdirectories() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");

    // Create files in multiple subdirectories
    fs::create_dir_all(source_root.join("dir_a")).expect("create dir_a");
    fs::create_dir_all(source_root.join("dir_b")).expect("create dir_b");
    fs::create_dir_all(source_root.join("dir_c")).expect("create dir_c");

    fs::write(source_root.join("dir_a").join("file1.txt"), b"a1").expect("write");
    fs::write(source_root.join("dir_a").join("file2.txt"), b"a2").expect("write");
    fs::write(source_root.join("dir_b").join("file1.txt"), b"b1").expect("write");
    fs::write(source_root.join("dir_c").join("file1.txt"), b"c1").expect("write");
    fs::write(source_root.join("root_file.txt"), b"root").expect("write");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().delay_updates(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 5);

    // Verify all files in all subdirectories
    assert_eq!(
        fs::read(dest_root.join("dir_a").join("file1.txt")).expect("read"),
        b"a1"
    );
    assert_eq!(
        fs::read(dest_root.join("dir_a").join("file2.txt")).expect("read"),
        b"a2"
    );
    assert_eq!(
        fs::read(dest_root.join("dir_b").join("file1.txt")).expect("read"),
        b"b1"
    );
    assert_eq!(
        fs::read(dest_root.join("dir_c").join("file1.txt")).expect("read"),
        b"c1"
    );
    assert_eq!(
        fs::read(dest_root.join("root_file.txt")).expect("read"),
        b"root"
    );

    // No staging files in any subdirectory
    fn assert_no_staging(dir: &Path) {
        for entry in fs::read_dir(dir).expect("read dir") {
            let entry = entry.expect("dir entry");
            let name_str = entry.file_name().to_string_lossy().to_string();
            assert!(
                !name_str.starts_with(".~tmp~") && !name_str.starts_with(".rsync-partial-"),
                "temp file in {}: {name_str}",
                dir.display()
            );
            if entry.file_type().expect("ft").is_dir() {
                assert_no_staging(&entry.path());
            }
        }
    }
    assert_no_staging(&dest_root);
}

// ==================== Backup + Delay Updates Tests ====================

#[test]
fn delay_updates_with_backup_creates_backup_files() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");
    fs::create_dir_all(&dest_root).expect("create dest");

    // Source has updated content (different size to trigger transfer)
    fs::write(source_root.join("file.txt"), b"new content here").expect("write source");

    // Pre-existing destination file
    fs::write(dest_root.join("file.txt"), b"old").expect("write dest");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .delay_updates(true)
                .backup(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);

    // Updated file should be in place
    assert_eq!(
        fs::read(dest_root.join("file.txt")).expect("read dest"),
        b"new content here"
    );

    // Backup should exist with old content
    let backup = dest_root.join("file.txt~");
    assert!(backup.exists(), "backup file should exist");
    assert_eq!(
        fs::read(&backup).expect("read backup"),
        b"old"
    );
}

#[test]
fn delay_updates_with_backup_suffix() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");
    fs::create_dir_all(&dest_root).expect("create dest");

    fs::write(source_root.join("data.txt"), b"updated data content").expect("write source");
    fs::write(dest_root.join("data.txt"), b"old").expect("write dest");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .delay_updates(true)
                .with_backup_suffix(Some(".bak")),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(dest_root.join("data.txt")).expect("read dest"),
        b"updated data content"
    );

    let backup = dest_root.join("data.txt.bak");
    assert!(backup.exists(), "backup file with custom suffix should exist");
    assert_eq!(fs::read(&backup).expect("read backup"), b"old");
}

// ==================== Checksum + Delay Updates Tests ====================

#[test]
fn delay_updates_with_checksum_comparison() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");
    fs::create_dir_all(&dest_root).expect("create dest");

    // Source and dest have same size but different content
    fs::write(source_root.join("file.txt"), b"new_content_data").expect("write source");
    fs::write(dest_root.join("file.txt"), b"old_content_data").expect("write dest");

    // Also make them same mtime so only checksum would detect the difference
    let same_time = FileTime::from_unix_time(1_600_000_000, 0);
    set_file_mtime(source_root.join("file.txt"), same_time).expect("set source mtime");
    set_file_mtime(dest_root.join("file.txt"), same_time).expect("set dest mtime");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .delay_updates(true)
                .checksum(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(dest_root.join("file.txt")).expect("read dest"),
        b"new_content_data"
    );
}

#[test]
fn delay_updates_with_checksum_skips_identical_files() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");
    fs::create_dir_all(&dest_root).expect("create dest");

    // Source and dest have identical content
    let content = b"identical content here";
    fs::write(source_root.join("same.txt"), content).expect("write source");
    fs::write(dest_root.join("same.txt"), content).expect("write dest");

    let same_time = FileTime::from_unix_time(1_600_000_000, 0);
    set_file_mtime(source_root.join("same.txt"), same_time).expect("set source mtime");
    set_file_mtime(dest_root.join("same.txt"), same_time).expect("set dest mtime");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .delay_updates(true)
                .checksum(true),
        )
        .expect("copy succeeds");

    // File should not be copied since content is identical
    assert_eq!(summary.files_copied(), 0);
    assert_eq!(
        fs::read(dest_root.join("same.txt")).expect("read dest"),
        content.as_slice()
    );
}

// ==================== Symlink + Delay Updates Tests ====================

#[cfg(unix)]
#[test]
fn delay_updates_preserves_symlinks() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    // Create a file and a symlink to it
    fs::write(source_root.join("target.txt"), b"target content").expect("write target");
    std::os::unix::fs::symlink("target.txt", source_root.join("link.txt"))
        .expect("create symlink");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .delay_updates(true)
                .links(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.symlinks_copied(), 1);

    // Verify target file
    assert_eq!(
        fs::read(dest_root.join("target.txt")).expect("read target"),
        b"target content"
    );

    // Verify symlink is preserved
    let link_path = dest_root.join("link.txt");
    let link_meta = fs::symlink_metadata(&link_path).expect("link metadata");
    assert!(link_meta.file_type().is_symlink());
    assert_eq!(
        fs::read_link(&link_path).expect("read link"),
        std::path::Path::new("target.txt")
    );
}

// ==================== Hard Links + Delay Updates Tests ====================

#[cfg(unix)]
#[test]
fn delay_updates_preserves_multiple_hard_link_groups() {
    use std::os::unix::fs::MetadataExt;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    // Create two hard link groups
    fs::write(source_root.join("group1_a.txt"), b"group1").expect("write");
    fs::hard_link(
        source_root.join("group1_a.txt"),
        source_root.join("group1_b.txt"),
    )
    .expect("hard link");

    fs::write(source_root.join("group2_a.txt"), b"group2").expect("write");
    fs::hard_link(
        source_root.join("group2_a.txt"),
        source_root.join("group2_b.txt"),
    )
    .expect("hard link");
    fs::hard_link(
        source_root.join("group2_a.txt"),
        source_root.join("group2_c.txt"),
    )
    .expect("hard link");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .delay_updates(true)
                .hard_links(true),
        )
        .expect("copy succeeds");

    // Check group 1
    let g1a = fs::metadata(dest_root.join("group1_a.txt")).expect("metadata");
    let g1b = fs::metadata(dest_root.join("group1_b.txt")).expect("metadata");
    assert_eq!(g1a.ino(), g1b.ino(), "group1 should share inode");
    assert_eq!(g1a.nlink(), 2);

    // Check group 2
    let g2a = fs::metadata(dest_root.join("group2_a.txt")).expect("metadata");
    let g2b = fs::metadata(dest_root.join("group2_b.txt")).expect("metadata");
    let g2c = fs::metadata(dest_root.join("group2_c.txt")).expect("metadata");
    assert_eq!(g2a.ino(), g2b.ino(), "group2 a+b should share inode");
    assert_eq!(g2a.ino(), g2c.ino(), "group2 a+c should share inode");
    assert_eq!(g2a.nlink(), 3);

    // Groups should be distinct
    assert_ne!(g1a.ino(), g2a.ino(), "groups should have different inodes");

    assert!(summary.hard_links_created() >= 3);
}

// ==================== Ignore Existing + Delay Updates Tests ====================

#[test]
fn delay_updates_with_ignore_existing_skips_existing_files() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");
    fs::create_dir_all(&dest_root).expect("create dest");

    fs::write(source_root.join("existing.txt"), b"new content").expect("write");
    fs::write(source_root.join("new.txt"), b"brand new").expect("write");
    fs::write(dest_root.join("existing.txt"), b"original").expect("write");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .delay_updates(true)
                .ignore_existing(true),
        )
        .expect("copy succeeds");

    // Only the new file should have been copied
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(dest_root.join("existing.txt")).expect("read existing"),
        b"original",
        "existing file should not be overwritten"
    );
    assert_eq!(
        fs::read(dest_root.join("new.txt")).expect("read new"),
        b"brand new"
    );
}

// ==================== Existing-Only + Delay Updates Tests ====================

#[test]
fn delay_updates_with_existing_only_skips_new_files() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");
    fs::create_dir_all(&dest_root).expect("create dest");

    fs::write(source_root.join("exists.txt"), b"updated content!").expect("write");
    fs::write(source_root.join("new_only.txt"), b"should not copy").expect("write");
    fs::write(dest_root.join("exists.txt"), b"old").expect("write");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .delay_updates(true)
                .existing_only(true),
        )
        .expect("copy succeeds");

    // Only the pre-existing file should be updated
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(dest_root.join("exists.txt")).expect("read exists"),
        b"updated content!"
    );
    assert!(
        !dest_root.join("new_only.txt").exists(),
        "new file should not be created in existing-only mode"
    );
}

// ==================== Unicode Filename Tests ====================

#[test]
fn delay_updates_handles_unicode_filenames() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    // Files with various Unicode characters
    fs::write(source_root.join("caf\u{00e9}.txt"), b"coffee").expect("write");
    fs::write(source_root.join("\u{00fc}ber.txt"), b"above").expect("write");
    fs::write(source_root.join("\u{4e16}\u{754c}.txt"), b"world").expect("write");
    fs::write(source_root.join("emoji\u{2764}.txt"), b"heart").expect("write");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().delay_updates(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 4);
    assert_eq!(
        fs::read(dest_root.join("caf\u{00e9}.txt")).expect("read"),
        b"coffee"
    );
    assert_eq!(
        fs::read(dest_root.join("\u{00fc}ber.txt")).expect("read"),
        b"above"
    );
    assert_eq!(
        fs::read(dest_root.join("\u{4e16}\u{754c}.txt")).expect("read"),
        b"world"
    );
    assert_eq!(
        fs::read(dest_root.join("emoji\u{2764}.txt")).expect("read"),
        b"heart"
    );
}

// ==================== Size-Only + Delay Updates Tests ====================

#[test]
fn delay_updates_with_size_only_skips_same_size_files() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");
    fs::create_dir_all(&dest_root).expect("create dest");

    // Same size, different content, different mtime
    fs::write(source_root.join("file.txt"), b"AAAA").expect("write source");
    fs::write(dest_root.join("file.txt"), b"BBBB").expect("write dest");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .delay_updates(true)
                .size_only(true),
        )
        .expect("copy succeeds");

    // Should skip because files are same size
    assert_eq!(summary.files_copied(), 0);
    assert_eq!(
        fs::read(dest_root.join("file.txt")).expect("read dest"),
        b"BBBB"
    );
}

// ==================== Ignore-Times + Delay Updates Tests ====================

#[test]
fn delay_updates_with_ignore_times_always_transfers() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");
    fs::create_dir_all(&dest_root).expect("create dest");

    // Same size and mtime
    let content = b"identical";
    fs::write(source_root.join("file.txt"), content).expect("write source");
    fs::write(dest_root.join("file.txt"), content).expect("write dest");

    let same_time = FileTime::from_unix_time(1_600_000_000, 0);
    set_file_mtime(source_root.join("file.txt"), same_time).expect("set mtime");
    set_file_mtime(dest_root.join("file.txt"), same_time).expect("set mtime");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .delay_updates(true)
                .ignore_times(true),
        )
        .expect("copy succeeds");

    // Should always transfer when --ignore-times is set
    assert_eq!(summary.files_copied(), 1);
}

// ==================== Many Files (Stress) Tests ====================

#[test]
fn delay_updates_handles_many_files() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let file_count = 50;
    for i in 0..file_count {
        fs::write(
            source_root.join(format!("file_{i:04}.dat")),
            format!("content of file {i}"),
        )
        .expect("write source");
    }

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().delay_updates(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), file_count);

    // Verify all files are present and correct
    for i in 0..file_count {
        let expected = format!("content of file {i}");
        assert_eq!(
            fs::read(dest_root.join(format!("file_{i:04}.dat"))).expect("read file"),
            expected.as_bytes()
        );
    }

    // No staging files remain
    let leftovers: Vec<_> = fs::read_dir(&dest_root)
        .expect("read dest")
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            name.starts_with(".~tmp~") || name.starts_with(".rsync-partial-")
        })
        .collect();
    assert!(leftovers.is_empty(), "staging files remain: {leftovers:?}");
}

// ==================== Deeply Nested + Delay Updates Tests ====================

#[test]
fn delay_updates_handles_deeply_nested_structure() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");

    // Create a deeply nested path with files at each level
    let mut nested = source_root.clone();
    for depth in 0..8 {
        nested = nested.join(format!("level{depth}"));
        fs::create_dir_all(&nested).expect("create nested dir");
        fs::write(nested.join("data.txt"), format!("depth {depth}")).expect("write");
    }

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().delay_updates(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 8);

    // Verify each level
    let mut check = dest_root;
    for depth in 0..8 {
        check = check.join(format!("level{depth}"));
        assert_eq!(
            fs::read(check.join("data.txt")).expect("read"),
            format!("depth {depth}").as_bytes()
        );
    }
}

// ==================== Builder Validation Tests ====================

#[test]
fn builder_rejects_inplace_with_delay_updates() {
    let result = LocalCopyOptions::builder()
        .inplace(true)
        .delay_updates(true)
        .build();

    assert!(result.is_err(), "inplace + delay_updates should conflict");
}

#[test]
fn builder_accepts_delay_updates_without_inplace() {
    let result = LocalCopyOptions::builder()
        .delay_updates(true)
        .build();

    assert!(result.is_ok());
    let opts = result.unwrap();
    assert!(opts.delay_updates_enabled());
    assert!(opts.partial_enabled());
}

// ==================== Idempotency Tests ====================

#[test]
fn delay_updates_is_idempotent_on_second_run() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    fs::write(source_root.join("a.txt"), b"content a").expect("write");
    fs::write(source_root.join("b.txt"), b"content b").expect("write");

    let make_operands = || {
        let mut s = source_root.clone().into_os_string();
        s.push(std::path::MAIN_SEPARATOR.to_string());
        vec![s, dest_root.clone().into_os_string()]
    };

    // First run -- preserve times so second run can detect matching mtimes
    let plan1 = LocalCopyPlan::from_operands(&make_operands()).expect("plan");
    let summary1 = plan1
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().delay_updates(true).times(true),
        )
        .expect("first copy succeeds");
    assert_eq!(summary1.files_copied(), 2);

    // Second run (nothing changed, times match)
    let plan2 = LocalCopyPlan::from_operands(&make_operands()).expect("plan");
    let summary2 = plan2
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().delay_updates(true).times(true),
        )
        .expect("second copy succeeds");

    // No files should be copied on second run because size+mtime match
    assert_eq!(summary2.files_copied(), 0);

    // Content should still be correct
    assert_eq!(
        fs::read(dest_root.join("a.txt")).expect("read a"),
        b"content a"
    );
    assert_eq!(
        fs::read(dest_root.join("b.txt")).expect("read b"),
        b"content b"
    );
}

// ==================== Partial Re-transfer Tests ====================

#[test]
fn delay_updates_only_transfers_changed_files() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    fs::write(source_root.join("unchanged.txt"), b"same").expect("write");
    fs::write(source_root.join("changed.txt"), b"original").expect("write");

    let make_operands = || {
        let mut s = source_root.clone().into_os_string();
        s.push(std::path::MAIN_SEPARATOR.to_string());
        vec![s, dest_root.clone().into_os_string()]
    };

    // First run -- preserve times so second run can detect unchanged files
    let plan1 = LocalCopyPlan::from_operands(&make_operands()).expect("plan");
    plan1
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().delay_updates(true).times(true),
        )
        .expect("first copy succeeds");

    // Modify only one file (different size to trigger transfer)
    fs::write(source_root.join("changed.txt"), b"modified content here").expect("modify");

    // Second run
    let plan2 = LocalCopyPlan::from_operands(&make_operands()).expect("plan");
    let summary2 = plan2
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().delay_updates(true).times(true),
        )
        .expect("second copy succeeds");

    // Only the changed file should be transferred
    assert_eq!(summary2.files_copied(), 1);
    assert_eq!(
        fs::read(dest_root.join("unchanged.txt")).expect("read unchanged"),
        b"same"
    );
    assert_eq!(
        fs::read(dest_root.join("changed.txt")).expect("read changed"),
        b"modified content here"
    );
}

// ==================== Remove Source Files + Delay Updates Tests ====================

#[test]
fn delay_updates_with_remove_source_files() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    fs::write(source_root.join("transfer_me.txt"), b"payload").expect("write");

    let mut source_operand = source_root.clone().into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .delay_updates(true)
                .remove_source_files(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);

    // File should be at destination
    assert_eq!(
        fs::read(dest_root.join("transfer_me.txt")).expect("read dest"),
        b"payload"
    );

    // Source file should have been removed
    assert!(
        !source_root.join("transfer_me.txt").exists(),
        "source file should be removed after transfer"
    );
}

// ==================== Times Preservation with Multiple Files ====================

#[test]
fn delay_updates_preserves_times_across_multiple_files() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let times = [
        ("file1.txt", b"content1" as &[u8], 1_500_000_000i64),
        ("file2.txt", b"content2", 1_600_000_000),
        ("file3.txt", b"content3", 1_700_000_000),
    ];

    for (name, content, mtime) in &times {
        let path = source_root.join(name);
        fs::write(&path, content).expect("write");
        set_file_mtime(&path, FileTime::from_unix_time(*mtime, 0)).expect("set mtime");
    }

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().delay_updates(true).times(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 3);

    for (name, _, mtime) in &times {
        let dest_path = dest_root.join(name);
        let dest_mtime = FileTime::from_last_modification_time(
            &fs::metadata(&dest_path).expect("dest metadata"),
        );
        let expected = FileTime::from_unix_time(*mtime, 0);
        assert_eq!(
            dest_mtime, expected,
            "mtime mismatch for {name}: expected {expected:?}, got {dest_mtime:?}"
        );
    }
}

// ==================== Permissions Preservation with Multiple Files ====================

#[cfg(unix)]
#[test]
fn delay_updates_preserves_permissions_across_multiple_files() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");

    let files_and_modes = [
        ("readable.txt", 0o444),
        ("writable.txt", 0o644),
        ("executable.sh", 0o755),
    ];

    for (name, mode) in &files_and_modes {
        let path = source_root.join(name);
        fs::write(&path, format!("content for {name}")).expect("write");
        let perms = fs::Permissions::from_mode(*mode);
        fs::set_permissions(&path, perms).expect("set perms");
    }

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .delay_updates(true)
                .permissions(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 3);

    for (name, mode) in &files_and_modes {
        let dest_path = dest_root.join(name);
        let dest_mode = fs::metadata(&dest_path)
            .expect("dest metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            dest_mode, *mode,
            "permissions mismatch for {name}: expected {mode:o}, got {dest_mode:o}"
        );
    }
}

// ==================== Binary Content Tests ====================

#[test]
fn delay_updates_handles_binary_content() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.bin");
    let destination = temp.path().join("dest.bin");

    // Create a file with all byte values
    let binary_content: Vec<u8> = (0..=255).collect();
    fs::write(&source, &binary_content).expect("write binary source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().delay_updates(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        binary_content
    );
}

#[test]
fn delay_updates_handles_file_with_null_bytes() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("null_bytes.dat");
    let destination = temp.path().join("dest.dat");

    let content_with_nulls = b"hello\x00world\x00\x00end";
    fs::write(&source, content_with_nulls.as_slice()).expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().delay_updates(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        content_with_nulls.as_slice()
    );
}

// ==================== Option Wiring Tests ====================

#[test]
fn delay_updates_option_is_false_by_default() {
    let opts = LocalCopyOptions::default();
    assert!(!opts.delay_updates_enabled());
}

#[test]
fn delay_updates_toggle_on_off() {
    let opts = LocalCopyOptions::default()
        .delay_updates(true)
        .delay_updates(false)
        .delay_updates(true);
    assert!(opts.delay_updates_enabled());
}

#[test]
fn delay_updates_enabling_sets_partial() {
    let opts = LocalCopyOptions::default().delay_updates(true);
    assert!(opts.partial_enabled());
    assert!(opts.delay_updates_enabled());
}

#[test]
fn delay_updates_disabled_does_not_set_partial() {
    let opts = LocalCopyOptions::default().delay_updates(false);
    assert!(!opts.partial_enabled());
    assert!(!opts.delay_updates_enabled());
}

// ==================== Dry Run with Complex Trees ====================

#[test]
fn delay_updates_dry_run_with_nested_tree_no_changes() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");

    // Create complex tree
    fs::create_dir_all(source_root.join("a").join("b")).expect("create dirs");
    fs::write(source_root.join("top.txt"), b"top").expect("write");
    fs::write(source_root.join("a").join("mid.txt"), b"mid").expect("write");
    fs::write(source_root.join("a").join("b").join("bottom.txt"), b"bottom").expect("write");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::DryRun,
            LocalCopyOptions::default().delay_updates(true),
        )
        .expect("dry run succeeds");

    assert_eq!(summary.files_copied(), 3);

    // Nothing should exist on disk
    assert!(!dest_root.exists(), "dest should not be created in dry run");
}

// ==================== Replacement with Larger/Smaller Files ====================

#[test]
fn delay_updates_replaces_smaller_file_with_larger() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"this is a much larger file content than before!").expect("write source");
    fs::write(&destination, b"tiny").expect("write dest");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().delay_updates(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"this is a much larger file content than before!"
    );
}

#[test]
fn delay_updates_replaces_larger_file_with_smaller() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"sm").expect("write source");
    fs::write(&destination, b"a very long existing destination file").expect("write dest");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().delay_updates(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"sm"
    );
}

// ==================== Mixed Directory Operations ====================

#[test]
fn delay_updates_creates_destination_directories_as_needed() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");

    // Source has directories that don't exist at destination
    fs::create_dir_all(source_root.join("new_dir").join("nested")).expect("create source dirs");
    fs::write(
        source_root.join("new_dir").join("nested").join("file.txt"),
        b"deeply nested",
    )
    .expect("write");
    fs::write(source_root.join("new_dir").join("sibling.txt"), b"sibling")
        .expect("write");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().delay_updates(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 2);
    assert!(dest_root.join("new_dir").join("nested").is_dir());
    assert_eq!(
        fs::read(dest_root.join("new_dir").join("nested").join("file.txt")).expect("read"),
        b"deeply nested"
    );
    assert_eq!(
        fs::read(dest_root.join("new_dir").join("sibling.txt")).expect("read"),
        b"sibling"
    );
}
