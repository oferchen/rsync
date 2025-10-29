
#[cfg(unix)]
#[test]
fn execute_copies_fifo() {
    use filetime::{FileTime, set_file_times};
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};

    let temp = tempdir().expect("tempdir");
    let source_fifo = temp.path().join("source.pipe");
    mkfifo_for_tests(&source_fifo, 0o640).expect("mkfifo");

    let atime = FileTime::from_unix_time(1_700_050_000, 123_000_000);
    let mtime = FileTime::from_unix_time(1_700_060_000, 456_000_000);
    set_file_times(&source_fifo, atime, mtime).expect("set fifo timestamps");
    fs::set_permissions(&source_fifo, PermissionsExt::from_mode(0o640))
        .expect("set fifo permissions");

    let source_fifo_path = source_fifo.clone();
    let destination_fifo = temp.path().join("dest.pipe");
    let operands = vec![
        source_fifo.into_os_string(),
        destination_fifo.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let src_metadata = fs::symlink_metadata(&source_fifo_path).expect("source metadata");
    assert_eq!(src_metadata.permissions().mode() & 0o777, 0o640);
    let src_atime = FileTime::from_last_access_time(&src_metadata);
    let src_mtime = FileTime::from_last_modification_time(&src_metadata);
    assert_eq!(src_atime, atime);
    assert_eq!(src_mtime, mtime);

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .permissions(true)
                .times(true)
                .specials(true),
        )
        .expect("fifo copy succeeds");

    let dest_metadata = fs::symlink_metadata(&destination_fifo).expect("dest metadata");
    assert!(dest_metadata.file_type().is_fifo());
    assert_eq!(dest_metadata.permissions().mode() & 0o777, 0o640);
    let dest_atime = FileTime::from_last_access_time(&dest_metadata);
    let dest_mtime = FileTime::from_last_modification_time(&dest_metadata);
    assert_eq!(dest_atime, atime);
    assert_eq!(dest_mtime, mtime);
    assert_eq!(summary.fifos_created(), 1);
}

#[cfg(unix)]
#[test]
fn execute_copies_fifo_within_directory() {
    use filetime::{FileTime, set_file_times};
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let nested = source_root.join("dir");
    fs::create_dir_all(&nested).expect("create nested");

    let source_fifo = nested.join("pipe");
    mkfifo_for_tests(&source_fifo, 0o620).expect("mkfifo");

    let atime = FileTime::from_unix_time(1_700_070_000, 111_000_000);
    let mtime = FileTime::from_unix_time(1_700_080_000, 222_000_000);
    set_file_times(&source_fifo, atime, mtime).expect("set fifo timestamps");
    fs::set_permissions(&source_fifo, PermissionsExt::from_mode(0o620))
        .expect("set fifo permissions");

    let source_fifo_path = source_fifo.clone();
    let dest_root = temp.path().join("dest");
    let mut source_operand = source_root.clone().into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let src_metadata = fs::symlink_metadata(&source_fifo_path).expect("source metadata");
    assert_eq!(src_metadata.permissions().mode() & 0o777, 0o620);
    let src_atime = FileTime::from_last_access_time(&src_metadata);
    let src_mtime = FileTime::from_last_modification_time(&src_metadata);
    assert_eq!(src_atime, atime);
    assert_eq!(src_mtime, mtime);

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .permissions(true)
                .times(true)
                .specials(true),
        )
        .expect("fifo copy succeeds");

    let dest_fifo = dest_root.join("dir").join("pipe");
    let metadata = fs::symlink_metadata(&dest_fifo).expect("dest fifo metadata");
    assert!(metadata.file_type().is_fifo());
    assert_eq!(metadata.permissions().mode() & 0o777, 0o620);
    let dest_atime = FileTime::from_last_access_time(&metadata);
    let dest_mtime = FileTime::from_last_modification_time(&metadata);
    assert_eq!(dest_atime, atime);
    assert_eq!(dest_mtime, mtime);
    assert_eq!(summary.fifos_created(), 1);
}

#[cfg(unix)]
#[test]
fn execute_without_specials_skips_fifo() {
    let temp = tempdir().expect("tempdir");
    let source_fifo = temp.path().join("source.pipe");
    mkfifo_for_tests(&source_fifo, 0o600).expect("mkfifo");

    let destination_fifo = temp.path().join("dest.pipe");
    let operands = vec![
        source_fifo.into_os_string(),
        destination_fifo.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds without specials");

    assert_eq!(summary.fifos_created(), 0);
    assert!(fs::symlink_metadata(&destination_fifo).is_err());
}

#[cfg(unix)]
#[test]
fn execute_without_specials_records_skip_event() {
    let temp = tempdir().expect("tempdir");
    let source_fifo = temp.path().join("skip.pipe");
    mkfifo_for_tests(&source_fifo, 0o600).expect("mkfifo");

    let destination = temp.path().join("dest.pipe");
    let operands = vec![
        source_fifo.clone().into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let report = plan
        .execute_with_report(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().collect_events(true),
        )
        .expect("copy executes");

    assert!(fs::symlink_metadata(&destination).is_err());
    assert!(report.records().iter().any(|record| {
        record.action() == &LocalCopyAction::SkippedNonRegular
            && record.relative_path() == Path::new("skip.pipe")
    }));
}

#[test]
fn execute_with_one_file_system_skips_mount_points() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let mount_dir = source_root.join("mount");
    let mount_file = mount_dir.join("inside.txt");
    let data_dir = source_root.join("data");
    let data_file = data_dir.join("file.txt");
    fs::create_dir_all(&mount_dir).expect("create mount dir");
    fs::create_dir_all(&data_dir).expect("create data dir");
    fs::write(&mount_file, b"other fs").expect("write mount file");
    fs::write(&data_file, b"same fs").expect("write data file");

    let destination = temp.path().join("dest");
    let operands = vec![
        source_root.clone().into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let report = with_device_id_override(
        |path, _metadata| {
            if path
                .components()
                .any(|component| component.as_os_str() == std::ffi::OsStr::new("mount"))
            {
                Some(2)
            } else {
                Some(1)
            }
        },
        || {
            plan.execute_with_report(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default()
                    .one_file_system(true)
                    .collect_events(true),
            )
        },
    )
    .expect("copy executes");

    assert!(destination.join("data").join("file.txt").exists());
    assert!(!destination.join("mount").exists());
    assert!(report.records().iter().any(|record| {
        record.action() == &LocalCopyAction::SkippedMountPoint
            && record.relative_path().to_string_lossy().contains("mount")
    }));
}

#[test]
fn execute_without_one_file_system_crosses_mount_points() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let mount_dir = source_root.join("mount");
    let mount_file = mount_dir.join("inside.txt");
    fs::create_dir_all(&mount_dir).expect("create mount dir");
    fs::write(&mount_file, b"other fs").expect("write mount file");

    let destination = temp.path().join("dest");
    let operands = vec![
        source_root.clone().into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let report = with_device_id_override(
        |path, _metadata| {
            if path
                .components()
                .any(|component| component.as_os_str() == std::ffi::OsStr::new("mount"))
            {
                Some(2)
            } else {
                Some(1)
            }
        },
        || {
            plan.execute_with_report(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default().collect_events(true),
            )
        },
    )
    .expect("copy executes");

    assert!(destination.join("mount").join("inside.txt").exists());
    assert!(
        report
            .records()
            .iter()
            .all(|record| { record.action() != &LocalCopyAction::SkippedMountPoint })
    );
}
