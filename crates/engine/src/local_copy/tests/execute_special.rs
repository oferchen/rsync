
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

// ==================== Symlink to Special File Tests ====================

#[cfg(unix)]
#[test]
fn execute_symlink_pointing_to_fifo_preserved_as_symlink() {
    use std::os::unix::fs::{symlink, FileTypeExt};

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    // Create a FIFO and a symlink pointing to it
    let fifo = source_root.join("real.pipe");
    mkfifo_for_tests(&fifo, 0o600).expect("mkfifo");

    let link = source_root.join("link_to_pipe");
    symlink(Path::new("real.pipe"), &link).expect("create symlink to fifo");

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

    // The symlink should be preserved as a symlink
    let dest_link = dest_root.join("link_to_pipe");
    let link_meta = fs::symlink_metadata(&dest_link).expect("link meta");
    assert!(link_meta.file_type().is_symlink());
    assert_eq!(
        fs::read_link(&dest_link).expect("read link"),
        Path::new("real.pipe")
    );

    // The FIFO should be recreated as a FIFO
    let dest_fifo = dest_root.join("real.pipe");
    let fifo_meta = fs::symlink_metadata(&dest_fifo).expect("fifo meta");
    assert!(fifo_meta.file_type().is_fifo());

    assert_eq!(summary.symlinks_copied(), 1);
    assert_eq!(summary.fifos_created(), 1);
}

#[cfg(unix)]
#[test]
fn execute_symlink_pointing_to_socket_preserved_as_symlink() {
    use std::os::unix::fs::{symlink, FileTypeExt};

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    // Create a socket and a symlink pointing to it
    let socket = source_root.join("real.sock");
    mksocket_for_tests(&socket).expect("mksocket");

    let link = source_root.join("link_to_sock");
    symlink(Path::new("real.sock"), &link).expect("create symlink to socket");

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

    // The symlink should be preserved as a symlink
    let dest_link = dest_root.join("link_to_sock");
    let link_meta = fs::symlink_metadata(&dest_link).expect("link meta");
    assert!(link_meta.file_type().is_symlink());
    assert_eq!(
        fs::read_link(&dest_link).expect("read link"),
        Path::new("real.sock")
    );

    // The socket should be recreated
    let dest_socket = dest_root.join("real.sock");
    let socket_meta = fs::symlink_metadata(&dest_socket).expect("socket meta");
    assert!(socket_meta.file_type().is_socket());

    assert_eq!(summary.symlinks_copied(), 1);
    assert_eq!(summary.fifos_created(), 1);
}

// ==================== FIFO Replacement Tests ====================

#[cfg(unix)]
#[test]
fn execute_fifo_replaces_existing_symlink() {
    use std::os::unix::fs::{symlink, FileTypeExt};

    let temp = tempdir().expect("tempdir");
    let source_fifo = temp.path().join("source.pipe");
    mkfifo_for_tests(&source_fifo, 0o600).expect("mkfifo");

    // Destination is currently a symlink
    let dest = temp.path().join("dest.pipe");
    let dummy_target = temp.path().join("dummy.txt");
    fs::write(&dummy_target, b"dummy").expect("write dummy");
    symlink(&dummy_target, &dest).expect("create dest symlink");

    let operands = vec![
        source_fifo.into_os_string(),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().specials(true),
    )
    .expect("fifo replacement succeeds");

    let metadata = fs::symlink_metadata(&dest).expect("dest metadata");
    assert!(
        metadata.file_type().is_fifo(),
        "symlink should be replaced by FIFO"
    );
    assert!(
        !metadata.file_type().is_symlink(),
        "should no longer be a symlink"
    );
}

#[cfg(unix)]
#[test]
fn execute_fifo_replaces_existing_regular_file() {
    use std::os::unix::fs::FileTypeExt;

    let temp = tempdir().expect("tempdir");
    let source_fifo = temp.path().join("source.pipe");
    mkfifo_for_tests(&source_fifo, 0o600).expect("mkfifo");

    // Destination is currently a regular file
    let dest = temp.path().join("dest.pipe");
    fs::write(&dest, b"regular file content").expect("write dest file");

    let operands = vec![
        source_fifo.into_os_string(),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().specials(true),
    )
    .expect("fifo replacement succeeds");

    let metadata = fs::symlink_metadata(&dest).expect("dest metadata");
    assert!(
        metadata.file_type().is_fifo(),
        "regular file should be replaced by FIFO"
    );
}

#[cfg(unix)]
#[test]
fn execute_symlink_replaces_existing_fifo() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let target = temp.path().join("target.txt");
    fs::write(&target, b"target content").expect("write target");

    let source_link = temp.path().join("source_link");
    symlink(&target, &source_link).expect("create symlink");

    // Destination is currently a FIFO
    let dest = temp.path().join("dest_link");
    mkfifo_for_tests(&dest, 0o600).expect("mkfifo at dest");

    let operands = vec![
        source_link.into_os_string(),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().links(true),
    )
    .expect("symlink replacement succeeds");

    let metadata = fs::symlink_metadata(&dest).expect("dest metadata");
    assert!(
        metadata.file_type().is_symlink(),
        "FIFO should be replaced by symlink"
    );
    assert_eq!(fs::read_link(&dest).expect("read link"), target);
}

#[cfg(unix)]
#[test]
fn execute_symlink_replaces_existing_socket() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let target = temp.path().join("target.txt");
    fs::write(&target, b"target content").expect("write target");

    let source_link = temp.path().join("source_link");
    symlink(&target, &source_link).expect("create symlink");

    // Destination is currently a socket
    let dest = temp.path().join("dest_link");
    mksocket_for_tests(&dest).expect("mksocket at dest");

    let operands = vec![
        source_link.into_os_string(),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().links(true),
    )
    .expect("symlink replacement succeeds");

    let metadata = fs::symlink_metadata(&dest).expect("dest metadata");
    assert!(
        metadata.file_type().is_symlink(),
        "socket should be replaced by symlink"
    );
    assert_eq!(fs::read_link(&dest).expect("read link"), target);
}

// ==================== FIFO Idempotent Re-copy ====================

#[cfg(unix)]
#[test]
fn execute_recopy_fifo_replaces_existing_fifo() {
    use std::os::unix::fs::FileTypeExt;

    let temp = tempdir().expect("tempdir");
    let source_fifo = temp.path().join("source.pipe");
    mkfifo_for_tests(&source_fifo, 0o600).expect("mkfifo source");

    // Pre-create a FIFO at the destination
    let dest_fifo = temp.path().join("dest.pipe");
    mkfifo_for_tests(&dest_fifo, 0o644).expect("mkfifo dest");

    let operands = vec![
        source_fifo.into_os_string(),
        dest_fifo.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .specials(true)
                .permissions(true),
        )
        .expect("re-copy succeeds");

    let metadata = fs::symlink_metadata(&dest_fifo).expect("dest metadata");
    assert!(metadata.file_type().is_fifo());
    assert_eq!(summary.fifos_created(), 1);
}

// ==================== Multiple FIFOs with Different Permissions ====================

#[cfg(unix)]
#[test]
fn execute_copies_multiple_fifos_with_different_permissions() {
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    // Create FIFOs with different permissions
    let fifo_public = source_root.join("public.pipe");
    mkfifo_for_tests(&fifo_public, 0o666).expect("mkfifo public");
    fs::set_permissions(&fifo_public, PermissionsExt::from_mode(0o666))
        .expect("set public permissions");

    let fifo_private = source_root.join("private.pipe");
    mkfifo_for_tests(&fifo_private, 0o600).expect("mkfifo private");
    fs::set_permissions(&fifo_private, PermissionsExt::from_mode(0o600))
        .expect("set private permissions");

    let fifo_group = source_root.join("group.pipe");
    mkfifo_for_tests(&fifo_group, 0o660).expect("mkfifo group");
    fs::set_permissions(&fifo_group, PermissionsExt::from_mode(0o660))
        .expect("set group permissions");

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
                .specials(true)
                .permissions(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.fifos_created(), 3);

    let dest_public = dest_root.join("public.pipe");
    let dest_private = dest_root.join("private.pipe");
    let dest_group = dest_root.join("group.pipe");

    assert!(fs::symlink_metadata(&dest_public).expect("meta").file_type().is_fifo());
    assert!(fs::symlink_metadata(&dest_private).expect("meta").file_type().is_fifo());
    assert!(fs::symlink_metadata(&dest_group).expect("meta").file_type().is_fifo());

    assert_eq!(
        fs::symlink_metadata(&dest_public).expect("meta").permissions().mode() & 0o777,
        0o666
    );
    assert_eq!(
        fs::symlink_metadata(&dest_private).expect("meta").permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::symlink_metadata(&dest_group).expect("meta").permissions().mode() & 0o777,
        0o660
    );
}

// ==================== Specials with Delete ====================

#[cfg(unix)]
#[test]
fn execute_delete_removes_extraneous_fifo() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");
    fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest");
    fs::write(dest_root.join("keep.txt"), b"keep").expect("write keep");

    // Create an extraneous FIFO in the destination
    let extraneous_fifo = dest_root.join("extraneous.pipe");
    mkfifo_for_tests(&extraneous_fifo, 0o600).expect("mkfifo");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .specials(true)
                .delete(true),
        )
        .expect("delete copy succeeds");

    assert!(
        !extraneous_fifo.exists(),
        "extraneous FIFO should be deleted"
    );
    assert!(dest_root.join("keep.txt").is_file(), "keep.txt should remain");
    assert!(summary.items_deleted() >= 1);
}

// ==================== Device File Skipped Without Proper Option ====================

#[cfg(unix)]
#[test]
fn execute_device_file_skipped_without_devices_or_copy_devices() {
    let temp = tempdir().expect("tempdir");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest root");

    let operands = vec![
        OsString::from("/dev/zero"),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Without devices(true) or copy_devices_as_files(true), device should be skipped
    let report = plan
        .execute_with_report(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().collect_events(true),
        )
        .expect("copy executes");

    let summary = report.summary();
    assert_eq!(summary.devices_created(), 0);
    assert_eq!(summary.files_copied(), 0);
    assert!(!dest_root.join("zero").exists());
    assert!(report.records().iter().any(|record| {
        record.action() == &LocalCopyAction::SkippedNonRegular
    }));
}

// ==================== copy_links with Symlink to FIFO (specials disabled) ====================

#[cfg(unix)]
#[test]
fn execute_copy_links_follows_symlink_to_fifo_specials_disabled_skips() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    fs::create_dir(&source_dir).expect("create source");

    // Create a FIFO outside the source tree
    let real_fifo = temp.path().join("real_fifo");
    mkfifo_for_tests(&real_fifo, 0o644).expect("create fifo");

    // Create a symlink inside source that points to the FIFO
    let fifo_link = source_dir.join("fifo_link");
    symlink(&real_fifo, &fifo_link).expect("create symlink to fifo");

    // Also create a regular file to verify the operation works overall
    fs::write(source_dir.join("regular.txt"), b"regular file").expect("write regular");

    let dest = temp.path().join("dest");
    let operands = vec![
        source_dir.into_os_string(),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // copy_links follows the symlink, but specials is disabled so the FIFO
    // target should be skipped (it is a non-regular file)
    let report = plan
        .execute_with_report(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .copy_links(true)
                .specials(false)
                .collect_events(true),
        )
        .expect("copy succeeds");

    // The regular file should be copied
    assert_eq!(
        fs::read(dest.join("regular.txt")).expect("read regular"),
        b"regular file"
    );

    // The symlink to the FIFO should have been dereferenced by copy_links,
    // but since specials is disabled, the FIFO should be skipped
    assert_eq!(report.summary().fifos_created(), 0);
    assert!(report.records().iter().any(|record| {
        record.action() == &LocalCopyAction::SkippedNonRegular
    }));
}

// ==================== copy_unsafe_links with Broken Unsafe Symlink ====================

#[cfg(unix)]
#[test]
fn execute_copy_unsafe_links_broken_target_errors() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("src");
    fs::create_dir_all(&source_dir).expect("create src");

    // Create a symlink pointing to a nonexistent path outside the tree
    let link_path = source_dir.join("broken_unsafe");
    symlink(Path::new("/nonexistent/path/that/does/not/exist.txt"), &link_path)
        .expect("create broken absolute symlink");

    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&dest_dir).expect("create dest");
    let operands = vec![
        link_path.into_os_string(),
        dest_dir.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // copy_unsafe_links would try to dereference the unsafe symlink,
    // but the target doesn't exist so it should fail
    let result = plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default()
            .links(true)
            .safe_links(true)
            .copy_unsafe_links(true),
    );

    assert!(result.is_err(), "copy should fail when unsafe symlink target is missing");
    assert!(!dest_dir.join("broken_unsafe").exists());
}

// ==================== copy_dirlinks in Directory Tree ====================

#[cfg(unix)]
#[test]
fn execute_copy_dirlinks_follows_dir_symlink_but_preserves_file_symlink_in_tree() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    // Create a real target directory with a file
    let target_dir = temp.path().join("target_dir");
    fs::create_dir(&target_dir).expect("create target dir");
    fs::write(target_dir.join("inner.txt"), b"dir data").expect("write inner");

    // Create a symlink to the directory inside the source tree
    let dir_link = source_root.join("dir_link");
    symlink(&target_dir, &dir_link).expect("create dir symlink");

    // Create a regular file and a symlink to it
    let target_file = source_root.join("target.txt");
    fs::write(&target_file, b"payload").expect("write target");
    let file_link = source_root.join("file_link");
    symlink(Path::new("target.txt"), &file_link).expect("create file symlink");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default()
            .links(true)
            .copy_dirlinks(true),
    )
    .expect("copy succeeds");

    // dir_link should be dereferenced (copy_dirlinks follows directory symlinks)
    let dest_dir_link = dest_root.join("dir_link");
    let dir_meta = fs::symlink_metadata(&dest_dir_link).expect("dir_link meta");
    assert!(
        dir_meta.file_type().is_dir(),
        "directory symlink should be dereferenced by copy_dirlinks"
    );
    assert!(
        !dir_meta.file_type().is_symlink(),
        "should not be a symlink after copy_dirlinks"
    );
    assert_eq!(
        fs::read(dest_dir_link.join("inner.txt")).expect("read inner"),
        b"dir data"
    );

    // file_link should remain a symlink (copy_dirlinks only affects directory symlinks)
    let dest_file_link = dest_root.join("file_link");
    let file_meta = fs::symlink_metadata(&dest_file_link).expect("file_link meta");
    assert!(
        file_meta.file_type().is_symlink(),
        "file symlink should remain a symlink with copy_dirlinks"
    );
    assert_eq!(
        fs::read_link(&dest_file_link).expect("read link"),
        Path::new("target.txt")
    );
}

// ==================== safe_links Combined with copy_dirlinks ====================

#[cfg(unix)]
#[test]
fn execute_safe_links_with_copy_dirlinks_follows_unsafe_dir_symlink() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    // Create an outside directory
    let outside_dir = temp.path().join("outside_dir");
    fs::create_dir(&outside_dir).expect("create outside dir");
    fs::write(outside_dir.join("file.txt"), b"outside").expect("write outside file");

    // Create an absolute (unsafe) symlink to the outside directory
    let unsafe_dir_link = source_root.join("unsafe_dir_link");
    symlink(&outside_dir, &unsafe_dir_link).expect("create unsafe dir link");

    // Create a safe file symlink
    let target_file = source_root.join("target.txt");
    fs::write(&target_file, b"safe").expect("write target");
    let safe_file_link = source_root.join("safe_file_link");
    symlink(Path::new("target.txt"), &safe_file_link).expect("create safe file link");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default()
            .links(true)
            .safe_links(true)
            .copy_dirlinks(true),
    )
    .expect("copy succeeds");

    // copy_dirlinks should follow the directory symlink (dereferencing it)
    // regardless of safe_links, since it copies the directory contents
    let dest_dir = dest_root.join("unsafe_dir_link");
    let dir_meta = fs::symlink_metadata(&dest_dir).expect("dir meta");
    assert!(
        dir_meta.file_type().is_dir(),
        "copy_dirlinks should dereference directory symlinks even with safe_links"
    );
    assert_eq!(
        fs::read(dest_dir.join("file.txt")).expect("read file"),
        b"outside"
    );

    // The safe file symlink should be preserved
    let dest_safe = dest_root.join("safe_file_link");
    assert!(
        fs::symlink_metadata(&dest_safe)
            .expect("meta")
            .file_type()
            .is_symlink()
    );
}

// ==================== FIFO Events with Collect Events ====================

#[cfg(unix)]
#[test]
fn execute_fifo_produces_fifo_copied_event() {
    let temp = tempdir().expect("tempdir");
    let source_fifo = temp.path().join("events.pipe");
    mkfifo_for_tests(&source_fifo, 0o600).expect("mkfifo");

    let dest_fifo = temp.path().join("dest.pipe");
    let operands = vec![
        source_fifo.into_os_string(),
        dest_fifo.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let report = plan
        .execute_with_report(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .specials(true)
                .collect_events(true),
        )
        .expect("copy executes");

    assert_eq!(report.summary().fifos_created(), 1);
    assert!(
        report.records().iter().any(|record| {
            record.action() == &LocalCopyAction::FifoCopied
        }),
        "should record a FifoCopied event for the FIFO"
    );
}

// ==================== copy_links with Symlink Pointing to Symlink to File ====================

#[cfg(unix)]
#[test]
fn execute_copy_links_follows_nested_symlink_chain_to_regular_file() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    fs::create_dir(&source_dir).expect("create source");

    // Create chain: link_a -> link_b -> real.txt
    let real_file = source_dir.join("real.txt");
    fs::write(&real_file, b"chain content").expect("write real");

    let link_b = source_dir.join("link_b");
    symlink(Path::new("real.txt"), &link_b).expect("create link_b");

    let link_a = source_dir.join("link_a");
    symlink(Path::new("link_b"), &link_a).expect("create link_a");

    let dest_dir = temp.path().join("dest");
    let operands = vec![
        source_dir.into_os_string(),
        dest_dir.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().copy_links(true),
        )
        .expect("copy succeeds");

    // All entries should be dereferenced to regular files
    for name in &["link_a", "link_b", "real.txt"] {
        let dest_file = dest_dir.join(name);
        let meta = fs::symlink_metadata(&dest_file).expect("metadata");
        assert!(
            meta.file_type().is_file(),
            "{name} should be a regular file"
        );
        assert!(
            !meta.file_type().is_symlink(),
            "{name} should not be a symlink"
        );
        assert_eq!(
            fs::read(&dest_file).expect("read"),
            b"chain content",
            "{name} should have the chain content"
        );
    }

    // All three entries should count as files, not symlinks
    assert_eq!(summary.symlinks_copied(), 0);
    assert_eq!(summary.files_copied(), 3);
}

// ==================== copy_links Overrides links Option ====================

#[cfg(unix)]
#[test]
fn execute_copy_links_overrides_links_option() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    let target = source_root.join("target.txt");
    fs::write(&target, b"overridden").expect("write target");

    let link = source_root.join("link");
    symlink(Path::new("target.txt"), &link).expect("create symlink");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // When both copy_links and links are set, copy_links takes precedence
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .copy_links(true)
                .links(true),
        )
        .expect("copy succeeds");

    let dest_link = dest_root.join("link");
    let meta = fs::symlink_metadata(&dest_link).expect("meta");
    assert!(
        meta.file_type().is_file(),
        "copy_links should override links, dereferencing the symlink"
    );
    assert!(
        !meta.file_type().is_symlink(),
        "should not be a symlink when copy_links is active"
    );
    assert_eq!(fs::read(&dest_link).expect("read"), b"overridden");
    assert_eq!(summary.symlinks_copied(), 0);
}

// ==================== Symlink Without links Option Skipped ====================

#[cfg(unix)]
#[test]
fn execute_without_links_skips_symlink_records_event() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    let target = source_root.join("target.txt");
    fs::write(&target, b"content").expect("write target");

    let link = source_root.join("link");
    symlink(Path::new("target.txt"), &link).expect("create symlink");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Without links(true) and without copy_links(true), symlinks are skipped
    let report = plan
        .execute_with_report(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().collect_events(true),
        )
        .expect("copy succeeds");

    let summary = report.summary();
    assert_eq!(summary.symlinks_copied(), 0);
    assert_eq!(summary.files_copied(), 1); // only target.txt
    assert!(!dest_root.join("link").exists(), "symlink should not be copied");
    assert!(dest_root.join("target.txt").exists(), "regular file should be copied");
    assert!(report.records().iter().any(|record| {
        record.action() == &LocalCopyAction::SkippedNonRegular
    }));
}

// ==================== Specials Combined with All Metadata Options ====================

#[cfg(unix)]
#[test]
fn execute_fifo_with_archive_options_preserves_all_metadata() {
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};
    use filetime::{FileTime, set_file_times};

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    let fifo = source_root.join("archive.pipe");
    mkfifo_for_tests(&fifo, 0o640).expect("mkfifo");

    let atime = FileTime::from_unix_time(1_700_050_000, 0);
    let mtime = FileTime::from_unix_time(1_700_060_000, 0);
    set_file_times(&fifo, atime, mtime).expect("set fifo timestamps");
    fs::set_permissions(&fifo, PermissionsExt::from_mode(0o640))
        .expect("set fifo permissions");

    // Also add a socket
    let socket = source_root.join("archive.sock");
    mksocket_for_tests(&socket).expect("mksocket");

    // And a regular file
    fs::write(source_root.join("file.txt"), b"archive").expect("write file");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            test_helpers::presets::archive_options(),
        )
        .expect("archive copy succeeds");

    // Verify FIFO metadata
    let dest_fifo = dest_root.join("archive.pipe");
    let fifo_meta = fs::symlink_metadata(&dest_fifo).expect("fifo meta");
    assert!(fifo_meta.file_type().is_fifo());
    assert_eq!(fifo_meta.permissions().mode() & 0o777, 0o640);
    let dest_mtime = FileTime::from_last_modification_time(&fifo_meta);
    assert_eq!(dest_mtime, mtime);

    // Verify socket was created
    let dest_socket = dest_root.join("archive.sock");
    assert!(
        fs::symlink_metadata(&dest_socket)
            .expect("meta")
            .file_type()
            .is_socket()
    );

    // Verify regular file
    assert_eq!(
        fs::read(dest_root.join("file.txt")).expect("read"),
        b"archive"
    );

    assert_eq!(summary.fifos_created(), 2); // FIFO + socket
    assert_eq!(summary.files_copied(), 1);
}

// ==================== copy_unsafe_links Preserves Safe Symlink in Dir Tree ====================

#[cfg(unix)]
#[test]
fn execute_copy_unsafe_links_in_tree_preserves_safe_and_dereferences_unsafe() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let subdir = source_root.join("sub");
    fs::create_dir_all(&subdir).expect("create subdir");

    // Create targets
    let inside_file = source_root.join("inside.txt");
    fs::write(&inside_file, b"inside").expect("write inside");

    let outside_file = temp.path().join("outside.txt");
    fs::write(&outside_file, b"outside").expect("write outside");

    // Safe symlink (relative, within tree)
    let safe_link = subdir.join("safe_link");
    symlink(Path::new("../inside.txt"), &safe_link).expect("create safe link");

    // Unsafe symlink (absolute, outside tree)
    let unsafe_link = subdir.join("unsafe_link");
    symlink(&outside_file, &unsafe_link).expect("create unsafe link");

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
                .safe_links(true)
                .copy_unsafe_links(true),
        )
        .expect("copy succeeds");

    // Safe link stays as symlink
    let dest_safe = dest_root.join("sub/safe_link");
    assert!(
        fs::symlink_metadata(&dest_safe)
            .expect("meta")
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        fs::read_link(&dest_safe).expect("read link"),
        Path::new("../inside.txt")
    );

    // Unsafe link is dereferenced to regular file
    let dest_unsafe = dest_root.join("sub/unsafe_link");
    let unsafe_meta = fs::symlink_metadata(&dest_unsafe).expect("meta");
    assert!(unsafe_meta.file_type().is_file());
    assert!(!unsafe_meta.file_type().is_symlink());
    assert_eq!(fs::read(&dest_unsafe).expect("read"), b"outside");

    assert_eq!(summary.files_copied(), 2); // inside.txt + dereferenced unsafe
}

// ==================== keep_dirlinks with Multiple Nested Symlink Directories ====================

#[cfg(unix)]
#[test]
fn execute_keep_dirlinks_multiple_symlink_subdirs_all_preserved() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(source_root.join("alpha")).expect("create alpha");
    fs::create_dir_all(source_root.join("beta")).expect("create beta");
    fs::write(source_root.join("alpha/a.txt"), b"alpha").expect("write alpha file");
    fs::write(source_root.join("beta/b.txt"), b"beta").expect("write beta file");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest");

    // Create real target directories and symlinks
    let real_alpha = temp.path().join("real_alpha");
    fs::create_dir(&real_alpha).expect("create real alpha");
    symlink(&real_alpha, dest_root.join("alpha")).expect("symlink alpha");

    let real_beta = temp.path().join("real_beta");
    fs::create_dir(&real_beta).expect("create real beta");
    symlink(&real_beta, dest_root.join("beta")).expect("symlink beta");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().keep_dirlinks(true),
    )
    .expect("copy succeeds");

    // Both symlinks should be preserved
    assert!(
        fs::symlink_metadata(dest_root.join("alpha"))
            .expect("meta")
            .file_type()
            .is_symlink()
    );
    assert!(
        fs::symlink_metadata(dest_root.join("beta"))
            .expect("meta")
            .file_type()
            .is_symlink()
    );

    // Files should be placed through the symlinks
    assert_eq!(fs::read(real_alpha.join("a.txt")).expect("read"), b"alpha");
    assert_eq!(fs::read(real_beta.join("b.txt")).expect("read"), b"beta");
}

// ==================== Dry Run with Specials and Symlinks Together ====================

#[cfg(unix)]
#[test]
fn execute_dry_run_mixed_specials_and_symlinks_no_side_effects() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    // Create FIFO
    let fifo = source_root.join("my.pipe");
    mkfifo_for_tests(&fifo, 0o600).expect("mkfifo");

    // Create socket
    let socket = source_root.join("my.sock");
    mksocket_for_tests(&socket).expect("mksocket");

    // Create symlink
    let target = source_root.join("target.txt");
    fs::write(&target, b"target").expect("write target");
    symlink(Path::new("target.txt"), source_root.join("link")).expect("create symlink");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::DryRun,
            LocalCopyOptions::default()
                .links(true)
                .specials(true),
        )
        .expect("dry run succeeds");

    // Statistics should reflect what would happen
    assert_eq!(summary.symlinks_copied(), 1);
    assert_eq!(summary.fifos_created(), 2); // FIFO + socket

    // But nothing should be created
    assert!(!dest_root.exists(), "dry run should not create destination");
}
