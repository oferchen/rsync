use super::*;
#[cfg(unix)]
use crate::id_lookup::{map_gid, map_uid};
#[cfg(unix)]
use crate::ownership;
use filetime::{FileTime, set_file_times};
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
#[cfg(unix)]
use std::path::Path;
use tempfile::tempdir;

#[cfg(unix)]
fn current_mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;

    fs::metadata(path).expect("metadata").permissions().mode()
}

#[test]
fn file_permissions_and_times_are_preserved() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let dest = temp.path().join("dest.txt");
    fs::write(&source, b"data").expect("write source");
    fs::write(&dest, b"data").expect("write dest");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&source, PermissionsExt::from_mode(0o640)).expect("set source perms");
    }

    let atime = FileTime::from_unix_time(1_700_000_000, 111_000_000);
    let mtime = FileTime::from_unix_time(1_700_000_100, 222_000_000);
    set_file_times(&source, atime, mtime).expect("set source times");

    let metadata = fs::metadata(&source).expect("metadata");
    apply_file_metadata(&dest, &metadata).expect("apply file metadata");

    let dest_meta = fs::metadata(&dest).expect("dest metadata");
    let dest_atime = FileTime::from_last_access_time(&dest_meta);
    let dest_mtime = FileTime::from_last_modification_time(&dest_meta);
    assert_eq!(dest_atime, atime);
    assert_eq!(dest_mtime, mtime);

    #[cfg(unix)]
    {
        assert_eq!(current_mode(&dest) & 0o777, 0o640);
    }
}

#[cfg(unix)]
#[test]
fn file_ownership_is_preserved_when_requested() {
    use rustix::fs::{AtFlags, CWD, chownat};

    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source-owner.txt");
    let dest = temp.path().join("dest-owner.txt");
    fs::write(&source, b"data").expect("write source");
    fs::write(&dest, b"data").expect("write dest");

    let owner = 12_345;
    let group = 54_321;
    chownat(
        CWD,
        &source,
        Some(ownership::uid_from_raw(owner)),
        Some(ownership::gid_from_raw(group)),
        AtFlags::empty(),
    )
    .expect("assign ownership");

    let metadata = fs::metadata(&source).expect("metadata");
    apply_file_metadata_with_options(
        &dest,
        &metadata,
        &MetadataOptions::new()
            .preserve_owner(true)
            .preserve_group(true),
    )
    .expect("preserve metadata");

    let dest_meta = fs::metadata(&dest).expect("dest metadata");
    assert_eq!(dest_meta.uid(), owner);
    assert_eq!(dest_meta.gid(), group);
}

#[cfg(unix)]
#[test]
fn file_permissions_respect_toggle() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source-perms.txt");
    let dest = temp.path().join("dest-perms.txt");
    fs::write(&source, b"data").expect("write source");
    fs::write(&dest, b"data").expect("write dest");

    fs::set_permissions(&source, PermissionsExt::from_mode(0o750)).expect("set source perms");
    let metadata = fs::metadata(&source).expect("metadata");

    apply_file_metadata_with_options(
        &dest,
        &metadata,
        &MetadataOptions::new().preserve_permissions(false),
    )
    .expect("apply metadata");

    let mode = current_mode(&dest) & 0o777;
    assert_ne!(mode, 0o750);
}

#[cfg(unix)]
#[test]
fn file_executability_can_be_preserved_without_other_bits() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source-exec.txt");
    let dest = temp.path().join("dest-exec.txt");

    fs::write(&source, b"data").expect("write source");
    fs::write(&dest, b"data").expect("write dest");

    fs::set_permissions(&source, PermissionsExt::from_mode(0o751)).expect("set source perms");
    fs::set_permissions(&dest, PermissionsExt::from_mode(0o620)).expect("set dest perms");

    let metadata = fs::metadata(&source).expect("metadata");

    apply_file_metadata_with_options(
        &dest,
        &metadata,
        &MetadataOptions::new()
            .preserve_permissions(false)
            .preserve_executability(true),
    )
    .expect("apply metadata");

    // upstream: rsync.c:457-465 - source has exec bits and dest has none,
    // so dest gets exec granted to whoever can already read: 0o620 has
    // owner-read so owner-exec is added, group has only write, other has
    // nothing. Result is 0o720, not 0o731 / 0o751.
    let mode = current_mode(&dest) & 0o777;
    assert_eq!(mode, 0o720);
}

#[cfg(unix)]
#[test]
fn file_executability_matches_upstream_dest_mode_fixture() {
    // upstream testsuite/executability.test (rsync 3.4.4) check_perms 3:
    // source mode 0o601, dest mode 0o604, expected 0o705 after `-E`.
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source-2");
    let dest = temp.path().join("dest-2");

    fs::write(&source, b"#!/bin/sh\necho Program Two!\n").expect("write source");
    fs::write(&dest, b"#!/bin/sh\necho Program Two!\n").expect("write dest");

    fs::set_permissions(&source, PermissionsExt::from_mode(0o601)).expect("set source perms");
    fs::set_permissions(&dest, PermissionsExt::from_mode(0o604)).expect("set dest perms");

    let metadata = fs::metadata(&source).expect("metadata");

    apply_file_metadata_with_options(
        &dest,
        &metadata,
        &MetadataOptions::new()
            .preserve_permissions(false)
            .preserve_executability(true)
            .preserve_times(false),
    )
    .expect("apply metadata");

    let mode = current_mode(&dest) & 0o777;
    assert_eq!(mode, 0o705, "expected 0o705 per upstream rsync.c:457-465");
}

#[test]
fn file_times_respect_toggle() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source-times.txt");
    let dest = temp.path().join("dest-times.txt");
    fs::write(&source, b"data").expect("write source");
    fs::write(&dest, b"data").expect("write dest");

    let atime = FileTime::from_unix_time(1_700_050_000, 100_000_000);
    let mtime = FileTime::from_unix_time(1_700_060_000, 200_000_000);
    set_file_times(&source, atime, mtime).expect("set source times");
    let metadata = fs::metadata(&source).expect("metadata");

    apply_file_metadata_with_options(
        &dest,
        &metadata,
        &MetadataOptions::new().preserve_times(false),
    )
    .expect("apply metadata");

    let dest_meta = fs::metadata(&dest).expect("dest metadata");
    let dest_mtime = FileTime::from_last_modification_time(&dest_meta);
    assert_ne!(dest_mtime, mtime);
}

#[test]
fn metadata_options_numeric_ids_toggle() {
    let opts = MetadataOptions::new().numeric_ids(true);
    assert!(opts.numeric_ids_enabled());
    assert!(!MetadataOptions::new().numeric_ids_enabled());
}

#[cfg(unix)]
#[test]
fn map_uid_round_trips_current_user_without_numeric_flag() {
    let uid = rustix::process::geteuid().as_raw();
    let mapped = map_uid(uid, false).expect("uid");
    assert_eq!(mapped.as_raw(), uid);
}

#[cfg(unix)]
#[test]
fn map_gid_round_trips_current_group_without_numeric_flag() {
    let gid = rustix::process::getegid().as_raw();
    let mapped = map_gid(gid, false).expect("gid");
    assert_eq!(mapped.as_raw(), gid);
}

#[test]
fn directory_permissions_and_times_are_preserved() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source-dir");
    let dest = temp.path().join("dest-dir");
    fs::create_dir(&source).expect("create source dir");
    fs::create_dir(&dest).expect("create dest dir");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&source, PermissionsExt::from_mode(0o751)).expect("set source perms");
    }

    let atime = FileTime::from_unix_time(1_700_010_000, 0);
    let mtime = FileTime::from_unix_time(1_700_020_000, 333_000_000);
    set_file_times(&source, atime, mtime).expect("set source times");

    let metadata = fs::metadata(&source).expect("metadata");
    apply_directory_metadata(&dest, &metadata).expect("apply dir metadata");

    let dest_meta = fs::metadata(&dest).expect("dest metadata");
    let dest_atime = FileTime::from_last_access_time(&dest_meta);
    let dest_mtime = FileTime::from_last_modification_time(&dest_meta);
    assert_eq!(dest_atime, atime);
    assert_eq!(dest_mtime, mtime);

    #[cfg(unix)]
    {
        assert_eq!(current_mode(&dest) & 0o777, 0o751);
    }
}

#[cfg(unix)]
#[test]
fn symlink_times_are_preserved_without_following_target() {
    use filetime::set_symlink_file_times;
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let target = temp.path().join("target.txt");
    fs::write(&target, b"data").expect("write target");

    let source_link = temp.path().join("source-link");
    let dest_link = temp.path().join("dest-link");
    symlink(&target, &source_link).expect("create source link");
    symlink(&target, &dest_link).expect("create dest link");

    let atime = FileTime::from_unix_time(1_700_030_000, 444_000_000);
    let mtime = FileTime::from_unix_time(1_700_040_000, 555_000_000);
    set_symlink_file_times(&source_link, atime, mtime).expect("set link times");

    let metadata = fs::symlink_metadata(&source_link).expect("metadata");
    apply_symlink_metadata(&dest_link, &metadata).expect("apply symlink metadata");

    let dest_meta = fs::symlink_metadata(&dest_link).expect("dest metadata");
    let dest_atime = FileTime::from_last_access_time(&dest_meta);
    let dest_mtime = FileTime::from_last_modification_time(&dest_meta);
    assert_eq!(dest_atime, atime);
    assert_eq!(dest_mtime, mtime);

    let dest_target = fs::read_link(&dest_link).expect("read dest link");
    assert_eq!(dest_target, target);
}

#[cfg(unix)]
#[test]
fn symlink_metadata_with_options_no_times() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let target = temp.path().join("target.txt");
    fs::write(&target, b"data").expect("write target");

    let source_link = temp.path().join("source-link2");
    let dest_link = temp.path().join("dest-link2");
    symlink(&target, &source_link).expect("create source link");
    symlink(&target, &dest_link).expect("create dest link");

    let metadata = fs::symlink_metadata(&source_link).expect("metadata");

    apply_symlink_metadata_with_options(
        &dest_link,
        &metadata,
        &MetadataOptions::new().preserve_times(false),
    )
    .expect("apply symlink metadata");

    assert!(fs::symlink_metadata(&dest_link).is_ok());
}

#[cfg(unix)]
#[test]
fn directory_metadata_with_options_no_times() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source-dir-notime");
    let dest = temp.path().join("dest-dir-notime");
    fs::create_dir(&source).expect("create source dir");
    fs::create_dir(&dest).expect("create dest dir");

    let metadata = fs::metadata(&source).expect("metadata");

    apply_directory_metadata_with_options(
        &dest,
        &metadata,
        MetadataOptions::new().preserve_times(false),
    )
    .expect("apply dir metadata");

    assert!(fs::metadata(&dest).is_ok());
}

#[test]
fn file_metadata_with_all_options_disabled() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source-noop.txt");
    let dest = temp.path().join("dest-noop.txt");
    fs::write(&source, b"data").expect("write source");
    fs::write(&dest, b"data").expect("write dest");

    let metadata = fs::metadata(&source).expect("metadata");

    apply_file_metadata_with_options(
        &dest,
        &metadata,
        &MetadataOptions::new()
            .preserve_times(false)
            .preserve_permissions(false)
            .preserve_owner(false)
            .preserve_group(false),
    )
    .expect("apply metadata");

    assert!(fs::metadata(&dest).is_ok());
}

#[cfg(unix)]
#[test]
fn executability_not_applied_to_directory() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source-exec-dir");
    let dest = temp.path().join("dest-exec-dir");
    fs::create_dir(&source).expect("create source dir");
    fs::create_dir(&dest).expect("create dest dir");

    fs::set_permissions(&source, PermissionsExt::from_mode(0o755)).expect("set source perms");
    fs::set_permissions(&dest, PermissionsExt::from_mode(0o700)).expect("set dest perms");

    let metadata = fs::metadata(&source).expect("metadata");

    apply_file_metadata_with_options(
        &dest,
        &metadata,
        &MetadataOptions::new()
            .preserve_permissions(false)
            .preserve_executability(true)
            .preserve_times(false),
    )
    .expect("apply metadata");

    assert!(fs::metadata(&dest).is_ok());
}

#[cfg(unix)]
#[test]
fn executability_removed_when_source_not_executable() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source-noexec.txt");
    let dest = temp.path().join("dest-noexec.txt");
    fs::write(&source, b"data").expect("write source");
    fs::write(&dest, b"data").expect("write dest");

    fs::set_permissions(&source, PermissionsExt::from_mode(0o644)).expect("set source perms");
    fs::set_permissions(&dest, PermissionsExt::from_mode(0o755)).expect("set dest perms");

    let metadata = fs::metadata(&source).expect("metadata");

    apply_file_metadata_with_options(
        &dest,
        &metadata,
        &MetadataOptions::new()
            .preserve_permissions(false)
            .preserve_executability(true)
            .preserve_times(false),
    )
    .expect("apply metadata");

    let mode = current_mode(&dest) & 0o111;
    assert_eq!(mode, 0);
}

#[cfg(unix)]
#[test]
fn owner_override_takes_precedence() {
    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source-override.txt");
    let dest = temp.path().join("dest-override.txt");
    fs::write(&source, b"data").expect("write source");
    fs::write(&dest, b"data").expect("write dest");

    let metadata = fs::metadata(&source).expect("metadata");

    apply_file_metadata_with_options(
        &dest,
        &metadata,
        &MetadataOptions::new()
            .preserve_owner(true)
            .with_owner_override(Some(1000))
            .preserve_times(false),
    )
    .expect("apply metadata");

    let dest_meta = fs::metadata(&dest).expect("dest metadata");
    assert_eq!(dest_meta.uid(), 1000);
}

#[cfg(unix)]
#[test]
fn group_override_takes_precedence() {
    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source-grp-override.txt");
    let dest = temp.path().join("dest-grp-override.txt");
    fs::write(&source, b"data").expect("write source");
    fs::write(&dest, b"data").expect("write dest");

    let metadata = fs::metadata(&source).expect("metadata");

    apply_file_metadata_with_options(
        &dest,
        &metadata,
        &MetadataOptions::new()
            .preserve_group(true)
            .with_group_override(Some(1000))
            .preserve_times(false),
    )
    .expect("apply metadata");

    let dest_meta = fs::metadata(&dest).expect("dest metadata");
    assert_eq!(dest_meta.gid(), 1000);
}

// Sub-100ns nanosecond preservation requires filesystem granularity finer than
// NTFS's 100ns FILETIME, so the exact-equality assertion is Unix-only.
#[cfg(unix)]
#[test]
fn apply_metadata_from_file_entry_with_timestamps() {
    use protocol::flist::FileEntry;

    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("entry-dest.txt");
    fs::write(&dest, b"data").expect("write dest");

    let mut entry = FileEntry::new_file("entry-dest.txt".into(), 4, 0o644);
    entry.set_mtime(1_700_000_000, 123_456_789);

    let opts = MetadataOptions::new().preserve_times(true);
    apply_metadata_from_file_entry(&dest, &entry, &opts).expect("apply from entry");

    let dest_meta = fs::metadata(&dest).expect("dest metadata");
    let dest_mtime = FileTime::from_last_modification_time(&dest_meta);
    assert_eq!(
        dest_mtime,
        FileTime::from_unix_time(1_700_000_000, 123_456_789)
    );
}

#[test]
fn apply_metadata_from_file_entry_no_times() {
    use protocol::flist::FileEntry;

    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("entry-notime.txt");
    fs::write(&dest, b"data").expect("write dest");

    let entry = FileEntry::new_file("entry-notime.txt".into(), 4, 0o644);

    let opts = MetadataOptions::new().preserve_times(false);
    apply_metadata_from_file_entry(&dest, &entry, &opts).expect("apply from entry");

    assert!(fs::metadata(&dest).is_ok());
}

#[cfg(unix)]
#[test]
fn apply_permissions_from_entry_respects_permissions_flag() {
    use protocol::flist::FileEntry;
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("entry-perms.txt");
    fs::write(&dest, b"data").expect("write dest");
    fs::set_permissions(&dest, PermissionsExt::from_mode(0o666)).expect("set dest perms");

    let entry = FileEntry::new_file("entry-perms.txt".into(), 4, 0o755);

    let opts = MetadataOptions::new()
        .preserve_permissions(true)
        .preserve_times(false);
    apply_metadata_from_file_entry(&dest, &entry, &opts).expect("apply from entry");

    let mode = current_mode(&dest) & 0o777;
    assert_eq!(mode, 0o755);
}

#[cfg(unix)]
#[test]
fn apply_permissions_from_entry_no_change_when_disabled() {
    use protocol::flist::FileEntry;
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("entry-noperms.txt");
    fs::write(&dest, b"data").expect("write dest");
    fs::set_permissions(&dest, PermissionsExt::from_mode(0o666)).expect("set dest perms");

    let entry = FileEntry::new_file("entry-noperms.txt".into(), 4, 0o755);

    let opts = MetadataOptions::new()
        .preserve_permissions(false)
        .preserve_times(false);
    apply_metadata_from_file_entry(&dest, &entry, &opts).expect("apply from entry");

    let mode = current_mode(&dest) & 0o777;
    assert_eq!(mode, 0o666);
}

#[test]
fn epoch_timestamp_zero_seconds_is_preserved() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("epoch-source.txt");
    let dest = temp.path().join("epoch-dest.txt");
    fs::write(&source, b"data").expect("write source");
    fs::write(&dest, b"data").expect("write dest");

    let epoch_time = FileTime::from_unix_time(0, 0);
    set_file_times(&source, epoch_time, epoch_time).expect("set epoch time");

    let metadata = fs::metadata(&source).expect("metadata");
    apply_file_metadata(&dest, &metadata).expect("apply file metadata");

    let dest_meta = fs::metadata(&dest).expect("dest metadata");
    let dest_atime = FileTime::from_last_access_time(&dest_meta);
    let dest_mtime = FileTime::from_last_modification_time(&dest_meta);

    assert_eq!(dest_atime, epoch_time, "atime should be preserved at epoch");
    assert_eq!(dest_mtime, epoch_time, "mtime should be preserved at epoch");
}

// NTFS FILETIME has 100ns granularity, so 123_456_789ns truncates to
// 123_456_700ns and breaks the equality round-trip on Windows.
#[cfg(unix)]
#[test]
fn epoch_timestamp_with_nanoseconds_is_preserved() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("epoch-nsec-source.txt");
    let dest = temp.path().join("epoch-nsec-dest.txt");
    fs::write(&source, b"data").expect("write source");
    fs::write(&dest, b"data").expect("write dest");

    let epoch_time = FileTime::from_unix_time(0, 123_456_789);
    set_file_times(&source, epoch_time, epoch_time).expect("set epoch time with nsec");

    let metadata = fs::metadata(&source).expect("metadata");
    apply_file_metadata(&dest, &metadata).expect("apply file metadata");

    let dest_meta = fs::metadata(&dest).expect("dest metadata");
    let dest_mtime = FileTime::from_last_modification_time(&dest_meta);

    assert_eq!(
        dest_mtime, epoch_time,
        "mtime with nanoseconds should be preserved at epoch"
    );
}

// NTFS FILETIME has 100ns granularity, so 999_999_999ns truncates to
// 999_999_900ns and breaks the equality round-trip on Windows.
#[cfg(unix)]
#[test]
fn epoch_timestamp_round_trip_file() {
    let temp = tempdir().expect("tempdir");
    let file1 = temp.path().join("epoch-rt1.txt");
    let file2 = temp.path().join("epoch-rt2.txt");
    let file3 = temp.path().join("epoch-rt3.txt");
    fs::write(&file1, b"data").expect("write file1");
    fs::write(&file2, b"data").expect("write file2");
    fs::write(&file3, b"data").expect("write file3");

    let epoch_time = FileTime::from_unix_time(0, 999_999_999);
    set_file_times(&file1, epoch_time, epoch_time).expect("set file1 epoch time");

    let meta1 = fs::metadata(&file1).expect("metadata file1");
    apply_file_metadata(&file2, &meta1).expect("apply to file2");

    let meta2 = fs::metadata(&file2).expect("metadata file2");
    apply_file_metadata(&file3, &meta2).expect("apply to file3");

    let time1 = FileTime::from_last_modification_time(&meta1);
    let time2 = FileTime::from_last_modification_time(&meta2);
    let time3 =
        FileTime::from_last_modification_time(&fs::metadata(&file3).expect("metadata file3"));

    assert_eq!(time1, epoch_time);
    assert_eq!(time2, epoch_time);
    assert_eq!(time3, epoch_time);
}

#[test]
fn epoch_timestamp_directory_preserved() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("epoch-source-dir");
    let dest_dir = temp.path().join("epoch-dest-dir");
    fs::create_dir(&source_dir).expect("create source dir");
    fs::create_dir(&dest_dir).expect("create dest dir");

    let epoch_time = FileTime::from_unix_time(0, 0);
    set_file_times(&source_dir, epoch_time, epoch_time).expect("set dir epoch time");

    let metadata = fs::metadata(&source_dir).expect("metadata");
    apply_directory_metadata(&dest_dir, &metadata).expect("apply directory metadata");

    let dest_meta = fs::metadata(&dest_dir).expect("dest metadata");
    let dest_mtime = FileTime::from_last_modification_time(&dest_meta);

    assert_eq!(
        dest_mtime, epoch_time,
        "directory mtime should be preserved at epoch"
    );
}

#[cfg(unix)]
#[test]
fn epoch_timestamp_symlink_preserved() {
    use filetime::set_symlink_file_times;
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let target = temp.path().join("epoch-target.txt");
    let source_link = temp.path().join("epoch-source-link");
    let dest_link = temp.path().join("epoch-dest-link");
    fs::write(&target, b"target data").expect("write target");
    symlink(&target, &source_link).expect("create source link");
    symlink(&target, &dest_link).expect("create dest link");

    let epoch_time = FileTime::from_unix_time(0, 500_000_000);
    set_symlink_file_times(&source_link, epoch_time, epoch_time).expect("set link epoch time");

    let metadata = fs::symlink_metadata(&source_link).expect("metadata");
    apply_symlink_metadata(&dest_link, &metadata).expect("apply symlink metadata");

    let dest_meta = fs::symlink_metadata(&dest_link).expect("dest metadata");
    let dest_mtime = FileTime::from_last_modification_time(&dest_meta);

    assert_eq!(
        dest_mtime, epoch_time,
        "symlink mtime should be preserved at epoch"
    );
}

#[test]
fn epoch_timestamp_from_file_entry() {
    use protocol::flist::FileEntry;

    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("epoch-entry.txt");
    fs::write(&dest, b"data").expect("write dest");

    let mut entry = FileEntry::new_file("epoch-entry.txt".into(), 4, 0o644);
    entry.set_mtime(0, 0);

    let opts = MetadataOptions::new().preserve_times(true);
    apply_metadata_from_file_entry(&dest, &entry, &opts).expect("apply from entry with epoch");

    let dest_meta = fs::metadata(&dest).expect("dest metadata");
    let dest_mtime = FileTime::from_last_modification_time(&dest_meta);

    assert_eq!(
        dest_mtime,
        FileTime::from_unix_time(0, 0),
        "FileEntry epoch timestamp should be preserved"
    );
}

// NTFS FILETIME has 100ns granularity, so 987_654_321ns truncates to
// 987_654_300ns and breaks the equality assertion on Windows.
#[cfg(unix)]
#[test]
fn epoch_timestamp_from_file_entry_with_nanoseconds() {
    use protocol::flist::FileEntry;

    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("epoch-entry-nsec.txt");
    fs::write(&dest, b"data").expect("write dest");

    let mut entry = FileEntry::new_file("epoch-entry-nsec.txt".into(), 4, 0o644);
    entry.set_mtime(0, 987_654_321);

    let opts = MetadataOptions::new().preserve_times(true);
    apply_metadata_from_file_entry(&dest, &entry, &opts).expect("apply from entry with epoch nsec");

    let dest_meta = fs::metadata(&dest).expect("dest metadata");
    let dest_mtime = FileTime::from_last_modification_time(&dest_meta);

    assert_eq!(
        dest_mtime,
        FileTime::from_unix_time(0, 987_654_321),
        "FileEntry epoch timestamp with nanoseconds should be preserved"
    );
}

#[test]
fn epoch_timestamp_formatting_is_correct() {
    let epoch_zero = FileTime::from_unix_time(0, 0);
    let epoch_nsec = FileTime::from_unix_time(0, 123_456_789);
    let one_second = FileTime::from_unix_time(1, 0);

    assert!(epoch_zero < one_second);
    assert!(epoch_zero < epoch_nsec);
    assert!(epoch_nsec < one_second);

    assert_eq!(epoch_zero, FileTime::from_unix_time(0, 0));
    assert_ne!(epoch_zero, epoch_nsec);

    let debug_str = format!("{epoch_zero:?}");
    assert!(!debug_str.is_empty());
}

#[test]
fn epoch_timestamp_edge_case_one_nanosecond() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("epoch-one-nsec-source.txt");
    let dest = temp.path().join("epoch-one-nsec-dest.txt");
    fs::write(&source, b"data").expect("write source");
    fs::write(&dest, b"data").expect("write dest");

    let one_nsec = FileTime::from_unix_time(0, 1);
    set_file_times(&source, one_nsec, one_nsec).expect("set one nsec time");

    let metadata = fs::metadata(&source).expect("metadata");
    apply_file_metadata(&dest, &metadata).expect("apply file metadata");

    let dest_meta = fs::metadata(&dest).expect("dest metadata");
    let dest_mtime = FileTime::from_last_modification_time(&dest_meta);

    // Some filesystems may not support nanosecond precision,
    // so we check that we at least preserved the second (0)
    assert_eq!(dest_mtime.unix_seconds(), 0, "seconds should be zero");
}

#[test]
fn attrs_flags_empty_applies_mtime_normally() {
    use protocol::flist::FileEntry;

    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("attrs-empty.txt");
    fs::write(&dest, b"data").expect("write dest");

    let mut entry = FileEntry::new_file("attrs-empty.txt".into(), 4, 0o644);
    entry.set_mtime(1_700_000_000, 0);

    let opts = MetadataOptions::new().preserve_times(true);
    apply_metadata_with_attrs_flags(&dest, &entry, &opts, None, AttrsFlags::empty())
        .expect("apply with empty flags");

    let dest_meta = fs::metadata(&dest).expect("dest metadata");
    let dest_mtime = FileTime::from_last_modification_time(&dest_meta);
    assert_eq!(dest_mtime.unix_seconds(), 1_700_000_000);
}

#[test]
fn attrs_flags_skip_mtime_prevents_mtime_application() {
    use protocol::flist::FileEntry;

    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("attrs-skip-mtime.txt");
    fs::write(&dest, b"data").expect("write dest");

    let original_mtime = FileTime::from_last_modification_time(&fs::metadata(&dest).expect("meta"));

    let mut entry = FileEntry::new_file("attrs-skip-mtime.txt".into(), 4, 0o644);
    entry.set_mtime(1_600_000_000, 0);

    let opts = MetadataOptions::new().preserve_times(true);
    apply_metadata_with_attrs_flags(&dest, &entry, &opts, None, AttrsFlags::SKIP_MTIME)
        .expect("apply with SKIP_MTIME");

    let dest_meta = fs::metadata(&dest).expect("dest metadata");
    let dest_mtime = FileTime::from_last_modification_time(&dest_meta);
    assert_eq!(dest_mtime, original_mtime);
}

#[test]
fn attrs_flags_skip_crtime_prevents_crtime_application() {
    use protocol::flist::FileEntry;

    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("attrs-skip-crtime.txt");
    fs::write(&dest, b"data").expect("write dest");

    let mut entry = FileEntry::new_file("attrs-skip-crtime.txt".into(), 4, 0o644);
    entry.set_mtime(1_700_000_000, 0);
    entry.set_crtime(1_600_000_000);

    let opts = MetadataOptions::new()
        .preserve_times(true)
        .preserve_crtimes(true);
    apply_metadata_with_attrs_flags(&dest, &entry, &opts, None, AttrsFlags::SKIP_CRTIME)
        .expect("apply with SKIP_CRTIME");

    let dest_meta = fs::metadata(&dest).expect("dest metadata");
    let dest_mtime = FileTime::from_last_modification_time(&dest_meta);
    assert_eq!(dest_mtime.unix_seconds(), 1_700_000_000);
}

#[test]
fn attrs_flags_skip_all_times_prevents_all_time_application() {
    use protocol::flist::FileEntry;

    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("attrs-skip-all.txt");
    fs::write(&dest, b"data").expect("write dest");

    let original_mtime = FileTime::from_last_modification_time(&fs::metadata(&dest).expect("meta"));

    let mut entry = FileEntry::new_file("attrs-skip-all.txt".into(), 4, 0o644);
    entry.set_mtime(1_600_000_000, 0);
    entry.set_crtime(1_500_000_000);

    let opts = MetadataOptions::new()
        .preserve_times(true)
        .preserve_atimes(true)
        .preserve_crtimes(true);
    apply_metadata_with_attrs_flags(&dest, &entry, &opts, None, AttrsFlags::SKIP_ALL_TIMES)
        .expect("apply with SKIP_ALL_TIMES");

    let dest_meta = fs::metadata(&dest).expect("dest metadata");
    let dest_mtime = FileTime::from_last_modification_time(&dest_meta);
    assert_eq!(dest_mtime, original_mtime);
}

#[test]
fn attrs_flags_skip_mtime_with_atime_still_applies_atime() {
    use protocol::flist::FileEntry;

    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("attrs-skip-mtime-keep-atime.txt");
    fs::write(&dest, b"data").expect("write dest");

    let original_mtime = FileTime::from_last_modification_time(&fs::metadata(&dest).expect("meta"));

    let mut entry = FileEntry::new_file("attrs-skip-mtime-keep-atime.txt".into(), 4, 0o644);
    entry.set_mtime(1_600_000_000, 0);
    entry.set_atime(1_650_000_000);

    let opts = MetadataOptions::new()
        .preserve_times(true)
        .preserve_atimes(true);
    apply_metadata_with_attrs_flags(&dest, &entry, &opts, None, AttrsFlags::SKIP_MTIME)
        .expect("apply with SKIP_MTIME only");

    let dest_meta = fs::metadata(&dest).expect("dest metadata");
    let dest_mtime = FileTime::from_last_modification_time(&dest_meta);
    assert_eq!(dest_mtime, original_mtime);

    let dest_atime = FileTime::from_last_access_time(&dest_meta);
    assert_eq!(dest_atime.unix_seconds(), 1_650_000_000);
}

#[test]
fn attrs_flags_delegating_function_matches_direct_call() {
    use protocol::flist::FileEntry;

    let temp = tempdir().expect("tempdir");
    let dest1 = temp.path().join("delegate1.txt");
    let dest2 = temp.path().join("delegate2.txt");
    fs::write(&dest1, b"data").expect("write");
    fs::write(&dest2, b"data").expect("write");

    let mut entry = FileEntry::new_file("test.txt".into(), 4, 0o644);
    entry.set_mtime(1_700_000_000, 0);

    let opts = MetadataOptions::new().preserve_times(true);

    apply_metadata_with_cached_stat(&dest1, &entry, &opts, None).expect("apply cached");
    apply_metadata_with_attrs_flags(&dest2, &entry, &opts, None, AttrsFlags::empty())
        .expect("apply flags");

    let m1 = FileTime::from_last_modification_time(&fs::metadata(&dest1).expect("m1"));
    let m2 = FileTime::from_last_modification_time(&fs::metadata(&dest2).expect("m2"));
    assert_eq!(m1, m2);
}

#[test]
fn attrs_flags_skip_atime_alone_does_not_affect_mtime() {
    use protocol::flist::FileEntry;

    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("attrs-skip-atime-only.txt");
    fs::write(&dest, b"data").expect("write dest");

    let mut entry = FileEntry::new_file("attrs-skip-atime-only.txt".into(), 4, 0o644);
    entry.set_mtime(1_700_000_000, 0);
    entry.set_atime(1_650_000_000);

    let opts = MetadataOptions::new()
        .preserve_times(true)
        .preserve_atimes(true);
    apply_metadata_with_attrs_flags(&dest, &entry, &opts, None, AttrsFlags::SKIP_ATIME)
        .expect("apply with SKIP_ATIME");

    let dest_meta = fs::metadata(&dest).expect("dest metadata");
    let dest_mtime = FileTime::from_last_modification_time(&dest_meta);
    assert_eq!(dest_mtime.unix_seconds(), 1_700_000_000);
}

#[cfg(unix)]
#[test]
fn attrs_flags_skip_mtime_does_not_affect_permissions() {
    use protocol::flist::FileEntry;
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("attrs-perms.txt");
    fs::write(&dest, b"data").expect("write dest");
    fs::set_permissions(&dest, PermissionsExt::from_mode(0o666)).expect("set dest perms");

    let entry = FileEntry::new_file("attrs-perms.txt".into(), 4, 0o755);

    let opts = MetadataOptions::new()
        .preserve_permissions(true)
        .preserve_times(true);
    apply_metadata_with_attrs_flags(&dest, &entry, &opts, None, AttrsFlags::SKIP_MTIME)
        .expect("apply with SKIP_MTIME");

    let dest_meta = fs::metadata(&dest).expect("dest metadata");
    assert_eq!(dest_meta.permissions().mode() & 0o777, 0o755);
}

#[cfg(all(unix, feature = "xattr"))]
#[test]
fn fake_super_writes_rsync_stat_xattr_for_regular_file() {
    use crate::fake_super::{FAKE_SUPER_XATTR, FakeSuperStat};
    use protocol::flist::FileEntry;

    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("fakesuper-regular.txt");
    fs::write(&dest, b"data").expect("write dest");

    let mut entry = FileEntry::new_file("fakesuper-regular.txt".into(), 4, 0o100_644);
    entry.set_uid(4242);
    entry.set_gid(4343);

    let opts = MetadataOptions::new()
        .fake_super(true)
        .preserve_owner(true)
        .preserve_group(true);

    apply_metadata_from_file_entry(&dest, &entry, &opts).expect("apply with fake-super");

    let raw = match xattr::get(&dest, FAKE_SUPER_XATTR) {
        Ok(Some(value)) => value,
        Ok(None) => {
            // Filesystem without xattr support (e.g. tmpfs without user_xattr).
            return;
        }
        Err(_) => return,
    };
    let decoded =
        FakeSuperStat::decode(std::str::from_utf8(&raw).expect("xattr utf-8")).expect("decode");

    assert_eq!(decoded.mode, 0o100_644);
    assert_eq!(decoded.uid, 4242);
    assert_eq!(decoded.gid, 4343);
    assert_eq!(decoded.rdev, None);
}

#[cfg(all(unix, feature = "xattr"))]
#[test]
fn fake_super_does_not_chown_destination() {
    use crate::fake_super::FAKE_SUPER_XATTR;
    use protocol::flist::FileEntry;
    use std::os::unix::fs::MetadataExt;

    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("fakesuper-nochown.txt");
    fs::write(&dest, b"data").expect("write dest");

    let original_uid = fs::metadata(&dest).expect("metadata").uid();
    let original_gid = fs::metadata(&dest).expect("metadata").gid();

    let mut entry = FileEntry::new_file("fakesuper-nochown.txt".into(), 4, 0o100_644);
    // Use a uid/gid the unprivileged test process cannot assume directly.
    entry.set_uid(original_uid + 1000);
    entry.set_gid(original_gid + 1000);

    let opts = MetadataOptions::new()
        .fake_super(true)
        .preserve_owner(true)
        .preserve_group(true);

    apply_metadata_from_file_entry(&dest, &entry, &opts)
        .expect("fake-super apply must not fail without root");

    let after = fs::metadata(&dest).expect("metadata");
    assert_eq!(
        after.uid(),
        original_uid,
        "fake-super must not invoke chown on the inode"
    );
    assert_eq!(
        after.gid(),
        original_gid,
        "fake-super must not invoke chown on the inode"
    );

    // Sanity: the xattr was written when the filesystem supports it.
    if let Ok(Some(_)) = xattr::get(&dest, FAKE_SUPER_XATTR) {
        // Nothing else to assert; existence proves the wire-up.
    }
}

// upstream: xattrs.c:set_stat_xattr() under am_root<0 - a `--fake-super
// --chmod=a=` directory keeps a self-accessible real mode (0700) while the
// intended mode (040000) lands in the `user.rsync.%stat` xattr. Without the
// deflection the local-copy path chmods the real dir to 000 and every later
// open of it fails, mirroring the `xattrs` conformance-test failure.
#[cfg(all(unix, feature = "xattr"))]
#[test]
fn fake_super_chmod_deflects_directory_real_mode() {
    use crate::chmod::ChmodModifiers;
    use crate::fake_super::load_fake_super;
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let src = temp.path().join("src_dir");
    let dst = temp.path().join("dst_dir");
    fs::create_dir(&src).expect("create src dir");
    fs::create_dir(&dst).expect("create dst dir");
    fs::set_permissions(&src, fs::Permissions::from_mode(0o700)).expect("chmod src");

    let src_meta = fs::symlink_metadata(&src).expect("stat src");
    let opts = MetadataOptions::new()
        .fake_super(true)
        .preserve_owner(true)
        .preserve_group(true)
        .preserve_permissions(true)
        .with_chmod(Some(ChmodModifiers::parse("a=").expect("parse a=")));

    apply_directory_metadata_with_options(&dst, &src_meta, opts).expect("apply dir metadata");

    // The real directory mode must stay self-accessible (0700), never 000.
    let real = fs::metadata(&dst).expect("stat dst").mode() & 0o777;
    if let Ok(Some(stat)) = load_fake_super(&dst) {
        // Filesystem supports xattrs: assert the full upstream contract.
        assert_eq!(real, 0o700, "real dir mode must be deflected to 0700");
        assert_eq!(
            stat.mode, 0o040_000,
            "xattr must record the chmod-applied mode (040000), not the source mode"
        );
    } else {
        // No xattr backing (e.g. tmpfs sans user_xattr): still must be readable.
        assert_eq!(real, 0o700, "real dir mode must be deflected to 0700");
    }
}

// upstream: xattrs.c:1220 - regular files are deflected to 0600 (not 0700).
#[cfg(all(unix, feature = "xattr"))]
#[test]
fn fake_super_chmod_deflects_regular_file_real_mode() {
    use crate::chmod::ChmodModifiers;
    use crate::fake_super::load_fake_super;
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let src = temp.path().join("src.txt");
    let dst = temp.path().join("dst.txt");
    fs::write(&src, b"data").expect("write src");
    fs::write(&dst, b"data").expect("write dst");
    fs::set_permissions(&src, fs::Permissions::from_mode(0o644)).expect("chmod src");

    let src_meta = fs::symlink_metadata(&src).expect("stat src");
    let opts = MetadataOptions::new()
        .fake_super(true)
        .preserve_owner(true)
        .preserve_group(true)
        .preserve_permissions(true)
        .with_chmod(Some(ChmodModifiers::parse("a=").expect("parse a=")));

    apply_file_metadata_with_options(&dst, &src_meta, &opts).expect("apply file metadata");

    let real = fs::metadata(&dst).expect("stat dst").mode() & 0o777;
    assert_eq!(real, 0o600, "real file mode must be deflected to 0600");
    if let Ok(Some(stat)) = load_fake_super(&dst) {
        assert_eq!(
            stat.mode, 0o100_000,
            "xattr must record the chmod-applied file mode (100000)"
        );
    }
}

// upstream: xattrs.c:1225-1237 - when the real mode/uid/gid already faithfully
// represent the intended values (a plain 0755 dir owned by the copying user),
// set_stat_xattr writes no shim and removes any stale %stat. An unprivileged
// same-owner fake-super copy of such a directory must leave no %stat behind.
#[cfg(all(unix, feature = "xattr"))]
#[test]
fn fake_super_faithful_directory_writes_no_stat_xattr() {
    use crate::fake_super::load_fake_super;
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let src = temp.path().join("src_dir");
    let dst = temp.path().join("dst_dir");
    fs::create_dir(&src).expect("create src dir");
    fs::create_dir(&dst).expect("create dst dir");
    fs::set_permissions(&src, fs::Permissions::from_mode(0o755)).expect("chmod src");

    let src_meta = fs::symlink_metadata(&src).expect("stat src");
    let opts = MetadataOptions::new()
        .fake_super(true)
        .preserve_owner(true)
        .preserve_group(true)
        .preserve_permissions(true);

    apply_directory_metadata_with_options(&dst, &src_meta, opts).expect("apply dir metadata");

    // Same-owner 0755 dir: the real 0755 mode already conveys the intent, so no
    // %stat shim is written (matching upstream's write-or-remove rule).
    assert!(
        matches!(load_fake_super(&dst), Ok(None)),
        "faithful same-owner dir must carry no rsync.%stat xattr"
    );
    let real = fs::metadata(&dst).expect("stat dst").mode() & 0o777;
    assert_eq!(real, 0o755, "real dir mode preserved");
}

#[cfg(all(unix, feature = "xattr"))]
#[test]
fn fake_super_skips_rewrite_when_xattr_already_matches() {
    use crate::fake_super::{FAKE_SUPER_XATTR, FakeSuperStat, store_fake_super};
    use protocol::flist::FileEntry;

    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("fakesuper-skip.txt");
    fs::write(&dest, b"data").expect("write dest");

    let stat = FakeSuperStat {
        mode: 0o100_640,
        uid: 7777,
        gid: 8888,
        rdev: None,
    };
    if store_fake_super(&dest, &stat).is_err() {
        // Filesystem without xattr support; skip silently.
        return;
    }
    let raw_before = xattr::get(&dest, FAKE_SUPER_XATTR)
        .expect("xattr get")
        .expect("xattr present");

    let mut entry = FileEntry::new_file("fakesuper-skip.txt".into(), 4, 0o100_640);
    entry.set_uid(7777);
    entry.set_gid(8888);

    let opts = MetadataOptions::new()
        .fake_super(true)
        .preserve_owner(true)
        .preserve_group(true);

    apply_metadata_from_file_entry(&dest, &entry, &opts).expect("apply with fake-super");

    let raw_after = xattr::get(&dest, FAKE_SUPER_XATTR)
        .expect("xattr get")
        .expect("xattr present");
    assert_eq!(raw_before, raw_after, "xattr must remain byte-identical");
}

/// Confirms the local-copy path also writes `user.rsync.%stat` under
/// `--fake-super`. This exercises `apply_file_metadata_with_options`, which
/// takes an `fs::Metadata` directly rather than a wire-protocol `FileEntry`.
// upstream: xattrs.c:set_stat_xattr() under am_root < 0
#[cfg(all(unix, feature = "xattr"))]
#[test]
fn fake_super_writes_stat_xattr_via_local_metadata() {
    use crate::fake_super::{FAKE_SUPER_XATTR, FakeSuperStat};

    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("fakesuper-localmeta.txt");
    fs::write(&dest, b"data").expect("write dest");

    let metadata = fs::metadata(&dest).expect("dest metadata");

    let opts = MetadataOptions::new()
        .fake_super(true)
        .preserve_owner(true)
        .preserve_group(true)
        .preserve_permissions(true);

    apply_file_metadata_with_options(&dest, &metadata, &opts)
        .expect("apply with fake-super via fs::Metadata");

    let raw = match xattr::get(&dest, FAKE_SUPER_XATTR) {
        Ok(Some(value)) => value,
        Ok(None) | Err(_) => return,
    };
    let decoded =
        FakeSuperStat::decode(std::str::from_utf8(&raw).expect("xattr utf-8")).expect("decode");

    assert_eq!(decoded.mode, metadata.mode());
    assert_eq!(decoded.uid, metadata.uid());
    assert_eq!(decoded.gid, metadata.gid());
    assert_eq!(decoded.rdev, None, "regular file must not carry rdev");
}

/// Without `--fake-super`, the local-copy ownership path must not synthesise
/// the `user.rsync.%stat` xattr.
#[cfg(all(unix, feature = "xattr"))]
#[test]
fn fake_super_off_does_not_write_stat_xattr_via_local_metadata() {
    use crate::fake_super::FAKE_SUPER_XATTR;

    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("fakesuper-off.txt");
    fs::write(&dest, b"data").expect("write dest");

    let metadata = fs::metadata(&dest).expect("dest metadata");

    let opts = MetadataOptions::new()
        .preserve_owner(true)
        .preserve_group(true)
        .preserve_permissions(true);

    apply_file_metadata_with_options(&dest, &metadata, &opts).expect("apply without fake-super");

    let raw = xattr::get(&dest, FAKE_SUPER_XATTR).ok().flatten();
    assert!(
        raw.is_none(),
        "user.rsync.%stat must not appear without --fake-super; got {raw:?}"
    );
}

#[cfg(unix)]
#[test]
fn metadata_unchanged_returns_true_when_all_attrs_match() {
    use protocol::flist::FileEntry;

    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("unchanged.txt");
    fs::write(&dest, b"data").expect("write dest");

    let meta = fs::metadata(&dest).expect("metadata");
    let mtime = FileTime::from_last_modification_time(&meta);

    let mut entry = FileEntry::new_file("unchanged.txt".into(), 4, meta.mode() & 0o7777);
    entry.set_mtime(mtime.unix_seconds(), mtime.nanoseconds());
    entry.set_uid(meta.uid());
    entry.set_gid(meta.gid());

    let opts = MetadataOptions::new()
        .preserve_permissions(true)
        .preserve_times(true)
        .preserve_owner(true)
        .preserve_group(true);

    assert!(
        metadata_unchanged(&entry, &opts, &meta),
        "should return true when all attributes match"
    );
}

#[cfg(unix)]
#[test]
fn metadata_unchanged_returns_false_on_permission_mismatch() {
    use protocol::flist::FileEntry;

    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("perm-mismatch.txt");
    fs::write(&dest, b"data").expect("write dest");

    let meta = fs::metadata(&dest).expect("metadata");
    let mtime = FileTime::from_last_modification_time(&meta);

    // Use different permissions than what's on disk
    let disk_mode = meta.mode() & 0o7777;
    let different_mode = disk_mode ^ 0o020; // flip group write bit

    let mut entry = FileEntry::new_file("perm-mismatch.txt".into(), 4, different_mode);
    entry.set_mtime(mtime.unix_seconds(), mtime.nanoseconds());
    entry.set_uid(meta.uid());
    entry.set_gid(meta.gid());

    let opts = MetadataOptions::new()
        .preserve_permissions(true)
        .preserve_times(true)
        .preserve_owner(true)
        .preserve_group(true);

    assert!(
        !metadata_unchanged(&entry, &opts, &meta),
        "should return false when permissions differ"
    );
}

#[cfg(unix)]
#[test]
fn metadata_unchanged_returns_false_on_mtime_mismatch() {
    use protocol::flist::FileEntry;

    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("mtime-mismatch.txt");
    fs::write(&dest, b"data").expect("write dest");

    let meta = fs::metadata(&dest).expect("metadata");

    let mut entry = FileEntry::new_file("mtime-mismatch.txt".into(), 4, meta.mode() & 0o7777);
    // Set a different mtime
    entry.set_mtime(1_600_000_000, 0);
    entry.set_uid(meta.uid());
    entry.set_gid(meta.gid());

    let opts = MetadataOptions::new()
        .preserve_permissions(true)
        .preserve_times(true)
        .preserve_owner(true)
        .preserve_group(true);

    assert!(
        !metadata_unchanged(&entry, &opts, &meta),
        "should return false when mtime differs"
    );
}

#[cfg(unix)]
#[test]
fn metadata_unchanged_ignores_perms_when_not_preserved() {
    use protocol::flist::FileEntry;

    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("no-perms.txt");
    fs::write(&dest, b"data").expect("write dest");

    let meta = fs::metadata(&dest).expect("metadata");
    let mtime = FileTime::from_last_modification_time(&meta);

    // Permissions differ but preservation is disabled
    let mut entry = FileEntry::new_file("no-perms.txt".into(), 4, 0o777);
    entry.set_mtime(mtime.unix_seconds(), mtime.nanoseconds());
    entry.set_uid(meta.uid());
    entry.set_gid(meta.gid());

    let opts = MetadataOptions::new()
        .preserve_permissions(false)
        .preserve_times(true)
        .preserve_owner(true)
        .preserve_group(true);

    assert!(
        metadata_unchanged(&entry, &opts, &meta),
        "should return true when perms differ but preservation is off"
    );
}

#[cfg(unix)]
#[test]
fn metadata_unchanged_returns_false_when_chmod_would_change_mode() {
    use protocol::flist::FileEntry;

    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("chmod-changes.txt");
    fs::write(&dest, b"data").expect("write dest");

    let meta = fs::metadata(&dest).expect("metadata");

    let entry = FileEntry::new_file("chmod-changes.txt".into(), 4, 0o644);

    // u+x would change 0o644 to 0o744
    let chmod = crate::ChmodModifiers::parse("u+x").expect("parse chmod");
    let opts = MetadataOptions::new().with_chmod(Some(chmod));

    assert!(
        !metadata_unchanged(&entry, &opts, &meta),
        "should return false when chmod would change mode"
    );
}

#[cfg(unix)]
#[test]
fn metadata_unchanged_returns_true_when_chmod_is_noop() {
    use protocol::flist::FileEntry;
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("chmod-noop.txt");
    fs::write(&dest, b"data").expect("write dest");
    fs::set_permissions(&dest, PermissionsExt::from_mode(0o755)).expect("set perms");

    let meta = fs::metadata(&dest).expect("metadata");
    let mtime = FileTime::from_last_modification_time(&meta);

    let mut entry = FileEntry::new_file("chmod-noop.txt".into(), 4, meta.mode() & 0o7777);
    entry.set_mtime(mtime.unix_seconds(), mtime.nanoseconds());
    entry.set_uid(meta.uid());
    entry.set_gid(meta.gid());

    // u+x on a file that already has u+x is a no-op
    let chmod = crate::ChmodModifiers::parse("u+x").expect("parse chmod");
    let opts = MetadataOptions::new()
        .preserve_permissions(true)
        .preserve_times(true)
        .preserve_owner(true)
        .preserve_group(true)
        .with_chmod(Some(chmod));

    assert!(
        metadata_unchanged(&entry, &opts, &meta),
        "should return true when chmod modifier does not change mode"
    );
}

#[cfg(unix)]
#[test]
fn metadata_unchanged_returns_true_when_owner_override_matches() {
    use protocol::flist::FileEntry;

    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("owner-match.txt");
    fs::write(&dest, b"data").expect("write dest");

    let meta = fs::metadata(&dest).expect("metadata");
    let mtime = FileTime::from_last_modification_time(&meta);

    let mut entry = FileEntry::new_file("owner-match.txt".into(), 4, meta.mode() & 0o7777);
    entry.set_mtime(mtime.unix_seconds(), mtime.nanoseconds());
    entry.set_uid(meta.uid());
    entry.set_gid(meta.gid());

    // Set owner override to current UID - no actual change needed
    let opts = MetadataOptions::new()
        .preserve_permissions(true)
        .preserve_times(true)
        .with_owner_override(Some(meta.uid()));

    assert!(
        metadata_unchanged(&entry, &opts, &meta),
        "should return true when owner override matches current uid"
    );
}

#[cfg(unix)]
#[test]
fn metadata_unchanged_returns_false_when_owner_override_differs() {
    use protocol::flist::FileEntry;

    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("owner-differ.txt");
    fs::write(&dest, b"data").expect("write dest");

    let meta = fs::metadata(&dest).expect("metadata");
    let mtime = FileTime::from_last_modification_time(&meta);

    let mut entry = FileEntry::new_file("owner-differ.txt".into(), 4, meta.mode() & 0o7777);
    entry.set_mtime(mtime.unix_seconds(), mtime.nanoseconds());
    entry.set_uid(meta.uid());
    entry.set_gid(meta.gid());

    // Set owner override to a different UID
    let opts = MetadataOptions::new()
        .preserve_permissions(true)
        .preserve_times(true)
        .with_owner_override(Some(meta.uid() + 1));

    assert!(
        !metadata_unchanged(&entry, &opts, &meta),
        "should return false when owner override differs from current uid"
    );
}

#[cfg(unix)]
#[test]
fn metadata_unchanged_returns_true_when_group_override_matches() {
    use protocol::flist::FileEntry;

    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("group-match.txt");
    fs::write(&dest, b"data").expect("write dest");

    let meta = fs::metadata(&dest).expect("metadata");
    let mtime = FileTime::from_last_modification_time(&meta);

    let mut entry = FileEntry::new_file("group-match.txt".into(), 4, meta.mode() & 0o7777);
    entry.set_mtime(mtime.unix_seconds(), mtime.nanoseconds());
    entry.set_uid(meta.uid());
    entry.set_gid(meta.gid());

    // Set group override to current GID - no actual change needed
    let opts = MetadataOptions::new()
        .preserve_permissions(true)
        .preserve_times(true)
        .with_group_override(Some(meta.gid()));

    assert!(
        metadata_unchanged(&entry, &opts, &meta),
        "should return true when group override matches current gid"
    );
}

/// UTS-16.b: applying permissions through a destination path whose parent
/// component is a symlink to an outside directory must NOT chmod the
/// outside target. Upstream `syscall.c:do_chmod_at()` (rsync 3.4.3+)
/// opens the parent under `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS` so a
/// symlink swapped into any parent component is rejected.
///
/// Regression coverage for the testsuite `chdir-symlink-race` failure on
/// the `-r --size-only into upload/ root` flavour: the symlinked
/// `subdir` was being chased by the path-based chmod, flipping the
/// outside sentinel from 0o600 to 0o666.
#[cfg(unix)]
#[test]
fn apply_permissions_from_entry_refuses_parent_symlink_escape() {
    use protocol::flist::FileEntry;
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let module = temp.path().join("module");
    let outside = temp.path().join("outside");
    fs::create_dir(&module).expect("create module");
    fs::create_dir(&outside).expect("create outside");

    // Outside-the-module sentinel the attacker is trying to chmod via
    // the symlink-traversed path.
    let outside_target = outside.join("target.txt");
    fs::write(&outside_target, b"OUTSIDE_SECRET_DATA").expect("write outside");
    fs::set_permissions(&outside_target, PermissionsExt::from_mode(0o600))
        .expect("set outside mode");

    // Attacker plants a symlink at module/subdir -> outside, then the
    // receiver tries to chmod module/subdir/target.txt (which resolves
    // to outside/target.txt via the symlink).
    std::os::unix::fs::symlink(&outside, module.join("subdir")).expect("plant symlink");

    let dest = module.join("subdir").join("target.txt");
    let entry = FileEntry::new_file("target.txt".into(), 19, 0o666);
    let opts = MetadataOptions::new()
        .preserve_permissions(true)
        .preserve_times(false);

    let result = apply_metadata_from_file_entry(&dest, &entry, &opts);
    assert!(
        result.is_err(),
        "chmod through a symlinked parent must fail, not silently succeed"
    );

    let outside_mode = fs::metadata(&outside_target)
        .expect("stat outside")
        .permissions()
        .mode()
        & 0o7777;
    assert_eq!(
        outside_mode, 0o600,
        "outside file mode must remain 0o600 after refused chmod escape (got {outside_mode:o})"
    );
}

/// KDL.8: with `--keep-dirlinks` active, a path-based chmod through a
/// destination whose parent is a symlink-to-a-real-dir must succeed by
/// resolving the symlink, instead of being refused by the dirfd-anchored
/// `secure_chmod_at` sandbox. Mirrors upstream `generator.c:1344`'s
/// `link_stat(fname, &sx.st, keep_dirlinks && is_dir)` which follows the
/// symlinked parent at stat time.
///
/// Regression coverage for the macOS panic in
/// `engine::local_copy::tests::execute_keep_dirlinks_multiple_symlink_subdirs_all_preserved`
/// where the `apply dest_mode` chmod hit `ENOTDIR` because `secure_open_dir`
/// rejects symlinked parents.
#[cfg(unix)]
#[test]
fn keep_dirlinks_bypasses_secure_chmod_sandbox() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let real_alpha = temp.path().join("real_alpha");
    fs::create_dir(&real_alpha).expect("create real_alpha");

    let dest_root = temp.path().join("dest");
    fs::create_dir(&dest_root).expect("create dest");
    // dest/alpha is a symlink to real_alpha/.
    std::os::unix::fs::symlink(&real_alpha, dest_root.join("alpha")).expect("symlink alpha");

    let source = temp.path().join("a.txt");
    fs::write(&source, b"alpha").expect("write source");
    fs::set_permissions(&source, PermissionsExt::from_mode(0o644)).expect("source mode");

    let dest_file = dest_root.join("alpha").join("a.txt");
    fs::write(&dest_file, b"alpha").expect("write dest through symlink");
    fs::set_permissions(&dest_file, PermissionsExt::from_mode(0o600)).expect("dest mode");

    let source_meta = fs::metadata(&source).expect("stat source");

    // Without keep_dirlinks the path-based sandbox refuses the chmod
    // because dest_root/alpha is a symlink. This is the bug symptom.
    let strict_opts = MetadataOptions::new()
        .preserve_permissions(true)
        .preserve_times(false);
    let strict_result = apply_file_metadata_with_options(&dest_file, &source_meta, &strict_opts);
    assert!(
        strict_result.is_err(),
        "without keep_dirlinks the dirfd sandbox must reject symlinked parents",
    );

    // With keep_dirlinks the bypass uses std::fs::set_permissions which
    // follows the symlink, so the chmod lands on real_alpha/a.txt.
    let opts = MetadataOptions::new()
        .preserve_permissions(true)
        .preserve_times(false)
        .with_keep_dirlinks(true);
    apply_file_metadata_with_options(&dest_file, &source_meta, &opts)
        .expect("chmod must succeed through symlinked parent under --keep-dirlinks");

    let landed_mode = fs::metadata(real_alpha.join("a.txt"))
        .expect("stat real_alpha/a.txt")
        .permissions()
        .mode()
        & 0o7777;
    assert_eq!(
        landed_mode, 0o644,
        "chmod must follow the symlink and land on real_alpha/a.txt (got {landed_mode:o})",
    );
}

// KDL.7.1 cross-platform regression: `--keep-dirlinks` must succeed through a
// symlinked destination directory on every supported platform. Pins the
// guarantee from PR #5793 (macOS chmod bypass), PRs #5798/#5799 (extended to
// four more chmod sites), and the KDL.7 audit that confirmed Linux
// (openat2 RESOLVE_BENEATH sandboxes parents only, leaf symlink legal) and
// Windows (chmod is a no-op) were CLEAN by construction but unpinned.
#[test]
fn keep_dirlinks_bypass_is_cross_platform_safe() {
    let temp = tempdir().expect("tempdir");
    let real_dir = temp.path().join("real_dir");
    fs::create_dir(&real_dir).expect("create real_dir");

    let dest_root = temp.path().join("dest");
    fs::create_dir(&dest_root).expect("create dest");
    let symlinked_dest = dest_root.join("alpha");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&real_dir, &symlinked_dest).expect("symlink alpha");
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(&real_dir, &symlinked_dest).expect("symlink_dir alpha");

    let source = temp.path().join("a.txt");
    fs::write(&source, b"alpha").expect("write source");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&source, PermissionsExt::from_mode(0o755)).expect("source mode");
    }

    let dest_file = symlinked_dest.join("a.txt");
    fs::write(&dest_file, b"alpha").expect("write dest through symlink");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dest_file, PermissionsExt::from_mode(0o600)).expect("dest mode");
    }

    let source_meta = fs::metadata(&source).expect("stat source");

    let opts = MetadataOptions::new()
        .preserve_permissions(true)
        .preserve_times(false)
        .with_keep_dirlinks(true);
    apply_file_metadata_with_options(&dest_file, &source_meta, &opts)
        .expect("apply_file_metadata_with_options must succeed under --keep-dirlinks");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let landed_mode = fs::metadata(real_dir.join("a.txt"))
            .expect("stat real_dir/a.txt")
            .permissions()
            .mode()
            & 0o7777;
        assert_eq!(
            landed_mode, 0o755,
            "chmod must follow the symlink and land on real_dir/a.txt (got {landed_mode:o})",
        );
    }

    #[cfg(windows)]
    {
        // Windows chmod is a no-op (FILE_ATTRIBUTE_READONLY only). The
        // KDL.7.1 guarantee on Windows is that the call succeeds without
        // ELOOP / permission errors through the symlinked dest dir.
        let landed = fs::metadata(real_dir.join("a.txt")).expect("stat real_dir/a.txt");
        assert!(
            landed.is_file(),
            "real_dir/a.txt must remain a regular file"
        );
    }
}

// Mirrors upstream rsync's `change_uid`/`change_gid` privilege gates
// (rsync.c:526-528): a non-root process must not attempt to set a file's owner
// uid, and may only set its group to one it belongs to. Before this gate,
// oc-rsync attempted the chown unconditionally and surfaced the resulting
// EPERM as a fatal exit-code-23 error (e.g. under `-aR` when an implied parent
// directory is owned by root).
#[cfg(unix)]
#[test]
fn non_root_ownership_gate_drops_owner_keeps_member_group() {
    if rustix::process::geteuid().is_root() {
        // As root the gate is a no-op; root behaviour is exercised by the
        // geteuid()==0 chown tests above.
        return;
    }

    let owner = Some(ownership::uid_from_raw(0));
    let group = Some(ownership::gid_from_raw(rustix::process::getegid().as_raw()));

    let gated_owner = super::ownership::gate_preserved_owner(owner);
    let gated_group = super::ownership::gate_preserved_group(group);

    assert!(
        gated_owner.is_none(),
        "non-root must not attempt to set the owner uid"
    );
    assert!(
        gated_group.is_some(),
        "non-root may set the group to its own effective gid"
    );
}
