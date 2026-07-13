use ::metadata::ModifyWindow;
use std::fs;

use ::metadata::MetadataOptions;

use super::{LocalCopyChangeSet, TimeChange};

#[test]
fn new_returns_all_flags_cleared() {
    let cs = LocalCopyChangeSet::new();
    assert!(!cs.checksum_changed());
    assert!(!cs.size_changed());
    assert!(cs.time_change().is_none());
    assert!(!cs.permissions_changed());
    assert!(!cs.owner_changed());
    assert!(!cs.group_changed());
    assert!(!cs.access_time_changed());
    assert!(!cs.create_time_changed());
    assert!(!cs.acl_changed());
    assert!(!cs.xattr_changed());
}

#[test]
fn default_returns_all_flags_cleared() {
    let cs = LocalCopyChangeSet::default();
    assert!(!cs.checksum_changed());
    assert!(!cs.size_changed());
    assert!(cs.time_change().is_none());
}

#[test]
fn with_checksum_changed_true() {
    let cs = LocalCopyChangeSet::new().with_checksum_changed(true);
    assert!(cs.checksum_changed());
}

#[test]
fn with_checksum_changed_false() {
    let cs = LocalCopyChangeSet::new()
        .with_checksum_changed(true)
        .with_checksum_changed(false);
    assert!(!cs.checksum_changed());
}

#[test]
fn with_size_changed_true() {
    let cs = LocalCopyChangeSet::new().with_size_changed(true);
    assert!(cs.size_changed());
}

#[test]
fn with_size_changed_false() {
    let cs = LocalCopyChangeSet::new()
        .with_size_changed(true)
        .with_size_changed(false);
    assert!(!cs.size_changed());
}

#[test]
fn with_time_change_modified() {
    let cs = LocalCopyChangeSet::new().with_time_change(Some(TimeChange::Modified));
    assert_eq!(cs.time_change(), Some(TimeChange::Modified));
}

#[test]
fn with_time_change_transfer_time() {
    let cs = LocalCopyChangeSet::new().with_time_change(Some(TimeChange::TransferTime));
    assert_eq!(cs.time_change(), Some(TimeChange::TransferTime));
}

#[test]
fn with_time_change_none() {
    let cs = LocalCopyChangeSet::new()
        .with_time_change(Some(TimeChange::Modified))
        .with_time_change(None);
    assert!(cs.time_change().is_none());
}

#[test]
fn with_permissions_changed_true() {
    let cs = LocalCopyChangeSet::new().with_permissions_changed(true);
    assert!(cs.permissions_changed());
}

#[test]
fn with_owner_changed_true() {
    let cs = LocalCopyChangeSet::new().with_owner_changed(true);
    assert!(cs.owner_changed());
}

#[test]
fn with_group_changed_true() {
    let cs = LocalCopyChangeSet::new().with_group_changed(true);
    assert!(cs.group_changed());
}

#[test]
fn with_access_time_changed_true() {
    let cs = LocalCopyChangeSet::new().with_access_time_changed(true);
    assert!(cs.access_time_changed());
}

#[test]
fn with_create_time_changed_true() {
    let cs = LocalCopyChangeSet::new().with_create_time_changed(true);
    assert!(cs.create_time_changed());
}

#[test]
fn with_acl_changed_true() {
    let cs = LocalCopyChangeSet::new().with_acl_changed(true);
    assert!(cs.acl_changed());
}

#[test]
fn with_xattr_changed_true() {
    let cs = LocalCopyChangeSet::new().with_xattr_changed(true);
    assert!(cs.xattr_changed());
}

#[test]
fn with_missing_data_true() {
    let cs = LocalCopyChangeSet::new().with_missing_data(true);
    assert!(cs.missing_data());
}

#[test]
fn with_missing_data_false() {
    let cs = LocalCopyChangeSet::new()
        .with_missing_data(true)
        .with_missing_data(false);
    assert!(!cs.missing_data());
}

#[test]
fn missing_data_not_set_by_default() {
    let cs = LocalCopyChangeSet::new();
    assert!(!cs.missing_data());
}

#[test]
fn builder_chain_multiple_flags() {
    let cs = LocalCopyChangeSet::new()
        .with_checksum_changed(true)
        .with_size_changed(true)
        .with_time_change(Some(TimeChange::Modified))
        .with_permissions_changed(true)
        .with_owner_changed(true)
        .with_group_changed(true)
        .with_access_time_changed(true)
        .with_create_time_changed(true)
        .with_acl_changed(true)
        .with_xattr_changed(true);

    assert!(cs.checksum_changed());
    assert!(cs.size_changed());
    assert_eq!(cs.time_change(), Some(TimeChange::Modified));
    assert!(cs.permissions_changed());
    assert!(cs.owner_changed());
    assert!(cs.group_changed());
    assert!(cs.access_time_changed());
    assert!(cs.create_time_changed());
    assert!(cs.acl_changed());
    assert!(cs.xattr_changed());
}

#[test]
fn time_change_marker_modified_returns_lowercase_t() {
    let cs = LocalCopyChangeSet::new().with_time_change(Some(TimeChange::Modified));
    assert_eq!(cs.time_change_marker(), Some('t'));
}

#[test]
fn time_change_marker_transfer_time_returns_uppercase_t() {
    let cs = LocalCopyChangeSet::new().with_time_change(Some(TimeChange::TransferTime));
    assert_eq!(cs.time_change_marker(), Some('T'));
}

#[test]
fn time_change_marker_none_returns_none() {
    let cs = LocalCopyChangeSet::new();
    assert!(cs.time_change_marker().is_none());
}

#[test]
#[allow(clippy::clone_on_copy)]
fn time_change_clone() {
    let tc = TimeChange::Modified;
    let cloned = tc.clone();
    assert_eq!(tc, cloned);
}

#[test]
fn time_change_copy() {
    let tc = TimeChange::TransferTime;
    let copied: TimeChange = tc;
    assert_eq!(tc, copied);
}

#[test]
fn time_change_debug() {
    let tc = TimeChange::Modified;
    let debug_str = format!("{tc:?}");
    assert!(debug_str.contains("Modified"));
}

#[test]
fn time_change_eq() {
    assert_eq!(TimeChange::Modified, TimeChange::Modified);
    assert_eq!(TimeChange::TransferTime, TimeChange::TransferTime);
    assert_ne!(TimeChange::Modified, TimeChange::TransferTime);
}

#[test]
#[allow(clippy::clone_on_copy)]
fn change_set_clone() {
    let cs = LocalCopyChangeSet::new()
        .with_checksum_changed(true)
        .with_owner_changed(true);
    let cloned = cs.clone();
    assert_eq!(cs, cloned);
}

#[test]
fn change_set_copy() {
    let cs = LocalCopyChangeSet::new().with_size_changed(true);
    let copied: LocalCopyChangeSet = cs;
    assert_eq!(cs, copied);
}

#[test]
fn change_set_debug() {
    let cs = LocalCopyChangeSet::new().with_checksum_changed(true);
    let debug_str = format!("{cs:?}");
    assert!(debug_str.contains("checksum_changed: true"));
}

#[test]
fn change_set_eq() {
    let cs1 = LocalCopyChangeSet::new().with_checksum_changed(true);
    let cs2 = LocalCopyChangeSet::new().with_checksum_changed(true);
    let cs3 = LocalCopyChangeSet::new().with_size_changed(true);
    assert_eq!(cs1, cs2);
    assert_ne!(cs1, cs3);
}

#[test]
fn for_file_new_destination_sets_size_changed() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("file.txt");
    fs::write(&path, b"content").expect("write");
    let metadata = fs::metadata(&path).expect("metadata");
    let options = MetadataOptions::new();

    let change_set = LocalCopyChangeSet::for_file(
        &metadata,
        None, // no existing
        &options,
        false, // destination did not previously exist
        false,
        false,
        false,
        ModifyWindow::ZERO,
    );

    assert!(change_set.size_changed());
}

#[test]
fn for_file_wrote_data_sets_checksum_changed() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("file.txt");
    fs::write(&path, b"content").expect("write");
    let metadata = fs::metadata(&path).expect("metadata");
    let options = MetadataOptions::new();

    let change_set = LocalCopyChangeSet::for_file_with_checksum(
        &metadata,
        Some(&metadata),
        &options,
        true,
        true, // wrote data
        false,
        false,
        true, // checksum mode active (gates position-2 `c` glyph per upstream generator.c:1942)
        ModifyWindow::ZERO,
    );

    assert!(change_set.checksum_changed());
}

#[test]
fn for_file_xattrs_enabled_sets_xattr_changed() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("file.txt");
    fs::write(&path, b"content").expect("write");
    let metadata = fs::metadata(&path).expect("metadata");
    let options = MetadataOptions::new();

    let change_set = LocalCopyChangeSet::for_file(
        &metadata,
        Some(&metadata),
        &options,
        true,
        false,
        true, // xattrs enabled
        false,
        ModifyWindow::ZERO,
    );

    assert!(change_set.xattr_changed());
}

#[test]
fn for_file_acls_enabled_sets_acl_changed() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("file.txt");
    fs::write(&path, b"content").expect("write");
    let metadata = fs::metadata(&path).expect("metadata");
    let options = MetadataOptions::new();

    let change_set = LocalCopyChangeSet::for_file(
        &metadata,
        Some(&metadata),
        &options,
        true,
        false,
        false,
        true, // acls enabled
        ModifyWindow::ZERO,
    );

    assert!(change_set.acl_changed());
}

#[test]
fn for_file_no_changes_same_metadata() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("file.txt");
    fs::write(&path, b"content").expect("write");
    let metadata = fs::metadata(&path).expect("metadata");
    let options = MetadataOptions::new();

    let change_set = LocalCopyChangeSet::for_file(
        &metadata,
        Some(&metadata),
        &options,
        true,  // previously existed
        false, // no data written
        false,
        false,
        ModifyWindow::ZERO,
    );

    assert!(!change_set.checksum_changed());
    assert!(!change_set.size_changed());
}

#[test]
fn for_file_size_difference_detected() {
    let temp = tempfile::tempdir().expect("tempdir");
    let small_path = temp.path().join("small.txt");
    let large_path = temp.path().join("large.txt");
    fs::write(&small_path, b"x").expect("write small");
    fs::write(&large_path, b"much larger content").expect("write large");
    let small_meta = fs::metadata(&small_path).expect("metadata");
    let large_meta = fs::metadata(&large_path).expect("metadata");
    let options = MetadataOptions::new();

    let change_set = LocalCopyChangeSet::for_file(
        &large_meta,
        Some(&small_meta),
        &options,
        true,
        false,
        false,
        false,
        ModifyWindow::ZERO,
    );

    assert!(change_set.size_changed());
}

#[test]
fn for_file_times_preserved_new_destination() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("file.txt");
    fs::write(&path, b"content").expect("write");
    let metadata = fs::metadata(&path).expect("metadata");
    let options = MetadataOptions::new().preserve_times(true);

    let change_set = LocalCopyChangeSet::for_file(
        &metadata,
        None,
        &options,
        false, // new destination
        false,
        false,
        false,
        ModifyWindow::ZERO,
    );

    assert_eq!(change_set.time_change(), Some(TimeChange::Modified));
}

#[test]
fn for_file_times_not_preserved_wrote_data() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("file.txt");
    fs::write(&path, b"content").expect("write");
    let metadata = fs::metadata(&path).expect("metadata");
    let options = MetadataOptions::new().preserve_times(false);

    let change_set = LocalCopyChangeSet::for_file(
        &metadata,
        Some(&metadata),
        &options,
        true,
        true, // wrote data
        false,
        false,
        ModifyWindow::ZERO,
    );

    assert_eq!(change_set.time_change(), Some(TimeChange::TransferTime));
}

#[test]
fn for_file_times_not_preserved_new_destination() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("file.txt");
    fs::write(&path, b"content").expect("write");
    let metadata = fs::metadata(&path).expect("metadata");
    let options = MetadataOptions::new().preserve_times(false);

    let change_set = LocalCopyChangeSet::for_file(
        &metadata,
        None,
        &options,
        false, // new destination
        false,
        false,
        false,
        ModifyWindow::ZERO,
    );

    assert_eq!(change_set.time_change(), Some(TimeChange::TransferTime));
}

#[test]
fn for_file_times_not_changed_existing_no_write() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("file.txt");
    fs::write(&path, b"content").expect("write");
    let metadata = fs::metadata(&path).expect("metadata");
    let options = MetadataOptions::new().preserve_times(false);

    let change_set = LocalCopyChangeSet::for_file(
        &metadata,
        Some(&metadata),
        &options,
        true,  // existed
        false, // no write
        false,
        false,
        ModifyWindow::ZERO,
    );

    assert!(change_set.time_change().is_none());
}

/// upstream: generator.c:526 - the ITEM_REPORT_TIME (`t`) glyph is gated by
/// `!same_time(...)`, which honours `--modify-window`. A same-size file whose
/// mtime drifts by less than the window must therefore leave the time slot
/// blank so the itemize line is suppressed. Guards the itemize path exercised
/// by the upstream `compare` testsuite (`-ain --modify-window=2`).
#[test]
fn for_file_within_window_mtime_drift_reports_no_time_change() {
    use filetime::{FileTime, set_file_mtime};

    let temp = tempfile::tempdir().expect("tempdir");
    let existing_path = temp.path().join("existing.txt");
    fs::write(&existing_path, b"same size").expect("write existing");
    set_file_mtime(&existing_path, FileTime::from_unix_time(1_700_000_001, 0))
        .expect("set existing mtime");
    let existing = fs::metadata(&existing_path).expect("metadata");

    let new_path = temp.path().join("new.txt");
    fs::write(&new_path, b"same size").expect("write new");
    set_file_mtime(&new_path, FileTime::from_unix_time(1_700_000_000, 0)).expect("set new mtime");
    let metadata = fs::metadata(&new_path).expect("metadata");

    let options = MetadataOptions::new().preserve_times(true);
    // 1s drift, 2s window -> within tolerance -> no `t` glyph.
    let within = LocalCopyChangeSet::for_file(
        &metadata,
        Some(&existing),
        &options,
        true,
        false,
        false,
        false,
        ModifyWindow::from_secs(2),
    );
    assert!(within.time_change().is_none());
    assert!(!within.has_any_change());

    // Same drift with a zero window still lights the `t` glyph.
    let exact = LocalCopyChangeSet::for_file(
        &metadata,
        Some(&existing),
        &options,
        true,
        false,
        false,
        false,
        ModifyWindow::ZERO,
    );
    assert_eq!(exact.time_change(), Some(TimeChange::Modified));
}

#[test]
fn change_set_detects_size_and_time_changes() {
    use filetime::{FileTime, set_file_mtime};

    let temp = tempfile::tempdir().expect("tempdir");
    let existing_path = temp.path().join("existing.txt");
    fs::write(&existing_path, b"old").expect("write existing");

    let epoch = FileTime::from_unix_time(1, 0);
    set_file_mtime(&existing_path, epoch).expect("set mtime");
    let existing = fs::metadata(&existing_path).expect("metadata");

    let new_path = temp.path().join("new.txt");
    fs::write(&new_path, b"new data").expect("write new");
    let metadata = fs::metadata(&new_path).expect("metadata");

    let options = MetadataOptions::new();
    let change_set = LocalCopyChangeSet::for_file(
        &metadata,
        Some(&existing),
        &options,
        true,
        true,
        false,
        false,
        ModifyWindow::ZERO,
    );

    assert!(change_set.size_changed());
    assert!(matches!(
        change_set.time_change(),
        Some(TimeChange::Modified)
    ));
}

#[cfg(unix)]
#[test]
fn change_set_detects_permission_changes_for_existing_destination() {
    use filetime::{FileTime, set_file_mtime};
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let temp = tempfile::tempdir().expect("tempdir");
    let existing_path = temp.path().join("existing.txt");
    fs::write(&existing_path, b"content").expect("write existing");
    let mut existing_perms = fs::metadata(&existing_path)
        .expect("metadata")
        .permissions();
    existing_perms.set_mode(0o644);
    fs::set_permissions(&existing_path, existing_perms).expect("set existing perms");
    let existing = fs::metadata(&existing_path).expect("metadata");
    let existing_mtime = FileTime::from_system_time(existing.modified().expect("existing mtime"));

    let new_path = temp.path().join("updated.txt");
    fs::write(&new_path, b"content").expect("write new");
    let mut new_perms = fs::metadata(&new_path).expect("metadata").permissions();
    new_perms.set_mode(0o600);
    fs::set_permissions(&new_path, new_perms).expect("set new perms");
    set_file_mtime(&new_path, existing_mtime).expect("align mtime");
    let metadata = fs::metadata(&new_path).expect("metadata");

    assert_ne!(metadata.mode(), existing.mode());

    let options = MetadataOptions::new();
    let change_set = LocalCopyChangeSet::for_file(
        &metadata,
        Some(&existing),
        &options,
        true,
        false,
        false,
        false,
        ModifyWindow::ZERO,
    );

    assert!(change_set.permissions_changed());
    assert!(!change_set.size_changed());
    assert!(change_set.time_change().is_none());
}

#[cfg(unix)]
#[test]
fn change_set_detects_owner_override_mismatch() {
    use std::os::unix::fs::MetadataExt;

    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("file.txt");
    fs::write(&path, b"content").expect("write file");
    let metadata = fs::metadata(&path).expect("metadata");

    let current_uid = metadata.uid();
    let override_uid = if current_uid == u32::MAX {
        current_uid - 1
    } else {
        current_uid + 1
    };

    let options = MetadataOptions::new()
        .preserve_owner(true)
        .with_owner_override(Some(override_uid));
    let change_set = LocalCopyChangeSet::for_file(
        &metadata,
        Some(&metadata),
        &options,
        true,
        false,
        false,
        false,
        ModifyWindow::ZERO,
    );

    assert!(change_set.owner_changed());
}

#[cfg(unix)]
#[test]
fn change_set_detects_group_override_mismatch() {
    use std::os::unix::fs::MetadataExt;

    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("file.txt");
    fs::write(&path, b"content").expect("write file");
    let metadata = fs::metadata(&path).expect("metadata");

    let current_gid = metadata.gid();
    let override_gid = if current_gid == u32::MAX {
        current_gid - 1
    } else {
        current_gid + 1
    };

    let options = MetadataOptions::new()
        .preserve_group(true)
        .with_group_override(Some(override_gid));
    let change_set = LocalCopyChangeSet::for_file(
        &metadata,
        Some(&metadata),
        &options,
        true,
        false,
        false,
        false,
        ModifyWindow::ZERO,
    );

    assert!(change_set.group_changed());
}

#[cfg(unix)]
#[test]
fn for_existing_directory_flags_time_change_when_mtimes_differ() {
    use filetime::{FileTime, set_file_mtime};

    let temp = tempfile::tempdir().expect("tempdir");
    let src_dir = temp.path().join("src");
    let dst_dir = temp.path().join("dst");
    fs::create_dir(&src_dir).expect("create src");
    fs::create_dir(&dst_dir).expect("create dst");

    set_file_mtime(&src_dir, FileTime::from_unix_time(2_000_000, 0)).expect("set src mtime");
    set_file_mtime(&dst_dir, FileTime::from_unix_time(1_000_000, 0)).expect("set dst mtime");

    let src_meta = fs::metadata(&src_dir).expect("src meta");
    let dst_meta = fs::metadata(&dst_dir).expect("dst meta");

    let options = MetadataOptions::new().preserve_times(true);
    let change_set = LocalCopyChangeSet::for_existing_directory(
        &src_meta,
        &dst_meta,
        &options,
        false,
        false,
        false,
        ModifyWindow::ZERO,
    );

    assert_eq!(change_set.time_change(), Some(TimeChange::Modified));
    assert!(change_set.has_any_change());
}

/// upstream: generator.c:526 via same_time() (util1.c:1478) - directory
/// itemize compares whole seconds under modify_window == 0, so a sub-second
/// mtime drift within the same whole second must NOT report a time change.
/// This mirrors the file/symlink paths, which already ignore sub-second drift.
#[cfg(unix)]
#[test]
fn for_existing_directory_ignores_sub_second_mtime_drift() {
    use filetime::{FileTime, set_file_mtime};

    let temp = tempfile::tempdir().expect("tempdir");
    let src_dir = temp.path().join("src");
    let dst_dir = temp.path().join("dst");
    fs::create_dir(&src_dir).expect("create src");
    fs::create_dir(&dst_dir).expect("create dst");

    // Same whole second, different nanoseconds: upstream same_time() treats
    // these as equal, so no `t` glyph.
    set_file_mtime(&src_dir, FileTime::from_unix_time(1_500_000, 500_000_000))
        .expect("set src mtime");
    set_file_mtime(&dst_dir, FileTime::from_unix_time(1_500_000, 100_000_000))
        .expect("set dst mtime");

    let src_meta = fs::metadata(&src_dir).expect("src meta");
    let dst_meta = fs::metadata(&dst_dir).expect("dst meta");

    let options = MetadataOptions::new().preserve_times(true);
    let change_set = LocalCopyChangeSet::for_existing_directory(
        &src_meta,
        &dst_meta,
        &options,
        false,
        false,
        false,
        ModifyWindow::ZERO,
    );

    assert_eq!(change_set.time_change(), None);
    assert!(!change_set.has_any_change());
}

#[cfg(unix)]
#[test]
fn for_existing_directory_no_change_when_mtimes_match() {
    use filetime::{FileTime, set_file_mtime};

    let temp = tempfile::tempdir().expect("tempdir");
    let src_dir = temp.path().join("src");
    let dst_dir = temp.path().join("dst");
    fs::create_dir(&src_dir).expect("create src");
    fs::create_dir(&dst_dir).expect("create dst");

    let ts = FileTime::from_unix_time(1_500_000, 0);
    set_file_mtime(&src_dir, ts).expect("set src mtime");
    set_file_mtime(&dst_dir, ts).expect("set dst mtime");

    let src_meta = fs::metadata(&src_dir).expect("src meta");
    let dst_meta = fs::metadata(&dst_dir).expect("dst meta");

    let options = MetadataOptions::new().preserve_times(true);
    let change_set = LocalCopyChangeSet::for_existing_directory(
        &src_meta,
        &dst_meta,
        &options,
        false,
        false,
        false,
        ModifyWindow::ZERO,
    );

    assert_eq!(change_set.time_change(), None);
    assert!(!change_set.has_any_change());
}

#[cfg(unix)]
#[test]
fn for_existing_directory_omit_dir_times_suppresses_time_flag() {
    use filetime::{FileTime, set_file_mtime};

    let temp = tempfile::tempdir().expect("tempdir");
    let src_dir = temp.path().join("src");
    let dst_dir = temp.path().join("dst");
    fs::create_dir(&src_dir).expect("create src");
    fs::create_dir(&dst_dir).expect("create dst");

    set_file_mtime(&src_dir, FileTime::from_unix_time(2_000_000, 0)).expect("set src mtime");
    set_file_mtime(&dst_dir, FileTime::from_unix_time(1_000_000, 0)).expect("set dst mtime");

    let src_meta = fs::metadata(&src_dir).expect("src meta");
    let dst_meta = fs::metadata(&dst_dir).expect("dst meta");

    let options = MetadataOptions::new().preserve_times(true);
    let change_set = LocalCopyChangeSet::for_existing_directory(
        &src_meta,
        &dst_meta,
        &options,
        true,
        false,
        false,
        ModifyWindow::ZERO,
    );

    assert_eq!(change_set.time_change(), None);
}

#[cfg(unix)]
#[test]
fn for_recreated_symlink_sets_checksum_and_time_when_mtimes_differ() {
    use filetime::{FileTime, set_symlink_file_times};
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("tempdir");
    let src_link = temp.path().join("src-link");
    let dst_link = temp.path().join("dst-link");
    symlink("target-a", &src_link).expect("create src");
    symlink("target-b", &dst_link).expect("create dst");

    set_symlink_file_times(
        &src_link,
        FileTime::from_unix_time(2_000_000, 0),
        FileTime::from_unix_time(2_000_000, 0),
    )
    .expect("set src times");
    set_symlink_file_times(
        &dst_link,
        FileTime::from_unix_time(1_000_000, 0),
        FileTime::from_unix_time(1_000_000, 0),
    )
    .expect("set dst times");

    let src_meta = fs::symlink_metadata(&src_link).expect("src meta");
    let dst_meta = fs::symlink_metadata(&dst_link).expect("dst meta");

    let options = MetadataOptions::new().preserve_times(true);
    let change_set = LocalCopyChangeSet::for_recreated_symlink(
        &src_meta,
        &dst_meta,
        &options,
        false,
        ModifyWindow::ZERO,
    );

    assert!(change_set.checksum_changed());
    assert_eq!(change_set.time_change(), Some(TimeChange::Modified));
}

#[cfg(unix)]
#[test]
fn for_recreated_symlink_ignores_sub_second_mtime_drift() {
    use filetime::{FileTime, set_symlink_file_times};
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("tempdir");
    let src_link = temp.path().join("src-link");
    let dst_link = temp.path().join("dst-link");
    symlink("target-a", &src_link).expect("create src");
    symlink("target-b", &dst_link).expect("create dst");

    // Same whole second, different nanoseconds. Upstream same_time()
    // (util1.c:1478) with the default modify_window == 0 compares whole seconds
    // only, so this must NOT light the `t` glyph (itemize `cLc........`, not
    // `cLc.t......`).
    set_symlink_file_times(
        &src_link,
        FileTime::from_unix_time(1_000_000, 500_000_000),
        FileTime::from_unix_time(1_000_000, 500_000_000),
    )
    .expect("set src times");
    set_symlink_file_times(
        &dst_link,
        FileTime::from_unix_time(1_000_000, 100_000_000),
        FileTime::from_unix_time(1_000_000, 100_000_000),
    )
    .expect("set dst times");

    let src_meta = fs::symlink_metadata(&src_link).expect("src meta");
    let dst_meta = fs::symlink_metadata(&dst_link).expect("dst meta");

    let options = MetadataOptions::new().preserve_times(true);
    let change_set = LocalCopyChangeSet::for_recreated_symlink(
        &src_meta,
        &dst_meta,
        &options,
        false,
        ModifyWindow::ZERO,
    );

    assert!(change_set.checksum_changed());
    assert_eq!(change_set.time_change(), None);
}

#[cfg(unix)]
#[test]
fn for_recreated_symlink_omit_link_times_suppresses_time_flag() {
    use filetime::{FileTime, set_symlink_file_times};
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("tempdir");
    let src_link = temp.path().join("src-link");
    let dst_link = temp.path().join("dst-link");
    symlink("target-a", &src_link).expect("create src");
    symlink("target-b", &dst_link).expect("create dst");

    set_symlink_file_times(
        &src_link,
        FileTime::from_unix_time(2_000_000, 0),
        FileTime::from_unix_time(2_000_000, 0),
    )
    .expect("set src times");
    set_symlink_file_times(
        &dst_link,
        FileTime::from_unix_time(1_000_000, 0),
        FileTime::from_unix_time(1_000_000, 0),
    )
    .expect("set dst times");

    let src_meta = fs::symlink_metadata(&src_link).expect("src meta");
    let dst_meta = fs::symlink_metadata(&dst_link).expect("dst meta");

    let options = MetadataOptions::new().preserve_times(true);
    let change_set = LocalCopyChangeSet::for_recreated_symlink(
        &src_meta,
        &dst_meta,
        &options,
        true,
        ModifyWindow::ZERO,
    );

    assert!(change_set.checksum_changed());
    assert_eq!(change_set.time_change(), None);
}

#[cfg(unix)]
#[test]
fn for_recreated_device_sets_checksum_and_time_when_content_differs() {
    use filetime::{FileTime, set_file_mtime};

    let temp = tempfile::tempdir().expect("tempdir");
    let src = temp.path().join("src");
    let dst = temp.path().join("dst");
    fs::write(&src, b"s").expect("write src");
    fs::write(&dst, b"d").expect("write dst");
    set_file_mtime(&src, FileTime::from_unix_time(2_000_000, 0)).expect("set src mtime");
    set_file_mtime(&dst, FileTime::from_unix_time(1_000_000, 0)).expect("set dst mtime");

    let src_meta = fs::metadata(&src).expect("src meta");
    let dst_meta = fs::metadata(&dst).expect("dst meta");

    let options = MetadataOptions::new().preserve_times(true);
    let change_set = LocalCopyChangeSet::for_recreated_device(
        &src_meta,
        &dst_meta,
        &options,
        ModifyWindow::ZERO,
        true,
        false,
        false,
    );

    // upstream: generator.c:1668-1669 rdev/type diff drives ITEM_REPORT_CHANGE
    // (`c`); with preserve-times and a differing mtime, ITEM_REPORT_TIME renders
    // `t` (`cDc.t.....`). Devices never report size.
    assert!(change_set.checksum_changed());
    assert_eq!(change_set.time_change(), Some(TimeChange::Modified));
    assert!(!change_set.size_changed());
}

#[cfg(unix)]
#[test]
fn for_recreated_device_reports_transfer_time_when_times_not_preserved() {
    let temp = tempfile::tempdir().expect("tempdir");
    let src = temp.path().join("src");
    let dst = temp.path().join("dst");
    fs::write(&src, b"s").expect("write src");
    fs::write(&dst, b"d").expect("write dst");

    let src_meta = fs::metadata(&src).expect("src meta");
    let dst_meta = fs::metadata(&dst).expect("dst meta");

    // No preserve-times: the ITEM_LOCAL_CHANGE recreate path sets
    // ITEM_REPORT_TIME rendered as `T` (log.c:716-717), matching the upstream
    // `-Di` replace itemize `cDc.T.....`.
    let options = MetadataOptions::new().preserve_times(false);
    let change_set = LocalCopyChangeSet::for_recreated_device(
        &src_meta,
        &dst_meta,
        &options,
        ModifyWindow::ZERO,
        true,
        false,
        false,
    );

    assert!(change_set.checksum_changed());
    assert_eq!(change_set.time_change(), Some(TimeChange::TransferTime));
    assert_eq!(change_set.time_change_marker(), Some('T'));
    assert!(!change_set.size_changed());
}

#[cfg(unix)]
#[test]
fn for_recreated_device_no_change_when_content_and_time_match() {
    use filetime::{FileTime, set_file_mtime};

    let temp = tempfile::tempdir().expect("tempdir");
    let src = temp.path().join("src");
    let dst = temp.path().join("dst");
    fs::write(&src, b"s").expect("write src");
    fs::write(&dst, b"d").expect("write dst");
    let ts = FileTime::from_unix_time(1_500_000, 0);
    set_file_mtime(&src, ts).expect("set src mtime");
    set_file_mtime(&dst, ts).expect("set dst mtime");

    let src_meta = fs::metadata(&src).expect("src meta");
    let dst_meta = fs::metadata(&dst).expect("dst meta");

    // Identical device (rdev matches, mtime matches): no `c`, no time; the row
    // collapses to blank like upstream's identical-device itemize(iflags 0).
    let options = MetadataOptions::new().preserve_times(true);
    let change_set = LocalCopyChangeSet::for_recreated_device(
        &src_meta,
        &dst_meta,
        &options,
        ModifyWindow::ZERO,
        false,
        false,
        false,
    );

    assert!(!change_set.checksum_changed());
    assert_eq!(change_set.time_change(), None);
    assert!(!change_set.has_any_change());
}
