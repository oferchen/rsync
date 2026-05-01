use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::*;

#[test]
fn file_type_from_mode() {
    assert_eq!(FileType::from_mode(0o100644), Some(FileType::Regular));
    assert_eq!(FileType::from_mode(0o040755), Some(FileType::Directory));
    assert_eq!(FileType::from_mode(0o120777), Some(FileType::Symlink));
    assert_eq!(FileType::from_mode(0o060660), Some(FileType::BlockDevice));
    assert_eq!(FileType::from_mode(0o020666), Some(FileType::CharDevice));
    assert_eq!(FileType::from_mode(0o010644), Some(FileType::Fifo));
    assert_eq!(FileType::from_mode(0o140755), Some(FileType::Socket));
}

#[test]
fn file_type_from_mode_invalid() {
    assert_eq!(FileType::from_mode(0o000644), None);
    assert_eq!(FileType::from_mode(0o050000), None);
    assert_eq!(FileType::from_mode(0o070000), None);
}

#[test]
fn file_type_round_trip() {
    for ft in [
        FileType::Regular,
        FileType::Directory,
        FileType::Symlink,
        FileType::BlockDevice,
        FileType::CharDevice,
        FileType::Fifo,
        FileType::Socket,
    ] {
        let mode = ft.to_mode_bits() | 0o644;
        assert_eq!(FileType::from_mode(mode), Some(ft));
    }
}

#[test]
fn file_type_predicates() {
    assert!(FileType::Regular.is_regular());
    assert!(!FileType::Directory.is_regular());

    assert!(FileType::Directory.is_dir());
    assert!(!FileType::Regular.is_dir());

    assert!(FileType::Symlink.is_symlink());
    assert!(!FileType::Regular.is_symlink());

    assert!(FileType::BlockDevice.is_device());
    assert!(FileType::CharDevice.is_device());
    assert!(!FileType::Regular.is_device());
    assert!(!FileType::Directory.is_device());
    assert!(!FileType::Fifo.is_device());
    assert!(!FileType::Socket.is_device());
}

#[test]
fn file_type_clone_and_eq() {
    let ft = FileType::Regular;
    let cloned = ft;
    assert_eq!(ft, cloned);
}

#[test]
fn file_type_hash() {
    use std::collections::HashSet;
    let mut set = HashSet::new();
    set.insert(FileType::Regular);
    set.insert(FileType::Directory);
    assert!(set.contains(&FileType::Regular));
    assert!(set.contains(&FileType::Directory));
    assert!(!set.contains(&FileType::Symlink));
}

#[test]
fn file_type_debug() {
    let debug = format!("{:?}", FileType::Regular);
    assert_eq!(debug, "Regular");
}

#[test]
fn new_file_entry() {
    let entry = FileEntry::new_file("test.txt".into(), 1024, 0o644);
    assert_eq!(entry.name(), "test.txt");
    assert_eq!(entry.size(), 1024);
    assert_eq!(entry.permissions(), 0o644);
    assert_eq!(entry.file_type(), FileType::Regular);
    assert!(entry.is_file());
    assert!(!entry.is_dir());
}

#[test]
fn new_file_entry_permissions_masked() {
    let entry = FileEntry::new_file("test.txt".into(), 100, 0o177777);
    assert_eq!(entry.permissions(), 0o7777);
}

#[test]
fn new_directory_entry() {
    let entry = FileEntry::new_directory("subdir".into(), 0o755);
    assert_eq!(entry.name(), "subdir");
    assert_eq!(entry.size(), 0);
    assert_eq!(entry.permissions(), 0o755);
    assert_eq!(entry.file_type(), FileType::Directory);
    assert!(entry.is_dir());
    assert!(!entry.is_file());
}

#[test]
fn new_symlink_entry() {
    let entry = FileEntry::new_symlink("link".into(), "target".into());
    assert_eq!(entry.name(), "link");
    assert!(entry.is_symlink());
    assert_eq!(
        entry.link_target().map(|p| p.as_path()),
        Some("target".as_ref())
    );
}

#[test]
fn entry_mtime_setting() {
    let mut entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
    entry.set_mtime(1700000000, 123456789);
    assert_eq!(entry.mtime(), 1700000000);
    assert_eq!(entry.mtime_nsec(), 123456789);
}

#[test]
fn entry_uid_gid_setting() {
    let mut entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
    assert_eq!(entry.uid(), None);
    assert_eq!(entry.gid(), None);

    entry.set_uid(1000);
    entry.set_gid(1001);

    assert_eq!(entry.uid(), Some(1000));
    assert_eq!(entry.gid(), Some(1001));
}

#[test]
fn entry_link_target_setting() {
    let mut entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
    assert!(entry.link_target().is_none());

    entry.set_link_target("/some/target".into());
    assert_eq!(
        entry.link_target().map(|p| p.as_path()),
        Some("/some/target".as_ref())
    );
}

#[test]
fn entry_rdev_setting() {
    let mut entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
    assert_eq!(entry.rdev_major(), None);
    assert_eq!(entry.rdev_minor(), None);

    entry.set_rdev(8, 1);

    assert_eq!(entry.rdev_major(), Some(8));
    assert_eq!(entry.rdev_minor(), Some(1));
}

#[test]
fn entry_path_accessor() {
    let entry = FileEntry::new_file("some/nested/path.txt".into(), 100, 0o644);
    assert_eq!(entry.path(), &PathBuf::from("some/nested/path.txt"));
}

#[test]
fn entry_mode_accessor() {
    let entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
    let mode = entry.mode();
    assert_eq!(mode & 0o7777, 0o644);
    assert_eq!(mode & 0o170000, 0o100000); // Regular file type
}

#[test]
fn entry_clone_and_eq() {
    let entry = FileEntry::new_file("test.txt".into(), 1024, 0o644);
    let cloned = entry.clone();
    assert_eq!(entry, cloned);
}

#[test]
fn entry_debug_format() {
    let entry = FileEntry::new_file("test.txt".into(), 1024, 0o644);
    let debug = format!("{entry:?}");
    assert!(debug.contains("FileEntry"));
    assert!(debug.contains("test.txt"));
}

#[test]
fn entry_from_raw() {
    let flags = crate::flist::flags::FileFlags::default();
    let entry = FileEntry::from_raw(
        "raw_file.txt".into(),
        2048,
        0o100755,
        1700000000,
        999999,
        flags,
    );

    assert_eq!(entry.name(), "raw_file.txt");
    assert_eq!(entry.size(), 2048);
    assert_eq!(entry.mode(), 0o100755);
    assert_eq!(entry.mtime(), 1700000000);
    assert_eq!(entry.mtime_nsec(), 999999);
    assert!(entry.is_file());
}

#[test]
fn entry_file_type_fallback() {
    let flags = crate::flist::flags::FileFlags::default();
    let entry = FileEntry::from_raw(
        "unknown.txt".into(),
        100,
        0o000644, // Invalid mode type bits
        0,
        0,
        flags,
    );

    // Should fall back to Regular
    assert_eq!(entry.file_type(), FileType::Regular);
}

#[test]
fn symlink_not_file() {
    let entry = FileEntry::new_symlink("link".into(), "target".into());
    assert!(!entry.is_file());
    assert!(!entry.is_dir());
    assert!(entry.is_symlink());
}

#[test]
fn directory_size_is_zero() {
    let entry = FileEntry::new_directory("dir".into(), 0o755);
    assert_eq!(entry.size(), 0);
}

#[test]
fn file_entry_flags_accessor() {
    let entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
    let _flags = entry.flags();
}

#[test]
fn dirname_root_level_entry() {
    let entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
    assert_eq!(&**entry.dirname(), Path::new(""));
}

#[test]
fn dirname_nested_entry() {
    let entry = FileEntry::new_file("src/lib/foo.rs".into(), 100, 0o644);
    assert_eq!(&**entry.dirname(), Path::new("src/lib"));
}

#[test]
fn dirname_single_level() {
    let entry = FileEntry::new_file("dir/file.txt".into(), 100, 0o644);
    assert_eq!(&**entry.dirname(), Path::new("dir"));
}

#[test]
fn set_dirname_replaces_existing() {
    let mut entry = FileEntry::new_file("dir/file.txt".into(), 100, 0o644);
    let shared = Arc::from(Path::new("other_dir"));
    entry.set_dirname(Arc::clone(&shared));
    assert!(Arc::ptr_eq(entry.dirname(), &shared));
}

#[test]
fn dirname_shared_across_entries() {
    use crate::flist::intern::PathInterner;

    let mut interner = PathInterner::new();
    let mut entry1 = FileEntry::new_file("dir/a.txt".into(), 100, 0o644);
    let mut entry2 = FileEntry::new_file("dir/b.txt".into(), 200, 0o644);

    let dir = interner.intern(Path::new("dir"));
    entry1.set_dirname(Arc::clone(&dir));
    entry2.set_dirname(Arc::clone(&dir));

    assert!(Arc::ptr_eq(entry1.dirname(), entry2.dirname()));
}

/// Verifies the struct size optimization: FileEntry should be <= 96 bytes
/// inline (down from ~295 bytes before the Box<FileEntryExtras> refactor).
/// This guards against accidental field additions that bloat the hot path.
#[test]
fn file_entry_size_optimized() {
    let size = std::mem::size_of::<FileEntry>();
    assert!(
        size <= 96,
        "FileEntry is {size} bytes; expected <= 96. \
         Did you add a field to FileEntry instead of FileEntryExtras?"
    );
}

/// Regular file entries should not allocate extras.
#[test]
fn regular_file_no_extras() {
    let entry = FileEntry::new_file("test.txt".into(), 1024, 0o644);
    assert!(
        entry.extras.is_none(),
        "Regular files should not allocate extras"
    );
}

/// Symlink entries should allocate extras for the link target.
#[test]
fn symlink_has_extras() {
    let entry = FileEntry::new_symlink("link".into(), "target".into());
    assert!(entry.extras.is_some());
    assert_eq!(
        entry.link_target().map(|p| p.as_path()),
        Some("target".as_ref())
    );
}

/// Extras should be lazily allocated on first setter call.
#[test]
fn extras_lazy_allocation() {
    let mut entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
    assert!(entry.extras.is_none());

    entry.set_atime(1700000000);
    assert!(entry.extras.is_some());
    assert_eq!(entry.atime(), 1700000000);
}

/// Default values for extras getters when extras is None.
#[test]
fn extras_default_values() {
    let entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
    assert_eq!(entry.atime(), 0);
    assert_eq!(entry.atime_nsec(), 0);
    assert_eq!(entry.crtime(), 0);
    assert_eq!(entry.link_target(), None);
    assert_eq!(entry.user_name(), None);
    assert_eq!(entry.group_name(), None);
    assert_eq!(entry.rdev_major(), None);
    assert_eq!(entry.rdev_minor(), None);
    assert_eq!(entry.hardlink_idx(), None);
    assert_eq!(entry.hardlink_dev(), None);
    assert_eq!(entry.hardlink_ino(), None);
    assert_eq!(entry.checksum(), None);
    assert_eq!(entry.acl_ndx(), None);
    assert_eq!(entry.def_acl_ndx(), None);
    assert_eq!(entry.xattr_ndx(), None);
}

/// Clone preserves extras correctly.
#[test]
fn clone_with_extras() {
    let mut entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
    entry.set_atime(1700000000);
    entry.set_checksum(vec![1, 2, 3]);
    entry.set_hardlink_idx(42);

    let cloned = entry.clone();
    assert_eq!(cloned.atime(), 1700000000);
    assert_eq!(cloned.checksum(), Some(&[1, 2, 3][..]));
    assert_eq!(cloned.hardlink_idx(), Some(42));
    assert_eq!(entry, cloned);
}

/// PartialEq handles None vs Some extras correctly.
#[test]
fn equality_with_different_extras() {
    let entry1 = FileEntry::new_file("test.txt".into(), 100, 0o644);
    let mut entry2 = FileEntry::new_file("test.txt".into(), 100, 0o644);
    assert_eq!(entry1, entry2);

    entry2.set_atime(1);
    assert_ne!(entry1, entry2);
}

#[test]
fn extras_user_name_set_get() {
    let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
    assert_eq!(entry.user_name(), None);

    entry.set_user_name("alice".to_string());
    assert_eq!(entry.user_name(), Some("alice"));
}

#[test]
fn extras_group_name_set_get() {
    let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
    assert_eq!(entry.group_name(), None);

    entry.set_group_name("staff".to_string());
    assert_eq!(entry.group_name(), Some("staff"));
}

#[test]
fn extras_atime_set_get() {
    let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
    assert_eq!(entry.atime(), 0);

    entry.set_atime(1_700_000_000);
    assert_eq!(entry.atime(), 1_700_000_000);
}

#[test]
fn extras_atime_negative() {
    let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
    entry.set_atime(-1);
    assert_eq!(entry.atime(), -1);
}

#[test]
fn extras_atime_nsec_set_get() {
    let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
    assert_eq!(entry.atime_nsec(), 0);

    entry.set_atime_nsec(999_999_999);
    assert_eq!(entry.atime_nsec(), 999_999_999);
}

#[test]
fn extras_crtime_set_get() {
    let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
    assert_eq!(entry.crtime(), 0);

    entry.set_crtime(1_600_000_000);
    assert_eq!(entry.crtime(), 1_600_000_000);
}

#[test]
fn extras_crtime_negative() {
    let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
    entry.set_crtime(-100);
    assert_eq!(entry.crtime(), -100);
}

#[test]
fn extras_hardlink_idx_set_get() {
    let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
    assert_eq!(entry.hardlink_idx(), None);

    entry.set_hardlink_idx(42);
    assert_eq!(entry.hardlink_idx(), Some(42));
}

#[test]
fn extras_hardlink_idx_zero() {
    let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
    entry.set_hardlink_idx(0);
    assert_eq!(entry.hardlink_idx(), Some(0));
}

#[test]
fn extras_hardlink_dev_set_get() {
    let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
    assert_eq!(entry.hardlink_dev(), None);

    entry.set_hardlink_dev(0xFD00);
    assert_eq!(entry.hardlink_dev(), Some(0xFD00));
}

#[test]
fn extras_hardlink_dev_negative() {
    let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
    entry.set_hardlink_dev(-1);
    assert_eq!(entry.hardlink_dev(), Some(-1));
}

#[test]
fn extras_hardlink_ino_set_get() {
    let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
    assert_eq!(entry.hardlink_ino(), None);

    entry.set_hardlink_ino(123_456);
    assert_eq!(entry.hardlink_ino(), Some(123_456));
}

#[test]
fn extras_hardlink_ino_negative() {
    let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
    entry.set_hardlink_ino(-999);
    assert_eq!(entry.hardlink_ino(), Some(-999));
}

#[test]
fn extras_checksum_set_get() {
    let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
    assert_eq!(entry.checksum(), None);

    let sum = vec![0xDE, 0xAD, 0xBE, 0xEF];
    entry.set_checksum(sum.clone());
    assert_eq!(entry.checksum(), Some(sum.as_slice()));
}

#[test]
fn extras_checksum_empty() {
    let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
    entry.set_checksum(vec![]);
    assert_eq!(entry.checksum(), Some(&[][..]));
}

#[test]
fn extras_checksum_max_length() {
    let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
    let sum = vec![0xFF; 32]; // max 32 bytes (e.g., SHA-256 / XXH128+MD5)
    entry.set_checksum(sum.clone());
    assert_eq!(entry.checksum(), Some(sum.as_slice()));
}

#[test]
fn extras_acl_ndx_set_get() {
    let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
    assert_eq!(entry.acl_ndx(), None);

    entry.set_acl_ndx(7);
    assert_eq!(entry.acl_ndx(), Some(7));
}

#[test]
fn extras_acl_ndx_zero() {
    let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
    entry.set_acl_ndx(0);
    assert_eq!(entry.acl_ndx(), Some(0));
}

#[test]
fn extras_def_acl_ndx_set_get() {
    let mut entry = FileEntry::new_directory("d".into(), 0o755);
    assert_eq!(entry.def_acl_ndx(), None);

    entry.set_def_acl_ndx(3);
    assert_eq!(entry.def_acl_ndx(), Some(3));
}

#[test]
fn extras_xattr_ndx_set_get() {
    let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
    assert_eq!(entry.xattr_ndx(), None);

    entry.set_xattr_ndx(99);
    assert_eq!(entry.xattr_ndx(), Some(99));
}

#[test]
fn extras_xattr_ndx_zero() {
    let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
    entry.set_xattr_ndx(0);
    assert_eq!(entry.xattr_ndx(), Some(0));
}

#[test]
fn extras_link_target_empty_path() {
    let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
    entry.set_link_target(PathBuf::new());
    assert_eq!(
        entry.link_target().map(|p| p.as_path()),
        Some(Path::new(""))
    );
}

#[test]
fn extras_user_name_empty_string() {
    let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
    entry.set_user_name(String::new());
    assert_eq!(entry.user_name(), Some(""));
}

#[test]
fn extras_group_name_empty_string() {
    let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
    entry.set_group_name(String::new());
    assert_eq!(entry.group_name(), Some(""));
}

/// Setting multiple independent extras fields allocates once and stores all.
#[test]
fn extras_multiple_fields_independent() {
    let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
    assert!(entry.extras.is_none());

    entry.set_atime(100);
    assert!(entry.extras.is_some());

    entry.set_crtime(200);
    entry.set_user_name("root".to_string());
    entry.set_group_name("wheel".to_string());
    entry.set_hardlink_idx(5);
    entry.set_acl_ndx(10);
    entry.set_xattr_ndx(20);
    entry.set_checksum(vec![0xAA]);
    entry.set_rdev(1, 2);
    entry.set_hardlink_dev(300);
    entry.set_hardlink_ino(400);
    entry.set_atime_nsec(500);
    entry.set_def_acl_ndx(15);
    entry.set_link_target("/target".into());

    assert_eq!(entry.atime(), 100);
    assert_eq!(entry.crtime(), 200);
    assert_eq!(entry.user_name(), Some("root"));
    assert_eq!(entry.group_name(), Some("wheel"));
    assert_eq!(entry.hardlink_idx(), Some(5));
    assert_eq!(entry.acl_ndx(), Some(10));
    assert_eq!(entry.xattr_ndx(), Some(20));
    assert_eq!(entry.checksum(), Some(&[0xAA][..]));
    assert_eq!(entry.rdev_major(), Some(1));
    assert_eq!(entry.rdev_minor(), Some(2));
    assert_eq!(entry.hardlink_dev(), Some(300));
    assert_eq!(entry.hardlink_ino(), Some(400));
    assert_eq!(entry.atime_nsec(), 500);
    assert_eq!(entry.def_acl_ndx(), Some(15));
    assert_eq!(
        entry.link_target().map(|p| p.as_path()),
        Some(Path::new("/target"))
    );
}

/// Overwriting an extras field replaces the old value.
#[test]
fn extras_overwrite_value() {
    let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
    entry.set_atime(1);
    assert_eq!(entry.atime(), 1);

    entry.set_atime(2);
    assert_eq!(entry.atime(), 2);
}

#[test]
fn extras_overwrite_checksum() {
    let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
    entry.set_checksum(vec![1, 2, 3]);
    entry.set_checksum(vec![4, 5]);
    assert_eq!(entry.checksum(), Some(&[4, 5][..]));
}

#[test]
fn extras_overwrite_user_name() {
    let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
    entry.set_user_name("alice".to_string());
    entry.set_user_name("bob".to_string());
    assert_eq!(entry.user_name(), Some("bob"));
}

#[test]
fn extras_overwrite_link_target() {
    let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
    entry.set_link_target("/old".into());
    entry.set_link_target("/new".into());
    assert_eq!(
        entry.link_target().map(|p| p.as_path()),
        Some(Path::new("/new"))
    );
}

/// Clone preserves all 15 extras fields.
#[test]
fn clone_preserves_all_extras() {
    let mut entry = FileEntry::new_file("f.txt".into(), 100, 0o644);
    entry.set_link_target("/tgt".into());
    entry.set_user_name("u".to_string());
    entry.set_group_name("g".to_string());
    entry.set_atime(10);
    entry.set_crtime(20);
    entry.set_atime_nsec(30);
    entry.set_rdev(40, 50);
    entry.set_hardlink_idx(60);
    entry.set_hardlink_dev(70);
    entry.set_hardlink_ino(80);
    entry.set_checksum(vec![0x90]);
    entry.set_acl_ndx(100);
    entry.set_def_acl_ndx(110);
    entry.set_xattr_ndx(120);

    let c = entry.clone();
    assert_eq!(
        c.link_target().map(|p| p.as_path()),
        Some(Path::new("/tgt"))
    );
    assert_eq!(c.user_name(), Some("u"));
    assert_eq!(c.group_name(), Some("g"));
    assert_eq!(c.atime(), 10);
    assert_eq!(c.crtime(), 20);
    assert_eq!(c.atime_nsec(), 30);
    assert_eq!(c.rdev_major(), Some(40));
    assert_eq!(c.rdev_minor(), Some(50));
    assert_eq!(c.hardlink_idx(), Some(60));
    assert_eq!(c.hardlink_dev(), Some(70));
    assert_eq!(c.hardlink_ino(), Some(80));
    assert_eq!(c.checksum(), Some(&[0x90][..]));
    assert_eq!(c.acl_ndx(), Some(100));
    assert_eq!(c.def_acl_ndx(), Some(110));
    assert_eq!(c.xattr_ndx(), Some(120));
    assert_eq!(entry, c);
}

/// Clone without extras produces None extras.
#[test]
fn clone_without_extras() {
    let entry = FileEntry::new_file("f.txt".into(), 100, 0o644);
    let c = entry.clone();
    assert!(c.extras.is_none());
    assert_eq!(entry, c);
}

/// PartialEq: both have extras with same values.
#[test]
fn equality_both_extras_same() {
    let mut a = FileEntry::new_file("f.txt".into(), 0, 0o644);
    let mut b = FileEntry::new_file("f.txt".into(), 0, 0o644);
    a.set_atime(99);
    a.set_checksum(vec![1]);
    b.set_atime(99);
    b.set_checksum(vec![1]);
    assert_eq!(a, b);
}

/// PartialEq: both have extras with different values.
#[test]
fn equality_both_extras_different() {
    let mut a = FileEntry::new_file("f.txt".into(), 0, 0o644);
    let mut b = FileEntry::new_file("f.txt".into(), 0, 0o644);
    a.set_atime(1);
    b.set_atime(2);
    assert_ne!(a, b);
}

/// PartialEq: one has extras, other does not.
#[test]
fn equality_one_has_extras() {
    let a = FileEntry::new_file("f.txt".into(), 0, 0o644);
    let mut b = FileEntry::new_file("f.txt".into(), 0, 0o644);
    b.set_hardlink_idx(1);
    assert_ne!(a, b);
    assert_ne!(b, a);
}

/// PartialEq: extras with all-default values vs None extras.
/// These are NOT equal because Option<Box<...>> Some(default) != None.
#[test]
fn equality_default_extras_vs_none() {
    let a = FileEntry::new_file("f.txt".into(), 0, 0o644);
    let mut b = FileEntry::new_file("f.txt".into(), 0, 0o644);
    // Force extras allocation with a zero/default value.
    b.set_atime(0);
    // a.extras is None, b.extras is Some(default) - not structurally equal.
    assert_ne!(a, b);
}

/// Device constructors allocate extras for rdev fields.
#[test]
fn block_device_constructor_extras() {
    let entry = FileEntry::new_block_device("dev/sda".into(), 0o660, 8, 0);
    assert!(entry.extras.is_some());
    assert_eq!(entry.rdev_major(), Some(8));
    assert_eq!(entry.rdev_minor(), Some(0));
    assert!(entry.is_block_device());
    assert!(entry.is_device());
    assert!(!entry.is_char_device());
}

#[test]
fn char_device_constructor_extras() {
    let entry = FileEntry::new_char_device("dev/null".into(), 0o666, 1, 3);
    assert!(entry.extras.is_some());
    assert_eq!(entry.rdev_major(), Some(1));
    assert_eq!(entry.rdev_minor(), Some(3));
    assert!(entry.is_char_device());
    assert!(entry.is_device());
    assert!(!entry.is_block_device());
}

/// FIFO and socket constructors do not allocate extras.
#[test]
fn fifo_no_extras() {
    let entry = FileEntry::new_fifo("pipe".into(), 0o644);
    assert!(entry.extras.is_none());
    assert!(entry.is_special());
}

#[test]
fn socket_no_extras() {
    let entry = FileEntry::new_socket("sock".into(), 0o755);
    assert!(entry.extras.is_none());
    assert!(entry.is_special());
}

/// Symlink constructor allocates extras; other extras fields default.
#[test]
fn symlink_extras_other_fields_default() {
    let entry = FileEntry::new_symlink("lnk".into(), "/dest".into());
    assert!(entry.extras.is_some());
    assert_eq!(
        entry.link_target().map(|p| p.as_path()),
        Some(Path::new("/dest"))
    );
    assert_eq!(entry.user_name(), None);
    assert_eq!(entry.group_name(), None);
    assert_eq!(entry.atime(), 0);
    assert_eq!(entry.atime_nsec(), 0);
    assert_eq!(entry.crtime(), 0);
    assert_eq!(entry.rdev_major(), None);
    assert_eq!(entry.rdev_minor(), None);
    assert_eq!(entry.hardlink_idx(), None);
    assert_eq!(entry.hardlink_dev(), None);
    assert_eq!(entry.hardlink_ino(), None);
    assert_eq!(entry.checksum(), None);
    assert_eq!(entry.acl_ndx(), None);
    assert_eq!(entry.def_acl_ndx(), None);
    assert_eq!(entry.xattr_ndx(), None);
}

/// u32::MAX boundary for Option<u32> extras fields.
#[test]
fn extras_u32_max_values() {
    let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
    entry.set_hardlink_idx(u32::MAX);
    entry.set_acl_ndx(u32::MAX);
    entry.set_def_acl_ndx(u32::MAX);
    entry.set_xattr_ndx(u32::MAX);
    entry.set_rdev(u32::MAX, u32::MAX);
    entry.set_atime_nsec(u32::MAX);

    assert_eq!(entry.hardlink_idx(), Some(u32::MAX));
    assert_eq!(entry.acl_ndx(), Some(u32::MAX));
    assert_eq!(entry.def_acl_ndx(), Some(u32::MAX));
    assert_eq!(entry.xattr_ndx(), Some(u32::MAX));
    assert_eq!(entry.rdev_major(), Some(u32::MAX));
    assert_eq!(entry.rdev_minor(), Some(u32::MAX));
    assert_eq!(entry.atime_nsec(), u32::MAX);
}

/// i64::MIN / i64::MAX boundary for i64 extras fields.
#[test]
fn extras_i64_boundary_values() {
    let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
    entry.set_atime(i64::MAX);
    entry.set_crtime(i64::MIN);
    entry.set_hardlink_dev(i64::MAX);
    entry.set_hardlink_ino(i64::MIN);

    assert_eq!(entry.atime(), i64::MAX);
    assert_eq!(entry.crtime(), i64::MIN);
    assert_eq!(entry.hardlink_dev(), Some(i64::MAX));
    assert_eq!(entry.hardlink_ino(), Some(i64::MIN));
}

/// Setting rdev sets both major and minor atomically.
#[test]
fn extras_rdev_atomic_set() {
    let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
    entry.set_rdev(0, 0);
    assert_eq!(entry.rdev_major(), Some(0));
    assert_eq!(entry.rdev_minor(), Some(0));
}

/// Debug output includes extras when present.
#[test]
fn debug_includes_extras() {
    let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
    let debug_no_extras = format!("{entry:?}");
    assert!(!debug_no_extras.contains("extras"));

    entry.set_atime(42);
    let debug_with_extras = format!("{entry:?}");
    assert!(debug_with_extras.contains("extras"));
}

/// content_dir default and setter (not in extras, but part of FileEntry).
#[test]
fn content_dir_default_and_set() {
    let mut entry = FileEntry::new_directory("d".into(), 0o755);
    assert!(entry.content_dir());

    entry.set_content_dir(false);
    assert!(!entry.content_dir());

    entry.set_content_dir(true);
    assert!(entry.content_dir());
}

/// flags_mut allows in-place mutation.
#[test]
fn flags_mut_accessor() {
    let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
    let original = entry.flags();
    let _flags_mut = entry.flags_mut();
    assert_eq!(entry.flags(), original);
}

/// set_flags replaces flags.
#[test]
fn set_flags_replaces() {
    let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
    let flags = crate::flist::flags::FileFlags::default();
    entry.set_flags(flags);
    assert_eq!(entry.flags(), flags);
}

/// Getter roundtrip: set each extras field individually from a fresh entry,
/// verify the value, and confirm extras was allocated.
#[test]
fn extras_roundtrip_link_target() {
    let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
    assert!(entry.extras.is_none());
    entry.set_link_target("/absolute/target".into());
    assert!(entry.extras.is_some());
    assert_eq!(
        entry.link_target().map(|p| p.as_path()),
        Some(Path::new("/absolute/target"))
    );
}

#[test]
fn extras_roundtrip_rdev() {
    let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
    assert!(entry.extras.is_none());
    entry.set_rdev(259, 17);
    assert!(entry.extras.is_some());
    assert_eq!(entry.rdev_major(), Some(259));
    assert_eq!(entry.rdev_minor(), Some(17));
}

#[test]
fn extras_roundtrip_user_name() {
    let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
    assert!(entry.extras.is_none());
    entry.set_user_name("nobody".to_string());
    assert!(entry.extras.is_some());
    assert_eq!(entry.user_name(), Some("nobody"));
}

#[test]
fn extras_roundtrip_group_name() {
    let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
    assert!(entry.extras.is_none());
    entry.set_group_name("nogroup".to_string());
    assert!(entry.extras.is_some());
    assert_eq!(entry.group_name(), Some("nogroup"));
}

#[test]
fn extras_roundtrip_crtime() {
    let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
    assert!(entry.extras.is_none());
    entry.set_crtime(1_500_000_000);
    assert!(entry.extras.is_some());
    assert_eq!(entry.crtime(), 1_500_000_000);
}

#[test]
fn extras_roundtrip_atime_nsec() {
    let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
    assert!(entry.extras.is_none());
    entry.set_atime_nsec(123_456);
    assert!(entry.extras.is_some());
    assert_eq!(entry.atime_nsec(), 123_456);
}

#[test]
fn extras_roundtrip_hardlink_dev() {
    let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
    assert!(entry.extras.is_none());
    entry.set_hardlink_dev(0x1234_5678);
    assert!(entry.extras.is_some());
    assert_eq!(entry.hardlink_dev(), Some(0x1234_5678));
}

#[test]
fn extras_roundtrip_hardlink_ino() {
    let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
    assert!(entry.extras.is_none());
    entry.set_hardlink_ino(98765);
    assert!(entry.extras.is_some());
    assert_eq!(entry.hardlink_ino(), Some(98765));
}

#[test]
fn extras_roundtrip_checksum() {
    let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
    assert!(entry.extras.is_none());
    let sum = vec![0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
    entry.set_checksum(sum.clone());
    assert!(entry.extras.is_some());
    assert_eq!(entry.checksum(), Some(sum.as_slice()));
}

#[test]
fn extras_roundtrip_acl_ndx() {
    let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
    assert!(entry.extras.is_none());
    entry.set_acl_ndx(42);
    assert!(entry.extras.is_some());
    assert_eq!(entry.acl_ndx(), Some(42));
}

#[test]
fn extras_roundtrip_def_acl_ndx() {
    let mut entry = FileEntry::new_directory("d".into(), 0o755);
    assert!(entry.extras.is_none());
    entry.set_def_acl_ndx(77);
    assert!(entry.extras.is_some());
    assert_eq!(entry.def_acl_ndx(), Some(77));
}

#[test]
fn extras_roundtrip_xattr_ndx() {
    let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
    assert!(entry.extras.is_none());
    entry.set_xattr_ndx(255);
    assert!(entry.extras.is_some());
    assert_eq!(entry.xattr_ndx(), Some(255));
}

#[test]
fn extras_roundtrip_hardlink_idx() {
    let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
    assert!(entry.extras.is_none());
    entry.set_hardlink_idx(1000);
    assert!(entry.extras.is_some());
    assert_eq!(entry.hardlink_idx(), Some(1000));
}

#[test]
fn extras_roundtrip_atime() {
    let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
    assert!(entry.extras.is_none());
    entry.set_atime(1_700_000_000);
    assert!(entry.extras.is_some());
    assert_eq!(entry.atime(), 1_700_000_000);
}

/// Clone of entry with extras produces independent copy - mutating clone
/// does not affect the original.
#[test]
fn clone_extras_independence() {
    let mut entry = FileEntry::new_file("f.txt".into(), 100, 0o644);
    entry.set_atime(500);
    entry.set_user_name("alice".to_string());

    let mut cloned = entry.clone();
    cloned.set_atime(999);
    cloned.set_user_name("bob".to_string());

    assert_eq!(entry.atime(), 500);
    assert_eq!(entry.user_name(), Some("alice"));
    assert_eq!(cloned.atime(), 999);
    assert_eq!(cloned.user_name(), Some("bob"));
}

/// PartialEq: entries differ only by an extras field deep inside.
#[test]
fn equality_differs_by_single_extras_field() {
    let mut a = FileEntry::new_file("f.txt".into(), 100, 0o644);
    let mut b = FileEntry::new_file("f.txt".into(), 100, 0o644);
    a.set_xattr_ndx(1);
    b.set_xattr_ndx(2);
    assert_ne!(a, b);
}

/// PartialEq: entries with different extras fields set are not equal.
#[test]
fn equality_different_extras_fields_set() {
    let mut a = FileEntry::new_file("f.txt".into(), 100, 0o644);
    let mut b = FileEntry::new_file("f.txt".into(), 100, 0o644);
    a.set_acl_ndx(1);
    b.set_xattr_ndx(1);
    assert_ne!(a, b);
}

/// PartialEq: entries with identical multiple extras fields are equal.
#[test]
fn equality_multiple_extras_fields_match() {
    let mut a = FileEntry::new_file("f.txt".into(), 100, 0o644);
    let mut b = FileEntry::new_file("f.txt".into(), 100, 0o644);
    for entry in [&mut a, &mut b] {
        entry.set_atime(100);
        entry.set_crtime(200);
        entry.set_user_name("root".to_string());
        entry.set_checksum(vec![0xAB, 0xCD]);
        entry.set_hardlink_idx(42);
    }
    assert_eq!(a, b);
}

/// Setters on different entry types (directory, symlink) allocate extras.
#[test]
fn extras_on_directory_entry() {
    let mut entry = FileEntry::new_directory("mydir".into(), 0o755);
    assert!(entry.extras.is_none());
    entry.set_acl_ndx(5);
    entry.set_def_acl_ndx(6);
    entry.set_xattr_ndx(7);
    assert!(entry.extras.is_some());
    assert_eq!(entry.acl_ndx(), Some(5));
    assert_eq!(entry.def_acl_ndx(), Some(6));
    assert_eq!(entry.xattr_ndx(), Some(7));
}

#[test]
fn extras_on_symlink_entry() {
    let mut entry = FileEntry::new_symlink("lnk".into(), "/target".into());
    assert!(entry.extras.is_some());
    entry.set_user_name("owner".to_string());
    assert_eq!(entry.user_name(), Some("owner"));
    assert_eq!(
        entry.link_target().map(|p| p.as_path()),
        Some(Path::new("/target"))
    );
}

/// from_raw constructor starts with no extras.
#[test]
fn from_raw_no_extras() {
    let flags = crate::flist::flags::FileFlags::default();
    let entry = FileEntry::from_raw("file.rs".into(), 512, 0o100644, 1000, 0, flags);
    assert!(entry.extras.is_none());
    assert_eq!(entry.atime(), 0);
    assert_eq!(entry.link_target(), None);
    assert_eq!(entry.checksum(), None);
}

/// from_raw_bytes constructor starts with no extras.
#[test]
fn from_raw_bytes_no_extras() {
    let flags = crate::flist::flags::FileFlags::default();
    let entry = FileEntry::from_raw_bytes(b"data.bin".to_vec(), 2048, 0o100755, 5000, 100, flags);
    assert!(entry.extras.is_none());
    assert_eq!(entry.atime(), 0);
    assert_eq!(entry.crtime(), 0);
    assert_eq!(entry.user_name(), None);
}

/// from_raw_bytes entry can have extras set after construction.
#[test]
fn from_raw_bytes_then_set_extras() {
    let flags = crate::flist::flags::FileFlags::default();
    let mut entry = FileEntry::from_raw_bytes(b"file.dat".to_vec(), 100, 0o100644, 0, 0, flags);
    entry.set_checksum(vec![0xFF; 16]);
    entry.set_hardlink_idx(7);
    assert!(entry.extras.is_some());
    assert_eq!(entry.checksum(), Some(&[0xFF; 16][..]));
    assert_eq!(entry.hardlink_idx(), Some(7));
}

/// prepend_dir preserves extras on the entry.
#[test]
fn prepend_dir_preserves_extras() {
    let mut entry = FileEntry::new_file("file.txt".into(), 100, 0o644);
    entry.set_atime(42);
    entry.set_checksum(vec![0xBE, 0xEF]);

    entry.prepend_dir(Path::new("parent/dir"));

    assert_eq!(entry.name(), "parent/dir/file.txt");
    assert_eq!(entry.atime(), 42);
    assert_eq!(entry.checksum(), Some(&[0xBE, 0xEF][..]));
}

/// strip_leading_slashes preserves extras on the entry.
#[cfg(unix)]
#[test]
fn strip_leading_slashes_preserves_extras() {
    let flags = crate::flist::flags::FileFlags::default();
    let mut entry = FileEntry::from_raw("/leading/file.txt".into(), 100, 0o100644, 0, 0, flags);
    entry.set_user_name("root".to_string());
    entry.set_xattr_ndx(3);

    entry.strip_leading_slashes();

    assert_eq!(entry.name(), "leading/file.txt");
    assert_eq!(entry.user_name(), Some("root"));
    assert_eq!(entry.xattr_ndx(), Some(3));
}

/// name_bytes returns correct byte representation.
#[test]
fn name_bytes_accessor() {
    let entry = FileEntry::new_file("hello.txt".into(), 100, 0o644);
    assert_eq!(&*entry.name_bytes(), b"hello.txt");
}

/// Extras with unicode user/group names.
#[test]
fn extras_unicode_names() {
    let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
    entry.set_user_name("\u{00E9}mile".to_string());
    entry.set_group_name("\u{00FC}sers".to_string());
    assert_eq!(entry.user_name(), Some("\u{00E9}mile"));
    assert_eq!(entry.group_name(), Some("\u{00FC}sers"));
}

/// Extras checksum with 16-byte MD5-sized value.
#[test]
fn extras_checksum_md5_sized() {
    let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
    let md5 = vec![
        0xd4, 0x1d, 0x8c, 0xd9, 0x8f, 0x00, 0xb2, 0x04, 0xe9, 0x80, 0x09, 0x98, 0xec, 0xf8, 0x42,
        0x7e,
    ];
    entry.set_checksum(md5.clone());
    assert_eq!(entry.checksum(), Some(md5.as_slice()));
    assert_eq!(entry.checksum().unwrap().len(), 16);
}

/// Multiple setters called, then clone, then verify independence.
#[test]
fn clone_all_extras_then_mutate() {
    let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
    entry.set_link_target("/a".into());
    entry.set_user_name("u1".to_string());
    entry.set_group_name("g1".to_string());
    entry.set_atime(1);
    entry.set_crtime(2);
    entry.set_atime_nsec(3);
    entry.set_rdev(4, 5);
    entry.set_hardlink_idx(6);
    entry.set_hardlink_dev(7);
    entry.set_hardlink_ino(8);
    entry.set_checksum(vec![9]);
    entry.set_acl_ndx(10);
    entry.set_def_acl_ndx(11);
    entry.set_xattr_ndx(12);

    let mut cloned = entry.clone();

    cloned.set_link_target("/b".into());
    cloned.set_user_name("u2".to_string());
    cloned.set_group_name("g2".to_string());
    cloned.set_atime(100);
    cloned.set_crtime(200);
    cloned.set_atime_nsec(300);
    cloned.set_rdev(400, 500);
    cloned.set_hardlink_idx(600);
    cloned.set_hardlink_dev(700);
    cloned.set_hardlink_ino(800);
    cloned.set_checksum(vec![90]);
    cloned.set_acl_ndx(1000);
    cloned.set_def_acl_ndx(1100);
    cloned.set_xattr_ndx(1200);

    assert_eq!(
        entry.link_target().map(|p| p.as_path()),
        Some(Path::new("/a"))
    );
    assert_eq!(entry.user_name(), Some("u1"));
    assert_eq!(entry.group_name(), Some("g1"));
    assert_eq!(entry.atime(), 1);
    assert_eq!(entry.crtime(), 2);
    assert_eq!(entry.atime_nsec(), 3);
    assert_eq!(entry.rdev_major(), Some(4));
    assert_eq!(entry.rdev_minor(), Some(5));
    assert_eq!(entry.hardlink_idx(), Some(6));
    assert_eq!(entry.hardlink_dev(), Some(7));
    assert_eq!(entry.hardlink_ino(), Some(8));
    assert_eq!(entry.checksum(), Some(&[9][..]));
    assert_eq!(entry.acl_ndx(), Some(10));
    assert_eq!(entry.def_acl_ndx(), Some(11));
    assert_eq!(entry.xattr_ndx(), Some(12));

    assert_eq!(
        cloned.link_target().map(|p| p.as_path()),
        Some(Path::new("/b"))
    );
    assert_eq!(cloned.user_name(), Some("u2"));
    assert_eq!(cloned.atime(), 100);
    assert_eq!(cloned.xattr_ndx(), Some(1200));

    assert_ne!(entry, cloned);
}
