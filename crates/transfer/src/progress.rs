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
/// - `sender.c:290,431` - `maybe_log_item()` / `log_item()` after transfer
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

/// Per-entry daemon transfer-log sink.
///
/// A daemon serving a module with `transfer logging = yes` writes one log-file
/// line per processed file, exactly as upstream does via `log_item(FLOG, ...)`.
/// This sink is invoked once per entry after the transfer completes, in
/// flist-index order, with a [`DaemonLogRow`] carrying the per-file fields the
/// module's `log format` can reference. The daemon-side implementation plugs
/// those into the format and writes the result to the log.
///
/// This is the daemon's OWN log-file write and is distinct from the client-side
/// itemize forwarding driven by [`ItemizeCallback`]: upstream reaches it through
/// `maybe_log_item()`'s `am_server` arm (`log.c:875`) and the unconditional
/// per-transfer `log_item()` (`receiver.c:1290` / `sender.c:462`), neither of
/// which is gated on the client's `-i`.
///
/// # Upstream Reference
///
/// - `log.c:866-874` - `log_item()` writes `FLOG` whenever `logfile_format` is set
/// - `log.c:875-891` - `maybe_log_item()` gates non-transfer items on the daemon
/// - `receiver.c:1290` / `sender.c:462` - the per-transfer `log_item()`
pub trait DaemonFileLog {
    /// Renders and writes one daemon-log line for a processed entry.
    fn on_entry(&mut self, row: &DaemonLogRow);
}

/// The per-file fields one daemon transfer-log line renders.
///
/// upstream: log.c `log_formatted()` reads each of these from the entry's
/// `struct file_struct` (plus the `hlink` argument of `log_item()`), so the
/// row snapshots them when the entry is logged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DaemonLogRow {
    /// Transfer-relative name (`%f`, and `%n` before its directory slash).
    pub name: std::path::PathBuf,
    /// File length (`%l`).
    pub size: u64,
    /// Rendered 11-character itemize string (`%i`).
    pub itemize: String,
    /// Full `st_mode`, type bits included (`%n` slash, `%B`, `%L`).
    pub mode: u32,
    /// Modification time in seconds since the epoch (`%M`).
    pub mtime: i64,
    /// Owner uid, or 0 when the entry carries none (`%U`; entries carry ids
    /// only under `-o`, upstream's `uid_ndx`).
    pub uid: u32,
    /// Group gid, or `None` when the entry carries none (no `-g`, upstream's
    /// `gid_ndx`) or the receiver cannot set the group (`FLAG_SKIP_GROUP`);
    /// `%G` then renders `DEFAULT`.
    pub gid: Option<u32>,
    /// Symlink target as stored in the file list (`%L` ` -> ` form).
    pub symlink_target: Option<std::path::PathBuf>,
    /// Hard-link leader name passed as `log_item()`'s `hlink` (`%L` ` => `).
    pub hardlink_target: Option<Vec<u8>>,
    /// Rendered `%C` field: the hex digest, or `sum_len * 2` spaces.
    pub checksum: String,
}

impl DaemonLogRow {
    /// Snapshots `entry` for one log line. The caller resolves the
    /// role-dependent fields: the gid after the receiver's `FLAG_SKIP_GROUP`
    /// gate, and the symlink target in its stored (munged/sanitized) form.
    pub(crate) fn new(
        entry: &protocol::flist::FileEntry,
        itemize: String,
        gid: Option<u32>,
        symlink_target: Option<std::path::PathBuf>,
        xname: Option<&[u8]>,
        checksum: String,
    ) -> Self {
        Self {
            name: entry.path().to_path_buf(),
            size: entry.size(),
            itemize,
            mode: entry.mode(),
            mtime: entry.mtime(),
            // upstream: log.c `case 'U'` - `uid_ndx ? F_OWNER(file) : 0`.
            uid: entry.uid().unwrap_or(0),
            gid,
            symlink_target,
            hardlink_target: xname.filter(|name| !name.is_empty()).map(<[u8]>::to_vec),
            checksum,
        }
    }
}

/// Largest whole-file digest (upstream `MAX_DIGEST_LEN`, SHA-512).
pub(crate) const MAX_FILE_SUM_LEN: usize = 64;

/// Copies `sum` into a zero-padded `sender_file_sum` buffer.
pub(crate) fn file_sum_buf(sum: &[u8]) -> [u8; MAX_FILE_SUM_LEN] {
    let mut buf = [0; MAX_FILE_SUM_LEN];
    let len = sum.len().min(MAX_FILE_SUM_LEN);
    buf[..len].copy_from_slice(&sum[..len]);
    buf
}

/// How a daemon-log `%C` field renders a whole-file digest.
///
/// upstream: log.c `case 'C'` renders `sum_as_hex()` (util2.c:93) of the
/// negotiated checksum, or `csum_len_for_type() * 2` spaces when there is no
/// digest to show or the type is not canonical.
#[derive(Clone, Copy, Debug)]
pub(crate) struct LogChecksumFormat {
    len: usize,
    order: Option<LogChecksumOrder>,
}

/// `checksum.c:canonical_checksum()` - the byte order `sum_as_hex()` prints.
#[derive(Clone, Copy, Debug)]
enum LogChecksumOrder {
    /// MD4/MD5/SHA (`-1`): digest bytes in order.
    Forward,
    /// The xxHash family (`1`): digest bytes reversed.
    Reversed,
}

impl LogChecksumFormat {
    /// Selects the format for the negotiated checksum `algorithm`.
    ///
    /// Below protocol 30 upstream's implied checksum is `CSUM_MD4_OLD`, which
    /// `canonical_checksum()` reports as non-canonical, so `%C` stays blank.
    pub(crate) const fn new(
        algorithm: protocol::ChecksumAlgorithm,
        protocol: protocol::ProtocolVersion,
    ) -> Self {
        use protocol::ChecksumAlgorithm as A;
        let len = match algorithm {
            // upstream: checksum.c:235 - `csum_len_for_type(CSUM_NONE)` is 1.
            A::None => 1,
            A::MD4 | A::MD5 | A::XXH128 => 16,
            A::SHA1 => 20,
            A::XXH64 | A::XXH3 => 8,
        };
        let order = match algorithm {
            _ if protocol.as_u8() < 30 => None,
            A::None => None,
            A::MD4 | A::MD5 | A::SHA1 => Some(LogChecksumOrder::Forward),
            A::XXH64 | A::XXH3 | A::XXH128 => Some(LogChecksumOrder::Reversed),
        };
        Self { len, order }
    }

    /// Renders `%C` for `entry` (log.c `case 'C'`): a regular file shows its
    /// file-list sum under `--checksum`, otherwise the transfer sum
    /// (`sender_file_sum`) when it was transferred; anything else is blank.
    pub(crate) fn render(
        self,
        entry: &protocol::flist::FileEntry,
        always_checksum: bool,
        is_transfer: bool,
        sender_file_sum: &[u8],
    ) -> String {
        if !entry.is_file() {
            return self.blank();
        }
        if always_checksum {
            return entry
                .checksum()
                .map_or_else(|| self.blank(), |sum| self.hex(sum));
        }
        if is_transfer {
            return self.hex(sender_file_sum);
        }
        self.blank()
    }

    /// The empty field: `sum_len * 2` spaces (log.c `case 'C'`).
    pub(crate) fn blank(self) -> String {
        " ".repeat(self.len * 2)
    }

    /// Renders `sum` as lowercase hex, or the blank field when the type is not
    /// canonical (upstream `sum_as_hex()` returns NULL).
    pub(crate) fn hex(self, sum: &[u8]) -> String {
        use std::fmt::Write as _;
        let Some(order) = self.order else {
            return self.blank();
        };
        let sum = &sum[..self.len.min(sum.len())];
        let mut out = String::with_capacity(sum.len() * 2);
        let mut push = |byte: &u8| {
            // Writing to a String cannot fail.
            let _ = write!(out, "{byte:02x}");
        };
        match order {
            LogChecksumOrder::Forward => sum.iter().for_each(&mut push),
            LogChecksumOrder::Reversed => sum.iter().rev().for_each(&mut push),
        }
        out
    }
}

/// Collected per-file daemon-log rows keyed by flist index.
///
/// Keyed by index so a drain flushes in the order upstream logs the entries; a
/// `Vec` per index tolerates a phase-2 redo re-recording the same entry.
pub type DaemonLogRows = std::collections::BTreeMap<usize, Vec<DaemonLogRow>>;

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
    /// Full source-side path of the entry (upstream `F_PATHNAME` joined with the
    /// file name), set only on a push where the local side is the sender. Renders
    /// the `%f` placeholder; `None` on a pull, where `%f` falls back to `name`.
    pub source_prefix: Option<&'a std::path::Path>,
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
    /// Hard-link group leader's transfer-relative name, when this row is a
    /// hard-link follower. Renders the `%L` ` => <leader>` suffix (upstream
    /// `hlink.c:232-234` passes `realname`; `log.c:643-646` renders ` => hlink`).
    /// `None` for every non-hard-link row; distinct from `symlink_target`, which
    /// carries a symlink's ` -> ` target.
    pub hardlink_leader: Option<&'a std::path::Path>,
    /// Whether the entry is newly created at the destination (`ITEM_IS_NEW`).
    pub is_new: bool,
    /// Whether the row reports a deletion (`ITEM_DELETED`).
    pub is_deletion: bool,
}

/// Owned counterpart of [`ItemizeRow`] for buffering a client-visible row until
/// the end of a transfer.
///
/// A pulling client's receiver renders its itemize rows in flist-index order and
/// only flushes them after the transfer loop finishes (see the receiver's
/// `event_rows` buffer). The borrowed [`ItemizeRow`] cannot outlive the
/// per-entry `FileEntry`, so the owned fields are copied here and re-borrowed via
/// [`OwnedItemizeRow::as_row`] when the callback is finally invoked.
#[derive(Debug, Clone)]
pub struct OwnedItemizeRow {
    /// Owned copy of [`ItemizeRow::line`].
    pub line: String,
    /// Owned copy of [`ItemizeRow::itemize`].
    pub itemize: String,
    /// Owned copy of [`ItemizeRow::name`].
    pub name: std::path::PathBuf,
    /// Owned copy of [`ItemizeRow::source_prefix`].
    pub source_prefix: Option<std::path::PathBuf>,
    /// See [`ItemizeRow::size`].
    pub size: u64,
    /// See [`ItemizeRow::mtime`].
    pub mtime: i64,
    /// See [`ItemizeRow::mtime_nsec`].
    pub mtime_nsec: u32,
    /// See [`ItemizeRow::mode`].
    pub mode: u32,
    /// See [`ItemizeRow::uid`].
    pub uid: Option<u32>,
    /// See [`ItemizeRow::gid`].
    pub gid: Option<u32>,
    /// See [`ItemizeRow::is_dir`].
    pub is_dir: bool,
    /// See [`ItemizeRow::is_symlink`].
    pub is_symlink: bool,
    /// Owned copy of [`ItemizeRow::symlink_target`].
    pub symlink_target: Option<std::path::PathBuf>,
    /// Owned copy of [`ItemizeRow::hardlink_leader`].
    pub hardlink_leader: Option<std::path::PathBuf>,
    /// See [`ItemizeRow::is_new`].
    pub is_new: bool,
    /// See [`ItemizeRow::is_deletion`].
    pub is_deletion: bool,
}

impl OwnedItemizeRow {
    /// Borrows the owned fields into an [`ItemizeRow`] for a callback invocation.
    #[must_use]
    pub fn as_row(&self) -> ItemizeRow<'_> {
        ItemizeRow {
            line: &self.line,
            itemize: &self.itemize,
            name: &self.name,
            source_prefix: self.source_prefix.as_deref(),
            size: self.size,
            mtime: self.mtime,
            mtime_nsec: self.mtime_nsec,
            mode: self.mode,
            uid: self.uid,
            gid: self.gid,
            is_dir: self.is_dir,
            is_symlink: self.is_symlink,
            symlink_target: self.symlink_target.as_deref(),
            hardlink_leader: self.hardlink_leader.as_deref(),
            is_new: self.is_new,
            is_deletion: self.is_deletion,
        }
    }
}

#[cfg(test)]
mod log_checksum_tests {
    use super::LogChecksumFormat;
    use protocol::{ChecksumAlgorithm, ProtocolVersion};

    const SUM: [u8; 16] = [
        0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
        0xff,
    ];

    #[test]
    fn md5_prints_digest_bytes_in_order() {
        // upstream: canonical_checksum(CSUM_MD5) == -1, so sum_as_hex walks forward.
        let format = LogChecksumFormat::new(ChecksumAlgorithm::MD5, ProtocolVersion::V32);
        assert_eq!(format.hex(&SUM), "00112233445566778899aabbccddeeff");
    }

    #[test]
    fn xxhash_prints_digest_bytes_reversed() {
        // upstream: canonical_checksum(CSUM_XXH3_128) == 1, so sum_as_hex walks backward.
        let format = LogChecksumFormat::new(ChecksumAlgorithm::XXH128, ProtocolVersion::V32);
        assert_eq!(format.hex(&SUM), "ffeeddccbbaa99887766554433221100");
        let format = LogChecksumFormat::new(ChecksumAlgorithm::XXH3, ProtocolVersion::V32);
        assert_eq!(format.hex(&SUM), "7766554433221100");
    }

    #[test]
    fn legacy_md4_is_not_canonical_and_stays_blank() {
        // upstream: protocol < 30 implies CSUM_MD4_OLD, which sum_as_hex rejects;
        // log.c then pads csum_len_for_type() * 2 spaces.
        let format = LogChecksumFormat::new(ChecksumAlgorithm::MD4, ProtocolVersion::V29);
        assert_eq!(format.hex(&SUM), " ".repeat(32));
        let format = LogChecksumFormat::new(ChecksumAlgorithm::MD4, ProtocolVersion::V30);
        assert_eq!(format.hex(&SUM), "00112233445566778899aabbccddeeff");
    }

    #[test]
    fn blank_width_follows_the_digest_length() {
        let width = |algorithm| {
            LogChecksumFormat::new(algorithm, ProtocolVersion::V32)
                .blank()
                .len()
        };
        assert_eq!(width(ChecksumAlgorithm::None), 2);
        assert_eq!(width(ChecksumAlgorithm::SHA1), 40);
        assert_eq!(width(ChecksumAlgorithm::XXH64), 16);
        assert_eq!(width(ChecksumAlgorithm::MD5), 32);
    }
}
