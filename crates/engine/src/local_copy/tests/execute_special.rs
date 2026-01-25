
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
fn execute_fifo_replaces_directory_when_force_enabled() {
    use std::os::unix::fs::FileTypeExt;

    let temp = tempdir().expect("tempdir");
    let source_fifo = temp.path().join("source.pipe");
    mkfifo_for_tests(&source_fifo, 0o600).expect("mkfifo");

    let destination_fifo = temp.path().join("dest.pipe");
    fs::create_dir_all(&destination_fifo).expect("create conflicting directory");

    let operands = vec![
        source_fifo.into_os_string(),
        destination_fifo.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().specials(true).force_replacements(true),
    )
    .expect("forced replacement succeeds");

    let metadata = fs::symlink_metadata(&destination_fifo).expect("dest metadata");
    assert!(metadata.file_type().is_fifo());
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

    let source_fifo_path = source_fifo;
    let dest_root = temp.path().join("dest");
    let mut source_operand = source_root.into_os_string();
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
fn execute_preserves_fifo_hard_links() {
    use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source root");
    let fifo_a = source_root.join("pipe-a");
    mkfifo_for_tests(&fifo_a, 0o600).expect("mkfifo a");
    let fifo_b = source_root.join("pipe-b");
    fs::hard_link(&fifo_a, &fifo_b).expect("link fifo");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .hard_links(true)
                .specials(true)
                .permissions(true),
        )
        .expect("copy succeeds");

    let dest_a = dest_root.join("pipe-a");
    let dest_b = dest_root.join("pipe-b");
    let meta_a = fs::symlink_metadata(&dest_a).expect("dest a metadata");
    let meta_b = fs::symlink_metadata(&dest_b).expect("dest b metadata");

    assert!(meta_a.file_type().is_fifo());
    assert!(meta_b.file_type().is_fifo());
    assert_eq!(meta_a.ino(), meta_b.ino());
    assert_eq!(meta_a.nlink(), 2);
    assert_eq!(meta_b.nlink(), 2);
    assert_eq!(meta_a.permissions().mode() & 0o777, 0o600);
    assert_eq!(meta_b.permissions().mode() & 0o777, 0o600);
    assert!(summary.hard_links_created() >= 1);
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
        source_fifo.into_os_string(),
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

#[cfg(unix)]
#[test]
fn execute_copies_devices_as_regular_files_when_requested() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest root");

    let operands = vec![
        OsString::from("/dev/zero"),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .copy_devices_as_files(true)
                .permissions(true)
                .times(true),
        )
        .expect("device copy succeeds");

    let destination = dest_root.join("zero");
    let metadata = fs::metadata(&destination).expect("destination metadata");
    assert!(metadata.is_file());
    assert_eq!(metadata.permissions().mode() & 0o777, 0o666);
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.devices_created(), 0);
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
        source_root.into_os_string(),
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
        source_root.into_os_string(),
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

// ==================== Symlink Tests ====================

#[cfg(unix)]
#[test]
fn execute_copies_symlink() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let target_file = temp.path().join("target.txt");
    fs::write(&target_file, b"target content").expect("write target");

    let source_link = temp.path().join("source_link");
    symlink(&target_file, &source_link).expect("create symlink");

    let dest_link = temp.path().join("dest_link");
    let operands = vec![
        source_link.into_os_string(),
        dest_link.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().links(true),
        )
        .expect("symlink copy succeeds");

    assert_eq!(summary.symlinks_copied(), 1);
    let dest_target = fs::read_link(&dest_link).expect("read dest link");
    assert_eq!(dest_target, target_file);
}

#[cfg(unix)]
#[test]
fn execute_copies_symlink_within_directory() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    let target_file = source_root.join("target.txt");
    fs::write(&target_file, b"target").expect("write target");

    let source_link = source_root.join("link");
    symlink(Path::new("target.txt"), &source_link).expect("create relative symlink");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().links(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.symlinks_copied(), 1);
    let dest_link = dest_root.join("link");
    let dest_target = fs::read_link(&dest_link).expect("read dest link");
    assert_eq!(dest_target, Path::new("target.txt"));
}

#[cfg(unix)]
#[test]
fn execute_copies_symlink_with_safe_links_keeps_safe() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    let target_file = source_root.join("target.txt");
    fs::write(&target_file, b"safe content").expect("write target");

    // Safe relative symlink within source tree
    let safe_link = source_root.join("safe_link");
    symlink(Path::new("target.txt"), &safe_link).expect("create safe symlink");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .links(true)
                .safe_links(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.symlinks_copied(), 1);
    let dest_link = dest_root.join("safe_link");
    assert!(fs::symlink_metadata(&dest_link).expect("meta").file_type().is_symlink());
}

#[cfg(unix)]
#[test]
fn execute_with_safe_links_skips_unsafe() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    // Create target outside source tree
    let outside_target = temp.path().join("outside.txt");
    fs::write(&outside_target, b"outside content").expect("write outside");

    // Create symlink pointing outside source tree (unsafe)
    let unsafe_link = source_root.join("unsafe_link");
    symlink(&outside_target, &unsafe_link).expect("create unsafe symlink");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let report = plan
        .execute_with_report(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .links(true)
                .safe_links(true)
                .collect_events(true),
        )
        .expect("copy succeeds");

    let summary = report.summary();
    assert_eq!(summary.symlinks_copied(), 0);
    assert!(!dest_root.join("unsafe_link").exists());
    assert!(report.records().iter().any(|record| {
        matches!(record.action(), LocalCopyAction::SkippedUnsafeSymlink)
    }));
}

// ==================== Hard Link Tests ====================

#[cfg(unix)]
#[test]
fn execute_preserves_hard_links_within_directory() {
    use std::os::unix::fs::MetadataExt;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    let file_a = source_root.join("file_a.txt");
    let file_b = source_root.join("file_b.txt");
    fs::write(&file_a, b"hard link content").expect("write file_a");
    fs::hard_link(&file_a, &file_b).expect("create hard link");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().hard_links(true),
        )
        .expect("copy succeeds");

    let dest_a = dest_root.join("file_a.txt");
    let dest_b = dest_root.join("file_b.txt");
    let meta_a = fs::metadata(&dest_a).expect("meta a");
    let meta_b = fs::metadata(&dest_b).expect("meta b");

    assert_eq!(meta_a.ino(), meta_b.ino());
    assert_eq!(meta_a.nlink(), 2);
    assert!(summary.hard_links_created() >= 1);
}

#[cfg(unix)]
#[test]
fn execute_without_hard_links_copies_separately() {
    use std::os::unix::fs::MetadataExt;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    let file_a = source_root.join("file_a.txt");
    let file_b = source_root.join("file_b.txt");
    fs::write(&file_a, b"content").expect("write file_a");
    fs::hard_link(&file_a, &file_b).expect("create hard link");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds");

    let dest_a = dest_root.join("file_a.txt");
    let dest_b = dest_root.join("file_b.txt");
    let meta_a = fs::metadata(&dest_a).expect("meta a");
    let meta_b = fs::metadata(&dest_b).expect("meta b");

    // Without hard_links option, files are copied separately
    assert_ne!(meta_a.ino(), meta_b.ino());
    assert_eq!(meta_a.nlink(), 1);
    assert_eq!(meta_b.nlink(), 1);
    assert_eq!(summary.files_copied(), 2);
}

// ==================== Dry Run Special Tests ====================

#[cfg(unix)]
#[test]
fn execute_dry_run_does_not_create_symlink() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let target_file = temp.path().join("target.txt");
    fs::write(&target_file, b"target").expect("write target");

    let source_link = temp.path().join("source_link");
    symlink(&target_file, &source_link).expect("create symlink");

    let dest_link = temp.path().join("dest_link");
    let operands = vec![
        source_link.into_os_string(),
        dest_link.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::DryRun,
            LocalCopyOptions::default().links(true),
        )
        .expect("dry run succeeds");

    assert_eq!(summary.symlinks_copied(), 1);
    assert!(!dest_link.exists());
}

#[cfg(unix)]
#[test]
fn execute_dry_run_does_not_create_fifo() {
    let temp = tempdir().expect("tempdir");
    let source_fifo = temp.path().join("source.pipe");
    mkfifo_for_tests(&source_fifo, 0o600).expect("mkfifo");

    let dest_fifo = temp.path().join("dest.pipe");
    let operands = vec![
        source_fifo.into_os_string(),
        dest_fifo.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::DryRun,
            LocalCopyOptions::default().specials(true),
        )
        .expect("dry run succeeds");

    assert_eq!(summary.fifos_created(), 1);
    assert!(!dest_fifo.exists());
}

// ==================== Symlink Edge Cases ====================

#[cfg(unix)]
#[test]
fn execute_copies_broken_symlink() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_link = temp.path().join("broken_link");
    symlink(Path::new("nonexistent_target"), &source_link).expect("create broken symlink");

    let dest_link = temp.path().join("dest_link");
    let operands = vec![
        source_link.into_os_string(),
        dest_link.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().links(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.symlinks_copied(), 1);
    let dest_target = fs::read_link(&dest_link).expect("read dest link");
    assert_eq!(dest_target, Path::new("nonexistent_target"));
}

// ==================== Multiple Special Files Tests ====================

#[cfg(unix)]
#[test]
fn execute_copies_mixed_special_files() {
    use std::os::unix::fs::{FileTypeExt, symlink};

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    // Create regular file
    fs::write(source_root.join("regular.txt"), b"regular").expect("write regular");

    // Create symlink
    let target = source_root.join("target.txt");
    fs::write(&target, b"target").expect("write target");
    symlink(Path::new("target.txt"), source_root.join("link")).expect("create symlink");

    // Create FIFO
    let fifo = source_root.join("fifo");
    mkfifo_for_tests(&fifo, 0o600).expect("mkfifo");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .links(true)
                .specials(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 2);
    assert_eq!(summary.symlinks_copied(), 1);
    assert_eq!(summary.fifos_created(), 1);

    assert!(dest_root.join("regular.txt").is_file());
    assert!(dest_root.join("target.txt").is_file());
    assert!(fs::symlink_metadata(dest_root.join("link"))
        .expect("meta")
        .file_type()
        .is_symlink());
    assert!(fs::symlink_metadata(dest_root.join("fifo"))
        .expect("meta")
        .file_type()
        .is_fifo());
}
