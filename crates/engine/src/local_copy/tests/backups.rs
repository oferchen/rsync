
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
