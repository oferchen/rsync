// Comprehensive tests for owner and group preservation.
//
// These tests cover the --owner and --group flags which control whether
// source file ownership is preserved when copying files. The tests verify:
//
// 1. Basic owner preservation with --owner flag
// 2. Basic group preservation with --group flag
// 3. Combined owner and group preservation
// 4. Non-root behavior and limitations
// 5. Edge cases and interactions with other options
//
// Note: Most ownership tests require root privileges to actually change
// file ownership. Tests that require root will skip when not running as root.

// ============================================================================
// Basic Owner Preservation Tests
// ============================================================================

#[cfg(unix)]
#[test]
fn owner_flag_preserves_source_uid() {
    use rustix::fs::{AtFlags, chownat};
    use std::os::unix::fs::MetadataExt;

    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"owner test content").expect("write source");

    let test_uid = 1234;
    chownat(
        rustix::fs::CWD,
        &source,
        Some(unix_ids::uid(test_uid)),
        None,
        AtFlags::empty(),
    )
    .expect("set source uid");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().owner(true),
        )
        .expect("copy succeeds");

    let metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(metadata.uid(), test_uid, "destination should have source UID");
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
fn owner_flag_disabled_does_not_preserve_uid() {
    use rustix::fs::{AtFlags, chownat};
    use std::os::unix::fs::MetadataExt;

    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"no owner test").expect("write source");

    let test_uid = 5678;
    chownat(
        rustix::fs::CWD,
        &source,
        Some(unix_ids::uid(test_uid)),
        None,
        AtFlags::empty(),
    )
    .expect("set source uid");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().owner(false),
        )
        .expect("copy succeeds");

    let metadata = fs::metadata(&destination).expect("dest metadata");
    // Without owner flag, destination should have current user's UID (root = 0)
    assert_ne!(metadata.uid(), test_uid, "destination should not preserve source UID");
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
fn owner_flag_with_root_uid() {
    use rustix::fs::{AtFlags, chownat};
    use std::os::unix::fs::MetadataExt;

    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"root owner test").expect("write source");

    // Set source to root (UID 0)
    chownat(
        rustix::fs::CWD,
        &source,
        Some(unix_ids::uid(0)),
        None,
        AtFlags::empty(),
    )
    .expect("set source to root");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().owner(true),
        )
        .expect("copy succeeds");

    let metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(metadata.uid(), 0, "destination should be owned by root");
    assert_eq!(summary.files_copied(), 1);
}

// ============================================================================
// Basic Group Preservation Tests
// ============================================================================

#[cfg(unix)]
#[test]
fn group_flag_preserves_source_gid() {
    use rustix::fs::{AtFlags, chownat};
    use std::os::unix::fs::MetadataExt;

    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"group test content").expect("write source");

    let test_gid = 4321;
    chownat(
        rustix::fs::CWD,
        &source,
        None,
        Some(unix_ids::gid(test_gid)),
        AtFlags::empty(),
    )
    .expect("set source gid");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().group(true),
        )
        .expect("copy succeeds");

    let metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(metadata.gid(), test_gid, "destination should have source GID");
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
fn group_flag_disabled_does_not_preserve_gid() {
    use rustix::fs::{AtFlags, chownat};
    use std::os::unix::fs::MetadataExt;

    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"no group test").expect("write source");

    let test_gid = 8765;
    chownat(
        rustix::fs::CWD,
        &source,
        None,
        Some(unix_ids::gid(test_gid)),
        AtFlags::empty(),
    )
    .expect("set source gid");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().group(false),
        )
        .expect("copy succeeds");

    let metadata = fs::metadata(&destination).expect("dest metadata");
    assert_ne!(metadata.gid(), test_gid, "destination should not preserve source GID");
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
fn group_flag_with_root_gid() {
    use rustix::fs::{AtFlags, chownat};
    use std::os::unix::fs::MetadataExt;

    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"root group test").expect("write source");

    // Set source to root group (GID 0)
    chownat(
        rustix::fs::CWD,
        &source,
        None,
        Some(unix_ids::gid(0)),
        AtFlags::empty(),
    )
    .expect("set source to root group");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().group(true),
        )
        .expect("copy succeeds");

    let metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(metadata.gid(), 0, "destination should belong to root group");
    assert_eq!(summary.files_copied(), 1);
}

// ============================================================================
// Combined Owner and Group Preservation Tests
// ============================================================================

#[cfg(unix)]
#[test]
fn owner_and_group_flags_preserve_both() {
    use rustix::fs::{AtFlags, chownat};
    use std::os::unix::fs::MetadataExt;

    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"combined ownership test").expect("write source");

    let test_uid = 1111;
    let test_gid = 2222;
    chownat(
        rustix::fs::CWD,
        &source,
        Some(unix_ids::uid(test_uid)),
        Some(unix_ids::gid(test_gid)),
        AtFlags::empty(),
    )
    .expect("set source ownership");

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
    assert_eq!(metadata.uid(), test_uid, "destination should have source UID");
    assert_eq!(metadata.gid(), test_gid, "destination should have source GID");
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
fn owner_only_preserves_uid_but_not_gid() {
    use rustix::fs::{AtFlags, chownat};
    use std::os::unix::fs::MetadataExt;

    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"owner only test").expect("write source");

    let test_uid = 3333;
    let test_gid = 4444;
    chownat(
        rustix::fs::CWD,
        &source,
        Some(unix_ids::uid(test_uid)),
        Some(unix_ids::gid(test_gid)),
        AtFlags::empty(),
    )
    .expect("set source ownership");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().owner(true).group(false),
        )
        .expect("copy succeeds");

    let metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(metadata.uid(), test_uid, "UID should be preserved");
    // GID is not preserved because group(false), so it will be the default
    assert_ne!(metadata.gid(), test_gid, "GID should not be preserved");
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
fn group_only_preserves_gid_but_not_uid() {
    use rustix::fs::{AtFlags, chownat};
    use std::os::unix::fs::MetadataExt;

    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"group only test").expect("write source");

    let test_uid = 5555;
    let test_gid = 6666;
    chownat(
        rustix::fs::CWD,
        &source,
        Some(unix_ids::uid(test_uid)),
        Some(unix_ids::gid(test_gid)),
        AtFlags::empty(),
    )
    .expect("set source ownership");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().owner(false).group(true),
        )
        .expect("copy succeeds");

    let metadata = fs::metadata(&destination).expect("dest metadata");
    // UID is not preserved because owner(false)
    assert_ne!(metadata.uid(), test_uid, "UID should not be preserved");
    assert_eq!(metadata.gid(), test_gid, "GID should be preserved");
    assert_eq!(summary.files_copied(), 1);
}

// ============================================================================
// Directory Ownership Preservation Tests
// ============================================================================

#[cfg(unix)]
#[test]
fn owner_and_group_preserved_on_directory() {
    use rustix::fs::{AtFlags, chownat};
    use std::os::unix::fs::MetadataExt;

    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source_dir");
    let dest_dir = temp.path().join("dest_dir");
    fs::create_dir(&source_dir).expect("create source dir");

    let test_uid = 7777;
    let test_gid = 8888;
    chownat(
        rustix::fs::CWD,
        &source_dir,
        Some(unix_ids::uid(test_uid)),
        Some(unix_ids::gid(test_gid)),
        AtFlags::empty(),
    )
    .expect("set directory ownership");

    let operands = vec![
        source_dir.into_os_string(),
        dest_dir.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .owner(true)
                .group(true)
                .dirs(true),
        )
        .expect("copy succeeds");

    let metadata = fs::metadata(&dest_dir).expect("dest metadata");
    assert!(metadata.is_dir());
    assert_eq!(metadata.uid(), test_uid);
    assert_eq!(metadata.gid(), test_gid);
    assert!(summary.directories_created() >= 1);
}

#[cfg(unix)]
#[test]
fn owner_preserved_recursively_in_directory_tree() {
    use rustix::fs::{AtFlags, chownat};
    use std::os::unix::fs::MetadataExt;

    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create source dir");

    let file1 = source_dir.join("file1.txt");
    let subdir = source_dir.join("subdir");
    fs::create_dir(&subdir).expect("create subdir");
    let file2 = subdir.join("file2.txt");
    fs::write(&file1, b"file1").expect("write file1");
    fs::write(&file2, b"file2").expect("write file2");

    let test_uid = 9000;
    let test_gid = 9001;
    for path in [&source_dir, &file1, &subdir, &file2] {
        chownat(
            rustix::fs::CWD,
            path,
            Some(unix_ids::uid(test_uid)),
            Some(unix_ids::gid(test_gid)),
            AtFlags::empty(),
        )
        .expect("set ownership");
    }

    let operands = vec![
        source_dir.into_os_string(),
        dest_dir.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .owner(true)
                .group(true)
                .recursive(true),
        )
        .expect("copy succeeds");

    // Verify all entries have the preserved ownership
    let dest_file1 = dest_dir.join("file1.txt");
    let dest_subdir = dest_dir.join("subdir");
    let dest_file2 = dest_subdir.join("file2.txt");

    assert_eq!(fs::metadata(&dest_dir).expect("dir").uid(), test_uid);
    assert_eq!(fs::metadata(&dest_dir).expect("dir").gid(), test_gid);
    assert_eq!(fs::metadata(&dest_file1).expect("file1").uid(), test_uid);
    assert_eq!(fs::metadata(&dest_file1).expect("file1").gid(), test_gid);
    assert_eq!(fs::metadata(&dest_subdir).expect("subdir").uid(), test_uid);
    assert_eq!(fs::metadata(&dest_subdir).expect("subdir").gid(), test_gid);
    assert_eq!(fs::metadata(&dest_file2).expect("file2").uid(), test_uid);
    assert_eq!(fs::metadata(&dest_file2).expect("file2").gid(), test_gid);
    assert!(summary.files_copied() >= 2);
}

// ============================================================================
// Symlink Ownership Preservation Tests
// ============================================================================

#[cfg(unix)]
#[test]
fn owner_and_group_preserved_on_symlink() {
    use rustix::fs::{AtFlags, chownat};
    use std::os::unix::fs::{MetadataExt, symlink};

    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    fs::create_dir(&source_dir).expect("create source dir");

    let target_file = source_dir.join("target.txt");
    fs::write(&target_file, b"target content").expect("write target");

    let symlink_path = source_dir.join("link");
    symlink("target.txt", &symlink_path).expect("create symlink");

    let test_uid = 11111;
    let test_gid = 22222;
    chownat(
        rustix::fs::CWD,
        &symlink_path,
        Some(unix_ids::uid(test_uid)),
        Some(unix_ids::gid(test_gid)),
        AtFlags::SYMLINK_NOFOLLOW,
    )
    .expect("set symlink ownership");

    let dest_dir = temp.path().join("dest");
    let operands = vec![
        source_dir.into_os_string(),
        dest_dir.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .owner(true)
                .group(true)
                .links(true)
                .recursive(true),
        )
        .expect("copy succeeds");

    let dest_link = dest_dir.join("link");
    let metadata = fs::symlink_metadata(&dest_link).expect("symlink metadata");
    assert!(metadata.is_symlink());
    assert_eq!(metadata.uid(), test_uid);
    assert_eq!(metadata.gid(), test_gid);
    assert!(summary.symlinks_copied() >= 1);
}

// ============================================================================
// Non-Root Behavior Tests
// ============================================================================

#[cfg(unix)]
#[test]
fn non_root_cannot_change_owner_to_different_user() {
    use std::os::unix::fs::MetadataExt;

    // Only run this test when NOT root
    if rustix::process::geteuid().as_raw() == 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"non-root test").expect("write source");

    let current_uid = fs::metadata(&source).expect("metadata").uid();
    let different_uid = if current_uid == 0 { 1000 } else { 0 };

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Trying to change to a different user's UID should fail
    let result = plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().with_owner_override(Some(different_uid)),
    );

    assert!(
        result.is_err(),
        "non-root user should not be able to change file ownership"
    );
}

#[cfg(unix)]
#[test]
fn non_root_cannot_change_group_to_non_member() {
    // Only run this test when NOT root
    if rustix::process::geteuid().as_raw() == 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"non-root group test").expect("write source");

    // Use a GID that the user is unlikely to be a member of
    let unlikely_gid = 29999;

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Trying to change to a group the user is not a member of should fail
    let result = plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().with_group_override(Some(unlikely_gid)),
    );

    assert!(
        result.is_err(),
        "non-root user should not be able to change to non-member group"
    );
}

#[cfg(unix)]
#[test]
fn non_root_can_preserve_own_uid() {
    use std::os::unix::fs::MetadataExt;

    // Only run this test when NOT root
    if rustix::process::geteuid().as_raw() == 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"own uid test").expect("write source");

    let current_uid = rustix::process::geteuid().as_raw();

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Preserving ownership when source is owned by current user should succeed
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().owner(true),
        )
        .expect("copy succeeds");

    let metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(metadata.uid(), current_uid);
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
fn non_root_can_preserve_own_gid() {
    use std::os::unix::fs::MetadataExt;

    // Only run this test when NOT root
    if rustix::process::geteuid().as_raw() == 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"own gid test").expect("write source");

    let current_gid = rustix::process::getegid().as_raw();

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Preserving group when source belongs to current group should succeed
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().group(true),
        )
        .expect("copy succeeds");

    let metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(metadata.gid(), current_gid);
    assert_eq!(summary.files_copied(), 1);
}

// ============================================================================
// Ownership with Other Metadata Options Tests
// ============================================================================

#[cfg(unix)]
#[test]
fn owner_with_permissions_and_times() {
    use rustix::fs::{AtFlags, chownat};
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"all metadata test").expect("write source");

    let test_uid = 12000;
    let test_gid = 13000;
    chownat(
        rustix::fs::CWD,
        &source,
        Some(unix_ids::uid(test_uid)),
        Some(unix_ids::gid(test_gid)),
        AtFlags::empty(),
    )
    .expect("set ownership");

    fs::set_permissions(&source, PermissionsExt::from_mode(0o640)).expect("set perms");
    let mtime = FileTime::from_unix_time(1_700_000_000, 123_000_000);
    let atime = FileTime::from_unix_time(1_700_000_100, 456_000_000);
    set_file_times(&source, atime, mtime).expect("set times");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .owner(true)
                .group(true)
                .permissions(true)
                .times(true),
        )
        .expect("copy succeeds");

    let metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(metadata.uid(), test_uid);
    assert_eq!(metadata.gid(), test_gid);
    assert_eq!(metadata.permissions().mode() & 0o777, 0o640);
    let dest_mtime = FileTime::from_last_modification_time(&metadata);
    assert_eq!(dest_mtime, mtime);
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
fn owner_with_archive_options() {
    use rustix::fs::{AtFlags, chownat};
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let dest_dir = temp.path().join("dest");
    fs::create_dir(&source_dir).expect("create source dir");

    let file_path = source_dir.join("file.txt");
    fs::write(&file_path, b"archive mode test").expect("write file");

    let test_uid = 14000;
    let test_gid = 15000;
    for path in [&source_dir, &file_path] {
        chownat(
            rustix::fs::CWD,
            path,
            Some(unix_ids::uid(test_uid)),
            Some(unix_ids::gid(test_gid)),
            AtFlags::empty(),
        )
        .expect("set ownership");
        fs::set_permissions(path, PermissionsExt::from_mode(0o755)).expect("set perms");
    }

    let operands = vec![
        source_dir.into_os_string(),
        dest_dir.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Archive mode typically enables: recursive, links, permissions, times, owner, group
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .recursive(true)
                .links(true)
                .permissions(true)
                .times(true)
                .owner(true)
                .group(true),
        )
        .expect("copy succeeds");

    let dest_file = dest_dir.join("file.txt");
    let metadata = fs::metadata(&dest_file).expect("dest metadata");
    assert_eq!(metadata.uid(), test_uid);
    assert_eq!(metadata.gid(), test_gid);
    assert_eq!(metadata.permissions().mode() & 0o777, 0o755);
    assert!(summary.files_copied() >= 1);
}

// ============================================================================
// Edge Cases
// ============================================================================

#[cfg(unix)]
#[test]
fn owner_preserved_with_existing_destination() {
    use rustix::fs::{AtFlags, chownat};
    use std::os::unix::fs::MetadataExt;

    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"new content").expect("write source");
    fs::write(&destination, b"old content").expect("write dest");

    let source_uid = 16000;
    let dest_uid = 17000;
    chownat(
        rustix::fs::CWD,
        &source,
        Some(unix_ids::uid(source_uid)),
        None,
        AtFlags::empty(),
    )
    .expect("set source ownership");
    chownat(
        rustix::fs::CWD,
        &destination,
        Some(unix_ids::uid(dest_uid)),
        None,
        AtFlags::empty(),
    )
    .expect("set dest ownership");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().owner(true),
        )
        .expect("copy succeeds");

    let metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(
        metadata.uid(),
        source_uid,
        "destination should have source UID, not original dest UID"
    );
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
fn owner_and_group_with_different_ids_on_source_and_dest() {
    use rustix::fs::{AtFlags, chownat};
    use std::os::unix::fs::MetadataExt;

    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"different ids test").expect("write source");
    fs::write(&destination, b"existing content").expect("write dest");

    let source_uid = 18000;
    let source_gid = 18001;
    let dest_uid = 19000;
    let dest_gid = 19001;

    chownat(
        rustix::fs::CWD,
        &source,
        Some(unix_ids::uid(source_uid)),
        Some(unix_ids::gid(source_gid)),
        AtFlags::empty(),
    )
    .expect("set source ownership");
    chownat(
        rustix::fs::CWD,
        &destination,
        Some(unix_ids::uid(dest_uid)),
        Some(unix_ids::gid(dest_gid)),
        AtFlags::empty(),
    )
    .expect("set dest ownership");

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
    assert_eq!(metadata.uid(), source_uid);
    assert_eq!(metadata.gid(), source_gid);
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
fn owner_preserved_on_multiple_files() {
    use rustix::fs::{AtFlags, chownat};
    use std::os::unix::fs::MetadataExt;

    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let dest_dir = temp.path().join("dest");
    fs::create_dir(&source_dir).expect("create source dir");
    fs::create_dir(&dest_dir).expect("create dest dir");

    // Create multiple files with different ownerships
    let file1 = source_dir.join("file1.txt");
    let file2 = source_dir.join("file2.txt");
    let file3 = source_dir.join("file3.txt");
    fs::write(&file1, b"content1").expect("write file1");
    fs::write(&file2, b"content2").expect("write file2");
    fs::write(&file3, b"content3").expect("write file3");

    let uid1 = 20001;
    let uid2 = 20002;
    let uid3 = 20003;

    chownat(
        rustix::fs::CWD,
        &file1,
        Some(unix_ids::uid(uid1)),
        None,
        AtFlags::empty(),
    )
    .expect("set file1 owner");
    chownat(
        rustix::fs::CWD,
        &file2,
        Some(unix_ids::uid(uid2)),
        None,
        AtFlags::empty(),
    )
    .expect("set file2 owner");
    chownat(
        rustix::fs::CWD,
        &file3,
        Some(unix_ids::uid(uid3)),
        None,
        AtFlags::empty(),
    )
    .expect("set file3 owner");

    let operands = vec![
        source_dir.into_os_string(),
        dest_dir.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().owner(true).recursive(true),
        )
        .expect("copy succeeds");

    let dest_file1 = dest_dir.join("file1.txt");
    let dest_file2 = dest_dir.join("file2.txt");
    let dest_file3 = dest_dir.join("file3.txt");

    assert_eq!(fs::metadata(&dest_file1).expect("m1").uid(), uid1);
    assert_eq!(fs::metadata(&dest_file2).expect("m2").uid(), uid2);
    assert_eq!(fs::metadata(&dest_file3).expect("m3").uid(), uid3);
    assert_eq!(summary.files_copied(), 3);
}

#[cfg(unix)]
#[test]
fn ownership_default_behavior_without_flags() {
    use std::os::unix::fs::MetadataExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"default behavior test").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Execute without any ownership flags
    let summary = plan.execute().expect("copy succeeds");

    let metadata = fs::metadata(&destination).expect("dest metadata");
    // By default, the file should be owned by the current user/group
    let current_uid = rustix::process::geteuid().as_raw();
    let current_gid = rustix::process::getegid().as_raw();
    assert_eq!(metadata.uid(), current_uid);
    assert_eq!(metadata.gid(), current_gid);
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
fn ownership_preserved_with_zero_ids() {
    use rustix::fs::{AtFlags, chownat};
    use std::os::unix::fs::MetadataExt;

    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"root ownership test").expect("write source");

    // Set ownership to root (UID 0, GID 0)
    chownat(
        rustix::fs::CWD,
        &source,
        Some(unix_ids::uid(0)),
        Some(unix_ids::gid(0)),
        AtFlags::empty(),
    )
    .expect("set root ownership");

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
    assert_eq!(metadata.uid(), 0);
    assert_eq!(metadata.gid(), 0);
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
fn ownership_preserved_with_high_ids() {
    use rustix::fs::{AtFlags, chownat};
    use std::os::unix::fs::MetadataExt;

    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"high ids test").expect("write source");

    // Use high UID/GID values (near u32 max but reasonable)
    let high_uid = 65534; // commonly 'nobody'
    let high_gid = 65534; // commonly 'nogroup'
    chownat(
        rustix::fs::CWD,
        &source,
        Some(unix_ids::uid(high_uid)),
        Some(unix_ids::gid(high_gid)),
        AtFlags::empty(),
    )
    .expect("set high ownership");

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
    assert_eq!(metadata.uid(), high_uid);
    assert_eq!(metadata.gid(), high_gid);
    assert_eq!(summary.files_copied(), 1);
}

// ============================================================================
// Interaction with --no-owner and --no-group
// ============================================================================

#[cfg(unix)]
#[test]
fn no_owner_overrides_archive_mode_owner() {
    use rustix::fs::{AtFlags, chownat};
    use std::os::unix::fs::MetadataExt;

    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"no-owner test").expect("write source");

    let test_uid = 21000;
    chownat(
        rustix::fs::CWD,
        &source,
        Some(unix_ids::uid(test_uid)),
        None,
        AtFlags::empty(),
    )
    .expect("set source owner");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Simulate archive mode (-a) but with --no-owner
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .recursive(true)
                .links(true)
                .permissions(true)
                .times(true)
                .owner(false) // --no-owner
                .group(true),
        )
        .expect("copy succeeds");

    let metadata = fs::metadata(&destination).expect("dest metadata");
    // Owner should NOT be preserved due to --no-owner
    assert_ne!(
        metadata.uid(),
        test_uid,
        "owner should not be preserved with --no-owner"
    );
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
fn no_group_overrides_archive_mode_group() {
    use rustix::fs::{AtFlags, chownat};
    use std::os::unix::fs::MetadataExt;

    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"no-group test").expect("write source");

    let test_gid = 22000;
    chownat(
        rustix::fs::CWD,
        &source,
        None,
        Some(unix_ids::gid(test_gid)),
        AtFlags::empty(),
    )
    .expect("set source group");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Simulate archive mode (-a) but with --no-group
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .recursive(true)
                .links(true)
                .permissions(true)
                .times(true)
                .owner(true)
                .group(false), // --no-group
        )
        .expect("copy succeeds");

    let metadata = fs::metadata(&destination).expect("dest metadata");
    // Group should NOT be preserved due to --no-group
    assert_ne!(
        metadata.gid(),
        test_gid,
        "group should not be preserved with --no-group"
    );
    assert_eq!(summary.files_copied(), 1);
}
