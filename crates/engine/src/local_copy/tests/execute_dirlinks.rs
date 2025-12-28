
#[cfg(unix)]
#[test]
fn execute_with_copy_unsafe_links_materialises_file_target() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("src");
    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create src dir");
    fs::create_dir_all(&dest_dir).expect("create dest dir");

    let outside_file = temp.path().join("outside.txt");
    fs::write(&outside_file, b"payload").expect("write outside file");

    let link_path = source_dir.join("escape");
    symlink(&outside_file, &link_path).expect("create symlink");
    let destination_path = dest_dir.join("escape");

    let operands = vec![
        link_path.into_os_string(),
        destination_path.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .links(true)
                .safe_links(true)
                .copy_unsafe_links(true),
        )
        .expect("copy succeeds");

    let metadata = fs::symlink_metadata(&destination_path).expect("materialised metadata");
    assert!(metadata.file_type().is_file());
    assert!(!metadata.file_type().is_symlink());
    assert_eq!(
        fs::read(&destination_path).expect("read materialised file"),
        b"payload"
    );
    assert_eq!(summary.symlinks_total(), 1);
    assert_eq!(summary.symlinks_copied(), 0);
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
fn execute_with_copy_unsafe_links_materialises_directory_target() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("src");
    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create src dir");
    fs::create_dir_all(&dest_dir).expect("create dest dir");

    let outside_dir = temp.path().join("outside-dir");
    fs::create_dir(&outside_dir).expect("create outside dir");
    let outside_file = outside_dir.join("file.txt");
    fs::write(&outside_file, b"external").expect("write outside file");

    let link_path = source_dir.join("dirlink");
    symlink(&outside_dir, &link_path).expect("create dir symlink");
    let destination_path = dest_dir.join("dirlink");

    let operands = vec![
        link_path.into_os_string(),
        destination_path.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .links(true)
                .safe_links(true)
                .copy_unsafe_links(true),
        )
        .expect("copy succeeds");

    let metadata = fs::symlink_metadata(&destination_path).expect("materialised metadata");
    assert!(metadata.file_type().is_dir());
    assert!(!metadata.file_type().is_symlink());
    let copied_file = destination_path.join("file.txt");
    assert_eq!(
        fs::read(&copied_file).expect("read copied file"),
        b"external"
    );
    assert_eq!(summary.symlinks_total(), 1);
    assert_eq!(summary.symlinks_copied(), 0);
    assert!(summary.directories_created() >= 1);
}

#[cfg(unix)]
#[test]
fn execute_with_keep_dirlinks_allows_destination_directory_symlink() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("src-dir");
    fs::create_dir(&source_dir).expect("create source dir");
    fs::write(source_dir.join("file.txt"), b"payload").expect("write source file");

    let actual_destination = temp.path().join("actual-destination");
    fs::create_dir(&actual_destination).expect("create destination dir");
    let destination_link = temp.path().join("dest-link");
    symlink(&actual_destination, &destination_link).expect("create destination link");

    let operands = vec![
        source_dir.into_os_string(),
        destination_link.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().keep_dirlinks(true),
        )
        .expect("copy succeeds");

    let copied_file = actual_destination.join("src-dir").join("file.txt");
    assert_eq!(
        fs::read(&copied_file).expect("read copied file"),
        b"payload"
    );
    assert!(
        fs::symlink_metadata(&destination_link)
            .expect("destination link metadata")
            .file_type()
            .is_symlink()
    );
    assert!(summary.directories_created() >= 1);
}

#[cfg(unix)]
#[test]
fn execute_without_keep_dirlinks_rejects_destination_directory_symlink() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("src-dir");
    fs::create_dir(&source_dir).expect("create source dir");
    fs::write(source_dir.join("file.txt"), b"payload").expect("write source file");

    let actual_destination = temp.path().join("actual-destination");
    fs::create_dir(&actual_destination).expect("create destination dir");
    let destination_link = temp.path().join("dest-link");
    symlink(&actual_destination, &destination_link).expect("create destination link");

    let operands = vec![
        source_dir.into_os_string(),
        destination_link.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let result = plan.execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default());

    let error = result.expect_err("keep-dirlinks disabled should reject destination symlink");
    assert!(matches!(
        error.kind(),
        LocalCopyErrorKind::InvalidArgument(
            LocalCopyArgumentError::ReplaceNonDirectoryWithDirectory
        )
    ));
    assert!(
        fs::symlink_metadata(&destination_link)
            .expect("destination link metadata")
            .file_type()
            .is_symlink()
    );
    assert!(!actual_destination.join("src-dir").join("file.txt").exists());
}
