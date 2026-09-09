//! DEBUG_FLIST tracing for file list operations.
//!
//! This module owns the `--debug=flist` emissions that mirror upstream
//! rsync's `DEBUG_GTE(FLIST, n)` sites. Each function corresponds to one
//! upstream emission text; the live send/receive paths call these so every
//! upstream line has exactly one oc owner.
//!
//! # Debug Levels (as measured on upstream 3.5.0)
//!
//! - **Level 1**: `delta-transmission %s` (generator.c:2763; owned by the
//!   receiver-side transfer setup and the local-copy frontend, not here) and
//!   the `expand file_list pointer array` realloc trace (flist.c:403, no oc
//!   analogue - see below).
//! - **Level 2**: `[%s] make_file(%s,*,%d)` (flist.c:1542),
//!   `send_file_list done` (flist.c:2838), `recv_file_name(%s)`
//!   (flist.c:3012), `received %d names` (flist.c:3019),
//!   `recv_file_list done` (flist.c:3088), and
//!   `[%s] receiving flist for dir %d` (io.c:1943, rsync.c:373).
//! - **Level 3**: `output_flist()` (flist.c:3489, called from flist.c:2470,
//!   :2835, :3085, :3756), `[%s] flist_eof=1` (seven sites: flist.c:2481,
//!   :2850, :2861, :3058, :3105, io.c:1931, rsync.c:357), `file list sent`
//!   (main.c:1374), the `[%s] receiving flist for dir %d` copy in
//!   `recv_additional_file_list` (flist.c:3118), and the item-list expand
//!   trace (util1.c:1956, no oc analogue).
//! - **Level 4**: `FILE_STRUCT_LEN=%d, EXTRA_LEN=%d` (flist.c:163, no oc
//!   analogue).
//!
//! # Deliberately unowned upstream sites
//!
//! - flist.c:403 and util1.c:1956 trace `realloc_array()` growth of pointer
//!   arrays. oc's file lists are `Vec`s of inline entries: there is no
//!   pointer-array realloc event, and reporting `Vec` doublings would emit
//!   lines upstream's cells never show (upstream's arrays start large enough
//!   that small transfers never grow them).
//! - flist.c:163 reports `FILE_STRUCT_LEN`/`EXTRA_LEN`, the constants of
//!   upstream's `file_struct` + trailing-extras allocation scheme. oc's
//!   `FileEntry` has no extras array, so `EXTRA_LEN` has no honest value and
//!   a partial line would break the format.

use std::path::Path;
use std::sync::Arc;

use logging::debug_log;

use super::entry::FileEntry;

/// Process identifier for debug messages (matches upstream's `who_am_i()`,
/// rsync.c:987).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessRole {
    /// The sender process.
    Sender,
    /// The receiver process.
    Receiver,
    /// The generator process.
    Generator,
    /// The receiving side before upstream forks it into receiver and
    /// generator; upstream prints this as capitalized `Receiver`
    /// (rsync.c:994 "pre-forked receiver"). The initial `recv_file_list()`
    /// runs in this state.
    PreForkReceiver,
}

impl ProcessRole {
    /// Returns the string representation matching upstream rsync.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Sender => "sender",
            Self::Receiver => "receiver",
            Self::Generator => "generator",
            Self::PreForkReceiver => "Receiver",
        }
    }
}

impl std::fmt::Display for ProcessRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Traces one `make_file()` call (level 2).
///
/// upstream: flist.c:1542 `[%s] make_file(%s,*,%d)`. The third argument is
/// the filter level: `NO_FILTERS` (0) for named command-line sources,
/// `SERVER_FILTERS` (1) on the daemon arg path, `ALL_FILTERS` (2) for
/// entries found by recursion and implied directories (rsync.h:212-214).
#[inline]
pub fn trace_make_file(role: ProcessRole, name: &dyn std::fmt::Display, filter_level: u8) {
    debug_log!(
        Flist,
        2,
        "[{}] make_file({},*,{})",
        role,
        name,
        filter_level
    );
}

/// Traces file list EOF (level 3).
///
/// upstream: `[%s] flist_eof=1` - written when a side sets its `flist_eof`
/// global (flist.c:2481, :2850, :2861, :3058, :3105, io.c:1931, rsync.c:357).
#[inline]
pub fn trace_flist_eof(role: ProcessRole) {
    debug_log!(Flist, 3, "[{}] flist_eof=1", role);
}

/// Traces send_file_list completion (level 2).
///
/// upstream: flist.c:2838 `send_file_list done`.
#[inline]
pub fn trace_send_file_list_done() {
    debug_log!(Flist, 2, "send_file_list done");
}

/// Traces one received file-list name (level 2).
///
/// upstream: flist.c:3012 `recv_file_name(%s)` - printed for each entry as
/// the receive loop stores it.
#[inline]
pub fn trace_recv_file_name(name: &str) {
    debug_log!(Flist, 2, "recv_file_name({})", name);
}

/// Traces received file count (level 2).
///
/// upstream: flist.c:3019 `received %d names` - printed once per
/// `recv_file_list()` call, after its entry loop.
#[inline]
pub fn trace_received_names(count: usize) {
    debug_log!(Flist, 2, "received {} names", count);
}

/// Traces recv_file_list completion (level 2).
///
/// upstream: flist.c:3088 `recv_file_list done`.
#[inline]
pub fn trace_recv_file_list_done() {
    debug_log!(Flist, 2, "recv_file_list done");
}

/// Traces receiving an incremental file list for a directory.
///
/// upstream prints the same text from three sites at two levels, so the
/// level is a per-call-site parameter: level 2 at io.c:1943 (generator) and
/// rsync.c:373 (receiver), level 3 at flist.c:3118
/// (`recv_additional_file_list`).
#[inline]
pub fn trace_receiving_flist_for_dir(role: ProcessRole, dir_ndx: i32, level: u8) {
    debug_log!(
        Flist,
        level,
        "[{}] receiving flist for dir {}",
        role,
        dir_ndx
    );
}

/// Traces client-side file list transmission completion (level 3).
///
/// upstream: main.c:1374 `file list sent` - printed by `client_run()` only
/// (the server sender has no such line).
#[inline]
pub fn trace_file_list_sent() {
    debug_log!(Flist, 3, "file list sent");
}

/// Dumps a file list (level 3).
///
/// upstream: flist.c:3489 `output_flist()` - a header line followed by one
/// line per slot (tombstoned slots print empty name fields, exactly as
/// upstream prints a `!F_IS_ACTIVE` slot):
///
/// ```text
/// [sender] flist start=1, used=3, low=0, high=2
/// [sender] i=1 /src ./ mode=040755 len=128 uid=501 gid=0 flags=1005
/// ```
///
/// `source_bases`, when given (the sender), supplies upstream's
/// `F_PATHNAME(file)` root column per entry; without it (receiver and
/// generator) the root column is the entry's depth, as upstream prints
/// `F_DEPTH(file)`. `show_uid` is upstream's `(am_root || am_sender) &&
/// uid_ndx` gate. Divergences forced by oc's internals: the `flags` word is
/// reconstructed from the bits oc tracks (upstream's receiver-only
/// `FLAG_SKIP_GROUP` is not among them, so the receiver's `gid` column also
/// never carries upstream's skip-group parentheses), and `low`/`high` are
/// derived from the active slots rather than kept as running fields.
pub fn output_flist(
    role: ProcessRole,
    entries: &[FileEntry],
    ndx_start: i32,
    source_bases: Option<&[Arc<Path>]>,
    show_uid: bool,
) {
    if !logging::debug_gte(logging::DebugFlag::Flist, 3) {
        return;
    }
    // upstream: flist->low/high bracket the active slots.
    let low = entries.iter().position(FileEntry::is_active).unwrap_or(0);
    let high = entries
        .iter()
        .rposition(FileEntry::is_active)
        .map_or_else(|| entries.len().max(1) - 1, |h| h);
    debug_log!(
        Flist,
        3,
        "[{}] flist start={}, used={}, low={}, high={}",
        role,
        ndx_start,
        entries.len(),
        low,
        high
    );
    for (i, entry) in entries.iter().enumerate() {
        let base = source_bases.and_then(|bases| bases.get(i));
        output_flist_entry(role, ndx_start + i as i32, entry, base, show_uid);
    }
}

/// Formats one `output_flist()` line (level 3).
///
/// upstream: flist.c:3524 `[%s] i=%d %s %s%s%s%s mode=0%o len=%s%s%s flags=%x`.
fn output_flist_entry(
    role: ProcessRole,
    ndx: i32,
    entry: &FileEntry,
    source_base: Option<&Arc<Path>>,
    show_uid: bool,
) {
    let (root, name, trail) = if entry.is_active() {
        let name = entry.name();
        let root = source_base.map_or_else(
            || {
                // upstream: the non-sender root column is F_DEPTH(file); the
                // implied root "." is depth 0, every path component adds one.
                let depth = if name == "." {
                    0
                } else {
                    name.split('/').count()
                };
                depth.to_string()
            },
            |base| {
                // upstream: F_PATHNAME never carries the operand's trailing
                // slash (send_file_list strips it before chdir).
                let shown = base.display().to_string();
                let trimmed = shown.trim_end_matches('/');
                if trimmed.is_empty() {
                    shown
                } else {
                    trimmed.to_owned()
                }
            },
        );
        let trail = if entry.is_dir() && !name.ends_with('/') {
            "/"
        } else {
            ""
        };
        (root, name.to_owned(), trail)
    } else {
        (String::new(), String::new(), "")
    };
    let uid = if show_uid {
        entry
            .uid()
            .map_or(String::new(), |uid| format!(" uid={uid}"))
    } else {
        String::new()
    };
    let gid = entry
        .gid()
        .map_or(String::new(), |gid| format!(" gid={gid}"));
    debug_log!(
        Flist,
        3,
        "[{}] i={} {} {}{} mode=0{:o} len={}{}{} flags={:x}",
        role,
        ndx,
        root,
        name,
        trail,
        entry.mode(),
        format_number(entry.size() as usize),
        uid,
        gid,
        upstream_flags_word(entry)
    );
}

/// Reconstructs upstream's in-memory `file->flags` word from the bits oc
/// tracks.
///
/// upstream: rsync.h:77-100. oc's `FileEntry` has no single runtime flags
/// word; the bits with oc state are `FLAG_TOP_DIR` (1<<0),
/// `FLAG_CONTENT_DIR` (1<<2), `FLAG_DUPLICATE` (1<<4), `FLAG_HLINKED`
/// (1<<5), `FLAG_HLINK_FIRST` (1<<6), `FLAG_LENGTH64` (1<<9), and
/// `FLAG_MOD_NSEC` (1<<12). Bits upstream tracks but oc does not (e.g. the
/// receiver's `FLAG_SKIP_GROUP`, 1<<10) are absent from the output.
fn upstream_flags_word(entry: &FileEntry) -> u32 {
    let mut flags = 0u32;
    if entry.top_dir() {
        flags |= 1 << 0;
    }
    if entry.is_dir() && entry.content_dir() {
        flags |= 1 << 2;
    }
    if entry.duplicate() {
        flags |= 1 << 4;
    }
    if entry.hlinked() {
        flags |= 1 << 5;
    }
    if entry.hlink_first() {
        flags |= 1 << 6;
    }
    if entry.size() > u64::from(u32::MAX) {
        flags |= 1 << 9;
    }
    if entry.mtime_nsec() != 0 {
        flags |= 1 << 12;
    }
    flags
}

/// Formats a number with comma separators for readability.
///
/// This matches upstream rsync's `comma_num()`/`big_num()` formatting.
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
        // upstream: rsync.c:994 - the pre-forked receiver capitalizes.
        assert_eq!(ProcessRole::PreForkReceiver.as_str(), "Receiver");
    }

    #[test]
    fn test_process_role_display() {
        assert_eq!(format!("{}", ProcessRole::Sender), "sender");
        assert_eq!(format!("{}", ProcessRole::Receiver), "receiver");
        assert_eq!(format!("{}", ProcessRole::Generator), "generator");
        assert_eq!(format!("{}", ProcessRole::PreForkReceiver), "Receiver");
    }

    /// upstream: flist.c:3524 - the entry line for a plain file at depth 1,
    /// gid shown, uid hidden (receiver without root).
    #[test]
    fn flags_word_tracks_top_dir_content_dir_and_nsec() {
        let mut dir = FileEntry::new_directory(".".into(), 0o755);
        dir.set_top_dir(true);
        dir.set_content_dir(true);
        dir.set_mtime(0, 1);
        assert_eq!(upstream_flags_word(&dir), 0x1005);

        let mut file = FileEntry::new_file("a.txt".into(), 6, 0o644);
        file.set_mtime(0, 1);
        assert_eq!(upstream_flags_word(&file), 0x1000);
    }
}
