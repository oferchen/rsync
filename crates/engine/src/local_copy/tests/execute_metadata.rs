
#[test]
fn reference_compare_destination_skips_matching_file() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let reference_dir = temp.path().join("reference");
    let destination_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create source dir");
    fs::create_dir_all(&reference_dir).expect("create reference dir");
    fs::create_dir_all(&destination_dir).expect("create dest dir");

    let source_file = source_dir.join("file.txt");
    let reference_file = reference_dir.join("file.txt");
    fs::write(&source_file, b"payload").expect("write source");
    fs::write(&reference_file, b"payload").expect("write reference");

    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source_file, timestamp).expect("source mtime");
    set_file_mtime(&reference_file, timestamp).expect("reference mtime");

    let destination_file = destination_dir.join("file.txt");
    let operands = vec![
        source_file.into_os_string(),
        destination_file.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .times(true)
        .extend_reference_directories([ReferenceDirectory::new(
            ReferenceDirectoryKind::Compare,
            &reference_dir,
        )]);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(!destination_file.exists());
    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_matched(), 1);
}

#[test]
fn reference_copy_destination_reuses_reference_payload() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let reference_dir = temp.path().join("reference");
    let destination_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create source dir");
    fs::create_dir_all(&reference_dir).expect("create reference dir");
    fs::create_dir_all(&destination_dir).expect("create dest dir");

    let source_file = source_dir.join("file.txt");
    let reference_file = reference_dir.join("file.txt");
    fs::write(&source_file, b"payload").expect("write source");
    fs::write(&reference_file, b"payload").expect("write reference");

    let timestamp = FileTime::from_unix_time(1_700_000_500, 0);
    set_file_mtime(&source_file, timestamp).expect("source mtime");
    set_file_mtime(&reference_file, timestamp).expect("reference mtime");

    let destination_file = destination_dir.join("file.txt");
    let operands = vec![
        source_file.into_os_string(),
        destination_file.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .times(true)
        .extend_reference_directories([ReferenceDirectory::new(
            ReferenceDirectoryKind::Copy,
            &reference_dir,
        )]);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(destination_file.exists());
    assert_eq!(fs::read(&destination_file).expect("read dest"), b"payload");
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.regular_files_matched(), 0);
}

#[test]
fn reference_link_destination_degrades_to_copy_on_cross_device_error() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let reference_dir = temp.path().join("reference");
    let destination_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create source dir");
    fs::create_dir_all(&reference_dir).expect("create reference dir");
    fs::create_dir_all(&destination_dir).expect("create dest dir");

    let source_file = source_dir.join("file.txt");
    let reference_file = reference_dir.join("file.txt");
    fs::write(&source_file, b"payload").expect("write source");
    fs::write(&reference_file, b"payload").expect("write reference");

    let timestamp = FileTime::from_unix_time(1_700_001_000, 0);
    set_file_mtime(&source_file, timestamp).expect("source mtime");
    set_file_mtime(&reference_file, timestamp).expect("reference mtime");

    let destination_file = destination_dir.join("file.txt");
    let operands = vec![
        source_file.into_os_string(),
        destination_file.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .times(true)
        .extend_reference_directories([ReferenceDirectory::new(
            ReferenceDirectoryKind::Link,
            &reference_dir,
        )]);

    let summary = super::with_hard_link_override(
        |_, _| Err(io::Error::from_raw_os_error(super::CROSS_DEVICE_ERROR_CODE)),
        || {
            plan.execute_with_options(LocalCopyExecution::Apply, options)
                .expect("execution succeeds")
        },
    );

    assert!(destination_file.exists());
    assert_eq!(fs::read(&destination_file).expect("read dest"), b"payload");
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.hard_links_created(), 0);
}

#[cfg(unix)]
#[test]
fn execute_does_not_preserve_metadata_by_default() {
    use filetime::{FileTime, set_file_times};
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"metadata").expect("write source");
    fs::write(&destination, b"metadata").expect("write dest");

    fs::set_permissions(&source, PermissionsExt::from_mode(0o640)).expect("set perms");
    let atime = FileTime::from_unix_time(1_700_000_000, 123_000_000);
    let mtime = FileTime::from_unix_time(1_700_000_100, 456_000_000);
    set_file_times(&source, atime, mtime).expect("set times");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let summary = plan.execute().expect("copy succeeds");

    let metadata = fs::metadata(&destination).expect("dest metadata");
    assert_ne!(metadata.permissions().mode() & 0o777, 0o640);
    let dest_atime = FileTime::from_last_access_time(&metadata);
    let dest_mtime = FileTime::from_last_modification_time(&metadata);
    assert_ne!(dest_atime, atime);
    assert_ne!(dest_mtime, mtime);
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
fn execute_preserves_metadata_when_requested() {
    use filetime::{FileTime, set_file_times};
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"metadata").expect("write source");
    fs::write(&destination, b"metadata").expect("write dest");

    fs::set_permissions(&source, PermissionsExt::from_mode(0o640)).expect("set perms");
    let atime = FileTime::from_unix_time(1_700_000_000, 123_000_000);
    let mtime = FileTime::from_unix_time(1_700_000_100, 456_000_000);
    set_file_times(&source, atime, mtime).expect("set times");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().permissions(true).times(true);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(metadata.permissions().mode() & 0o777, 0o640);
    let dest_atime = FileTime::from_last_access_time(&metadata);
    let dest_mtime = FileTime::from_last_modification_time(&metadata);
    assert_eq!(dest_atime, atime);
    assert_eq!(dest_mtime, mtime);
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
fn execute_applies_chmod_modifiers() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"payload").expect("write source");
    fs::set_permissions(&source, PermissionsExt::from_mode(0o666)).expect("set perms");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let modifiers = ChmodModifiers::parse("Fgo-w").expect("chmod parses");
    let options = LocalCopyOptions::default().with_chmod(Some(modifiers));
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(metadata.permissions().mode() & 0o777, 0o644);
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
fn execute_preserves_ownership_when_requested() {
    use rustix::fs::{AtFlags, chownat};
    use std::os::unix::fs::MetadataExt;

    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"metadata").expect("write source");

    let owner = 23_456;
    let group = 65_432;
    chownat(
        rustix::fs::CWD,
        &source,
        Some(unix_ids::uid(owner)),
        Some(unix_ids::gid(group)),
        AtFlags::empty(),
    )
    .expect("assign ownership");

    let operands = vec![
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().owner(true).group(true),
        )
        .expect("copy succeeds");

    let metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(metadata.uid(), owner);
    assert_eq!(metadata.gid(), group);
    assert_eq!(summary.files_copied(), 1);
}
