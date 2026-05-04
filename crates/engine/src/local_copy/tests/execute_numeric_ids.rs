
#[cfg(unix)]
#[test]
fn numeric_ids_preserves_uid_without_name_lookup() {
    use rustix::fs::{AtFlags, chownat};
    use std::os::unix::fs::MetadataExt;

    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"test content").expect("write source");

    // Use an arbitrary UID that may not have a corresponding username
    let test_uid = 12345;
    chownat(
        rustix::fs::CWD,
        &source,
        Some(unix_ids::uid(test_uid)),
        None,
        AtFlags::empty(),
    )
    .expect("assign ownership");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .owner(true)
        .numeric_ids(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(metadata.uid(), test_uid);
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
fn numeric_ids_preserves_gid_without_name_lookup() {
    use rustix::fs::{AtFlags, chownat};
    use std::os::unix::fs::MetadataExt;

    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"test content").expect("write source");

    // Use an arbitrary GID that may not have a corresponding group name
    let test_gid = 54321;
    chownat(
        rustix::fs::CWD,
        &source,
        None,
        Some(unix_ids::gid(test_gid)),
        AtFlags::empty(),
    )
    .expect("assign ownership");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .group(true)
        .numeric_ids(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(metadata.gid(), test_gid);
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
fn numeric_ids_works_with_owner_and_group_flags() {
    use rustix::fs::{AtFlags, chownat};
    use std::os::unix::fs::MetadataExt;

    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"test content").expect("write source");

    let test_uid = 13579;
    let test_gid = 24680;
    chownat(
        rustix::fs::CWD,
        &source,
        Some(unix_ids::uid(test_uid)),
        Some(unix_ids::gid(test_gid)),
        AtFlags::empty(),
    )
    .expect("assign ownership");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .owner(true)
        .group(true)
        .numeric_ids(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(metadata.uid(), test_uid);
    assert_eq!(metadata.gid(), test_gid);
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
fn numeric_ids_handles_non_existent_uid() {
    use rustix::fs::{AtFlags, chownat};
    use std::os::unix::fs::MetadataExt;

    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"test content").expect("write source");

    // Use a UID that's unlikely to exist on the system
    let non_existent_uid = 99999;
    chownat(
        rustix::fs::CWD,
        &source,
        Some(unix_ids::uid(non_existent_uid)),
        None,
        AtFlags::empty(),
    )
    .expect("assign ownership");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .owner(true)
        .numeric_ids(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds even with non-existent UID");

    let metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(metadata.uid(), non_existent_uid);
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
fn numeric_ids_handles_non_existent_gid() {
    use rustix::fs::{AtFlags, chownat};
    use std::os::unix::fs::MetadataExt;

    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"test content").expect("write source");

    // Use a GID that's unlikely to exist on the system
    let non_existent_gid = 88888;
    chownat(
        rustix::fs::CWD,
        &source,
        None,
        Some(unix_ids::gid(non_existent_gid)),
        AtFlags::empty(),
    )
    .expect("assign ownership");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .group(true)
        .numeric_ids(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds even with non-existent GID");

    let metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(metadata.gid(), non_existent_gid);
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
fn numeric_ids_handles_both_non_existent_ids() {
    use rustix::fs::{AtFlags, chownat};
    use std::os::unix::fs::MetadataExt;

    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"test content").expect("write source");

    // Use UIDs/GIDs that are unlikely to exist on the system
    let non_existent_uid = 77777;
    let non_existent_gid = 66666;
    chownat(
        rustix::fs::CWD,
        &source,
        Some(unix_ids::uid(non_existent_uid)),
        Some(unix_ids::gid(non_existent_gid)),
        AtFlags::empty(),
    )
    .expect("assign ownership");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .owner(true)
        .group(true)
        .numeric_ids(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds even with non-existent UIDs and GIDs");

    let metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(metadata.uid(), non_existent_uid);
    assert_eq!(metadata.gid(), non_existent_gid);
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
fn numeric_ids_with_owner_flag_only() {
    use rustix::fs::{AtFlags, chownat};
    use std::os::unix::fs::MetadataExt;

    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"test content").expect("write source");

    let test_uid = 15555;
    let test_gid = 25555;
    chownat(
        rustix::fs::CWD,
        &source,
        Some(unix_ids::uid(test_uid)),
        Some(unix_ids::gid(test_gid)),
        AtFlags::empty(),
    )
    .expect("assign ownership");

    let source_gid = fs::metadata(&source).expect("source metadata").gid();

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Only preserve owner, not group
    let options = LocalCopyOptions::default()
        .owner(true)
        .numeric_ids(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(metadata.uid(), test_uid);
    // Group should NOT be preserved
    assert_ne!(metadata.gid(), source_gid);
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
fn numeric_ids_with_group_flag_only() {
    use rustix::fs::{AtFlags, chownat};
    use std::os::unix::fs::MetadataExt;

    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"test content").expect("write source");

    let test_uid = 35555;
    let test_gid = 45555;
    chownat(
        rustix::fs::CWD,
        &source,
        Some(unix_ids::uid(test_uid)),
        Some(unix_ids::gid(test_gid)),
        AtFlags::empty(),
    )
    .expect("assign ownership");

    let source_uid = fs::metadata(&source).expect("source metadata").uid();

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Only preserve group, not owner
    let options = LocalCopyOptions::default()
        .group(true)
        .numeric_ids(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(metadata.gid(), test_gid);
    // Owner should NOT be preserved
    assert_ne!(metadata.uid(), source_uid);
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
fn numeric_ids_disabled_attempts_name_lookup() {
    use rustix::fs::{AtFlags, chownat};
    use std::os::unix::fs::MetadataExt;

    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"test content").expect("write source");

    // Use current user's UID which should have a name
    let current_uid = rustix::process::getuid().as_raw();
    chownat(
        rustix::fs::CWD,
        &source,
        Some(unix_ids::uid(current_uid)),
        None,
        AtFlags::empty(),
    )
    .expect("assign ownership");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // numeric_ids is false, so it will attempt name lookup
    let options = LocalCopyOptions::default()
        .owner(true)
        .numeric_ids(false);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let metadata = fs::metadata(&destination).expect("dest metadata");
    // Should still preserve the UID (same system, so name lookup succeeds)
    assert_eq!(metadata.uid(), current_uid);
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
fn numeric_ids_with_permissions_and_times() {
    use rustix::fs::{AtFlags, chownat};
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"test content").expect("write source");

    let test_uid = 16789;
    let test_gid = 26789;
    chownat(
        rustix::fs::CWD,
        &source,
        Some(unix_ids::uid(test_uid)),
        Some(unix_ids::gid(test_gid)),
        AtFlags::empty(),
    )
    .expect("assign ownership");

    fs::set_permissions(&source, PermissionsExt::from_mode(0o640)).expect("set perms");
    let atime = FileTime::from_unix_time(1_700_000_000, 123_000_000);
    let mtime = FileTime::from_unix_time(1_700_000_100, 456_000_000);
    set_file_times(&source, atime, mtime).expect("set times");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .owner(true)
        .group(true)
        .numeric_ids(true)
        .permissions(true)
        .times(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(metadata.uid(), test_uid);
    assert_eq!(metadata.gid(), test_gid);
    assert_eq!(metadata.permissions().mode() & 0o777, 0o640);
    let dest_atime = FileTime::from_last_access_time(&metadata);
    let dest_mtime = FileTime::from_last_modification_time(&metadata);
    assert_eq!(dest_atime, atime);
    assert_eq!(dest_mtime, mtime);
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
fn numeric_ids_with_directory() {
    use rustix::fs::{AtFlags, chownat};
    use std::os::unix::fs::MetadataExt;

    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source_dir");
    let destination = temp.path().join("dest_dir");
    fs::create_dir(&source).expect("create source dir");

    let test_uid = 17890;
    let test_gid = 27890;
    chownat(
        rustix::fs::CWD,
        &source,
        Some(unix_ids::uid(test_uid)),
        Some(unix_ids::gid(test_gid)),
        AtFlags::empty(),
    )
    .expect("assign ownership");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .owner(true)
        .group(true)
        .numeric_ids(true)
        .recursive(false)
        .dirs(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let metadata = fs::metadata(&destination).expect("dest metadata");
    assert!(metadata.is_dir());
    assert_eq!(metadata.uid(), test_uid);
    assert_eq!(metadata.gid(), test_gid);
    assert!(summary.directories_created() > 0);
}

#[cfg(unix)]
#[test]
fn numeric_ids_with_symlink() {
    use rustix::fs::{AtFlags, chownat};
    use std::os::unix::fs::MetadataExt;

    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let target = temp.path().join("target.txt");
    let source = temp.path().join("source_link");
    let destination = temp.path().join("dest_link");
    fs::write(&target, b"target content").expect("write target");
    std::os::unix::fs::symlink(&target, &source).expect("create symlink");

    let test_uid = 18901;
    let test_gid = 28901;
    chownat(
        rustix::fs::CWD,
        &source,
        Some(unix_ids::uid(test_uid)),
        Some(unix_ids::gid(test_gid)),
        AtFlags::SYMLINK_NOFOLLOW,
    )
    .expect("assign ownership");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .owner(true)
        .group(true)
        .numeric_ids(true)
        .links(true);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Check symlink ownership (use lstat to avoid following the link)
    let metadata = fs::symlink_metadata(&destination).expect("dest metadata");
    assert!(metadata.is_symlink());
    assert_eq!(metadata.uid(), test_uid);
    assert_eq!(metadata.gid(), test_gid);
    assert_eq!(summary.symlinks_copied(), 1);
}
