
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
