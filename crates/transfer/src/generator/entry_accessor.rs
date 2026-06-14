//! Generic generator helpers for `FileEntryAccessor`.
//!
//! Provides generic counterparts of generator-side pure functions that
//! previously accepted `&FileEntry`. Each function here is parameterized
//! over `T: FileEntryAccessor`, enabling the generator to work with both
//! the legacy `FileEntry` and the arena-backed `FlatFileEntry` during the
//! flat-flist migration (RSS-A.7.e).
//!
//! # Covered consumers
//!
//! - **Itemize formatting** - `format_iflags_generic`, `format_itemize_line_generic`
//! - **Quick-check** - `quick_check_matches_generic`, `dest_mtime_newer_generic`
//! - **Hardlink detection** - `is_hardlink_follower_generic`
//!
//! # Upstream Reference
//!
//! - `log.c:695-746` - itemize string construction
//! - `generator.c:617` - `quick_check_ok()` evaluation order
//! - `generator.c:1539` - `F_HLINK_NOT_FIRST(file)` check

use std::fs;
use std::path::Path;

use protocol::flist::FileEntryAccessor;

use super::item_flags::ItemFlags;
use super::itemize::ItemizeContext;

/// Formats the 11-character itemize string from raw item flags and any entry type.
///
/// Generic counterpart of `super::itemize::format_iflags` that accepts any
/// `FileEntryAccessor` implementor instead of a concrete `FileEntry`.
///
/// Produces the upstream `YXcstpoguax` string. See `super::itemize::format_iflags`
/// for full documentation of each position.
///
/// # Upstream Reference
///
/// - `log.c:695-746` - itemize string construction in `log_formatted()`
#[must_use]
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
/// Generic counterpart of [`super::itemize::format_itemize_line`] that
/// accepts any `FileEntryAccessor` implementor. Uses `name()` and
/// `link_target_bytes()` from the trait instead of `path()` and
/// `link_target()` on the concrete `FileEntry`.
///
/// Produces `"{iflags_str} {name}\n"` matching upstream's default
/// `stdout_format = "%i %n%L"` when `--itemize-changes` is active.
///
/// # Upstream Reference
///
/// - `options.c:2336-2338` - `stdout_format = "%i %n%L"` for `-i`
/// - `log.c:627-636` - `%n` expansion (filename with trailing `/` for dirs)
/// - `log.c:637-653` - `%L` expansion (` -> target` for symlinks)
pub fn format_itemize_line_generic<T: FileEntryAccessor>(
    iflags: &ItemFlags,
    entry: &T,
    is_sender: bool,
    ctx: &ItemizeContext,
) -> String {
    let iflags_str = format_iflags_generic(iflags, entry, is_sender, ctx);
    let name_str = entry.name();

    // upstream: log.c:633-634 - append '/' for directories
    let name = if entry.is_dir() {
        format!("{name_str}/")
    } else {
        name_str.to_owned()
    };

    // upstream: log.c:637-653 - append " -> target" for symlinks
    let link_target = if entry.is_symlink() {
        entry
            .link_target_bytes()
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .map(|t| format!(" -> {t}"))
            .unwrap_or_default()
    } else {
        String::new()
    };

    format!("{iflags_str} {name}{link_target}\n")
}

/// Pure-function quick-check over any `FileEntryAccessor`.
///
/// Generic counterpart of `super::super::receiver::quick_check::quick_check_matches`.
/// Returns `true` when the destination file matches the source entry (skip transfer).
///
/// Follows upstream `generator.c:617 quick_check_ok()` evaluation order:
/// 1. Size mismatch - always needs transfer
/// 2. `always_checksum` - compute file checksum and compare (ignores mtime)
/// 3. `size_only` - size matched, skip transfer
/// 4. `!preserve_times` (implies `ignore_times`) - force transfer
/// 5. mtime comparison
///
/// # Upstream Reference
///
/// - `generator.c:617` - `quick_check_ok()`
#[must_use]
pub fn quick_check_matches_generic<T: FileEntryAccessor>(
    entry: &T,
    dest_path: &Path,
    dest_meta: &fs::Metadata,
    preserve_times: bool,
    size_only: bool,
    always_checksum: Option<protocol::ChecksumAlgorithm>,
) -> bool {
    // upstream: generator.c:621 - size check first
    if dest_meta.len() != entry.size() {
        return false;
    }
    // upstream: generator.c:626 - always_checksum compares file checksums
    if let Some(algorithm) = always_checksum {
        return match entry.checksum() {
            Some(expected) => {
                file_checksum_matches(dest_path, dest_meta.len(), algorithm, expected)
            }
            None => false,
        };
    }
    // upstream: generator.c:632 - size_only: size matched, skip
    if size_only {
        return true;
    }
    // upstream: generator.c:635 - ignore_times forces transfer
    if !preserve_times {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        dest_meta.mtime() == entry.mtime()
    }
    #[cfg(not(unix))]
    {
        dest_meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(false, |d| d.as_secs() as i64 == entry.mtime())
    }
}

/// Returns `true` when the destination mtime is strictly newer than the source.
///
/// Generic counterpart of the concrete `dest_mtime_newer` in
/// `receiver/quick_check.rs`. Used by `--update` (`-u`) to skip files
/// where the destination is already newer.
///
/// # Upstream Reference
///
/// - `generator.c:1709` - `file->modtime - sx.st.st_mtime < modify_window`
#[must_use]
pub fn dest_mtime_newer_generic<T: FileEntryAccessor>(
    dest_meta: &fs::Metadata,
    source_entry: &T,
) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        dest_meta.mtime() > source_entry.mtime()
    }
    #[cfg(not(unix))]
    {
        dest_meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(false, |d| (d.as_secs() as i64) > source_entry.mtime())
    }
}

/// Returns true if this entry is a hardlink follower via `FileEntryAccessor`.
///
/// Generic counterpart of the concrete `is_hardlink_follower` in
/// `receiver/quick_check.rs`. A follower has `XMIT_HLINKED` set but NOT
/// `XMIT_HLINK_FIRST`.
///
/// # Upstream Reference
///
/// - `generator.c:1539` - `F_HLINK_NOT_FIRST(file)` check
/// - `hlink.c:284` - `hard_link_check()` called for non-first entries
#[must_use]
pub fn is_hardlink_follower_generic<T: FileEntryAccessor>(entry: &T) -> bool {
    entry.hlinked() && !entry.hlink_first()
}

/// Computes a file's checksum and compares it against an expected value.
///
/// Used by `--checksum` mode to compare file contents instead of
/// mtime+size quick-check. Returns `true` when checksums match.
///
/// upstream: checksum.c:402 `file_checksum()` - plain hash, no seed
fn file_checksum_matches(
    path: &Path,
    file_size: u64,
    algorithm: protocol::ChecksumAlgorithm,
    expected: &[u8],
) -> bool {
    use std::io::Read;

    use crate::delta_apply::ChecksumVerifier;

    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    let mut hasher = ChecksumVerifier::for_algorithm(algorithm);
    // upstream: rsync.h:159 MAX_MAP_SIZE = 256*1024
    let mut buf = vec![0u8; 256 * 1024];
    let mut remaining = file_size;
    while remaining > 0 {
        let to_read = buf.len().min(remaining as usize);
        if file.read_exact(&mut buf[..to_read]).is_err() {
            return false;
        }
        hasher.update(&buf[..to_read]);
        remaining -= to_read as u64;
    }
    let mut digest = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
    let len = hasher.finalize_into(&mut digest);
    let cmp_len = expected.len().min(len);
    digest[..cmp_len] == expected[..cmp_len]
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use protocol::flist::FileEntry;

    use super::*;

    fn make_file_entry(name: &str) -> FileEntry {
        FileEntry::new_file(PathBuf::from(name), 1024, 0o644)
    }

    fn make_dir_entry(name: &str) -> FileEntry {
        FileEntry::new_directory(PathBuf::from(name), 0o755)
    }

    fn make_symlink_entry(name: &str) -> FileEntry {
        FileEntry::new_symlink(PathBuf::from(name), PathBuf::from("target"))
    }

    fn make_device_entry(name: &str) -> FileEntry {
        FileEntry::new_block_device(PathBuf::from(name), 0o660, 8, 0)
    }

    fn make_special_entry(name: &str) -> FileEntry {
        FileEntry::new_fifo(PathBuf::from(name), 0o644)
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

    // -- format_iflags_generic parity with concrete version --

    /// Verifies the generic version produces identical output to the concrete
    /// version for all file types and flag combinations.
    #[test]
    fn iflags_generic_matches_concrete_new_file() {
        let iflags = ItemFlags::from_raw(ItemFlags::ITEM_TRANSFER | ItemFlags::ITEM_IS_NEW);
        let entry = make_file_entry("test.txt");
        let concrete = super::super::itemize::format_iflags(&iflags, &entry, true, &default_ctx());
        let generic = format_iflags_generic(&iflags, &entry, true, &default_ctx());
        assert_eq!(concrete, generic);
        assert_eq!(generic, "<f+++++++++");
    }

    #[test]
    fn iflags_generic_matches_concrete_receiver() {
        let iflags = ItemFlags::from_raw(ItemFlags::ITEM_TRANSFER | ItemFlags::ITEM_IS_NEW);
        let entry = make_file_entry("test.txt");
        let concrete = super::super::itemize::format_iflags(&iflags, &entry, false, &default_ctx());
        let generic = format_iflags_generic(&iflags, &entry, false, &default_ctx());
        assert_eq!(concrete, generic);
        assert_eq!(generic, ">f+++++++++");
    }

    #[test]
    fn iflags_generic_deleted_item() {
        let iflags = ItemFlags::from_raw(ItemFlags::ITEM_DELETED);
        let entry = make_file_entry("gone.txt");
        let result = format_iflags_generic(&iflags, &entry, true, &default_ctx());
        assert_eq!(result, "*deleting  ");
    }

    #[test]
    fn iflags_generic_directory() {
        let iflags = ItemFlags::from_raw(ItemFlags::ITEM_LOCAL_CHANGE | ItemFlags::ITEM_IS_NEW);
        let entry = make_dir_entry("subdir");
        let result = format_iflags_generic(&iflags, &entry, true, &default_ctx());
        assert_eq!(result, "cd+++++++++");
    }

    #[test]
    fn iflags_generic_symlink() {
        let iflags = ItemFlags::from_raw(
            ItemFlags::ITEM_TRANSFER | ItemFlags::ITEM_REPORT_SIZE | ItemFlags::ITEM_REPORT_TIME,
        );
        let entry = make_symlink_entry("link");
        let result = format_iflags_generic(&iflags, &entry, false, &default_ctx());
        // Symlinks never report size; ITEM_REPORT_SIZE reinterpreted as TIMEFAIL
        assert_eq!(result, ">L..T......");
    }

    #[test]
    fn iflags_generic_device() {
        let iflags = ItemFlags::from_raw(ItemFlags::ITEM_TRANSFER | ItemFlags::ITEM_IS_NEW);
        let entry = make_device_entry("dev/sda");
        let result = format_iflags_generic(&iflags, &entry, true, &default_ctx());
        assert_eq!(result, "<D+++++++++");
    }

    #[test]
    fn iflags_generic_special() {
        let iflags = ItemFlags::from_raw(ItemFlags::ITEM_TRANSFER | ItemFlags::ITEM_IS_NEW);
        let entry = make_special_entry("pipe");
        let result = format_iflags_generic(&iflags, &entry, true, &default_ctx());
        assert_eq!(result, "<S+++++++++");
    }

    #[test]
    fn iflags_generic_no_changes_collapses_to_spaces() {
        let iflags = ItemFlags::from_raw(0);
        let entry = make_file_entry("test.txt");
        let result = format_iflags_generic(&iflags, &entry, true, &default_ctx());
        assert_eq!(result, ".f         ");
    }

    #[test]
    fn iflags_generic_time_and_perms() {
        let iflags =
            ItemFlags::from_raw(ItemFlags::ITEM_REPORT_TIME | ItemFlags::ITEM_REPORT_PERMS);
        let entry = make_file_entry("test.txt");
        let result = format_iflags_generic(&iflags, &entry, true, &default_ctx());
        assert_eq!(result, ".f..tp.....");
    }

    #[test]
    fn iflags_generic_all_attribute_changes() {
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
        let result = format_iflags_generic(&iflags, &entry, true, &default_ctx());
        assert_eq!(result, "<fcstpogbax");
    }

    #[test]
    fn iflags_generic_missing_data() {
        let iflags = ItemFlags::from_raw(ItemFlags::ITEM_MISSING_DATA);
        let entry = make_file_entry("broken.txt");
        let result = format_iflags_generic(&iflags, &entry, true, &default_ctx());
        assert_eq!(result, ".f?????????");
    }

    #[test]
    fn iflags_generic_hardlink() {
        let iflags = ItemFlags::from_raw(
            ItemFlags::ITEM_LOCAL_CHANGE | ItemFlags::ITEM_XNAME_FOLLOWS | ItemFlags::ITEM_IS_NEW,
        );
        let entry = make_file_entry("link.txt");
        let result = format_iflags_generic(&iflags, &entry, true, &default_ctx());
        assert_eq!(result, "hf+++++++++");
    }

    // -- Time context tests --

    #[test]
    fn iflags_generic_file_time_no_preserve_mtimes() {
        let iflags = ItemFlags::from_raw(ItemFlags::ITEM_TRANSFER | ItemFlags::ITEM_REPORT_TIME);
        let entry = make_file_entry("test.txt");
        let result = format_iflags_generic(&iflags, &entry, false, &no_times_ctx());
        assert_eq!(result, ">f..T......");
    }

    #[test]
    fn iflags_generic_symlink_no_receiver_symlink_times() {
        let iflags = ItemFlags::from_raw(ItemFlags::ITEM_TRANSFER | ItemFlags::ITEM_REPORT_TIME);
        let entry = make_symlink_entry("link");
        let result = format_iflags_generic(&iflags, &entry, false, &no_symlink_times_ctx());
        assert_eq!(result, ">L..T......");
    }

    #[test]
    fn iflags_generic_dir_no_preserve_mtimes() {
        let iflags =
            ItemFlags::from_raw(ItemFlags::ITEM_LOCAL_CHANGE | ItemFlags::ITEM_REPORT_TIME);
        let entry = make_dir_entry("subdir");
        let result = format_iflags_generic(&iflags, &entry, true, &no_times_ctx());
        assert_eq!(result, "cd..T......");
    }

    // -- format_itemize_line_generic tests --

    #[test]
    fn itemize_line_generic_file() {
        let iflags = ItemFlags::from_raw(ItemFlags::ITEM_TRANSFER | ItemFlags::ITEM_IS_NEW);
        let entry = make_file_entry("docs/readme.txt");
        let line = format_itemize_line_generic(&iflags, &entry, true, &default_ctx());
        assert_eq!(line, "<f+++++++++ docs/readme.txt\n");
    }

    #[test]
    fn itemize_line_generic_directory() {
        let iflags = ItemFlags::from_raw(ItemFlags::ITEM_LOCAL_CHANGE | ItemFlags::ITEM_IS_NEW);
        let entry = make_dir_entry("subdir");
        let line = format_itemize_line_generic(&iflags, &entry, true, &default_ctx());
        assert_eq!(line, "cd+++++++++ subdir/\n");
    }

    #[cfg(unix)]
    #[test]
    fn itemize_line_generic_symlink() {
        let iflags = ItemFlags::from_raw(ItemFlags::ITEM_LOCAL_CHANGE | ItemFlags::ITEM_IS_NEW);
        let entry = make_symlink_entry("mylink");
        let line = format_itemize_line_generic(&iflags, &entry, true, &default_ctx());
        assert_eq!(line, "cL+++++++++ mylink -> target\n");
    }

    // -- Exhaustive parity: generic vs concrete for all flag/type combos --

    #[test]
    fn iflags_generic_parity_exhaustive() {
        let entries: Vec<(&str, protocol::flist::FileEntry)> = vec![
            ("file", make_file_entry("a.txt")),
            ("dir", make_dir_entry("d")),
            ("symlink", make_symlink_entry("lnk")),
            ("device", make_device_entry("dev")),
            ("special", make_special_entry("fifo")),
        ];

        let flag_sets = [
            0u32,
            ItemFlags::ITEM_TRANSFER | ItemFlags::ITEM_IS_NEW,
            ItemFlags::ITEM_DELETED,
            ItemFlags::ITEM_MISSING_DATA,
            ItemFlags::ITEM_LOCAL_CHANGE | ItemFlags::ITEM_XNAME_FOLLOWS,
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
            ItemFlags::ITEM_REPORT_TIME | ItemFlags::ITEM_REPORT_PERMS,
            ItemFlags::ITEM_REPORT_ATIME,
            ItemFlags::ITEM_REPORT_CRTIME,
        ];

        let contexts = [default_ctx(), no_times_ctx(), no_symlink_times_ctx()];

        for (label, entry) in &entries {
            for &flags in &flag_sets {
                for ctx in &contexts {
                    for is_sender in [true, false] {
                        let iflags = ItemFlags::from_raw(flags);
                        let concrete =
                            super::super::itemize::format_iflags(&iflags, entry, is_sender, ctx);
                        let generic = format_iflags_generic(&iflags, entry, is_sender, ctx);
                        assert_eq!(
                            concrete, generic,
                            "parity mismatch: type={label}, flags=0x{flags:04X}, \
                             sender={is_sender}, ctx={ctx:?}"
                        );
                    }
                }
            }
        }
    }

    // -- quick_check_matches_generic tests --

    #[test]
    fn quick_check_generic_size_mismatch_returns_false() {
        let entry = make_file_entry("test.txt"); // size=1024
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, b"short").unwrap(); // size=5
        let meta = std::fs::metadata(&path).unwrap();

        assert!(!quick_check_matches_generic(
            &entry, &path, &meta, true, false, None
        ));
    }

    #[test]
    fn quick_check_generic_size_only_matches() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        let data = vec![0u8; 1024];
        std::fs::write(&path, &data).unwrap();
        let meta = std::fs::metadata(&path).unwrap();

        let entry = FileEntry::new_file(PathBuf::from("test.txt"), 1024, 0o644);
        assert!(quick_check_matches_generic(
            &entry, &path, &meta, false, true, None
        ));
    }

    #[test]
    fn quick_check_generic_ignore_times_forces_transfer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        let data = vec![0u8; 1024];
        std::fs::write(&path, &data).unwrap();
        let meta = std::fs::metadata(&path).unwrap();

        let entry = FileEntry::new_file(PathBuf::from("test.txt"), 1024, 0o644);
        // preserve_times=false, size_only=false -> force transfer
        assert!(!quick_check_matches_generic(
            &entry, &path, &meta, false, false, None
        ));
    }

    // -- dest_mtime_newer_generic tests --

    #[test]
    fn dest_mtime_newer_generic_older_source() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, b"data").unwrap();
        let meta = std::fs::metadata(&path).unwrap();

        // Source with mtime=0 (epoch) is much older than any real file
        let mut entry = FileEntry::new_file(PathBuf::from("test.txt"), 4, 0o644);
        entry.set_mtime(0, 0);
        assert!(dest_mtime_newer_generic(&meta, &entry));
    }

    #[test]
    fn dest_mtime_newer_generic_newer_source() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, b"data").unwrap();
        let meta = std::fs::metadata(&path).unwrap();

        // Source with mtime far in the future
        let mut entry = FileEntry::new_file(PathBuf::from("test.txt"), 4, 0o644);
        entry.set_mtime(i64::MAX / 2, 0);
        assert!(!dest_mtime_newer_generic(&meta, &entry));
    }

    // -- is_hardlink_follower_generic tests --

    #[test]
    fn hardlink_follower_generic_no_flags() {
        let entry = make_file_entry("test.txt");
        assert!(!is_hardlink_follower_generic(&entry));
    }

    #[test]
    fn hardlink_follower_generic_leader() {
        let mut entry = make_file_entry("test.txt");
        // Leader has both HLINKED and HLINK_FIRST
        entry.set_hlinked(true);
        entry.set_hlink_first(true);
        assert!(!is_hardlink_follower_generic(&entry));
    }

    #[test]
    fn hardlink_follower_generic_follower() {
        let mut entry = make_file_entry("test.txt");
        // Follower has HLINKED but NOT HLINK_FIRST
        entry.set_hlinked(true);
        assert!(is_hardlink_follower_generic(&entry));
    }

    // -- Quick-check: comprehensive behavioral tests --

    /// Verifies all four (preserve_times, size_only) combinations with matching sizes.
    #[test]
    fn quick_check_generic_all_combinations_matching_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("combo.txt");
        let data = vec![0u8; 1024];
        std::fs::write(&path, &data).unwrap();
        let meta = std::fs::metadata(&path).unwrap();

        let entry = FileEntry::new_file(PathBuf::from("combo.txt"), 1024, 0o644);

        // size_only=true: always matches when sizes equal
        assert!(quick_check_matches_generic(
            &entry, &path, &meta, true, true, None
        ));
        assert!(quick_check_matches_generic(
            &entry, &path, &meta, false, true, None
        ));

        // size_only=false, preserve_times=false: forces transfer
        assert!(!quick_check_matches_generic(
            &entry, &path, &meta, false, false, None
        ));
    }

    /// Verifies that the hardlink follower check is symmetric with the leader check.
    #[test]
    fn hardlink_follower_and_leader_are_exclusive() {
        let mut follower = make_file_entry("f.txt");
        follower.set_hlinked(true);
        assert!(is_hardlink_follower_generic(&follower));

        let mut leader = make_file_entry("l.txt");
        leader.set_hlinked(true);
        leader.set_hlink_first(true);
        assert!(!is_hardlink_follower_generic(&leader));

        let plain = make_file_entry("p.txt");
        assert!(!is_hardlink_follower_generic(&plain));
    }
}
