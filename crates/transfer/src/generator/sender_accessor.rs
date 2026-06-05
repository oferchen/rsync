//! `FileEntryAccessor`-generic sender helpers.
//!
//! Provides trait-generic versions of sender-side operations that previously
//! required concrete `&FileEntry`. These functions accept any type implementing
//! [`FileEntryAccessor`], enabling the flat arena-backed file list
//! (`FlatFileList`) to be used interchangeably with the legacy `Vec<FileEntry>`.
//!
//! # Consumer sites migrated
//!
//! - [`format_iflags_generic`] - 11-character `YXcstpoguax` itemize string
//! - [`format_itemize_line_generic`] - full `"%i %n%L\n"` itemize output line
//! - [`should_skip_entry`] - sender-side file type filter (skip non-regular files)
//! - [`entry_display_name`] - directory-aware display name with trailing `/`
//!
//! # Upstream Reference
//!
//! - `log.c:695-746` - `%i` expansion in `log_formatted()`
//! - `sender.c:277-278` - `send_files()` file-type gating

#[cfg(feature = "flat-flist")]
use protocol::flist::FileEntryAccessor;

#[cfg(feature = "flat-flist")]
use super::item_flags::ItemFlags;
#[cfg(feature = "flat-flist")]
use super::itemize::ItemizeContext;

/// Formats the 11-character itemize string from raw item flags and a generic entry.
///
/// Trait-generic version of [`super::itemize::format_iflags`]. Accepts any type
/// implementing [`FileEntryAccessor`] instead of concrete `&FileEntry`.
///
/// Produces the upstream `YXcstpoguax` string where:
/// - Y = update type (`<` send, `>` receive, `c` local change, `h` hard link, `.` no transfer)
/// - X = file type (`f` file, `d` directory, `L` symlink, `S` special, `D` device)
/// - Positions 2-10 = attribute change indicators
///
/// # Upstream Reference
///
/// - `log.c:695-746` - itemize string construction in `log_formatted()`
#[cfg(feature = "flat-flist")]
pub fn format_iflags_generic<T: FileEntryAccessor>(
    iflags: &ItemFlags,
    entry: &T,
    is_sender: bool,
    ctx: &ItemizeContext,
) -> String {
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

    // upstream: log.c:708-710,716-717 - time with T/t distinction
    c[4] = if raw & ItemFlags::ITEM_REPORT_TIME == 0 {
        '.'
    } else if is_symlink {
        if !ctx.preserve_mtimes
            || !ctx.receiver_symlink_times
            || (raw & ItemFlags::ITEM_REPORT_TIMEFAIL != 0)
        {
            'T'
        } else {
            't'
        }
    } else if !ctx.preserve_mtimes {
        'T'
    } else {
        't'
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
/// Trait-generic version of [`super::itemize::format_itemize_line`]. Uses
/// [`FileEntryAccessor::name`] for the path string and
/// [`FileEntryAccessor::link_target_bytes`] for the symlink target.
///
/// Produces `"{iflags_str} {filename}\n"` matching upstream's default
/// `stdout_format = "%i %n%L"` when `--itemize-changes` is active.
///
/// # Upstream Reference
///
/// - `options.c:2336-2338` - `stdout_format = "%i %n%L"` for `-i`
/// - `log.c:627-636` - `%n` expansion (filename with trailing `/` for dirs)
/// - `log.c:637-653` - `%L` expansion (` -> target` for symlinks)
#[cfg(feature = "flat-flist")]
pub fn format_itemize_line_generic<T: FileEntryAccessor>(
    iflags: &ItemFlags,
    entry: &T,
    is_sender: bool,
    ctx: &ItemizeContext,
) -> String {
    let iflags_str = format_iflags_generic(iflags, entry, is_sender, ctx);
    let path = entry.name();

    // upstream: log.c:633-634 - append '/' for directories
    let name = if entry.is_dir() {
        format!("{path}/")
    } else {
        path.to_owned()
    };

    // upstream: log.c:637-653 - append " -> target" for symlinks
    let link_target = if entry.is_symlink() {
        entry
            .link_target_bytes()
            .and_then(|b| std::str::from_utf8(b).ok())
            .map(|t| format!(" -> {t}"))
            .unwrap_or_default()
    } else {
        String::new()
    };

    format!("{iflags_str} {name}{link_target}\n")
}

/// Returns the directory-aware display name for an entry.
///
/// Appends a trailing `/` for directories, matching upstream rsync's `%n`
/// format expansion.
///
/// # Upstream Reference
///
/// - `log.c:633-634` - directory names get trailing `/`
#[cfg(feature = "flat-flist")]
pub fn entry_display_name<T: FileEntryAccessor>(entry: &T) -> String {
    let name = entry.name();
    if entry.is_dir() {
        format!("{name}/")
    } else {
        name.to_owned()
    }
}

/// Returns `true` when the sender should skip this entry during file transfer.
///
/// The sender only transfers regular files - directories, symlinks, devices,
/// and specials are handled by metadata-only paths. This mirrors the
/// `!file_entry.is_file()` guard in the transfer loop.
///
/// # Upstream Reference
///
/// - `sender.c:329` - `if (!S_ISREG(file->mode))` skip guard
#[cfg(feature = "flat-flist")]
pub fn should_skip_entry<T: FileEntryAccessor>(entry: &T) -> bool {
    !entry.is_file()
}

#[cfg(all(test, feature = "flat-flist"))]
mod tests {
    use protocol::flist::{FileEntry, FileEntryAccessor};

    use super::*;

    /// Test adapter: wraps extracted accessor values for testing generic functions
    /// without coupling to `FileEntry` construction. Demonstrates that the generic
    /// functions work with any `FileEntryAccessor` implementation.
    struct MockEntry {
        name: String,
        mode: u32,
        size: u64,
        link_target: Option<Vec<u8>>,
    }

    impl MockEntry {
        fn file(name: &str, size: u64) -> Self {
            Self {
                name: name.to_owned(),
                mode: 0o100644,
                size,
                link_target: None,
            }
        }

        fn dir(name: &str) -> Self {
            Self {
                name: name.to_owned(),
                mode: 0o040755,
                size: 0,
                link_target: None,
            }
        }

        fn symlink(name: &str, target: &str) -> Self {
            Self {
                name: name.to_owned(),
                mode: 0o120777,
                size: 0,
                link_target: Some(target.as_bytes().to_vec()),
            }
        }

        fn device(name: &str) -> Self {
            Self {
                name: name.to_owned(),
                mode: 0o060660,
                size: 0,
                link_target: None,
            }
        }

        fn special(name: &str) -> Self {
            Self {
                name: name.to_owned(),
                mode: 0o010644,
                size: 0,
                link_target: None,
            }
        }
    }

    impl FileEntryAccessor for MockEntry {
        fn name(&self) -> &str {
            &self.name
        }
        fn name_bytes(&self) -> std::borrow::Cow<'_, [u8]> {
            std::borrow::Cow::Borrowed(self.name.as_bytes())
        }
        fn dirname_str(&self) -> &str {
            ""
        }
        fn size(&self) -> u64 {
            self.size
        }
        fn mode(&self) -> u32 {
            self.mode
        }
        fn mtime(&self) -> i64 {
            0
        }
        fn mtime_nsec(&self) -> u32 {
            0
        }
        fn uid(&self) -> Option<u32> {
            None
        }
        fn gid(&self) -> Option<u32> {
            None
        }
        fn top_dir(&self) -> bool {
            false
        }
        fn hlinked(&self) -> bool {
            false
        }
        fn hlink_first(&self) -> bool {
            false
        }
        fn content_dir(&self) -> bool {
            self.is_dir()
        }
        fn link_target_bytes(&self) -> Option<&[u8]> {
            self.link_target.as_deref()
        }
        fn rdev_major(&self) -> Option<u32> {
            None
        }
        fn rdev_minor(&self) -> Option<u32> {
            None
        }
        fn hardlink_idx(&self) -> Option<u32> {
            None
        }
        fn hardlink_dev(&self) -> Option<i64> {
            None
        }
        fn hardlink_ino(&self) -> Option<i64> {
            None
        }
        fn checksum(&self) -> Option<&[u8]> {
            None
        }
        fn acl_ndx(&self) -> Option<u32> {
            None
        }
        fn def_acl_ndx(&self) -> Option<u32> {
            None
        }
        fn xattr_ndx(&self) -> Option<u32> {
            None
        }
        fn user_name(&self) -> Option<&str> {
            None
        }
        fn group_name(&self) -> Option<&str> {
            None
        }
        fn atime(&self) -> i64 {
            0
        }
        fn atime_nsec(&self) -> u32 {
            0
        }
        fn crtime(&self) -> i64 {
            0
        }
    }

    fn default_ctx() -> ItemizeContext {
        ItemizeContext::default()
    }

    fn no_times_ctx() -> ItemizeContext {
        ItemizeContext {
            preserve_mtimes: false,
            receiver_symlink_times: true,
        }
    }

    fn no_symlink_times_ctx() -> ItemizeContext {
        ItemizeContext {
            preserve_mtimes: true,
            receiver_symlink_times: false,
        }
    }

    // -- format_iflags_generic tests --

    #[test]
    fn generic_new_file_sender() {
        let iflags = ItemFlags::from_raw(ItemFlags::ITEM_TRANSFER | ItemFlags::ITEM_IS_NEW);
        let entry = MockEntry::file("test.txt", 1024);
        let result = format_iflags_generic(&iflags, &entry, true, &default_ctx());
        assert_eq!(result, "<f+++++++++");
    }

    #[test]
    fn generic_new_file_receiver() {
        let iflags = ItemFlags::from_raw(ItemFlags::ITEM_TRANSFER | ItemFlags::ITEM_IS_NEW);
        let entry = MockEntry::file("test.txt", 1024);
        let result = format_iflags_generic(&iflags, &entry, false, &default_ctx());
        assert_eq!(result, ">f+++++++++");
    }

    #[test]
    fn generic_deleted_item() {
        let iflags = ItemFlags::from_raw(ItemFlags::ITEM_DELETED);
        let entry = MockEntry::file("gone.txt", 0);
        let result = format_iflags_generic(&iflags, &entry, true, &default_ctx());
        assert_eq!(result, "*deleting  ");
    }

    #[test]
    fn generic_directory_new() {
        let iflags = ItemFlags::from_raw(ItemFlags::ITEM_LOCAL_CHANGE | ItemFlags::ITEM_IS_NEW);
        let entry = MockEntry::dir("subdir");
        let result = format_iflags_generic(&iflags, &entry, true, &default_ctx());
        assert_eq!(result, "cd+++++++++");
    }

    #[test]
    fn generic_symlink_no_size() {
        let iflags = ItemFlags::from_raw(
            ItemFlags::ITEM_TRANSFER | ItemFlags::ITEM_REPORT_SIZE | ItemFlags::ITEM_REPORT_TIME,
        );
        let entry = MockEntry::symlink("link", "target");
        let result = format_iflags_generic(&iflags, &entry, false, &default_ctx());
        assert_eq!(result, ">L..T......");
    }

    #[test]
    fn generic_device_type_indicator() {
        let iflags = ItemFlags::from_raw(ItemFlags::ITEM_LOCAL_CHANGE | ItemFlags::ITEM_IS_NEW);
        let entry = MockEntry::device("dev/sda");
        let result = format_iflags_generic(&iflags, &entry, true, &default_ctx());
        assert_eq!(result, "cD+++++++++");
    }

    #[test]
    fn generic_special_type_indicator() {
        let iflags = ItemFlags::from_raw(ItemFlags::ITEM_LOCAL_CHANGE | ItemFlags::ITEM_IS_NEW);
        let entry = MockEntry::special("pipe");
        let result = format_iflags_generic(&iflags, &entry, true, &default_ctx());
        assert_eq!(result, "cS+++++++++");
    }

    #[test]
    fn generic_missing_data() {
        let iflags = ItemFlags::from_raw(ItemFlags::ITEM_MISSING_DATA);
        let entry = MockEntry::file("broken.txt", 0);
        let result = format_iflags_generic(&iflags, &entry, true, &default_ctx());
        assert_eq!(result, ".f?????????");
    }

    #[test]
    fn generic_hardlink() {
        let iflags = ItemFlags::from_raw(
            ItemFlags::ITEM_LOCAL_CHANGE | ItemFlags::ITEM_XNAME_FOLLOWS | ItemFlags::ITEM_IS_NEW,
        );
        let entry = MockEntry::file("link.txt", 0);
        let result = format_iflags_generic(&iflags, &entry, true, &default_ctx());
        assert_eq!(result, "hf+++++++++");
    }

    #[test]
    fn generic_all_attribute_changes() {
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
        let entry = MockEntry::file("test.txt", 1024);
        let result = format_iflags_generic(&iflags, &entry, true, &default_ctx());
        assert_eq!(result, "<fcstpogbax");
    }

    #[test]
    fn generic_metadata_only_no_changes() {
        let iflags = ItemFlags::from_raw(0);
        let entry = MockEntry::file("test.txt", 1024);
        let result = format_iflags_generic(&iflags, &entry, true, &default_ctx());
        assert_eq!(result, ".f         ");
    }

    #[test]
    fn generic_time_without_preserve_mtimes() {
        let iflags = ItemFlags::from_raw(ItemFlags::ITEM_TRANSFER | ItemFlags::ITEM_REPORT_TIME);
        let entry = MockEntry::file("test.txt", 1024);
        let result = format_iflags_generic(&iflags, &entry, false, &no_times_ctx());
        assert_eq!(result, ">f..T......");
    }

    #[test]
    fn generic_symlink_time_without_receiver_support() {
        let iflags = ItemFlags::from_raw(ItemFlags::ITEM_TRANSFER | ItemFlags::ITEM_REPORT_TIME);
        let entry = MockEntry::symlink("link", "target");
        let result = format_iflags_generic(&iflags, &entry, false, &no_symlink_times_ctx());
        assert_eq!(result, ">L..T......");
    }

    #[test]
    fn generic_symlink_timefail() {
        let iflags = ItemFlags::from_raw(
            ItemFlags::ITEM_TRANSFER
                | ItemFlags::ITEM_REPORT_TIME
                | ItemFlags::ITEM_REPORT_TIMEFAIL,
        );
        let entry = MockEntry::symlink("link", "target");
        let result = format_iflags_generic(&iflags, &entry, false, &default_ctx());
        assert_eq!(result, ">L..T......");
    }

    // -- format_itemize_line_generic tests --

    #[test]
    fn generic_itemize_line_file() {
        let iflags = ItemFlags::from_raw(ItemFlags::ITEM_TRANSFER | ItemFlags::ITEM_IS_NEW);
        let entry = MockEntry::file("docs/readme.txt", 512);
        let line = format_itemize_line_generic(&iflags, &entry, true, &default_ctx());
        assert_eq!(line, "<f+++++++++ docs/readme.txt\n");
    }

    #[test]
    fn generic_itemize_line_directory() {
        let iflags = ItemFlags::from_raw(ItemFlags::ITEM_LOCAL_CHANGE | ItemFlags::ITEM_IS_NEW);
        let entry = MockEntry::dir("subdir");
        let line = format_itemize_line_generic(&iflags, &entry, true, &default_ctx());
        assert_eq!(line, "cd+++++++++ subdir/\n");
    }

    #[test]
    fn generic_itemize_line_symlink() {
        let iflags = ItemFlags::from_raw(ItemFlags::ITEM_LOCAL_CHANGE | ItemFlags::ITEM_IS_NEW);
        let entry = MockEntry::symlink("mylink", "target");
        let line = format_itemize_line_generic(&iflags, &entry, true, &default_ctx());
        assert_eq!(line, "cL+++++++++ mylink -> target\n");
    }

    #[test]
    fn generic_itemize_line_symlink_no_target() {
        let iflags = ItemFlags::from_raw(ItemFlags::ITEM_LOCAL_CHANGE | ItemFlags::ITEM_IS_NEW);
        let mut entry = MockEntry::symlink("mylink", "target");
        entry.link_target = None;
        let line = format_itemize_line_generic(&iflags, &entry, true, &default_ctx());
        assert_eq!(line, "cL+++++++++ mylink\n");
    }

    // -- Parity tests: generic == concrete --

    #[test]
    fn parity_iflags_new_file() {
        use std::path::PathBuf;
        let iflags = ItemFlags::from_raw(ItemFlags::ITEM_TRANSFER | ItemFlags::ITEM_IS_NEW);
        let concrete = FileEntry::new_file(PathBuf::from("test.txt"), 1024, 0o644);
        let mock = MockEntry::file("test.txt", 1024);
        let ctx = default_ctx();

        let concrete_result = super::super::itemize::format_iflags(&iflags, &concrete, true, &ctx);
        let generic_result = format_iflags_generic(&iflags, &mock, true, &ctx);
        assert_eq!(concrete_result, generic_result);
    }

    #[test]
    fn parity_iflags_directory() {
        use std::path::PathBuf;
        let iflags = ItemFlags::from_raw(ItemFlags::ITEM_LOCAL_CHANGE | ItemFlags::ITEM_IS_NEW);
        let concrete = FileEntry::new_directory(PathBuf::from("subdir"), 0o755);
        let mock = MockEntry::dir("subdir");
        let ctx = default_ctx();

        let concrete_result = super::super::itemize::format_iflags(&iflags, &concrete, true, &ctx);
        let generic_result = format_iflags_generic(&iflags, &mock, true, &ctx);
        assert_eq!(concrete_result, generic_result);
    }

    #[test]
    fn parity_iflags_deleted() {
        use std::path::PathBuf;
        let iflags = ItemFlags::from_raw(ItemFlags::ITEM_DELETED);
        let concrete = FileEntry::new_file(PathBuf::from("gone.txt"), 0, 0o644);
        let mock = MockEntry::file("gone.txt", 0);
        let ctx = default_ctx();

        let concrete_result = super::super::itemize::format_iflags(&iflags, &concrete, true, &ctx);
        let generic_result = format_iflags_generic(&iflags, &mock, true, &ctx);
        assert_eq!(concrete_result, generic_result);
    }

    #[test]
    fn parity_iflags_all_attributes() {
        use std::path::PathBuf;
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
        let concrete = FileEntry::new_file(PathBuf::from("test.txt"), 1024, 0o644);
        let mock = MockEntry::file("test.txt", 1024);
        let ctx = default_ctx();

        let concrete_result = super::super::itemize::format_iflags(&iflags, &concrete, false, &ctx);
        let generic_result = format_iflags_generic(&iflags, &mock, false, &ctx);
        assert_eq!(concrete_result, generic_result);
    }

    #[test]
    fn parity_itemize_line_file() {
        use std::path::PathBuf;
        let iflags = ItemFlags::from_raw(ItemFlags::ITEM_TRANSFER | ItemFlags::ITEM_IS_NEW);
        let concrete = FileEntry::new_file(PathBuf::from("docs/readme.txt"), 512, 0o644);
        let mock = MockEntry::file("docs/readme.txt", 512);
        let ctx = default_ctx();

        let concrete_line =
            super::super::itemize::format_itemize_line(&iflags, &concrete, true, &ctx);
        let generic_line = format_itemize_line_generic(&iflags, &mock, true, &ctx);
        assert_eq!(concrete_line, generic_line);
    }

    #[test]
    fn parity_itemize_line_directory() {
        use std::path::PathBuf;
        let iflags = ItemFlags::from_raw(ItemFlags::ITEM_LOCAL_CHANGE | ItemFlags::ITEM_IS_NEW);
        let concrete = FileEntry::new_directory(PathBuf::from("subdir"), 0o755);
        let mock = MockEntry::dir("subdir");
        let ctx = default_ctx();

        let concrete_line =
            super::super::itemize::format_itemize_line(&iflags, &concrete, true, &ctx);
        let generic_line = format_itemize_line_generic(&iflags, &mock, true, &ctx);
        assert_eq!(concrete_line, generic_line);
    }

    // -- FileEntry through the accessor trait (integration test) --

    #[test]
    fn file_entry_through_accessor_trait() {
        use std::path::PathBuf;
        let entry = FileEntry::new_file(PathBuf::from("data.bin"), 2048, 0o755);
        let iflags = ItemFlags::from_raw(ItemFlags::ITEM_TRANSFER | ItemFlags::ITEM_IS_NEW);
        let ctx = default_ctx();

        // FileEntry implements FileEntryAccessor, so it works with the generic functions
        let result = format_iflags_generic(&iflags, &entry, true, &ctx);
        assert_eq!(result, "<f+++++++++");

        let line = format_itemize_line_generic(&iflags, &entry, true, &ctx);
        assert_eq!(line, "<f+++++++++ data.bin\n");
    }

    // -- entry_display_name tests --

    #[test]
    fn display_name_file() {
        let entry = MockEntry::file("readme.txt", 100);
        assert_eq!(entry_display_name(&entry), "readme.txt");
    }

    #[test]
    fn display_name_directory() {
        let entry = MockEntry::dir("docs");
        assert_eq!(entry_display_name(&entry), "docs/");
    }

    #[test]
    fn display_name_symlink() {
        let entry = MockEntry::symlink("link", "target");
        assert_eq!(entry_display_name(&entry), "link");
    }

    // -- should_skip_entry tests --

    #[test]
    fn skip_directory() {
        let entry = MockEntry::dir("subdir");
        assert!(should_skip_entry(&entry));
    }

    #[test]
    fn skip_symlink() {
        let entry = MockEntry::symlink("link", "target");
        assert!(should_skip_entry(&entry));
    }

    #[test]
    fn skip_device() {
        let entry = MockEntry::device("dev/sda");
        assert!(should_skip_entry(&entry));
    }

    #[test]
    fn skip_special() {
        let entry = MockEntry::special("pipe");
        assert!(should_skip_entry(&entry));
    }

    #[test]
    fn no_skip_regular_file() {
        let entry = MockEntry::file("data.bin", 1024);
        assert!(!should_skip_entry(&entry));
    }

    #[test]
    fn skip_entry_matches_concrete() {
        use std::path::PathBuf;
        let file = FileEntry::new_file(PathBuf::from("f"), 0, 0o644);
        let dir = FileEntry::new_directory(PathBuf::from("d"), 0o755);
        let sym = FileEntry::new_symlink(PathBuf::from("s"), PathBuf::from("t"));

        assert!(!should_skip_entry(&file));
        assert!(should_skip_entry(&dir));
        assert!(should_skip_entry(&sym));
    }

    #[test]
    fn entry_display_name_with_file_entry() {
        use std::path::PathBuf;
        let file = FileEntry::new_file(PathBuf::from("readme.txt"), 0, 0o644);
        let dir = FileEntry::new_directory(PathBuf::from("docs"), 0o755);

        assert_eq!(entry_display_name(&file), "readme.txt");
        assert_eq!(entry_display_name(&dir), "docs/");
    }
}
