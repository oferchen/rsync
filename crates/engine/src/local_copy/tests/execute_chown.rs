
#[cfg(unix)]
#[test]
fn execute_applies_owner_override() {
    use rustix::fs::{AtFlags, chownat};
    use std::os::unix::fs::MetadataExt;

    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"chown content").expect("write source");

    let source_uid = 1234;
    let source_gid = 5678;
    chownat(
        rustix::fs::CWD,
        &source,
        Some(unix_ids::uid(source_uid)),
        Some(unix_ids::gid(source_gid)),
        AtFlags::empty(),
    )
    .expect("assign source ownership");

    let override_uid = 9999;
    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().with_owner_override(Some(override_uid)),
        )
        .expect("copy succeeds");

    let metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(metadata.uid(), override_uid);
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
fn execute_applies_group_override() {
    use rustix::fs::{AtFlags, chownat};
    use std::os::unix::fs::MetadataExt;

    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"chgrp content").expect("write source");

    let source_uid = 1234;
    let source_gid = 5678;
    chownat(
        rustix::fs::CWD,
        &source,
        Some(unix_ids::uid(source_uid)),
        Some(unix_ids::gid(source_gid)),
        AtFlags::empty(),
    )
    .expect("assign source ownership");

    let override_gid = 8888;
    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().with_group_override(Some(override_gid)),
        )
        .expect("copy succeeds");

    let metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(metadata.gid(), override_gid);
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
fn execute_applies_both_owner_and_group_override() {
    use rustix::fs::{AtFlags, chownat};
    use std::os::unix::fs::MetadataExt;

    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"chown user:group content").expect("write source");

    let source_uid = 1234;
    let source_gid = 5678;
    chownat(
        rustix::fs::CWD,
        &source,
        Some(unix_ids::uid(source_uid)),
        Some(unix_ids::gid(source_gid)),
        AtFlags::empty(),
    )
    .expect("assign source ownership");

    let override_uid = 9999;
    let override_gid = 7777;
    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .with_owner_override(Some(override_uid))
                .with_group_override(Some(override_gid)),
        )
        .expect("copy succeeds");

    let metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(metadata.uid(), override_uid);
    assert_eq!(metadata.gid(), override_gid);
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
fn execute_owner_override_takes_precedence_over_preserve_owner() {
    use rustix::fs::{AtFlags, chownat};
    use std::os::unix::fs::MetadataExt;

    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"override precedence").expect("write source");

    let source_uid = 1234;
    chownat(
        rustix::fs::CWD,
        &source,
        Some(unix_ids::uid(source_uid)),
        None,
        AtFlags::empty(),
    )
    .expect("assign source ownership");

    let override_uid = 9999;
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
                .with_owner_override(Some(override_uid)),
        )
        .expect("copy succeeds");

    let metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(
        metadata.uid(),
        override_uid,
        "override should take precedence over preserve"
    );
    assert_ne!(metadata.uid(), source_uid);
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
fn execute_group_override_takes_precedence_over_preserve_group() {
    use rustix::fs::{AtFlags, chownat};
    use std::os::unix::fs::MetadataExt;

    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"override precedence").expect("write source");

    let source_gid = 5678;
    chownat(
        rustix::fs::CWD,
        &source,
        None,
        Some(unix_ids::gid(source_gid)),
        AtFlags::empty(),
    )
    .expect("assign source group");

    let override_gid = 8888;
    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .group(true)
                .with_group_override(Some(override_gid)),
        )
        .expect("copy succeeds");

    let metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(
        metadata.gid(),
        override_gid,
        "override should take precedence over preserve"
    );
    assert_ne!(metadata.gid(), source_gid);
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
fn execute_applies_owner_override_to_directory() {
    use rustix::fs::{AtFlags, chownat};
    use std::os::unix::fs::MetadataExt;

    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source dir");

    let source_uid = 1234;
    let source_gid = 5678;
    chownat(
        rustix::fs::CWD,
        &source_root,
        Some(unix_ids::uid(source_uid)),
        Some(unix_ids::gid(source_gid)),
        AtFlags::empty(),
    )
    .expect("assign dir ownership");

    let override_uid = 9999;
    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().with_owner_override(Some(override_uid)),
        )
        .expect("copy succeeds");

    let metadata = fs::metadata(&dest_root).expect("dest metadata");
    assert_eq!(metadata.uid(), override_uid);
    assert!(summary.directories_created() >= 1);
}

#[cfg(unix)]
#[test]
fn execute_applies_owner_override_to_symlink() {
    use rustix::fs::{AtFlags, chownat};
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::fs::symlink;

    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source dir");

    let target_file = source_root.join("target.txt");
    fs::write(&target_file, b"target content").expect("write target");

    let symlink_path = source_root.join("link");
    symlink("target.txt", &symlink_path).expect("create symlink");

    let source_uid = 1234;
    let source_gid = 5678;
    chownat(
        rustix::fs::CWD,
        &symlink_path,
        Some(unix_ids::uid(source_uid)),
        Some(unix_ids::gid(source_gid)),
        AtFlags::SYMLINK_NOFOLLOW,
    )
    .expect("assign symlink ownership");

    let override_uid = 9999;
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
                .with_owner_override(Some(override_uid)),
        )
        .expect("copy succeeds");

    let dest_link = dest_root.join("link");
    let metadata = fs::symlink_metadata(&dest_link).expect("symlink metadata");
    assert_eq!(metadata.uid(), override_uid);
    assert!(summary.symlinks_copied() >= 1);
}

#[cfg(unix)]
#[test]
fn execute_applies_group_override_to_multiple_files() {
    use rustix::fs::{AtFlags, chownat};
    use std::os::unix::fs::MetadataExt;

    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source dir");

    let file1 = source_root.join("file1.txt");
    let file2 = source_root.join("file2.txt");
    fs::write(&file1, b"content1").expect("write file1");
    fs::write(&file2, b"content2").expect("write file2");

    let source_gid = 5678;
    chownat(
        rustix::fs::CWD,
        &file1,
        None,
        Some(unix_ids::gid(source_gid)),
        AtFlags::empty(),
    )
    .expect("assign file1 group");
    chownat(
        rustix::fs::CWD,
        &file2,
        None,
        Some(unix_ids::gid(source_gid)),
        AtFlags::empty(),
    )
    .expect("assign file2 group");

    let override_gid = 8888;
    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().with_group_override(Some(override_gid)),
        )
        .expect("copy succeeds");

    let dest1 = dest_root.join("file1.txt");
    let dest2 = dest_root.join("file2.txt");
    let metadata1 = fs::metadata(&dest1).expect("metadata1");
    let metadata2 = fs::metadata(&dest2).expect("metadata2");

    assert_eq!(metadata1.gid(), override_gid);
    assert_eq!(metadata2.gid(), override_gid);
    assert_eq!(summary.files_copied(), 2);
}

#[cfg(unix)]
#[test]
fn execute_owner_override_without_privileges_fails_gracefully() {
    use std::os::unix::fs::MetadataExt;

    if rustix::process::geteuid().as_raw() == 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"unprivileged attempt").expect("write source");

    let current_uid = fs::metadata(&source).expect("metadata").uid();
    let different_uid = if current_uid == 0 { 1000 } else { 0 };

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let result = plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().with_owner_override(Some(different_uid)),
    );

    assert!(
        result.is_err(),
        "changing to a different user without privileges should fail"
    );
}

#[cfg(unix)]
#[test]
fn execute_group_override_to_non_member_group_without_privileges() {
    

    if rustix::process::geteuid().as_raw() == 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"group test").expect("write source");

    let unlikely_gid = 29999;

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let result = plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().with_group_override(Some(unlikely_gid)),
    );

    assert!(
        result.is_err(),
        "changing to a group the user is not a member of should fail"
    );
}

#[cfg(unix)]
#[test]
fn execute_owner_override_only_leaves_group_unchanged() {
    use rustix::fs::{AtFlags, chownat};
    use std::os::unix::fs::MetadataExt;

    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"owner only").expect("write source");

    let source_uid = 1234;
    let source_gid = 5678;
    chownat(
        rustix::fs::CWD,
        &source,
        Some(unix_ids::uid(source_uid)),
        Some(unix_ids::gid(source_gid)),
        AtFlags::empty(),
    )
    .expect("assign source ownership");

    let override_uid = 9999;
    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().with_owner_override(Some(override_uid)),
        )
        .expect("copy succeeds");

    let metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(metadata.uid(), override_uid);
    assert_ne!(
        metadata.gid(),
        source_gid,
        "group should not be preserved when only owner override is set"
    );
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
fn execute_group_override_only_leaves_owner_unchanged() {
    use rustix::fs::{AtFlags, chownat};
    use std::os::unix::fs::MetadataExt;

    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"group only").expect("write source");

    let source_uid = 1234;
    let source_gid = 5678;
    chownat(
        rustix::fs::CWD,
        &source,
        Some(unix_ids::uid(source_uid)),
        Some(unix_ids::gid(source_gid)),
        AtFlags::empty(),
    )
    .expect("assign source ownership");

    let override_gid = 8888;
    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().with_group_override(Some(override_gid)),
        )
        .expect("copy succeeds");

    let metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(metadata.gid(), override_gid);
    assert_ne!(
        metadata.uid(),
        source_uid,
        "owner should not be preserved when only group override is set"
    );
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
fn execute_applies_owner_override_with_existing_destination() {
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

    let existing_uid = 2000;
    let existing_gid = 3000;
    chownat(
        rustix::fs::CWD,
        &destination,
        Some(unix_ids::uid(existing_uid)),
        Some(unix_ids::gid(existing_gid)),
        AtFlags::empty(),
    )
    .expect("assign dest ownership");

    let override_uid = 9999;
    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().with_owner_override(Some(override_uid)),
        )
        .expect("copy succeeds");

    let metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(
        metadata.uid(),
        override_uid,
        "should override existing ownership"
    );
    assert_ne!(metadata.uid(), existing_uid);
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
fn execute_applies_group_override_with_existing_destination() {
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

    let existing_uid = 2000;
    let existing_gid = 3000;
    chownat(
        rustix::fs::CWD,
        &destination,
        Some(unix_ids::uid(existing_uid)),
        Some(unix_ids::gid(existing_gid)),
        AtFlags::empty(),
    )
    .expect("assign dest ownership");

    let override_gid = 7777;
    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().with_group_override(Some(override_gid)),
        )
        .expect("copy succeeds");

    let metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(
        metadata.gid(),
        override_gid,
        "should override existing group"
    );
    assert_ne!(metadata.gid(), existing_gid);
    assert_eq!(summary.files_copied(), 1);
}
