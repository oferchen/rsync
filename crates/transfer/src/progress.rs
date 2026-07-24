//! Progress reporting for server-side transfer operations.
//!
//! Provides the [`TransferProgressCallback`] trait for receiving incremental
//! progress notifications as files are transferred. This enables callers
//! (CLI, embedding library, daemon) to display live progress indicators
//! during remote transfers over SSH or daemon connections.

use std::path::Path;

/// Progress event emitted when a file transfer completes.
///
/// Reports per-file completion along with aggregate counters that enable
/// callers to compute overall progress (e.g., "5 of 42 files").
pub struct TransferProgressEvent<'a> {
    /// Relative path of the file that was transferred.
    pub path: &'a Path,
    /// Bytes transferred for this file.
    pub file_bytes: u64,
    /// Total size of the file, if known from the file list.
    pub total_file_bytes: Option<u64>,
    /// Number of files transferred so far (including this one).
    pub files_done: usize,
    /// Total number of files to transfer.
    pub total_files: usize,
    /// Whether the file list is complete (no more INC_RECURSE sub-lists pending).
    ///
    /// Mirrors upstream's global `flist_eof` flag, which controls the
    /// `to-chk` vs `ir-chk` suffix on the per-file progress line.
    ///
    /// upstream: progress.c:79-82 rprint_progress - prints
    /// `flist_eof ? "to" : "ir"` as the chk prefix.
    pub flist_eof: bool,
}

/// Callback trait for transfer progress reporting.
///
/// Implement this trait to receive notifications as each file completes
/// during a remote transfer. The trait is object-safe for use with
/// `dyn TransferProgressCallback`.
pub trait TransferProgressCallback {
    /// Called when a file transfer completes.
    fn on_file_transferred(&mut self, event: &TransferProgressEvent<'_>);
}

impl<F: FnMut(&TransferProgressEvent<'_>)> TransferProgressCallback for F {
    fn on_file_transferred(&mut self, event: &TransferProgressEvent<'_>) {
        self(event);
    }
}

/// Callback trait for client-side itemize output.
///
/// When the client (not the server) generates files, itemize lines must be
/// written directly to the process stdout rather than sent via MSG_INFO.
/// Upstream rsync routes itemize through `rwrite()` which writes to `FCLIENT`
/// (stdout) when `am_server` is false.
///
/// # Upstream Reference
///
/// - `log.c:330-340` - `rwrite()`: when `!am_server`, writes to stdout
/// - `sender.c:287,430` - `maybe_log_item()` / `log_item()` after transfer
pub trait ItemizeCallback {
    /// Called with a pre-formatted itemize line (including trailing newline).
    fn on_itemize(&mut self, line: &str);

    /// Called with the structured per-file itemize data.
    ///
    /// The default implementation forwards the pre-formatted [`ItemizeRow::line`]
    /// to [`ItemizeCallback::on_itemize`], preserving the plain server-side print
    /// path. A client that renders a custom `--out-format` overrides this to
    /// build a metadata-bearing event from the structured fields instead.
    fn on_itemize_row(&mut self, row: &ItemizeRow<'_>) {
        self.on_itemize(row.line);
    }
}

impl<F: FnMut(&str)> ItemizeCallback for F {
    fn on_itemize(&mut self, line: &str) {
        self(line);
    }
}

/// Structured per-file data for one client-visible itemize/name emission.
///
/// Carries both the pre-formatted default line (`%i %n%L` or `%n%L`) and the raw
/// fields a client needs to render an arbitrary `--out-format` template, so the
/// callback can either print the line verbatim or reconstruct a rich event
/// without depending on the sender's `FileEntry` internals.
#[derive(Debug, Clone, Copy)]
pub struct ItemizeRow<'a> {
    /// The pre-formatted default line, including trailing newline.
    pub line: &'a str,
    /// The 11-character `%i` itemize string (upstream `YXcstpoguax`).
    pub itemize: &'a str,
    /// Transfer-relative path of the entry.
    pub name: &'a std::path::Path,
    /// File length in bytes.
    pub size: u64,
    /// Modification time, whole seconds since the Unix epoch.
    pub mtime: i64,
    /// Modification time sub-second component, nanoseconds.
    pub mtime_nsec: u32,
    /// POSIX mode bits (type + permissions).
    pub mode: u32,
    /// Owner uid, when carried by the file list (`-o`).
    pub uid: Option<u32>,
    /// Owner gid, when carried by the file list (`-g`).
    pub gid: Option<u32>,
    /// Whether the entry is a directory.
    pub is_dir: bool,
    /// Whether the entry is a symlink.
    pub is_symlink: bool,
    /// Symlink target, when the entry is a symlink.
    pub symlink_target: Option<&'a std::path::Path>,
    /// Whether the entry is newly created at the destination (`ITEM_IS_NEW`).
    pub is_new: bool,
    /// Whether the row reports a deletion (`ITEM_DELETED`).
    pub is_deletion: bool,
}
