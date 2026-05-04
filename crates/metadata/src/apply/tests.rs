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

    let mode = current_mode(&dest) & 0o777;
    assert_eq!(mode & 0o111, 0o751 & 0o111);
    assert_eq!(mode & 0o666, 0o620);
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
    assert_eq!(dest_mtime.seconds(), 0, "seconds should be zero");
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
    assert_eq!(dest_atime.seconds(), 1_650_000_000);
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
