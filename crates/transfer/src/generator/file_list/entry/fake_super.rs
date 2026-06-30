//! Fake-super `user.rsync.%stat` xattr override for the generator role.
//!
//! Builds the wire `FileEntry` for a regular placeholder file whose
//! `user.rsync.%stat` xattr decoded successfully, mapping the decoded mode
//! bits back to the effective file type.

#[cfg(unix)]
use std::path::PathBuf;

#[cfg(unix)]
use protocol::flist::FileEntry;

/// Builds the wire `FileEntry` for a regular placeholder file whose
/// `user.rsync.%stat` xattr decoded successfully.
///
/// The xattr's mode encodes the *effective* file type (regular, device,
/// symlink, fifo, socket) plus permission bits. For devices, the decoded
/// `rdev` major/minor populate the wire fields. When the xattr's mode does
/// not encode a recognised type, fall back to a regular file with the
/// decoded permission bits.
///
/// # Upstream Reference
///
/// - `xattrs.c:1172 from_wire_mode()` - the xattr's mode replaces st_mode
/// - `flist.c:make_file()` - downstream branches pick the wire encoding
///   from the (now overridden) mode
#[cfg(unix)]
pub(super) fn build_entry_from_fake_super(
    relative_path: PathBuf,
    size: u64,
    stat: &metadata::FakeSuperStat,
) -> FileEntry {
    use protocol::flist::FileType;

    let perm_bits = stat.mode & 0o7777;
    let (rdev_major, rdev_minor) = stat.rdev.unwrap_or((0, 0));

    match FileType::from_mode(stat.mode) {
        Some(FileType::Regular) | None => FileEntry::new_file(relative_path, size, perm_bits),
        Some(FileType::Directory) => FileEntry::new_directory(relative_path, perm_bits),
        Some(FileType::Symlink) => {
            // upstream: fake-super symlinks stash the target separately;
            // when the xattr alone is the source of truth, we emit an empty
            // target to match the placeholder content.
            FileEntry::new_symlink(relative_path, PathBuf::new())
        }
        Some(FileType::BlockDevice) => {
            FileEntry::new_block_device(relative_path, perm_bits, rdev_major, rdev_minor)
        }
        Some(FileType::CharDevice) => {
            FileEntry::new_char_device(relative_path, perm_bits, rdev_major, rdev_minor)
        }
        Some(FileType::Fifo) => FileEntry::new_fifo(relative_path, perm_bits),
        Some(FileType::Socket) => FileEntry::new_socket(relative_path, perm_bits),
    }
}

#[cfg(all(test, unix))]
mod fake_super_tests {
    //! Sender-side `user.rsync.%stat` consumption tests.
    //!
    //! Verifies that under `--fake-super` the source-stored xattr overrides
    //! the on-disk stat values when populating the wire file-list entry,
    //! matching upstream rsync 3.4.1 `xattrs.c:get_stat_xattr()` semantics.

    use super::*;
    use ::metadata::FakeSuperStat;
    use protocol::flist::FileType;

    #[test]
    fn build_from_fake_super_emits_regular_file_for_regular_mode() {
        let stat = FakeSuperStat {
            mode: 0o100644,
            uid: 1234,
            gid: 5678,
            rdev: None,
        };
        let entry = build_entry_from_fake_super(PathBuf::from("a"), 42, &stat);
        assert_eq!(entry.file_type(), FileType::Regular);
        assert_eq!(entry.permissions() & 0o7777, 0o644);
        assert_eq!(entry.size(), 42);
    }

    #[test]
    fn build_from_fake_super_emits_block_device_from_mode_bits() {
        // 0o60660 = S_IFBLK | 0660
        let stat = FakeSuperStat {
            mode: 0o60660,
            uid: 0,
            gid: 6,
            rdev: Some((8, 0)),
        };
        let entry = build_entry_from_fake_super(PathBuf::from("sda"), 0, &stat);
        assert_eq!(entry.file_type(), FileType::BlockDevice);
        assert_eq!(entry.permissions() & 0o7777, 0o660);
        assert_eq!(entry.rdev_major(), Some(8));
        assert_eq!(entry.rdev_minor(), Some(0));
    }

    #[test]
    fn build_from_fake_super_emits_char_device_from_mode_bits() {
        // 0o20666 = S_IFCHR | 0666
        let stat = FakeSuperStat {
            mode: 0o20666,
            uid: 0,
            gid: 0,
            rdev: Some((1, 3)),
        };
        let entry = build_entry_from_fake_super(PathBuf::from("null"), 0, &stat);
        assert_eq!(entry.file_type(), FileType::CharDevice);
        assert_eq!(entry.rdev_major(), Some(1));
        assert_eq!(entry.rdev_minor(), Some(3));
    }

    #[test]
    fn build_from_fake_super_emits_fifo_from_mode_bits() {
        let stat = FakeSuperStat {
            mode: 0o10644,
            uid: 0,
            gid: 0,
            rdev: None,
        };
        let entry = build_entry_from_fake_super(PathBuf::from("pipe"), 0, &stat);
        assert_eq!(entry.file_type(), FileType::Fifo);
    }

    #[test]
    fn build_from_fake_super_emits_socket_from_mode_bits() {
        let stat = FakeSuperStat {
            mode: 0o140755,
            uid: 0,
            gid: 0,
            rdev: None,
        };
        let entry = build_entry_from_fake_super(PathBuf::from("sock"), 0, &stat);
        assert_eq!(entry.file_type(), FileType::Socket);
    }

    #[test]
    fn build_from_fake_super_emits_directory_from_mode_bits() {
        let stat = FakeSuperStat {
            mode: 0o40755,
            uid: 0,
            gid: 0,
            rdev: None,
        };
        let entry = build_entry_from_fake_super(PathBuf::from("d"), 0, &stat);
        assert_eq!(entry.file_type(), FileType::Directory);
    }

    #[test]
    fn build_from_fake_super_emits_symlink_with_empty_target() {
        let stat = FakeSuperStat {
            mode: 0o120777,
            uid: 1000,
            gid: 1000,
            rdev: None,
        };
        let entry = build_entry_from_fake_super(PathBuf::from("link"), 0, &stat);
        assert_eq!(entry.file_type(), FileType::Symlink);
    }

    #[test]
    fn build_from_fake_super_unknown_mode_falls_back_to_regular() {
        // No file-type bits set: treat as regular with the given perms.
        let stat = FakeSuperStat {
            mode: 0o0644,
            uid: 1,
            gid: 2,
            rdev: None,
        };
        let entry = build_entry_from_fake_super(PathBuf::from("f"), 7, &stat);
        assert_eq!(entry.file_type(), FileType::Regular);
        assert_eq!(entry.size(), 7);
    }
}
