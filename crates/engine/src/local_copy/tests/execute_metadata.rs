
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
        source.into_os_string(),
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

#[cfg(unix)]
#[test]
#[ignore = "mode 0000 files cannot be read by owner on most systems"]
fn execute_preserves_mode_0000_with_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"no permissions").expect("write source");
    fs::set_permissions(&source, PermissionsExt::from_mode(0o000)).expect("set mode 0000");

    let operands = vec![
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().permissions(true);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(metadata.permissions().mode() & 0o777, 0o000);
    assert_eq!(summary.files_copied(), 1);

    // Verify we can read the destination file (as the owner who created it)
    assert_eq!(fs::read(&destination).expect("read dest"), b"no permissions");
}

#[cfg(unix)]
#[test]
#[ignore = "mode 0000 files cannot be read by owner on most systems"]
fn execute_can_read_mode_0000_source_as_owner() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    let content = b"readable by owner";
    fs::write(&source, content).expect("write source");

    // Set mode 0000 - as the owner, we can still read it
    fs::set_permissions(&source, PermissionsExt::from_mode(0o000)).expect("set mode 0000");

    // Verify source has mode 0000
    let source_metadata = fs::metadata(&source).expect("source metadata");
    assert_eq!(source_metadata.permissions().mode() & 0o777, 0o000);

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Should succeed because we're the owner
    let summary = plan.execute().expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read dest"), content);
}

#[cfg(unix)]
#[test]
#[ignore = "mode 0000 files cannot be read by owner on most systems"]
fn execute_preserves_mode_0000_on_destination_file() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"no access").expect("write source");
    fs::set_permissions(&source, PermissionsExt::from_mode(0o000)).expect("set mode 0000");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().permissions(true);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Verify destination has mode 0000
    let dest_metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(dest_metadata.permissions().mode() & 0o777, 0o000);
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
#[ignore = "mode 0000 files cannot be read by owner on most systems"]
fn execute_mode_0000_with_times_preservation() {
    use filetime::{FileTime, set_file_times};
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"no permissions with times").expect("write source");
    fs::set_permissions(&source, PermissionsExt::from_mode(0o000)).expect("set mode 0000");

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
    assert_eq!(metadata.permissions().mode() & 0o777, 0o000);
    let dest_atime = FileTime::from_last_access_time(&metadata);
    let dest_mtime = FileTime::from_last_modification_time(&metadata);
    assert_eq!(dest_atime, atime);
    assert_eq!(dest_mtime, mtime);
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
#[ignore = "mode 0000 files cannot be read by owner on most systems"]
fn execute_mode_0000_without_permissions_option() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"default perms").expect("write source");
    fs::set_permissions(&source, PermissionsExt::from_mode(0o000)).expect("set mode 0000");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Without permissions option, mode should not be preserved
    let summary = plan.execute().expect("copy succeeds");

    let metadata = fs::metadata(&destination).expect("dest metadata");
    // Destination should NOT have mode 0000
    assert_ne!(metadata.permissions().mode() & 0o777, 0o000);
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
#[ignore = "mode 0000 files cannot be read by owner on most systems"]
fn execute_mode_0000_directory_with_files() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");

    fs::create_dir_all(&source_root).expect("create source dir");

    let source_file = source_root.join("file.txt");
    fs::write(&source_file, b"in restricted dir").expect("write source");
    fs::set_permissions(&source_file, PermissionsExt::from_mode(0o000)).expect("set file mode 0000");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .permissions(true)
        .recursive(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let dest_file = dest_root.join("source").join("file.txt");
    assert!(dest_file.exists());

    let file_metadata = fs::metadata(&dest_file).expect("dest file metadata");
    assert_eq!(file_metadata.permissions().mode() & 0o777, 0o000);
    assert_eq!(fs::read(&dest_file).expect("read dest"), b"in restricted dir");
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
#[ignore = "mode 0000 files cannot be read by owner on most systems"]
fn execute_mode_0000_with_chmod_modifiers() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"chmod override").expect("write source");
    fs::set_permissions(&source, PermissionsExt::from_mode(0o000)).expect("set mode 0000");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Apply chmod modifier to add read/write for user
    let modifiers = ChmodModifiers::parse("u+rw").expect("chmod parses");
    let options = LocalCopyOptions::default()
        .permissions(true)
        .with_chmod(Some(modifiers));

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let metadata = fs::metadata(&destination).expect("dest metadata");
    // Should have user read+write (0o600) added to mode 0000
    assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
#[ignore = "mode 0000 files cannot be read by owner on most systems"]
fn execute_mode_0000_update_existing_destination() {
    use filetime::{FileTime, set_file_times};
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    // Create source with mode 0000 and newer mtime
    fs::write(&source, b"updated content").expect("write source");
    fs::set_permissions(&source, PermissionsExt::from_mode(0o000)).expect("set mode 0000");
    let source_time = FileTime::from_unix_time(1_700_000_200, 0);
    set_file_times(&source, source_time, source_time).expect("set source times");

    // Create destination with older mtime
    fs::write(&destination, b"old content").expect("write dest");
    let dest_time = FileTime::from_unix_time(1_700_000_100, 0);
    set_file_times(&destination, dest_time, dest_time).expect("set dest times");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .permissions(true)
        .times(true)
        .update(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Should update because source is newer
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(fs::read(&destination).expect("read dest"), b"updated content");

    let metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(metadata.permissions().mode() & 0o777, 0o000);
}

#[cfg(unix)]
#[test]
fn execute_preserves_all_permission_bits_including_special() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"all permissions").expect("write source");

    // Set all permission bits: setuid (4000), setgid (2000), sticky (1000), rwx for all (777)
    fs::set_permissions(&source, PermissionsExt::from_mode(0o7777)).expect("set all perms");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().permissions(true);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let metadata = fs::metadata(&destination).expect("dest metadata");
    let dest_mode = metadata.permissions().mode();

    // Verify all permission bits are preserved
    assert_eq!(dest_mode & 0o7777, 0o7777, "all permission bits should be preserved");

    // Verify setuid bit
    assert_eq!(dest_mode & 0o4000, 0o4000, "setuid bit should be set");

    // Verify setgid bit
    assert_eq!(dest_mode & 0o2000, 0o2000, "setgid bit should be set");

    // Verify sticky bit
    assert_eq!(dest_mode & 0o1000, 0o1000, "sticky bit should be set");

    // Verify standard permission bits
    assert_eq!(dest_mode & 0o777, 0o777, "all rwx bits should be set");

    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
fn execute_preserves_setuid_bit() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"setuid test").expect("write source");

    // Set setuid bit (4000) with owner execute permission (0100)
    fs::set_permissions(&source, PermissionsExt::from_mode(0o4755)).expect("set setuid");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().permissions(true);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let metadata = fs::metadata(&destination).expect("dest metadata");
    let dest_mode = metadata.permissions().mode();

    assert_eq!(dest_mode & 0o4000, 0o4000, "setuid bit should be preserved");
    assert_eq!(dest_mode & 0o777, 0o755, "standard permissions should match");
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
fn execute_preserves_setgid_bit() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"setgid test").expect("write source");

    // Set setgid bit (2000) with group execute permission (0010)
    fs::set_permissions(&source, PermissionsExt::from_mode(0o2755)).expect("set setgid");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().permissions(true);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let metadata = fs::metadata(&destination).expect("dest metadata");
    let dest_mode = metadata.permissions().mode();

    assert_eq!(dest_mode & 0o2000, 0o2000, "setgid bit should be preserved");
    assert_eq!(dest_mode & 0o777, 0o755, "standard permissions should match");
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
fn execute_preserves_sticky_bit() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"sticky test").expect("write source");

    // Set sticky bit (1000)
    fs::set_permissions(&source, PermissionsExt::from_mode(0o1777)).expect("set sticky");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().permissions(true);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let metadata = fs::metadata(&destination).expect("dest metadata");
    let dest_mode = metadata.permissions().mode();

    assert_eq!(dest_mode & 0o1000, 0o1000, "sticky bit should be preserved");
    assert_eq!(dest_mode & 0o777, 0o777, "standard permissions should match");
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
fn execute_round_trip_preserves_all_bits() {
    use filetime::{FileTime, set_file_times};
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let intermediate = temp.path().join("intermediate.txt");
    let final_dest = temp.path().join("final.txt");

    fs::write(&source, b"round trip").expect("write source");

    // Set all permission bits
    fs::set_permissions(&source, PermissionsExt::from_mode(0o7777)).expect("set all perms");
    let atime = FileTime::from_unix_time(1_700_000_000, 123_000_000);
    let mtime = FileTime::from_unix_time(1_700_000_100, 456_000_000);
    set_file_times(&source, atime, mtime).expect("set times");

    // First copy: source -> intermediate
    let operands = vec![
        source.clone().into_os_string(),
        intermediate.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan 1");
    let options = LocalCopyOptions::default().permissions(true).times(true);
    plan.execute_with_options(LocalCopyExecution::Apply, options.clone())
        .expect("first copy succeeds");

    // Second copy: intermediate -> final_dest
    let operands = vec![
        intermediate.into_os_string(),
        final_dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan 2");
    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("second copy succeeds");

    // Verify all bits are preserved through round trip
    let src_metadata = fs::metadata(&source).expect("source metadata");
    let final_metadata = fs::metadata(&final_dest).expect("final metadata");

    assert_eq!(
        src_metadata.permissions().mode() & 0o7777,
        final_metadata.permissions().mode() & 0o7777,
        "all permission bits should be preserved through round trip"
    );

    let final_atime = FileTime::from_last_access_time(&final_metadata);
    let final_mtime = FileTime::from_last_modification_time(&final_metadata);
    assert_eq!(final_atime, atime, "atime should be preserved");
    assert_eq!(final_mtime, mtime, "mtime should be preserved");
}

#[cfg(unix)]
#[test]
fn execute_special_bits_not_preserved_without_perms_flag() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"no perms flag").expect("write source");

    // Set all permission bits
    fs::set_permissions(&source, PermissionsExt::from_mode(0o7777)).expect("set all perms");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Execute without permissions flag
    let summary = plan.execute().expect("copy succeeds");

    let metadata = fs::metadata(&destination).expect("dest metadata");
    let dest_mode = metadata.permissions().mode();

    // Special bits should NOT be preserved without --perms flag
    assert_eq!(dest_mode & 0o7000, 0, "special bits should not be preserved without --perms");
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
fn execute_combined_special_bits_with_restrictive_perms() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"combined bits").expect("write source");

    // Set setuid + setgid + sticky with restrictive permissions (0600)
    fs::set_permissions(&source, PermissionsExt::from_mode(0o7600)).expect("set special+restrictive");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().permissions(true);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let metadata = fs::metadata(&destination).expect("dest metadata");
    let dest_mode = metadata.permissions().mode();

    // Verify all special bits are preserved even with restrictive base permissions
    assert_eq!(dest_mode & 0o7000, 0o7000, "all special bits should be preserved");
    assert_eq!(dest_mode & 0o777, 0o600, "restrictive permissions should be preserved");
    assert_eq!(dest_mode & 0o7777, 0o7600, "combined mode should match exactly");
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
fn execute_directory_with_special_bits() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let dest_dir = temp.path().join("dest");

    fs::create_dir(&source_dir).expect("create source dir");

    // Set setgid and sticky bits on directory (common for shared directories)
    fs::set_permissions(&source_dir, PermissionsExt::from_mode(0o3775)).expect("set dir special perms");

    let source_file = source_dir.join("file.txt");
    fs::write(&source_file, b"in special dir").expect("write file");

    let operands = vec![
        source_dir.clone().into_os_string(),
        dest_dir.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().permissions(true).recursive(true);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let dest_metadata = fs::metadata(&dest_dir).expect("dest dir metadata");
    let dest_mode = dest_metadata.permissions().mode();

    // Verify directory special bits are preserved
    assert_eq!(dest_mode & 0o2000, 0o2000, "setgid bit should be preserved on directory");
    assert_eq!(dest_mode & 0o1000, 0o1000, "sticky bit should be preserved on directory");
    assert_eq!(dest_mode & 0o777, 0o775, "directory permissions should match");

    assert!(summary.files_copied() >= 1);
}

#[cfg(unix)]
#[test]
fn execute_multiple_files_with_different_special_bits() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let dest_dir = temp.path().join("dest");

    fs::create_dir(&source_dir).expect("create source dir");
    fs::create_dir(&dest_dir).expect("create dest dir");

    // Create files with different special permission combinations
    let setuid_file = source_dir.join("setuid.txt");
    fs::write(&setuid_file, b"setuid").expect("write setuid file");
    fs::set_permissions(&setuid_file, PermissionsExt::from_mode(0o4755)).expect("set setuid");

    let setgid_file = source_dir.join("setgid.txt");
    fs::write(&setgid_file, b"setgid").expect("write setgid file");
    fs::set_permissions(&setgid_file, PermissionsExt::from_mode(0o2755)).expect("set setgid");

    let sticky_file = source_dir.join("sticky.txt");
    fs::write(&sticky_file, b"sticky").expect("write sticky file");
    fs::set_permissions(&sticky_file, PermissionsExt::from_mode(0o1777)).expect("set sticky");

    let all_bits_file = source_dir.join("all.txt");
    fs::write(&all_bits_file, b"all bits").expect("write all bits file");
    fs::set_permissions(&all_bits_file, PermissionsExt::from_mode(0o7777)).expect("set all bits");

    let operands = vec![
        source_dir.into_os_string(),
        dest_dir.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().permissions(true).recursive(true);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Verify each file's special bits
    let setuid_dest = dest_dir.join("source/setuid.txt");
    let setuid_mode = fs::metadata(&setuid_dest).expect("setuid metadata").permissions().mode();
    assert_eq!(setuid_mode & 0o4000, 0o4000, "setuid bit preserved");
    assert_eq!(setuid_mode & 0o777, 0o755);

    let setgid_dest = dest_dir.join("source/setgid.txt");
    let setgid_mode = fs::metadata(&setgid_dest).expect("setgid metadata").permissions().mode();
    assert_eq!(setgid_mode & 0o2000, 0o2000, "setgid bit preserved");
    assert_eq!(setgid_mode & 0o777, 0o755);

    let sticky_dest = dest_dir.join("source/sticky.txt");
    let sticky_mode = fs::metadata(&sticky_dest).expect("sticky metadata").permissions().mode();
    assert_eq!(sticky_mode & 0o1000, 0o1000, "sticky bit preserved");
    assert_eq!(sticky_mode & 0o777, 0o777);

    let all_dest = dest_dir.join("source/all.txt");
    let all_mode = fs::metadata(&all_dest).expect("all bits metadata").permissions().mode();
    assert_eq!(all_mode & 0o7777, 0o7777, "all bits preserved");

    assert!(summary.files_copied() >= 4);
}
