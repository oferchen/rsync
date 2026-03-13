//! Itemize string formatting from raw wire item flags.
//!
//! Converts [`ItemFlags`] into the upstream 11-character `YXcstpoguax` string
//! for `--itemize-changes` output. This is the server-side counterpart of the
//! CLI's `format_itemized_changes()`, operating directly on raw iflags rather
//! than `ClientEvent` abstractions.
//!
//! # Upstream Reference
//!
//! - `log.c:695-746` - `%i` expansion in `log_formatted()`
//! - `rsync.h:214-236` - `ITEM_*` flag definitions

use protocol::flist::FileEntry;

use super::item_flags::ItemFlags;

/// Formats the 11-character itemize string from raw item flags and file entry.
///
/// Produces the upstream `YXcstpoguax` string where:
/// - Y = update type (`<` send, `>` receive, `c` local change, `h` hard link, `.` no transfer)
/// - X = file type (`f` file, `d` directory, `L` symlink, `S` special, `D` device)
/// - Positions 2-10 = attribute change indicators
///
/// The `is_sender` parameter controls direction: `true` for daemon sending
/// files (Generator role), `false` for daemon receiving files.
///
/// # Upstream Reference
///
/// - `log.c:695-746` - itemize string construction in `log_formatted()`
pub(crate) fn format_iflags(iflags: &ItemFlags, entry: &FileEntry, is_sender: bool) -> String {
    let raw = iflags.raw();

    // upstream: log.c:696-698 - deleted items
    if raw & ItemFlags::ITEM_DELETED != 0 {
        return "*deleting  ".to_owned();
    }

    let mut c = ['.'; 11];

    // Position 0: update type
    // upstream: log.c:701-704
    c[0] = if raw & ItemFlags::ITEM_LOCAL_CHANGE != 0 {
        if raw & ItemFlags::ITEM_XNAME_FOLLOWS != 0 {
            'h'
        } else {
            'c'
        }
    } else if raw & ItemFlags::ITEM_TRANSFER == 0 {
        '.'
    } else if is_sender {
        '<'
    } else {
        '>'
    };

    // Position 1: file type
    // upstream: log.c:705-714
    let is_symlink = entry.is_symlink();
    c[1] = if is_symlink {
        'L'
    } else if entry.is_dir() {
        'd'
    } else if entry.is_special() {
        'S'
    } else if entry.is_device() {
        'D'
    } else {
        'f'
    };

    // Positions 2-10: attribute change indicators
    // upstream: log.c:719 - checksum
    c[2] = if raw & ItemFlags::ITEM_REPORT_CHANGE != 0 {
        'c'
    } else {
        '.'
    };

    // upstream: log.c:707,715 - symlinks never report size
    c[3] = if is_symlink {
        '.'
    } else if raw & ItemFlags::ITEM_REPORT_SIZE != 0 {
        's'
    } else {
        '.'
    };

    // upstream: log.c:708-710,716-717 - time
    c[4] = if raw & ItemFlags::ITEM_REPORT_TIME != 0 {
        't'
    } else {
        '.'
    };

    // upstream: log.c:720-722 - perms, owner, group
    c[5] = if raw & ItemFlags::ITEM_REPORT_PERMS != 0 {
        'p'
    } else {
        '.'
    };
    c[6] = if raw & ItemFlags::ITEM_REPORT_OWNER != 0 {
        'o'
    } else {
        '.'
    };
    c[7] = if raw & ItemFlags::ITEM_REPORT_GROUP != 0 {
        'g'
    } else {
        '.'
    };

    // upstream: log.c:723-725 - atime/crtime
    let has_atime = raw & ItemFlags::ITEM_REPORT_ATIME != 0;
    let has_crtime = raw & ItemFlags::ITEM_REPORT_CRTIME != 0;
    c[8] = match (has_atime, has_crtime) {
        (true, true) => 'b',
        (true, false) => 'u',
        (false, true) => 'n',
        (false, false) => '.',
    };

    // upstream: log.c:726-727 - ACL, xattr
    c[9] = if raw & ItemFlags::ITEM_REPORT_ACL != 0 {
        'a'
    } else {
        '.'
    };
    c[10] = if raw & ItemFlags::ITEM_REPORT_XATTR != 0 {
        'x'
    } else {
        '.'
    };

    // upstream: log.c:730-734 - new items fill with '+', missing data with '?'
    if raw & (ItemFlags::ITEM_IS_NEW | ItemFlags::ITEM_MISSING_DATA) != 0 {
        let ch = if raw & ItemFlags::ITEM_IS_NEW != 0 {
            '+'
        } else {
            '?'
        };
        for slot in c[2..].iter_mut() {
            *slot = ch;
        }
    } else if matches!(c[0], '.' | 'h' | 'c') && c[2..].iter().all(|&ch| ch == '.') {
        // upstream: log.c:735-744 - collapse trailing dots to spaces
        for slot in c[2..].iter_mut() {
            *slot = ' ';
        }
    }

    c.iter().collect()
}

/// Formats a complete itemize output line for MSG_INFO emission.
///
/// Produces `"{iflags_str} {filename}\n"` matching upstream's default
/// `stdout_format = "%i %n%L"` when `--itemize-changes` is active.
///
/// # Upstream Reference
///
/// - `options.c:2336-2338` - `stdout_format = "%i %n%L"` for `-i`
/// - `log.c:627-636` - `%n` expansion (filename with trailing `/` for dirs)
/// - `log.c:637-653` - `%L` expansion (` -> target` for symlinks)
pub(crate) fn format_itemize_line(
    iflags: &ItemFlags,
    entry: &FileEntry,
    is_sender: bool,
) -> String {
    let iflags_str = format_iflags(iflags, entry, is_sender);
    let path = entry.path();
    let path_display = path.display();

    // upstream: log.c:633-634 - append '/' for directories
    let name = if entry.is_dir() {
        format!("{path_display}/")
    } else {
        path_display.to_string()
    };

    // upstream: log.c:637-653 - append " -> target" for symlinks
    let link_target = if entry.is_symlink() {
        entry
            .link_target()
            .map(|t| format!(" -> {}", t.display()))
            .unwrap_or_default()
    } else {
        String::new()
    };

    format!("{iflags_str} {name}{link_target}\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;

    fn make_file_entry(name: &str) -> FileEntry {
        FileEntry::new_file(PathBuf::from(name), 1024, 0o644)
    }

    fn make_dir_entry(name: &str) -> FileEntry {
        FileEntry::new_directory(PathBuf::from(name), 0o755)
    }

    fn make_symlink_entry(name: &str) -> FileEntry {
        FileEntry::new_symlink(PathBuf::from(name), PathBuf::from("target"))
    }

    #[test]
    fn format_new_file_transfer() {
        let iflags = ItemFlags::from_raw(ItemFlags::ITEM_TRANSFER | ItemFlags::ITEM_IS_NEW);
        let entry = make_file_entry("test.txt");
        let result = format_iflags(&iflags, &entry, true);
        assert_eq!(result, "<f+++++++++");
    }

    #[test]
    fn format_new_file_receiver() {
        let iflags = ItemFlags::from_raw(ItemFlags::ITEM_TRANSFER | ItemFlags::ITEM_IS_NEW);
        let entry = make_file_entry("test.txt");
        let result = format_iflags(&iflags, &entry, false);
        assert_eq!(result, ">f+++++++++");
    }

    #[test]
    fn format_metadata_only_no_changes() {
        let iflags = ItemFlags::from_raw(0);
        let entry = make_file_entry("test.txt");
        let result = format_iflags(&iflags, &entry, true);
        // No transfer, no changes -> dots collapse to spaces
        assert_eq!(result, ".f         ");
    }

    #[test]
    fn format_time_and_perms_change() {
        let iflags =
            ItemFlags::from_raw(ItemFlags::ITEM_REPORT_TIME | ItemFlags::ITEM_REPORT_PERMS);
        let entry = make_file_entry("test.txt");
        let result = format_iflags(&iflags, &entry, true);
        assert_eq!(result, ".f..tp.....");
    }

    #[test]
    fn format_size_change() {
        let iflags = ItemFlags::from_raw(ItemFlags::ITEM_TRANSFER | ItemFlags::ITEM_REPORT_SIZE);
        let entry = make_file_entry("test.txt");
        let result = format_iflags(&iflags, &entry, true);
        assert_eq!(result, "<f.s.......");
    }

    #[test]
    fn format_directory_new() {
        let iflags = ItemFlags::from_raw(ItemFlags::ITEM_LOCAL_CHANGE | ItemFlags::ITEM_IS_NEW);
        let entry = make_dir_entry("subdir");
        let result = format_iflags(&iflags, &entry, true);
        assert_eq!(result, "cd+++++++++");
    }

    #[test]
    fn format_deleted_item() {
        let iflags = ItemFlags::from_raw(ItemFlags::ITEM_DELETED);
        let entry = make_file_entry("gone.txt");
        let result = format_iflags(&iflags, &entry, true);
        assert_eq!(result, "*deleting  ");
    }

    #[test]
    fn format_missing_data() {
        let iflags = ItemFlags::from_raw(ItemFlags::ITEM_MISSING_DATA);
        let entry = make_file_entry("broken.txt");
        let result = format_iflags(&iflags, &entry, true);
        assert_eq!(result, ".f?????????");
    }

    #[test]
    fn format_hardlink() {
        let iflags = ItemFlags::from_raw(
            ItemFlags::ITEM_LOCAL_CHANGE | ItemFlags::ITEM_XNAME_FOLLOWS | ItemFlags::ITEM_IS_NEW,
        );
        let entry = make_file_entry("link.txt");
        let result = format_iflags(&iflags, &entry, true);
        assert_eq!(result, "hf+++++++++");
    }

    #[test]
    fn format_all_attribute_changes() {
        let iflags = ItemFlags::from_raw(
            ItemFlags::ITEM_TRANSFER
                | ItemFlags::ITEM_REPORT_CHANGE
                | ItemFlags::ITEM_REPORT_SIZE
                | ItemFlags::ITEM_REPORT_TIME
                | ItemFlags::ITEM_REPORT_PERMS
                | ItemFlags::ITEM_REPORT_OWNER
                | ItemFlags::ITEM_REPORT_GROUP
                | ItemFlags::ITEM_REPORT_ATIME
                | ItemFlags::ITEM_REPORT_CRTIME
                | ItemFlags::ITEM_REPORT_ACL
                | ItemFlags::ITEM_REPORT_XATTR,
        );
        let entry = make_file_entry("test.txt");
        let result = format_iflags(&iflags, &entry, true);
        assert_eq!(result, "<fcstpogbax");
    }

    #[test]
    fn format_atime_only() {
        let iflags = ItemFlags::from_raw(ItemFlags::ITEM_REPORT_ATIME);
        let entry = make_file_entry("test.txt");
        let result = format_iflags(&iflags, &entry, true);
        assert_eq!(result, ".f.....u...");
    }

    #[test]
    fn format_crtime_only() {
        let iflags = ItemFlags::from_raw(ItemFlags::ITEM_REPORT_CRTIME);
        let entry = make_file_entry("test.txt");
        let result = format_iflags(&iflags, &entry, true);
        assert_eq!(result, ".f.....n...");
    }

    #[test]
    fn format_itemize_line_file() {
        let iflags = ItemFlags::from_raw(ItemFlags::ITEM_TRANSFER | ItemFlags::ITEM_IS_NEW);
        let entry = make_file_entry("docs/readme.txt");
        let line = format_itemize_line(&iflags, &entry, true);
        assert_eq!(line, "<f+++++++++ docs/readme.txt\n");
    }

    #[test]
    fn format_itemize_line_directory() {
        let iflags = ItemFlags::from_raw(ItemFlags::ITEM_LOCAL_CHANGE | ItemFlags::ITEM_IS_NEW);
        let entry = make_dir_entry("subdir");
        let line = format_itemize_line(&iflags, &entry, true);
        assert_eq!(line, "cd+++++++++ subdir/\n");
    }

    #[test]
    fn format_symlink_no_size() {
        // Symlinks never report size changes (position 3 stays '.')
        let iflags = ItemFlags::from_raw(
            ItemFlags::ITEM_TRANSFER | ItemFlags::ITEM_REPORT_SIZE | ItemFlags::ITEM_REPORT_TIME,
        );
        let entry = make_symlink_entry("link");
        let result = format_iflags(&iflags, &entry, false);
        assert_eq!(result, ">L..t......");
    }

    #[test]
    fn format_itemize_line_symlink() {
        let iflags = ItemFlags::from_raw(ItemFlags::ITEM_LOCAL_CHANGE | ItemFlags::ITEM_IS_NEW);
        let entry = make_symlink_entry("mylink");
        let line = format_itemize_line(&iflags, &entry, true);
        assert_eq!(line, "cL+++++++++ mylink -> target\n");
    }
}
