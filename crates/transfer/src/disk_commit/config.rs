//! Configuration types for the disk commit thread.
//!
//! Provides `DiskCommitConfig`, `BackupConfig`, and `PartialMode` which
//! control how the disk thread writes, syncs, commits files, and handles
//! interrupted transfers.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;

use filters::FilterSet;
use metadata::MetadataOptions;
use protocol::acl::AclCache;

/// Controls partial file retention on interrupted transfers.
///
/// When a transfer is interrupted (signal, connection drop, error), upstream
/// rsync either deletes the temp file (default) or retains it for later resume
/// via `--partial` or `--partial-dir`.
///
/// # Upstream Reference
///
/// - `cleanup.c:105-115` - `handle_partial_dir()` renames temp to partial-dir
/// - `options.c:keep_partial` - `--partial` flag
/// - `options.c:partial_dir` - `--partial-dir=DIR` option
/// - `receiver.c:340-345` - `do_rename(partialptr, fname)` on interrupt
#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub enum PartialMode {
    /// No partial retention: delete temp files on interrupt (default behavior).
    #[default]
    None,
    /// Retain partial files at the destination path on interrupt (`--partial`).
    ///
    /// The incomplete temp file is renamed to the final destination, replacing
    /// any existing file. This allows resuming the transfer on a subsequent run.
    Partial,
    /// Retain partial files in a designated directory on interrupt (`--partial-dir=DIR`).
    ///
    /// The incomplete temp file is moved to `DIR/relative_path` instead of
    /// being deleted. On subsequent transfers, the receiver checks this
    /// directory for a basis file to resume from.
    PartialDir(PathBuf),
}

/// Backup configuration for creating backup copies before overwriting.
///
/// # Upstream Reference
///
/// - `backup.c:make_backup()` - renames existing file to backup path
/// - `options.c:2854-2876` - `--backup`, `--backup-dir`, `--suffix`
#[derive(Debug, Clone)]
pub struct BackupConfig {
    /// Destination root directory for computing relative backup paths.
    pub dest_dir: PathBuf,
    /// Optional backup directory (`--backup-dir`).
    pub backup_dir: Option<PathBuf>,
    /// Backup file suffix (default `~`).
    pub suffix: OsString,
}

/// Subdirectory name used by upstream rsync for staging files when
/// `--delay-updates` is active and no explicit `--partial-dir` is given.
///
/// upstream: options.c - `static char tmp_partialdir[] = ".~tmp~";`
pub const DELAY_UPDATES_PARTIAL_DIR: &str = ".~tmp~";

/// Default SPSC channel capacity for the disk commit thread.
///
/// 128 slots x ~32 KB average chunk = 4 MB peak memory from buffered messages.
pub const DEFAULT_CHANNEL_CAPACITY: usize = 128;

/// Minimum allowed channel capacity (prevents degenerate single-slot behavior).
const MIN_CHANNEL_CAPACITY: usize = 8;

/// Maximum allowed channel capacity (prevents unbounded memory growth).
const MAX_CHANNEL_CAPACITY: usize = 4096;

/// Configuration for the disk commit thread.
#[derive(Debug, Clone)]
pub struct DiskCommitConfig {
    /// Whether to fsync files after writing.
    pub do_fsync: bool,
    /// Whether to use sparse file writing.
    pub use_sparse: bool,
    /// Whether to preallocate each destination file to its eventual length
    /// before writing (`--preallocate`). Reserves blocks up front to reduce
    /// fragmentation. upstream: receiver.c:320 do_fallocate(fd, 0, total_size).
    pub preallocate: bool,
    /// Destination tree root used to anchor SEC-1.r/SEC-1.j cross-thread
    /// `*at` syscalls. `None` when the destination root could not be opened
    /// at receiver setup or when running on a platform without the carrier.
    pub dest_dir: Option<PathBuf>,
    /// SEC-1.r parent-dirfd carrier rooted at the destination tree.
    ///
    /// `Arc` so the disk-commit thread can outlive the borrow on the
    /// receiver's `PipelineSetup::sandbox`. When `Some`, the temp-file
    /// create and the temp-file unlink-on-drop route through
    /// `openat` / `unlinkat` instead of path-based syscalls so a TOCTOU
    /// symlink swap on the temp parent cannot redirect the create or the
    /// cleanup. `None` on non-Unix targets and on Unix when the receiver
    /// could not open the destination root.
    #[cfg(unix)]
    pub sandbox: Option<Arc<fast_io::DirSandbox>>,
    /// Temporary directory for staging received files before final placement.
    /// Shared across all files in a transfer session.
    pub temp_dir: Option<PathBuf>,
    /// Shared file list for metadata application. The disk thread looks up
    /// entries by `file_entry_index` instead of receiving cloned entries
    /// per file, eliminating ~88-295 bytes of cloning per file.
    pub file_list: Option<Arc<Vec<protocol::flist::FileEntry>>>,
    /// Metadata options for applying file attributes after commit.
    /// When `Some`, the disk thread applies metadata (mtime, perms, ownership)
    /// immediately after rename - mirroring upstream `finish_transfer()` ->
    /// `set_file_attrs()` in receiver.c.
    pub metadata_opts: Option<MetadataOptions>,
    /// Backup configuration. When `Some`, existing files are renamed to a
    /// backup path before being overwritten.
    pub backup: Option<BackupConfig>,
    /// ACL cache from flist reception, shared via `Arc` for thread safety.
    /// When `Some`, the disk thread applies cached ACLs after metadata.
    pub acl_cache: Option<Arc<AclCache>>,
    /// Cross-host id remapper for named ACL entries, shared via `Arc`. Built
    /// from the received uid/gid id-lists plus `--usermap`/`--groupmap` so the
    /// disk thread remaps ACL ids like file owners.
    ///
    /// upstream: acls.c:1059-1081 `match_acl_ids()`.
    pub acl_id_map: Option<Arc<metadata::AclIdMapper>>,
    /// Compiled `x`-modifier xattr-name filter, shared via `Arc`. When `Some`,
    /// the disk thread screens each received xattr name before applying it and
    /// preserves excluded names already on the destination.
    ///
    /// upstream: `saw_xattr_filter` gate in `receive_xattr()` (xattrs.c:822)
    /// and `rsync_xal_set()` (xattrs.c:1026).
    pub xattr_filter: Option<Arc<FilterSet>>,
    /// SPSC channel capacity for the disk commit thread.
    ///
    /// Controls how many `FileMessage` items can be buffered between the
    /// network thread and the disk thread. Clamped to
    /// `MIN_CHANNEL_CAPACITY..=MAX_CHANNEL_CAPACITY` at runtime.
    ///
    /// Defaults to [`DEFAULT_CHANNEL_CAPACITY`] (128).
    pub channel_capacity: usize,
    /// Policy controlling io_uring usage for disk writes.
    ///
    /// When `Auto` (default), the disk thread attempts to create an
    /// `IoUringDiskBatch` for batched writes. On Linux 5.6+ with the
    /// `io_uring` feature enabled, this reduces syscall overhead by
    /// batching multiple writes into a single `io_uring_enter` call.
    /// Falls back to standard buffered I/O on non-Linux or older kernels.
    pub io_uring_policy: fast_io::IoUringPolicy,
    /// Optional override for the io_uring submission queue depth (`--io-uring-depth=N`).
    ///
    /// `None` keeps the upstream default
    /// ([`fast_io::IoUringConfig::sq_entries`]). `Some(n)` overrides the
    /// default with a power-of-two value previously validated via
    /// [`fast_io::validate_io_uring_depth`].
    pub io_uring_depth: Option<u32>,
    /// Policy controlling IOCP usage for disk writes on Windows.
    ///
    /// When `Auto` (default), the disk thread attempts to create an
    /// `IocpDiskBatch` for batched overlapped writes. On Windows with the
    /// `iocp` feature enabled, this submits multiple `WriteFile` calls
    /// concurrently and drains completions via
    /// `GetQueuedCompletionStatusEx`. Falls back to standard buffered
    /// I/O on non-Windows targets or when the policy is `Disabled`.
    pub iocp_policy: fast_io::IocpPolicy,
    /// Controls partial file retention on interrupted transfers.
    ///
    /// When set to [`PartialMode::Partial`] or [`PartialMode::PartialDir`],
    /// the disk thread retains temp files on shutdown/abort instead of
    /// deleting them, matching upstream rsync's `--partial` / `--partial-dir`
    /// behavior.
    ///
    /// # Upstream Reference
    ///
    /// - `cleanup.c:105-115` - `handle_partial_dir()` in cleanup path
    /// - `receiver.c:340-345` - partial file rename on interrupt
    pub partial_mode: PartialMode,
    /// When true, files are staged to a `.~tmp~` partial directory instead
    /// of being renamed to their final destination immediately. The caller
    /// must perform a final rename sweep at the phase 2 boundary.
    ///
    /// # Upstream Reference
    ///
    /// - `receiver.c:546-547`: `delayed_bits = bitbag_create()`
    /// - `receiver.c:906-929`: staging to partial dir when `delay_updates`
    /// - `receiver.c:584-585`: `handle_delayed_updates()` at phase 2
    pub delay_updates: bool,
    /// Whether `--append-verify` (append_mode == 2) is active for the session.
    ///
    /// When true and a file is transferred with a non-zero append offset, the
    /// disk thread reads the existing prefix `[0, append_offset)` and folds it
    /// into the whole-file checksum before hashing the appended tokens, so a
    /// corrupted prefix fails verification and triggers a re-transmit. Plain
    /// `--append` leaves this false and trusts the prefix.
    ///
    /// # Upstream Reference
    ///
    /// - `receiver.c:357-373` - `if (append_mode == 2 && mapbuf)` prefix `sum_update`
    pub append_verify: bool,
}

impl Default for DiskCommitConfig {
    fn default() -> Self {
        Self {
            do_fsync: false,
            use_sparse: false,
            preallocate: false,
            dest_dir: None,
            #[cfg(unix)]
            sandbox: None,
            temp_dir: None,
            file_list: None,
            metadata_opts: None,
            backup: None,
            acl_cache: None,
            acl_id_map: None,
            xattr_filter: None,
            channel_capacity: DEFAULT_CHANNEL_CAPACITY,
            io_uring_policy: fast_io::IoUringPolicy::Auto,
            io_uring_depth: None,
            iocp_policy: fast_io::IocpPolicy::Auto,
            partial_mode: PartialMode::None,
            delay_updates: false,
            append_verify: false,
        }
    }
}

impl DiskCommitConfig {
    /// Returns the effective channel capacity, clamped to valid bounds.
    ///
    /// Values below `MIN_CHANNEL_CAPACITY` are raised to the minimum;
    /// values above `MAX_CHANNEL_CAPACITY` are lowered to the maximum.
    #[must_use]
    pub fn effective_channel_capacity(&self) -> usize {
        self.channel_capacity
            .clamp(MIN_CHANNEL_CAPACITY, MAX_CHANNEL_CAPACITY)
    }
}
