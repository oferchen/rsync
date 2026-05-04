//! DEBUG_FLIST tracing for file list operations.
//!
//! This module provides tracing functionality that matches upstream rsync's
//! DEBUG_FLIST debug output at levels 1-4.
//!
//! # Debug Levels
//!
//! - **Level 1**: Basic file list operations (expand, flist_eof)
//! - **Level 2**: File list completion messages (send_file_list done, received N names)
//! - **Level 3**: Full file list dump via output_flist
//! - **Level 4**: Internal structure information (FILE_STRUCT_LEN, EXTRA_LEN)
//!
//! # Upstream Reference
//!
//! See `flist.c` for the canonical debug output format:
//! - `output_flist()` at DEBUG_GTE(FLIST, 3)
//! - `init_flist()` at DEBUG_GTE(FLIST, 4)
//! - Various completion messages at DEBUG_GTE(FLIST, 2)

use logging::debug_log;

use super::entry::FileEntry;
use super::state::FileListStats;

/// Process identifier for debug messages (matches upstream's who_am_i()).
///
/// In upstream rsync, this is "sender", "receiver", or "generator".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessRole {
    /// The sender process.
    Sender,
    /// The receiver process.
    Receiver,
    /// The generator process.
    Generator,
}

impl ProcessRole {
    /// Returns the string representation matching upstream rsync.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Sender => "sender",
            Self::Receiver => "receiver",
            Self::Generator => "generator",
        }
    }
}

impl std::fmt::Display for ProcessRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Traces file list pointer array expansion (level 1).
///
/// Matches upstream's `flist_expand()` debug output:
/// ```text
/// [sender] expand file_list pointer array to 16,384 bytes, did not move
/// ```
#[inline]
pub fn trace_flist_expand(role: ProcessRole, new_size_bytes: usize, did_move: bool) {
    debug_log!(
        Flist,
        1,
        "[{}] expand file_list pointer array to {} bytes, did{} move",
        role,
        format_number(new_size_bytes),
        if did_move { "" } else { " not" }
    );
}

/// Traces file list EOF marker (level 3).
///
/// Matches upstream's `flist_eof` debug output:
/// ```text
/// [sender] flist_eof=1
/// ```
#[inline]
pub fn trace_flist_eof(role: ProcessRole) {
    debug_log!(Flist, 3, "[{}] flist_eof=1", role);
}

/// Traces send_file_list completion (level 2).
///
/// Matches upstream:
/// ```text
/// send_file_list done
/// ```
#[inline]
pub fn trace_send_file_list_done() {
    debug_log!(Flist, 2, "send_file_list done");
}

/// Traces received file count (level 2).
///
/// Matches upstream:
/// ```text
/// received 42 names
/// ```
#[inline]
pub fn trace_received_names(count: usize) {
    debug_log!(Flist, 2, "received {} names", count);
}

/// Traces recv_file_list completion (level 2).
///
/// Matches upstream:
/// ```text
/// recv_file_list done
/// ```
#[inline]
pub fn trace_recv_file_list_done() {
    debug_log!(Flist, 2, "recv_file_list done");
}

/// Traces receiving incremental file list for a directory (level 3).
///
/// Matches upstream:
/// ```text
/// [receiver] receiving flist for dir 5
/// ```
#[inline]
pub fn trace_receiving_flist_for_dir(role: ProcessRole, dir_ndx: i32) {
    debug_log!(Flist, 3, "[{}] receiving flist for dir {}", role, dir_ndx);
}

/// Traces internal structure sizes (level 4).
///
/// Matches upstream's `init_flist()` debug output:
/// ```text
/// FILE_STRUCT_LEN=136, EXTRA_LEN=8
/// ```
///
/// In our Rust implementation, we report the size of FileEntry and
/// additional metadata overhead.
#[inline]
pub fn trace_struct_sizes() {
    let file_entry_size = std::mem::size_of::<FileEntry>();
    let extra_len = std::mem::size_of::<usize>(); // Pointer size for Vec overhead
    debug_log!(
        Flist,
        4,
        "FILE_STRUCT_LEN={}, EXTRA_LEN={}",
        file_entry_size,
        extra_len
    );
}

/// Outputs the complete file list for debugging (level 3).
///
/// Matches upstream's `output_flist()` function format:
/// ```text
/// [sender] i=0 root /path/to/dir/ mode=040755 len=4,096 uid=1000 gid=1000 flags=0
/// [sender] i=1 /file.txt mode=0100644 len=1,234 uid=1000 gid=1000 flags=0
/// ```
///
/// # Arguments
///
/// * `role` - The process role (sender, receiver, generator)
/// * `entries` - Slice of file entries to output
/// * `first_ndx` - Starting index for the file list (usually 0)
pub fn output_flist(role: ProcessRole, entries: &[FileEntry], first_ndx: i32) {
    for (i, entry) in entries.iter().enumerate() {
        let ndx = first_ndx + i as i32;
        output_flist_entry(role, ndx, entry);
    }
}

/// Outputs a single file entry for debugging (level 3).
///
/// Format:
/// ```text
/// [sender] i=0 root /path/ mode=040755 len=4,096 uid=1000 gid=1000 flags=0x0
/// ```
#[inline]
pub fn output_flist_entry(role: ProcessRole, ndx: i32, entry: &FileEntry) {
    // Build the path string
    let name_str = entry.name();

    // Determine if this is root (empty path or ".")
    let is_root = name_str.is_empty() || name_str == ".";
    let root_marker = if is_root { "root " } else { "" };

    // Add trailing slash for directories
    let trailing = if entry.is_dir() && !name_str.ends_with('/') {
        "/"
    } else {
        ""
    };

    // Format length with commas
    let len_str = format_number(entry.size() as usize);

    // Format UID/GID
    let uid_str = entry
        .uid()
        .map_or(String::new(), |uid| format!(" uid={uid}"));
    let gid_str = entry
        .gid()
        .map_or(String::new(), |gid| format!(" gid={gid}"));

    // Get flags value
    let flags = entry.flags();
    let flags_value = flags.primary as u32 | ((flags.extended as u32) << 8);

    debug_log!(
        Flist,
        3,
        "[{}] i={} {}{}{} mode=0{:o} len={}{}{}{}",
        role,
        ndx,
        root_marker,
        name_str,
        trailing,
        entry.mode(),
        len_str,
        uid_str,
        gid_str,
        format!(" flags={:#x}", flags_value)
    );
}

/// Traces file entry being read from wire (level 3).
///
/// This provides detailed per-entry tracing during file list reception.
#[inline]
pub fn trace_read_entry(ndx: i32, name: &str, mode: u32, size: u64) {
    debug_log!(
        Flist,
        3,
        "recv_file_entry: i={} name={} mode=0{:o} size={}",
        ndx,
        name,
        mode,
        format_number(size as usize)
    );
}

/// Traces file entry being written to wire (level 3).
///
/// This provides detailed per-entry tracing during file list sending.
#[inline]
pub fn trace_write_entry(ndx: i32, name: &str, mode: u32, size: u64) {
    debug_log!(
        Flist,
        3,
        "send_file_entry: i={} name={} mode=0{:o} size={}",
        ndx,
        name,
        mode,
        format_number(size as usize)
    );
}

/// Traces file list statistics (level 2).
///
/// Provides a summary of the file list contents.
#[inline]
pub fn trace_file_list_stats(stats: &FileListStats) {
    debug_log!(
        Flist,
        2,
        "file list stats: files={} dirs={} symlinks={} devices={} specials={} total_size={}",
        stats.num_files,
        stats.num_dirs,
        stats.num_symlinks,
        stats.num_devices,
        stats.num_specials,
        format_number(stats.total_size as usize)
    );
}

/// Traces file count for progress (level 1).
///
/// Matches upstream's emit_filelist_progress format:
/// ```text
/// 42 files...
/// ```
#[inline]
pub fn trace_file_count_progress(count: usize) {
    debug_log!(Flist, 1, "{} files...", count);
}

/// Traces file list completion count (level 2, via INFO).
///
/// Matches upstream's finish_filelist_progress format:
/// ```text
/// 42 files to consider
/// ```
///
/// Note: This uses level 2 to match upstream's INFO_GTE(FLIST, 2).
#[inline]
pub fn trace_files_to_consider(count: usize) {
    let plural = if count == 1 { " " } else { "s " };
    debug_log!(Flist, 2, "{} file{} to consider", count, plural);
}

/// Traces sorting operation (level 2).
#[inline]
pub fn trace_sort_start(count: usize) {
    debug_log!(Flist, 2, "sorting {} entries", count);
}

/// Traces cleaning/deduplication results (level 2).
#[inline]
pub fn trace_clean_result(original: usize, cleaned: usize, duplicates: usize) {
    debug_log!(
        Flist,
        2,
        "cleaned file list: {} -> {} ({} duplicates removed)",
        original,
        cleaned,
        duplicates
    );
}

/// Traces hardlink detection (level 3).
///
/// Matches upstream's DEBUG_GTE(HLINK, 1) but reported under FLIST:
/// ```text
/// [sender] #5 hard-links #2 (abbrev)
/// ```
#[inline]
pub fn trace_hardlink(role: ProcessRole, ndx: i32, target_ndx: i32, is_abbrev: bool) {
    let abbrev = if is_abbrev { "abbrev" } else { "full" };
    debug_log!(
        Flist,
        3,
        "[{}] #{} hard-links #{} ({})",
        role,
        ndx,
        target_ndx,
        abbrev
    );
}

/// Traces hardlink dev:inode mapping (level 4).
///
/// Matches upstream's DEBUG_GTE(HLINK, 3):
/// ```text
/// [sender] dev:inode for #5 is 2049:12345
/// ```
#[inline]
pub fn trace_hardlink_dev_ino(role: ProcessRole, ndx: i32, dev: i64, ino: i64) {
    debug_log!(
        Flist,
        4,
        "[{}] dev:inode for #{} is {}:{}",
        role,
        ndx,
        dev,
        ino
    );
}

/// Formats a number with comma separators for readability.
///
/// This matches upstream rsync's `big_num()` formatting.
fn format_number(n: usize) -> String {
    let s = n.to_string();
    let mut result = String::with_capacity(s.len() + s.len() / 3);
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();

    for (i, ch) in chars.iter().enumerate() {
        if i > 0 && (len - i) % 3 == 0 {
            result.push(',');
        }
        result.push(*ch);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_number() {
        assert_eq!(format_number(0), "0");
        assert_eq!(format_number(1), "1");
        assert_eq!(format_number(12), "12");
        assert_eq!(format_number(123), "123");
        assert_eq!(format_number(1234), "1,234");
        assert_eq!(format_number(12345), "12,345");
        assert_eq!(format_number(123456), "123,456");
        assert_eq!(format_number(1234567), "1,234,567");
        assert_eq!(format_number(1234567890), "1,234,567,890");
    }

    #[test]
    fn test_process_role_as_str() {
        assert_eq!(ProcessRole::Sender.as_str(), "sender");
        assert_eq!(ProcessRole::Receiver.as_str(), "receiver");
        assert_eq!(ProcessRole::Generator.as_str(), "generator");
    }

    #[test]
    fn test_process_role_display() {
        assert_eq!(format!("{}", ProcessRole::Sender), "sender");
        assert_eq!(format!("{}", ProcessRole::Receiver), "receiver");
        assert_eq!(format!("{}", ProcessRole::Generator), "generator");
    }
}
