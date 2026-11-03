//! Which file-list entries carry extended attributes.

use crate::flist::FileEntry;

/// Reports whether the sender reads `entry`'s extended attributes into the
/// file list.
///
/// Regular files and directories always carry them. A symlink, special file
/// or device carries them only while its own type is preserved, since an entry
/// the receiver will not create has nothing to put them on.
///
/// upstream: `xattrs.c:322-345 get_xattr()`
#[must_use]
pub fn entry_carries_xattrs(
    entry: &FileEntry,
    preserve_links: bool,
    preserve_devices: bool,
    preserve_specials: bool,
) -> bool {
    entry.is_file()
        || entry.is_dir()
        || (entry.is_symlink() && preserve_links)
        || (entry.is_device() && preserve_devices)
        || (entry.is_special() && preserve_specials)
}

#[cfg(test)]
mod tests {
    use super::entry_carries_xattrs;
    use crate::flist::FileEntry;
    use std::path::PathBuf;

    /// Each type answers to its own preserve option and no other, so `-D`'s
    /// two halves (`--devices`, `--specials`) stay independent.
    #[test]
    fn each_entry_type_follows_its_own_preserve_option() {
        let file = FileEntry::new_file(PathBuf::from("f"), 1, 0o644);
        let dir = FileEntry::new_directory(PathBuf::from("d"), 0o755);
        let link = FileEntry::new_symlink(PathBuf::from("l"), 0o777, PathBuf::from("f"));
        let fifo = FileEntry::new_fifo(PathBuf::from("p"), 0o644);
        let device = FileEntry::new_char_device(PathBuf::from("c"), 0o644, 1, 3);
        for entry in [&file, &dir] {
            assert!(entry_carries_xattrs(entry, false, false, false));
        }
        assert!(!entry_carries_xattrs(&link, false, true, true));
        assert!(entry_carries_xattrs(&link, true, false, false));
        assert!(!entry_carries_xattrs(&fifo, true, true, false));
        assert!(entry_carries_xattrs(&fifo, false, false, true));
        assert!(!entry_carries_xattrs(&device, true, false, true));
        assert!(entry_carries_xattrs(&device, false, true, false));
    }
}
