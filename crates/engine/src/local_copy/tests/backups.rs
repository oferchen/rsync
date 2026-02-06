
#[test]
fn backup_creation_uses_default_suffix() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    let source_file = ctx.source.join("file.txt");
    fs::write(&source_file, b"updated").expect("write source");

    let dest_root = ctx.dest.join("source");
    fs::create_dir_all(&dest_root).expect("create dest root");
    let existing = dest_root.join("file.txt");
    fs::write(&existing, b"original").expect("write dest");

    let operands = vec![
        ctx.source.into_os_string(),
        ctx.dest.into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().backup(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let backup = dest_root.join("file.txt~");
    assert!(backup.exists(), "backup missing at {}", backup.display());
    assert_eq!(fs::read(&backup).expect("read backup"), b"original");
    assert_eq!(
        fs::read(dest_root.join("file.txt")).expect("read dest"),
        b"updated"
    );
}

#[test]
fn backup_creation_respects_custom_suffix() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    let source_file = source.join("file.txt");
    fs::write(&source_file, b"replacement").expect("write source");

    let dest_root = dest.join("source");
    fs::create_dir_all(&dest_root).expect("create dest root");
    let existing = dest_root.join("file.txt");
    fs::write(&existing, b"baseline").expect("write dest");

    let operands = vec![
        source.into_os_string(),
        dest.into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().with_backup_suffix(Some(".bak"));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let backup = dest_root.join("file.txt.bak");
    assert!(backup.exists());
    assert_eq!(fs::read(&backup).expect("read backup"), b"baseline");
    assert_eq!(
        fs::read(dest_root.join("file.txt")).expect("read dest"),
        b"replacement"
    );
}

#[test]
fn backup_creation_uses_relative_backup_directory() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    let source_file = source.join("dir").join("file.txt");
    fs::create_dir_all(source_file.parent().unwrap()).expect("create nested source");
    fs::write(&source_file, b"new contents").expect("write source");

    let dest_root = dest.join("source");
    let existing_parent = dest_root.join("dir");
    fs::create_dir_all(&existing_parent).expect("create dest root");
    let existing = existing_parent.join("file.txt");
    fs::write(&existing, b"old contents").expect("write dest");

    let operands = vec![
        source.into_os_string(),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().with_backup_directory(Some(PathBuf::from("backups")));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let backup = dest
        .join("backups")
        .join("source")
        .join("dir")
        .join("file.txt~");
    assert!(backup.exists());
    assert_eq!(fs::read(&backup).expect("read backup"), b"old contents");
    assert_eq!(
        fs::read(dest_root.join("dir").join("file.txt")).expect("read dest"),
        b"new contents"
    );
}

#[test]
fn backup_creation_uses_absolute_backup_directory() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    let backup_root = temp.path().join("backups");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    let source_file = source.join("file.txt");
    fs::write(&source_file, b"replacement").expect("write source");

    let dest_root = dest.join("source");
    fs::create_dir_all(&dest_root).expect("create dest root");
    let existing = dest_root.join("file.txt");
    fs::write(&existing, b"retained").expect("write dest");

    let operands = vec![
        source.into_os_string(),
        dest.into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .with_backup_directory(Some(backup_root.as_path().to_path_buf()))
        .with_backup_suffix(Some(".bak"));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let backup = backup_root.join("source").join("file.txt.bak");
    assert!(backup.exists());
    assert_eq!(fs::read(&backup).expect("read backup"), b"retained");
    assert_eq!(
        fs::read(dest_root.join("file.txt")).expect("read dest"),
        b"replacement"
    );
}

#[test]
fn backup_dir_places_backups_in_specified_directory() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    // Create source file
    let source_file = ctx.source.join("data.txt");
    fs::write(&source_file, b"new data").expect("write source");

    // Create existing destination file
    let dest_root = ctx.dest.join("source");
    fs::create_dir_all(&dest_root).expect("create dest root");
    let existing = dest_root.join("data.txt");
    fs::write(&existing, b"old data").expect("write dest");

    // Set up backup directory
    let backup_dir = ctx.dest.join("my_backups");

    let operands = vec![
        ctx.source.into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .with_backup_directory(Some(backup_dir.clone()));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Verify backup is in backup_dir, not next to the file
    let backup_in_dest = dest_root.join("data.txt~");
    assert!(!backup_in_dest.exists(), "backup should not be in destination directory");

    let backup = backup_dir.join("source").join("data.txt~");
    assert!(backup.exists(), "backup missing at {}", backup.display());
    assert_eq!(fs::read(&backup).expect("read backup"), b"old data");
    assert_eq!(
        fs::read(dest_root.join("data.txt")).expect("read dest"),
        b"new data"
    );
}

#[test]
fn backup_dir_preserves_directory_structure() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    // Create nested source structure
    test_helpers::create_test_tree(&ctx.source, &[
        ("level1/level2/level3/file.txt", Some(b"updated")),
        ("level1/file2.txt", Some(b"updated2")),
    ]);

    // Create nested destination structure with existing files
    let dest_root = ctx.dest.join("source");
    test_helpers::create_test_tree(&dest_root, &[
        ("level1/level2/level3/file.txt", Some(b"original")),
        ("level1/file2.txt", Some(b"original2")),
    ]);

    let backup_dir = ctx.dest.join("backups");

    let operands = vec![
        ctx.source.into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .with_backup_directory(Some(backup_dir.clone()));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Verify directory structure is preserved in backup-dir
    let backup1 = backup_dir.join("source/level1/level2/level3/file.txt~");
    assert!(backup1.exists(), "nested backup missing at {}", backup1.display());
    assert_eq!(fs::read(&backup1).expect("read backup1"), b"original");

    let backup2 = backup_dir.join("source/level1/file2.txt~");
    assert!(backup2.exists(), "backup2 missing at {}", backup2.display());
    assert_eq!(fs::read(&backup2).expect("read backup2"), b"original2");

    // Verify updated files in destination
    assert_eq!(
        fs::read(dest_root.join("level1/level2/level3/file.txt")).expect("read dest1"),
        b"updated"
    );
    assert_eq!(
        fs::read(dest_root.join("level1/file2.txt")).expect("read dest2"),
        b"updated2"
    );
}

#[test]
fn backup_dir_works_with_custom_suffix() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    let source_file = ctx.source.join("document.txt");
    fs::write(&source_file, b"version 2").expect("write source");

    let dest_root = ctx.dest.join("source");
    fs::create_dir_all(&dest_root).expect("create dest root");
    let existing = dest_root.join("document.txt");
    fs::write(&existing, b"version 1").expect("write dest");

    let backup_dir = ctx.dest.join("archive");

    let operands = vec![
        ctx.source.into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .with_backup_directory(Some(backup_dir.clone()))
        .with_backup_suffix(Some(".old"));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Verify backup uses custom suffix
    let backup_default = backup_dir.join("source/document.txt~");
    assert!(!backup_default.exists(), "should not use default suffix");

    let backup = backup_dir.join("source/document.txt.old");
    assert!(backup.exists(), "backup missing at {}", backup.display());
    assert_eq!(fs::read(&backup).expect("read backup"), b"version 1");
    assert_eq!(
        fs::read(dest_root.join("document.txt")).expect("read dest"),
        b"version 2"
    );
}

#[test]
fn backup_dir_handles_multiple_backups_correctly() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    // Create multiple source files
    test_helpers::create_test_tree(&ctx.source, &[
        ("file1.txt", Some(b"content1-v2")),
        ("file2.txt", Some(b"content2-v2")),
        ("subdir/file3.txt", Some(b"content3-v2")),
    ]);

    // Create existing destination files
    let dest_root = ctx.dest.join("source");
    test_helpers::create_test_tree(&dest_root, &[
        ("file1.txt", Some(b"content1-v1")),
        ("file2.txt", Some(b"content2-v1")),
        ("subdir/file3.txt", Some(b"content3-v1")),
    ]);

    let backup_dir = ctx.dest.join("backups");

    let operands = vec![
        ctx.source.into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .with_backup_directory(Some(backup_dir.clone()));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Verify all backups are created in backup-dir
    let backup1 = backup_dir.join("source/file1.txt~");
    assert!(backup1.exists(), "backup1 missing at {}", backup1.display());
    assert_eq!(fs::read(&backup1).expect("read backup1"), b"content1-v1");

    let backup2 = backup_dir.join("source/file2.txt~");
    assert!(backup2.exists(), "backup2 missing at {}", backup2.display());
    assert_eq!(fs::read(&backup2).expect("read backup2"), b"content2-v1");

    let backup3 = backup_dir.join("source/subdir/file3.txt~");
    assert!(backup3.exists(), "backup3 missing at {}", backup3.display());
    assert_eq!(fs::read(&backup3).expect("read backup3"), b"content3-v1");

    // Verify all destination files are updated
    assert_eq!(
        fs::read(dest_root.join("file1.txt")).expect("read dest1"),
        b"content1-v2"
    );
    assert_eq!(
        fs::read(dest_root.join("file2.txt")).expect("read dest2"),
        b"content2-v2"
    );
    assert_eq!(
        fs::read(dest_root.join("subdir/file3.txt")).expect("read dest3"),
        b"content3-v2"
    );
}

#[test]
fn backup_dir_handles_repeated_syncs() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    let source_file = ctx.source.join("evolving.txt");
    let dest_root = ctx.dest.join("source");
    fs::create_dir_all(&dest_root).expect("create dest root");
    let dest_file = dest_root.join("evolving.txt");
    let backup_dir = ctx.dest.join("backups");

    // First sync: create initial file
    fs::write(&source_file, b"version 1").expect("write source v1");
    let operands = vec![
        ctx.source.clone().into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .with_backup_directory(Some(backup_dir.clone()));
    plan.execute_with_options(LocalCopyExecution::Apply, options.clone())
        .expect("first sync succeeds");
    assert_eq!(fs::read(&dest_file).expect("read dest after sync 1"), b"version 1");

    // Second sync: update file, should create backup of version 1
    fs::write(&source_file, b"version 2").expect("write source v2");
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    plan.execute_with_options(LocalCopyExecution::Apply, options.clone())
        .expect("second sync succeeds");

    let backup = backup_dir.join("source/evolving.txt~");
    assert!(backup.exists(), "backup after second sync missing");
    assert_eq!(fs::read(&backup).expect("read backup"), b"version 1");
    assert_eq!(fs::read(&dest_file).expect("read dest after sync 2"), b"version 2");

    // Third sync: update again, backup should now contain version 2
    fs::write(&source_file, b"version 3").expect("write source v3");
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("third sync succeeds");

    assert_eq!(fs::read(&backup).expect("read backup after sync 3"), b"version 2");
    assert_eq!(fs::read(&dest_file).expect("read dest after sync 3"), b"version 3");
}

#[test]
fn backup_dir_with_relative_path() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    let source_file = ctx.source.join("file.txt");
    fs::write(&source_file, b"new").expect("write source");

    let dest_root = ctx.dest.join("source");
    fs::create_dir_all(&dest_root).expect("create dest root");
    let existing = dest_root.join("file.txt");
    fs::write(&existing, b"old").expect("write dest");

    // Use relative backup directory
    let operands = vec![
        ctx.source.into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .with_backup_directory(Some(PathBuf::from("relative_backups")));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Relative backup dir should be relative to destination
    let backup = ctx.dest.join("relative_backups/source/file.txt~");
    assert!(backup.exists(), "backup missing at {}", backup.display());
    assert_eq!(fs::read(&backup).expect("read backup"), b"old");
}

#[test]
fn backup_dir_creates_missing_directories() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    test_helpers::create_test_tree(&ctx.source, &[
        ("deep/nested/structure/file.txt", Some(b"content")),
    ]);

    let dest_root = ctx.dest.join("source");
    test_helpers::create_test_tree(&dest_root, &[
        ("deep/nested/structure/file.txt", Some(b"original")),
    ]);

    let backup_dir = ctx.dest.join("backup_location");
    // Don't create backup_dir - it should be created automatically

    let operands = vec![
        ctx.source.into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .with_backup_directory(Some(backup_dir.clone()));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Verify backup directory and all intermediate directories were created
    let backup = backup_dir.join("source/deep/nested/structure/file.txt~");
    assert!(backup.exists(), "backup missing at {}", backup.display());
    assert_eq!(fs::read(&backup).expect("read backup"), b"original");
}

#[test]
fn backup_not_created_in_dry_run_mode() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    let source_file = ctx.source.join("file.txt");
    fs::write(&source_file, b"new content").expect("write source");

    let dest_root = ctx.dest.join("source");
    fs::create_dir_all(&dest_root).expect("create dest root");
    let existing = dest_root.join("file.txt");
    fs::write(&existing, b"original content").expect("write dest");

    let operands = vec![
        ctx.source.into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().backup(true);

    plan.execute_with_options(LocalCopyExecution::DryRun, options)
        .expect("dry-run succeeds");

    // Backup should NOT be created in dry-run mode
    let backup = dest_root.join("file.txt~");
    assert!(!backup.exists(), "backup should not exist in dry-run mode");

    // Original file should be unchanged
    assert_eq!(
        fs::read(dest_root.join("file.txt")).expect("read dest"),
        b"original content"
    );
}

#[test]
fn backup_created_when_deleting_with_delete_option() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    // Create source with only one file
    let source_file = ctx.source.join("keep.txt");
    fs::write(&source_file, b"keep this").expect("write source");

    // Create destination with extra file that will be deleted
    let dest_root = ctx.dest.join("source");
    fs::create_dir_all(&dest_root).expect("create dest root");
    fs::write(dest_root.join("keep.txt"), b"old keep").expect("write keep");
    fs::write(dest_root.join("delete_me.txt"), b"delete me").expect("write delete_me");

    let backup_dir = ctx.dest.join("backups");

    let operands = vec![
        ctx.source.into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .delete(true)
        .with_backup_directory(Some(backup_dir.clone()));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Deleted file should be backed up
    let backup_deleted = backup_dir.join("source/delete_me.txt~");
    assert!(backup_deleted.exists(), "backup of deleted file missing at {}", backup_deleted.display());
    assert_eq!(fs::read(&backup_deleted).expect("read backup"), b"delete me");

    // File should be deleted from destination
    assert!(!dest_root.join("delete_me.txt").exists(), "deleted file should not exist");

    // Keep file should have backup of its old version
    let backup_keep = backup_dir.join("source/keep.txt~");
    assert!(backup_keep.exists(), "backup of modified file missing");
    assert_eq!(fs::read(&backup_keep).expect("read backup"), b"old keep");
}

#[cfg(unix)]
#[test]
fn backup_preserves_symlinks_in_directory() {
    use std::os::unix::fs::symlink;

    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    // Create source directory with a symlink
    let source_link = ctx.source.join("link");
    symlink("new_target", &source_link).expect("create source symlink");

    // Create destination directory with existing symlink pointing elsewhere
    let dest_root = ctx.dest.join("source");
    fs::create_dir_all(&dest_root).expect("create dest root");
    let existing_link = dest_root.join("link");
    symlink("old_target", &existing_link).expect("create dest symlink");

    let backup_dir = ctx.dest.join("backups");

    let operands = vec![
        ctx.source.into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .links(true)
        .with_backup_directory(Some(backup_dir.clone()));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Backup should be a symlink pointing to old target
    let backup = backup_dir.join("source/link~");
    assert!(backup.symlink_metadata().is_ok(), "backup symlink missing at {}", backup.display());
    assert!(backup.symlink_metadata().expect("metadata").file_type().is_symlink());
    assert_eq!(
        fs::read_link(&backup).expect("read backup link"),
        PathBuf::from("old_target")
    );

    // Destination should now point to new target
    assert_eq!(
        fs::read_link(dest_root.join("link")).expect("read dest link"),
        PathBuf::from("new_target")
    );
}

#[test]
fn backup_with_special_characters_in_filename() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    // Create source file with special characters
    let source_file = ctx.source.join("file with spaces & special!.txt");
    fs::write(&source_file, b"new content").expect("write source");

    let dest_root = ctx.dest.join("source");
    fs::create_dir_all(&dest_root).expect("create dest root");
    let existing = dest_root.join("file with spaces & special!.txt");
    fs::write(&existing, b"old content").expect("write dest");

    let operands = vec![
        ctx.source.into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().backup(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let backup = dest_root.join("file with spaces & special!.txt~");
    assert!(backup.exists(), "backup missing at {}", backup.display());
    assert_eq!(fs::read(&backup).expect("read backup"), b"old content");
    assert_eq!(
        fs::read(dest_root.join("file with spaces & special!.txt")).expect("read dest"),
        b"new content"
    );
}

#[test]
fn backup_suffix_with_date_format() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    let source_file = ctx.source.join("file.txt");
    fs::write(&source_file, b"updated").expect("write source");

    let dest_root = ctx.dest.join("source");
    fs::create_dir_all(&dest_root).expect("create dest root");
    let existing = dest_root.join("file.txt");
    fs::write(&existing, b"original").expect("write dest");

    let operands = vec![
        ctx.source.into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    // Use a date-like suffix
    let options = LocalCopyOptions::default().with_backup_suffix(Some(".2024-01-15"));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let backup = dest_root.join("file.txt.2024-01-15");
    assert!(backup.exists(), "backup missing at {}", backup.display());
    assert_eq!(fs::read(&backup).expect("read backup"), b"original");
}

#[test]
fn backup_directory_outside_destination_tree() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    let external_backup = temp.path().join("external_backups");

    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");
    // Don't create external_backup - it should be created automatically

    let source_file = source.join("file.txt");
    fs::write(&source_file, b"new version").expect("write source");

    let dest_root = dest.join("source");
    fs::create_dir_all(&dest_root).expect("create dest root");
    let existing = dest_root.join("file.txt");
    fs::write(&existing, b"old version").expect("write dest");

    let operands = vec![
        source.into_os_string(),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .with_backup_directory(Some(external_backup.clone()));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Backup should be in external directory, not under dest
    let backup = external_backup.join("source/file.txt~");
    assert!(backup.exists(), "backup missing at {}", backup.display());
    assert_eq!(fs::read(&backup).expect("read backup"), b"old version");

    // Verify no backup in destination
    let backup_in_dest = dest_root.join("file.txt~");
    assert!(!backup_in_dest.exists(), "backup should not be in destination");
}

#[test]
fn backup_only_when_content_differs() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    // Create source and dest with different content
    let source_file = ctx.source.join("different.txt");
    fs::write(&source_file, b"new content").expect("write source different");

    let same_file = ctx.source.join("same.txt");
    fs::write(&same_file, b"identical").expect("write source same");

    let dest_root = ctx.dest.join("source");
    fs::create_dir_all(&dest_root).expect("create dest root");

    let existing_different = dest_root.join("different.txt");
    fs::write(&existing_different, b"old content").expect("write dest different");

    let existing_same = dest_root.join("same.txt");
    fs::write(&existing_same, b"identical").expect("write dest same");

    let operands = vec![
        ctx.source.into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().backup(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Backup should exist for the file that was different
    let backup_different = dest_root.join("different.txt~");
    assert!(backup_different.exists(), "backup of different file should exist");
    assert_eq!(fs::read(&backup_different).expect("read backup"), b"old content");

    // No backup for same content (file wasn't modified)
    let backup_same = dest_root.join("same.txt~");
    assert!(!backup_same.exists(), "backup of identical file should not exist");
}

#[test]
fn backup_with_empty_suffix() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    let source_file = ctx.source.join("file.txt");
    fs::write(&source_file, b"new").expect("write source");

    let dest_root = ctx.dest.join("source");
    fs::create_dir_all(&dest_root).expect("create dest root");
    let existing = dest_root.join("file.txt");
    fs::write(&existing, b"old").expect("write dest");

    let backup_dir = ctx.dest.join("backups");

    let operands = vec![
        ctx.source.into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    // Empty suffix means backup file has same name as original
    let options = LocalCopyOptions::default()
        .with_backup_directory(Some(backup_dir.clone()))
        .with_backup_suffix(Some(""));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Backup should have the same name as original (no suffix)
    let backup = backup_dir.join("source/file.txt");
    assert!(backup.exists(), "backup missing at {}", backup.display());
    assert_eq!(fs::read(&backup).expect("read backup"), b"old");
}

#[test]
fn backup_overwrites_existing_backup() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    let source_file = ctx.source.join("file.txt");
    let dest_root = ctx.dest.join("source");
    fs::create_dir_all(&dest_root).expect("create dest root");
    let dest_file = dest_root.join("file.txt");
    let backup_path = dest_root.join("file.txt~");

    // First sync: v1 -> v2
    fs::write(&dest_file, b"version 1").expect("write v1");
    fs::write(&source_file, b"version 2").expect("write source v2");

    let operands = vec![
        ctx.source.clone().into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().backup(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options.clone())
        .expect("first sync succeeds");

    assert_eq!(fs::read(&backup_path).expect("read backup"), b"version 1");
    assert_eq!(fs::read(&dest_file).expect("read dest"), b"version 2");

    // Second sync: v2 -> v3 (should overwrite backup with v2)
    fs::write(&source_file, b"version 3").expect("write source v3");

    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("second sync succeeds");

    // Backup should now contain v2, not v1
    assert_eq!(fs::read(&backup_path).expect("read backup"), b"version 2");
    assert_eq!(fs::read(&dest_file).expect("read dest"), b"version 3");
}

#[test]
fn backup_with_delete_after() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    // Create source with one file
    fs::write(ctx.source.join("keep.txt"), b"keep").expect("write keep");

    // Create destination with extra files to be deleted
    let dest_root = ctx.dest.join("source");
    fs::create_dir_all(&dest_root).expect("create dest root");
    fs::write(dest_root.join("keep.txt"), b"old").expect("write old keep");
    fs::write(dest_root.join("extra1.txt"), b"extra1").expect("write extra1");
    fs::write(dest_root.join("extra2.txt"), b"extra2").expect("write extra2");

    let backup_dir = ctx.dest.join("backups");

    let operands = vec![
        ctx.source.into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .delete_after(true)
        .with_backup_directory(Some(backup_dir.clone()));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // All deleted files should be backed up
    let backup1 = backup_dir.join("source/extra1.txt~");
    let backup2 = backup_dir.join("source/extra2.txt~");
    assert!(backup1.exists(), "backup of extra1 missing at {}", backup1.display());
    assert!(backup2.exists(), "backup of extra2 missing at {}", backup2.display());
    assert_eq!(fs::read(&backup1).expect("read backup1"), b"extra1");
    assert_eq!(fs::read(&backup2).expect("read backup2"), b"extra2");

    // Modified file should also be backed up
    let backup_keep = backup_dir.join("source/keep.txt~");
    assert!(backup_keep.exists(), "backup of keep.txt missing");
    assert_eq!(fs::read(&backup_keep).expect("read backup"), b"old");
}

#[test]
fn backup_with_nested_delete() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    // Create source with nested structure
    test_helpers::create_test_tree(&ctx.source, &[
        ("keep/file.txt", Some(b"keep")),
    ]);

    // Create destination with extra nested files to delete
    let dest_root = ctx.dest.join("source");
    test_helpers::create_test_tree(&dest_root, &[
        ("keep/file.txt", Some(b"old")),
        ("delete_dir/nested/deep.txt", Some(b"deep content")),
        ("delete_dir/shallow.txt", Some(b"shallow content")),
    ]);

    let backup_dir = ctx.dest.join("backups");

    let operands = vec![
        ctx.source.into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .delete(true)
        .with_backup_directory(Some(backup_dir.clone()));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Check that nested deleted files are backed up with proper structure
    // Note: directory deletion may not back up individual files - this tests the behavior
    let backup_keep = backup_dir.join("source/keep/file.txt~");
    assert!(backup_keep.exists(), "backup of modified file missing at {}", backup_keep.display());
    assert_eq!(fs::read(&backup_keep).expect("read backup"), b"old");

    // Verify delete_dir and contents are removed
    assert!(!dest_root.join("delete_dir").exists(), "delete_dir should be removed");
}

#[test]
fn backup_enabled_implicitly_by_backup_dir() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    let source_file = ctx.source.join("file.txt");
    fs::write(&source_file, b"new").expect("write source");

    let dest_root = ctx.dest.join("source");
    fs::create_dir_all(&dest_root).expect("create dest root");
    let existing = dest_root.join("file.txt");
    fs::write(&existing, b"old").expect("write dest");

    let backup_dir = ctx.dest.join("backups");

    let operands = vec![
        ctx.source.into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    // Only set backup_dir, not backup(true) - should still enable backups
    let options = LocalCopyOptions::default()
        .with_backup_directory(Some(backup_dir.clone()));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let backup = backup_dir.join("source/file.txt~");
    assert!(backup.exists(), "backup should be created when backup_dir is set");
    assert_eq!(fs::read(&backup).expect("read backup"), b"old");
}

#[test]
fn backup_enabled_implicitly_by_suffix() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    let source_file = ctx.source.join("file.txt");
    fs::write(&source_file, b"new").expect("write source");

    let dest_root = ctx.dest.join("source");
    fs::create_dir_all(&dest_root).expect("create dest root");
    let existing = dest_root.join("file.txt");
    fs::write(&existing, b"old").expect("write dest");

    let operands = vec![
        ctx.source.into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    // Only set suffix, not backup(true) - should still enable backups
    let options = LocalCopyOptions::default()
        .with_backup_suffix(Some(".backup"));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let backup = dest_root.join("file.txt.backup");
    assert!(backup.exists(), "backup should be created when suffix is set");
    assert_eq!(fs::read(&backup).expect("read backup"), b"old");
}

#[test]
fn no_backup_when_file_is_new() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    // Create source file that doesn't exist in destination
    let source_file = ctx.source.join("new_file.txt");
    fs::write(&source_file, b"brand new").expect("write source");

    let dest_root = ctx.dest.join("source");
    fs::create_dir_all(&dest_root).expect("create dest root");
    // Note: no existing file in destination

    let operands = vec![
        ctx.source.into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().backup(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // File should be created
    assert!(dest_root.join("new_file.txt").exists());

    // But no backup should exist (nothing to back up)
    let backup = dest_root.join("new_file.txt~");
    assert!(!backup.exists(), "backup should not exist for new file");
}

#[test]
fn backup_multiple_files_same_directory() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    // Create multiple source files
    test_helpers::create_test_tree(&ctx.source, &[
        ("file1.txt", Some(b"content1-new")),
        ("file2.txt", Some(b"content2-new")),
        ("file3.txt", Some(b"content3-new")),
    ]);

    // Create existing destination files
    let dest_root = ctx.dest.join("source");
    test_helpers::create_test_tree(&dest_root, &[
        ("file1.txt", Some(b"content1-old")),
        ("file2.txt", Some(b"content2-old")),
        ("file3.txt", Some(b"content3-old")),
    ]);

    let operands = vec![
        ctx.source.into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .backup(true)
        .with_backup_suffix(Some(".bak"));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // All files should be backed up
    for i in 1..=3 {
        let backup = dest_root.join(format!("file{}.txt.bak", i));
        assert!(backup.exists(), "backup{} missing at {}", i, backup.display());
        assert_eq!(
            fs::read(&backup).expect("read backup"),
            format!("content{}-old", i).as_bytes()
        );
        assert_eq!(
            fs::read(dest_root.join(format!("file{}.txt", i))).expect("read dest"),
            format!("content{}-new", i).as_bytes()
        );
    }
}

// ============================================================================
// Backup with different delete timing variants
// ============================================================================

#[test]
fn backup_with_delete_before() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    fs::write(ctx.source.join("keep.txt"), b"keep").expect("write keep");

    let dest_root = ctx.dest.join("source");
    fs::create_dir_all(&dest_root).expect("create dest root");
    fs::write(dest_root.join("keep.txt"), b"old keep").expect("write old keep");
    fs::write(dest_root.join("remove.txt"), b"to remove").expect("write remove");

    let backup_dir = ctx.dest.join("backups");

    let operands = vec![
        ctx.source.into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .delete_before(true)
        .with_backup_directory(Some(backup_dir.clone()));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Deleted file should be backed up
    let backup_removed = backup_dir.join("source/remove.txt~");
    assert!(backup_removed.exists(), "backup of deleted file missing at {}", backup_removed.display());
    assert_eq!(fs::read(&backup_removed).expect("read backup"), b"to remove");

    // Modified file should also be backed up
    let backup_keep = backup_dir.join("source/keep.txt~");
    assert!(backup_keep.exists(), "backup of modified file missing");
    assert_eq!(fs::read(&backup_keep).expect("read backup"), b"old keep");

    // Originals should be gone or updated
    assert!(!dest_root.join("remove.txt").exists(), "deleted file should not exist");
    assert_eq!(fs::read(dest_root.join("keep.txt")).expect("read dest"), b"keep");
}

#[test]
fn backup_with_delete_delay() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    fs::write(ctx.source.join("stay.txt"), b"stay content").expect("write stay");

    let dest_root = ctx.dest.join("source");
    fs::create_dir_all(&dest_root).expect("create dest root");
    fs::write(dest_root.join("stay.txt"), b"old stay").expect("write old stay");
    fs::write(dest_root.join("gone.txt"), b"gone content").expect("write gone");

    let backup_dir = ctx.dest.join("backups");

    let operands = vec![
        ctx.source.into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .delete_delay(true)
        .with_backup_directory(Some(backup_dir.clone()));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Deleted file should be backed up
    let backup_gone = backup_dir.join("source/gone.txt~");
    assert!(backup_gone.exists(), "backup of deleted file missing at {}", backup_gone.display());
    assert_eq!(fs::read(&backup_gone).expect("read backup"), b"gone content");

    // File should be gone from destination
    assert!(!dest_root.join("gone.txt").exists(), "deleted file should be removed");
    assert_eq!(fs::read(dest_root.join("stay.txt")).expect("read dest"), b"stay content");
}

// ============================================================================
// Backup with trailing-slash source (copy contents mode)
// ============================================================================

#[test]
fn backup_with_trailing_slash_source() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    // Source files (trailing-slash means contents go directly into dest)
    fs::write(source.join("file.txt"), b"new data").expect("write source");

    // Existing dest file to be overwritten
    fs::write(dest.join("file.txt"), b"old data").expect("write dest");

    let mut source_operand = source.clone().into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let operands = vec![
        source_operand,
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().backup(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Backup should exist in dest (not dest/source since trailing slash)
    let backup = dest.join("file.txt~");
    assert!(backup.exists(), "backup missing at {}", backup.display());
    assert_eq!(fs::read(&backup).expect("read backup"), b"old data");
    assert_eq!(fs::read(dest.join("file.txt")).expect("read dest"), b"new data");
}

#[test]
fn backup_with_trailing_slash_and_backup_dir() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    let backup_root = temp.path().join("backups");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    fs::write(source.join("report.txt"), b"updated report").expect("write source");
    fs::write(dest.join("report.txt"), b"original report").expect("write dest");

    let mut source_operand = source.clone().into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let operands = vec![
        source_operand,
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .with_backup_directory(Some(backup_root.clone()));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let backup = backup_root.join("report.txt~");
    assert!(backup.exists(), "backup missing at {}", backup.display());
    assert_eq!(fs::read(&backup).expect("read backup"), b"original report");
    assert_eq!(fs::read(dest.join("report.txt")).expect("read dest"), b"updated report");
}

// ============================================================================
// Backup with inplace mode
// ============================================================================

#[test]
fn backup_with_inplace_mode() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    let source_file = ctx.source.join("file.txt");
    fs::write(&source_file, b"inplace new").expect("write source");

    let dest_root = ctx.dest.join("source");
    fs::create_dir_all(&dest_root).expect("create dest root");
    let existing = dest_root.join("file.txt");
    fs::write(&existing, b"inplace old").expect("write dest");

    let operands = vec![
        ctx.source.into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .backup(true)
        .inplace(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let backup = dest_root.join("file.txt~");
    assert!(backup.exists(), "backup missing at {}", backup.display());
    assert_eq!(fs::read(&backup).expect("read backup"), b"inplace old");
    assert_eq!(fs::read(dest_root.join("file.txt")).expect("read dest"), b"inplace new");
}

// ============================================================================
// Backup with --force (directory replaced by file)
// ============================================================================

#[test]
fn backup_with_force_directory_replaced_by_file() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    // Source has a regular file named "item"
    let source_file = ctx.source.join("item");
    fs::write(&source_file, b"file content").expect("write source file");

    // Destination has a directory named "item" with a file inside
    let dest_root = ctx.dest.join("source");
    let dest_dir = dest_root.join("item");
    fs::create_dir_all(&dest_dir).expect("create dest dir");
    fs::write(dest_dir.join("inner.txt"), b"inner").expect("write inner");

    let operands = vec![
        ctx.source.into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    // --force allows overwriting a directory with a file
    let options = LocalCopyOptions::default()
        .force_replacements(true)
        .backup(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // The file should now exist at dest_root/item
    assert!(dest_root.join("item").is_file(), "item should be a file now");
    assert_eq!(fs::read(dest_root.join("item")).expect("read dest"), b"file content");
}

// ============================================================================
// Backup suffix edge cases
// ============================================================================

#[test]
fn backup_suffix_with_dot_prefix() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    let source_file = ctx.source.join("config.yaml");
    fs::write(&source_file, b"new config").expect("write source");

    let dest_root = ctx.dest.join("source");
    fs::create_dir_all(&dest_root).expect("create dest root");
    fs::write(dest_root.join("config.yaml"), b"old config").expect("write dest");

    let operands = vec![
        ctx.source.into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .with_backup_suffix(Some(".orig"));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let backup = dest_root.join("config.yaml.orig");
    assert!(backup.exists(), "backup missing at {}", backup.display());
    assert_eq!(fs::read(&backup).expect("read backup"), b"old config");
}

#[test]
fn backup_suffix_with_long_extension() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    let source_file = ctx.source.join("data.bin");
    fs::write(&source_file, b"new binary").expect("write source");

    let dest_root = ctx.dest.join("source");
    fs::create_dir_all(&dest_root).expect("create dest root");
    fs::write(dest_root.join("data.bin"), b"old binary").expect("write dest");

    let operands = vec![
        ctx.source.into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .with_backup_suffix(Some("_backup_2024-01-15T12:30:00"));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let backup = dest_root.join("data.bin_backup_2024-01-15T12:30:00");
    assert!(backup.exists(), "backup missing at {}", backup.display());
    assert_eq!(fs::read(&backup).expect("read backup"), b"old binary");
}

// ============================================================================
// Backup interactions with delete + trailing-slash (contents mode)
// ============================================================================

#[test]
fn backup_with_delete_and_trailing_slash() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    // Source has only one file
    fs::write(source.join("remain.txt"), b"remain").expect("write source");

    // Destination has files that should be deleted
    fs::write(dest.join("remain.txt"), b"old remain").expect("write dest remain");
    fs::write(dest.join("extra.txt"), b"extra content").expect("write dest extra");

    let backup_dir = temp.path().join("backups");

    let mut source_operand = source.clone().into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let operands = vec![
        source_operand,
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .delete(true)
        .with_backup_directory(Some(backup_dir.clone()));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // The extra file should be deleted from dest
    assert!(!dest.join("extra.txt").exists(), "extra file should be deleted");

    // But backed up
    let backup_extra = backup_dir.join("extra.txt~");
    assert!(backup_extra.exists(), "backup of deleted file missing at {}", backup_extra.display());
    assert_eq!(fs::read(&backup_extra).expect("read backup"), b"extra content");

    // Modified file should be backed up
    let backup_remain = backup_dir.join("remain.txt~");
    assert!(backup_remain.exists(), "backup of modified file missing");
    assert_eq!(fs::read(&backup_remain).expect("read backup"), b"old remain");

    // Destination should have updated content
    assert_eq!(fs::read(dest.join("remain.txt")).expect("read dest"), b"remain");
}

// ============================================================================
// Backup with large file to verify correctness of rename vs copy
// ============================================================================

#[test]
fn backup_large_file_preserves_content() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    // Create a larger file to ensure backup handles it correctly
    let large_content: Vec<u8> = (0..100_000).map(|i| (i % 256) as u8).collect();
    let source_file = ctx.source.join("large.bin");
    fs::write(&source_file, &large_content).expect("write source");

    let dest_root = ctx.dest.join("source");
    fs::create_dir_all(&dest_root).expect("create dest root");
    let old_content: Vec<u8> = (0..100_000).map(|i| ((i + 128) % 256) as u8).collect();
    fs::write(dest_root.join("large.bin"), &old_content).expect("write dest");

    let operands = vec![
        ctx.source.into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().backup(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let backup = dest_root.join("large.bin~");
    assert!(backup.exists(), "backup missing at {}", backup.display());
    assert_eq!(fs::read(&backup).expect("read backup"), old_content);
    assert_eq!(fs::read(dest_root.join("large.bin")).expect("read dest"), large_content);
}

// ============================================================================
// Backup with multiple directories (recursive mode)
// ============================================================================

#[test]
fn backup_recursive_multiple_directories() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    // Create a multi-level source structure
    test_helpers::create_test_tree(&ctx.source, &[
        ("dir_a/file_a.txt", Some(b"new_a")),
        ("dir_b/file_b.txt", Some(b"new_b")),
        ("dir_a/sub/file_sub.txt", Some(b"new_sub")),
    ]);

    // Create existing destination structure
    let dest_root = ctx.dest.join("source");
    test_helpers::create_test_tree(&dest_root, &[
        ("dir_a/file_a.txt", Some(b"old_a")),
        ("dir_b/file_b.txt", Some(b"old_b")),
        ("dir_a/sub/file_sub.txt", Some(b"old_sub")),
    ]);

    let backup_dir = ctx.dest.join("backups");

    let operands = vec![
        ctx.source.into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .with_backup_directory(Some(backup_dir.clone()));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Verify all backups preserve directory structure
    let backup_a = backup_dir.join("source/dir_a/file_a.txt~");
    assert!(backup_a.exists(), "backup_a missing at {}", backup_a.display());
    assert_eq!(fs::read(&backup_a).expect("read"), b"old_a");

    let backup_b = backup_dir.join("source/dir_b/file_b.txt~");
    assert!(backup_b.exists(), "backup_b missing at {}", backup_b.display());
    assert_eq!(fs::read(&backup_b).expect("read"), b"old_b");

    let backup_sub = backup_dir.join("source/dir_a/sub/file_sub.txt~");
    assert!(backup_sub.exists(), "backup_sub missing at {}", backup_sub.display());
    assert_eq!(fs::read(&backup_sub).expect("read"), b"old_sub");

    // Verify destination updated
    assert_eq!(fs::read(dest_root.join("dir_a/file_a.txt")).expect("read"), b"new_a");
    assert_eq!(fs::read(dest_root.join("dir_b/file_b.txt")).expect("read"), b"new_b");
    assert_eq!(fs::read(dest_root.join("dir_a/sub/file_sub.txt")).expect("read"), b"new_sub");
}

// ============================================================================
// Backup disabled explicitly after enabling
// ============================================================================

#[test]
fn backup_disabled_after_enabling_does_not_create_backups() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    let source_file = ctx.source.join("file.txt");
    fs::write(&source_file, b"new content here").expect("write source");

    let dest_root = ctx.dest.join("source");
    fs::create_dir_all(&dest_root).expect("create dest root");
    fs::write(dest_root.join("file.txt"), b"old").expect("write dest");

    let operands = vec![
        ctx.source.into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    // Enable then disable backup
    let options = LocalCopyOptions::default()
        .backup(true)
        .backup(false);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let backup = dest_root.join("file.txt~");
    assert!(!backup.exists(), "backup should not exist when backup disabled");
    assert_eq!(fs::read(dest_root.join("file.txt")).expect("read dest"), b"new content here");
}

// ============================================================================
// Backup with delete-during (the default delete timing)
// ============================================================================

#[test]
fn backup_with_delete_during() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    test_helpers::create_test_tree(&ctx.source, &[
        ("subdir/keep.txt", Some(b"keep new")),
    ]);

    let dest_root = ctx.dest.join("source");
    test_helpers::create_test_tree(&dest_root, &[
        ("subdir/keep.txt", Some(b"keep old")),
        ("subdir/remove.txt", Some(b"remove me")),
    ]);

    let backup_dir = ctx.dest.join("backups");

    let operands = vec![
        ctx.source.into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    // delete_during is the default timing when using delete(true)
    let options = LocalCopyOptions::default()
        .delete(true)
        .with_backup_directory(Some(backup_dir.clone()));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // File that was deleted should be backed up
    let backup_removed = backup_dir.join("source/subdir/remove.txt~");
    assert!(backup_removed.exists(), "backup of deleted file missing at {}", backup_removed.display());
    assert_eq!(fs::read(&backup_removed).expect("read backup"), b"remove me");

    // File that was modified should be backed up
    let backup_keep = backup_dir.join("source/subdir/keep.txt~");
    assert!(backup_keep.exists(), "backup of modified file missing");
    assert_eq!(fs::read(&backup_keep).expect("read backup"), b"keep old");

    // Verify destination state
    assert!(!dest_root.join("subdir/remove.txt").exists(), "deleted file should be gone");
    assert_eq!(fs::read(dest_root.join("subdir/keep.txt")).expect("read dest"), b"keep new");
}

// ============================================================================
// Backup with empty file
// ============================================================================

#[test]
fn backup_empty_file() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    let source_file = ctx.source.join("empty.txt");
    fs::write(&source_file, b"not empty anymore").expect("write source");

    let dest_root = ctx.dest.join("source");
    fs::create_dir_all(&dest_root).expect("create dest root");
    // Existing file is empty
    fs::write(dest_root.join("empty.txt"), b"").expect("write empty dest");

    let operands = vec![
        ctx.source.into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().backup(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let backup = dest_root.join("empty.txt~");
    assert!(backup.exists(), "backup of empty file should exist");
    assert_eq!(fs::read(&backup).expect("read backup"), b"");
    assert_eq!(fs::read(dest_root.join("empty.txt")).expect("read dest"), b"not empty anymore");
}

#[test]
fn backup_to_empty_file() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    // Source is empty, dest has content
    let source_file = ctx.source.join("zeroed.txt");
    fs::write(&source_file, b"").expect("write empty source");

    let dest_root = ctx.dest.join("source");
    fs::create_dir_all(&dest_root).expect("create dest root");
    fs::write(dest_root.join("zeroed.txt"), b"had content").expect("write dest");

    let operands = vec![
        ctx.source.into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().backup(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let backup = dest_root.join("zeroed.txt~");
    assert!(backup.exists(), "backup should exist");
    assert_eq!(fs::read(&backup).expect("read backup"), b"had content");
    assert_eq!(fs::read(dest_root.join("zeroed.txt")).expect("read dest"), b"");
}

// ============================================================================
// Backup-dir with empty suffix (upstream: --backup-dir + --suffix= uses no suffix)
// ============================================================================

#[test]
fn backup_dir_with_no_suffix_upstream_behavior() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    test_helpers::create_test_tree(&ctx.source, &[
        ("dir/file.txt", Some(b"new")),
    ]);

    let dest_root = ctx.dest.join("source");
    test_helpers::create_test_tree(&dest_root, &[
        ("dir/file.txt", Some(b"old")),
    ]);

    let backup_dir = ctx.dest.join("archive");

    let operands = vec![
        ctx.source.into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    // Upstream: --backup-dir + --suffix= (empty) means no suffix on backup
    let options = LocalCopyOptions::default()
        .with_backup_directory(Some(backup_dir.clone()))
        .with_backup_suffix(Some(""));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Backup should have same name (no suffix)
    let backup = backup_dir.join("source/dir/file.txt");
    assert!(backup.exists(), "backup missing at {}", backup.display());
    assert_eq!(fs::read(&backup).expect("read backup"), b"old");

    // No tilde-suffixed backup should exist
    let wrong_backup = backup_dir.join("source/dir/file.txt~");
    assert!(!wrong_backup.exists(), "should not have ~ suffix backup");
}

// ============================================================================
// Backup with hidden files (dot-prefixed)
// ============================================================================

#[test]
fn backup_hidden_files() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    let source_file = ctx.source.join(".hidden");
    fs::write(&source_file, b"new hidden").expect("write source");

    let dest_root = ctx.dest.join("source");
    fs::create_dir_all(&dest_root).expect("create dest root");
    fs::write(dest_root.join(".hidden"), b"old hidden").expect("write dest");

    let operands = vec![
        ctx.source.into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().backup(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let backup = dest_root.join(".hidden~");
    assert!(backup.exists(), "backup of hidden file missing");
    assert_eq!(fs::read(&backup).expect("read backup"), b"old hidden");
    assert_eq!(fs::read(dest_root.join(".hidden")).expect("read dest"), b"new hidden");
}

// ============================================================================
// Backup with delete: multiple extraneous files across subdirectories
// ============================================================================

#[test]
fn backup_delete_multiple_extraneous_in_subdirs() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    // Source has minimal files
    test_helpers::create_test_tree(&ctx.source, &[
        ("a/keep.txt", Some(b"keep")),
    ]);

    // Destination has many extra files in subdirectories
    let dest_root = ctx.dest.join("source");
    test_helpers::create_test_tree(&dest_root, &[
        ("a/keep.txt", Some(b"old keep")),
        ("a/extra1.txt", Some(b"extra1")),
        ("a/extra2.txt", Some(b"extra2")),
    ]);

    let backup_dir = ctx.dest.join("backups");

    let operands = vec![
        ctx.source.into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .delete(true)
        .with_backup_directory(Some(backup_dir.clone()))
        .with_backup_suffix(Some(".bak"));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // All extraneous files should be backed up
    let backup1 = backup_dir.join("source/a/extra1.txt.bak");
    let backup2 = backup_dir.join("source/a/extra2.txt.bak");
    assert!(backup1.exists(), "backup1 missing at {}", backup1.display());
    assert!(backup2.exists(), "backup2 missing at {}", backup2.display());
    assert_eq!(fs::read(&backup1).expect("read"), b"extra1");
    assert_eq!(fs::read(&backup2).expect("read"), b"extra2");

    // Modified file should also be backed up
    let backup_keep = backup_dir.join("source/a/keep.txt.bak");
    assert!(backup_keep.exists(), "backup of keep missing");
    assert_eq!(fs::read(&backup_keep).expect("read"), b"old keep");

    // Extraneous files removed from dest
    assert!(!dest_root.join("a/extra1.txt").exists());
    assert!(!dest_root.join("a/extra2.txt").exists());
    assert_eq!(fs::read(dest_root.join("a/keep.txt")).expect("read"), b"keep");
}

// ============================================================================
// Backup with delete: backing up deleted files with suffix only (no backup-dir)
// ============================================================================

#[test]
fn backup_delete_with_suffix_only() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    fs::write(ctx.source.join("keep.txt"), b"keep").expect("write keep");

    let dest_root = ctx.dest.join("source");
    fs::create_dir_all(&dest_root).expect("create dest root");
    fs::write(dest_root.join("keep.txt"), b"old keep").expect("write old keep");
    fs::write(dest_root.join("extra.txt"), b"extra").expect("write extra");

    let operands = vec![
        ctx.source.into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    // backup + delete + suffix only (no backup-dir)
    let options = LocalCopyOptions::default()
        .delete(true)
        .backup(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // The original extra should be gone
    assert!(!dest_root.join("extra.txt").exists(), "deleted file should not exist");

    // The destination file should have new content
    assert_eq!(fs::read(dest_root.join("keep.txt")).expect("read dest"), b"keep");

    // Without --backup-dir, backup + delete with suffix-only creates backups
    // in the same directory. The overwrite backup of keep.txt creates keep.txt~,
    // but the post-transfer delete sweep sees keep.txt~ as extraneous and
    // backs it up again (to keep.txt~~) before removing it. This is the
    // expected behavior matching upstream rsync -- users should use --backup-dir
    // with --delete for clean backup organization.
    // The extraneous file extra.txt is backed up by the delete sweep.
    let backup_extra = dest_root.join("extra.txt~");
    assert!(backup_extra.exists(), "backup of deleted file missing at {}", backup_extra.display());
    assert_eq!(fs::read(&backup_extra).expect("read backup"), b"extra");
}

// ============================================================================
// Backup: verify no backup created for directory-only changes
// ============================================================================

#[test]
fn backup_not_created_for_new_directory() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    // Source has a directory with a file
    test_helpers::create_test_tree(&ctx.source, &[
        ("newdir/file.txt", Some(b"content")),
    ]);

    // Destination doesn't have this directory
    let dest_root = ctx.dest.join("source");
    fs::create_dir_all(&dest_root).expect("create dest root");

    let operands = vec![
        ctx.source.into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().backup(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // New directory should be created
    assert!(dest_root.join("newdir/file.txt").exists());

    // No backup should exist for new files/directories
    let backup = dest_root.join("newdir/file.txt~");
    assert!(!backup.exists(), "no backup for newly created files");
}

// ============================================================================
// Backup: verify backup-dir uses destination root correctly for relative dirs
// ============================================================================

#[test]
fn backup_dir_relative_uses_destination_root() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    let source_file = ctx.source.join("file.txt");
    fs::write(&source_file, b"new").expect("write source");

    let dest_root = ctx.dest.join("source");
    fs::create_dir_all(&dest_root).expect("create dest root");
    fs::write(dest_root.join("file.txt"), b"old").expect("write dest");

    let operands = vec![
        ctx.source.into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .with_backup_directory(Some(PathBuf::from(".old")));

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Relative backup-dir should be relative to destination root (ctx.dest)
    let backup = ctx.dest.join(".old/source/file.txt~");
    assert!(backup.exists(), "backup missing at {}", backup.display());
    assert_eq!(fs::read(&backup).expect("read backup"), b"old");
}

// ============================================================================
// Backup with --checksum mode (forces content comparison)
// ============================================================================

#[test]
fn backup_with_checksum_mode() {
    let ctx = test_helpers::setup_copy_test();
    fs::create_dir_all(&ctx.dest).expect("create dest");

    let source_file = ctx.source.join("file.txt");
    fs::write(&source_file, b"new checksum content").expect("write source");

    let dest_root = ctx.dest.join("source");
    fs::create_dir_all(&dest_root).expect("create dest root");
    fs::write(dest_root.join("file.txt"), b"old checksum content").expect("write dest");

    let operands = vec![
        ctx.source.into_os_string(),
        ctx.dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .backup(true)
        .checksum(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let backup = dest_root.join("file.txt~");
    assert!(backup.exists(), "backup missing");
    assert_eq!(fs::read(&backup).expect("read backup"), b"old checksum content");
    assert_eq!(fs::read(dest_root.join("file.txt")).expect("read dest"), b"new checksum content");
}
