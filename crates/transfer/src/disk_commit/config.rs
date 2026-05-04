//! Configuration types for the disk commit thread.
//!
//! Provides `DiskCommitConfig` and `BackupConfig` which control how the disk
//! thread writes, syncs, and commits files.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;

use metadata::MetadataOptions;
use protocol::acl::AclCache;

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
    /// SPSC channel capacity for the disk commit thread.
    ///
    /// Controls how many `FileMessage` items can be buffered between the
    /// network thread and the disk thread. Clamped to
    /// [`MIN_CHANNEL_CAPACITY`]..=[`MAX_CHANNEL_CAPACITY`] at runtime.
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
    /// Policy controlling IOCP usage for disk writes on Windows.
    ///
    /// When `Auto` (default), the disk thread attempts to create an
    /// `IocpDiskBatch` for batched overlapped writes. On Windows with the
    /// `iocp` feature enabled, this submits multiple `WriteFile` calls
    /// concurrently and drains completions via
    /// `GetQueuedCompletionStatusEx`. Falls back to standard buffered
    /// I/O on non-Windows targets or when the policy is `Disabled`.
    pub iocp_policy: fast_io::IocpPolicy,
}

impl Default for DiskCommitConfig {
    fn default() -> Self {
        Self {
            do_fsync: false,
            use_sparse: false,
            temp_dir: None,
            file_list: None,
            metadata_opts: None,
            backup: None,
            acl_cache: None,
            channel_capacity: DEFAULT_CHANNEL_CAPACITY,
            io_uring_policy: fast_io::IoUringPolicy::Auto,
            iocp_policy: fast_io::IocpPolicy::Auto,
        }
    }
}

impl DiskCommitConfig {
    /// Returns the effective channel capacity, clamped to valid bounds.
    ///
    /// Values below [`MIN_CHANNEL_CAPACITY`] are raised to the minimum;
    /// values above [`MAX_CHANNEL_CAPACITY`] are lowered to the maximum.
    #[must_use]
    pub fn effective_channel_capacity(&self) -> usize {
        self.channel_capacity
            .clamp(MIN_CHANNEL_CAPACITY, MAX_CHANNEL_CAPACITY)
    }
}
