//! Core type definitions for the [`LocalCopyOptions`] configuration surface.
//!
//! Defines the `LocalCopyOptions` struct, its defaults, and the supporting
//! enums (`DeleteTiming`, `ReferenceDirectoryKind`) and helper structs
//! (`ReferenceDirectory`, `LinkDestEntry`) used by the per-concern setter
//! and accessor submodules.

use std::ffi::OsString;
use std::num::{NonZeroU8, NonZeroU32, NonZeroU64};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use ::metadata::{ChmodModifiers, CopyAsIds, GroupMapping, ModifyWindow, UserMapping};
use compress::algorithm::CompressionAlgorithm;
use compress::zlib::CompressionLevel;
use fast_io::{DefaultPlatformCopy, PlatformCopy};
use filters::FilterSet;
use protocol::iconv::FilenameConverter;

use crate::batch::BatchWriter;
use crate::local_copy::executor::SparseDetectStrategy;
use crate::local_copy::filter_program::FilterProgram;
use crate::local_copy::skip_compress::SkipCompressList;
use crate::signature::SignatureAlgorithm;

/// Controls when deletion sweeps run relative to content transfers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeleteTiming {
    /// Remove extraneous entries before copying new content.
    Before,
    /// Remove extraneous entries as directories are processed.
    During,
    /// Record deletions during the walk and apply them after transfers finish.
    Delay,
    /// Remove extraneous entries after the full transfer completes.
    After,
}

impl DeleteTiming {
    pub(super) const fn default() -> Self {
        Self::During
    }
}

/// Identifies how a reference directory should be treated when evaluating
/// `--compare-dest`, `--copy-dest`, and `--link-dest` semantics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReferenceDirectoryKind {
    /// Skip creating the destination entry when the reference file matches.
    Compare,
    /// Copy the payload from the reference directory when the file matches.
    Copy,
    /// Create a hard link to the reference directory when the file matches.
    Link,
}

/// Reference directory consulted during copy execution.
///
/// This represents a `--compare-dest`, `--copy-dest`, or `--link-dest` directory
/// used for basis file lookup during both local and remote transfers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReferenceDirectory {
    /// The kind of reference directory operation.
    pub kind: ReferenceDirectoryKind,
    /// The path to the reference directory, as resolved for use.
    ///
    /// The daemon rewrites this in place when it confines a client-supplied
    /// basis under the module root, so it is the path to *open*, not the name
    /// the peer asked for. Anything matching against operator-written rules
    /// wants [`Self::requested`] instead.
    pub path: PathBuf,
    /// The basis path exactly as the peer or operator wrote it.
    ///
    /// Upstream never rewrites `basis_dir[]` before matching it against the
    /// daemon filter list: `main.c:1264-1270` runs `sanitize_path(clean,
    /// *dir_p, "/", 0, SP_DEFAULT)` over the *given* argument, which leaves a
    /// relative `--link-dest=secret` as `secret`. That is why upstream's own
    /// diagnostics quote `secret` and not a resolved path, and why an anchored
    /// module rule like `exclude = secret/` matches it.
    ///
    /// oc resolves the basis against the destination and confines it under the
    /// module root before the receiver sees it, so `path` by then reads
    /// `<module>/<dest>/secret` and no anchored rule can ever match. Keeping
    /// the original here preserves the only value the filter check can use.
    pub requested: PathBuf,
}

impl ReferenceDirectory {
    /// Creates a new reference directory entry.
    ///
    /// `requested` starts equal to `path`; only a later resolution step (the
    /// daemon's module-root confinement) moves them apart.
    #[must_use]
    pub fn new(kind: ReferenceDirectoryKind, path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        Self {
            kind,
            requested: path.clone(),
            path,
        }
    }

    /// Returns the basis path as the peer or operator wrote it.
    #[must_use]
    pub fn requested(&self) -> &std::path::Path {
        &self.requested
    }

    /// Returns the reference directory kind.
    #[must_use]
    pub const fn kind(&self) -> ReferenceDirectoryKind {
        self.kind
    }

    /// Returns the base directory path associated with the entry.
    #[must_use]
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// Anchors the basis on `curr_dir`, the directory the receiver writes
    /// into.
    ///
    /// upstream: `main.c:889-911` `check_alt_basis_dirs()` strips one
    /// trailing `/` and rewrites a relative `basis_dir[j]` to
    /// `curr_dir/bdir`, so every later `pathjoin(basis_dir[j], fname)` is
    /// independent of the process cwd. `dest_dir_pending` is upstream's
    /// `dry_run > 1`, a dry run whose destination directory was never
    /// created: one leading `../` is then dropped and the rest joined onto
    /// `curr_dir`'s parent, so a sibling of the missing destination still
    /// resolves as it would in a live run.
    ///
    /// Only `path` changes; [`Self::requested`] keeps what the peer wrote.
    /// Upstream skips the rewrite under `sanitize_paths`, so a daemon
    /// receiver must not call this.
    #[must_use]
    pub fn anchored_at(&self, curr_dir: &Path, dest_dir_pending: bool) -> Self {
        let basis = strip_one_trailing_separator(&self.path);
        let path = if basis.is_absolute() {
            basis
        } else {
            match (
                dest_dir_pending,
                curr_dir.parent(),
                strip_leading_parent(&basis),
            ) {
                (true, Some(parent), Some(rest)) => parent.join(rest),
                _ => curr_dir.join(&basis),
            }
        };
        Self {
            kind: self.kind,
            path,
            requested: self.requested.clone(),
        }
    }
}

/// Removes at most ONE trailing separator, and never from a bare root.
///
/// upstream: `main.c:889-891`
/// `if (bd_len > 1 && bdir[bd_len-1] == '/') bdir[--bd_len] = '\0';`
///
/// Exactly one, because the count decides *which* diagnostic fires. Measured
/// against real 3.5.0 with a plain file at an absolute path:
///
/// | arg | upstream |
/// |---|---|
/// | `<abs>/f` | `is not a dir: <abs>/f` |
/// | `<abs>/f/` | `is not a dir: <abs>/f` - stripped, so the stat lands on the file |
/// | `<abs>/f//` | `does not exist: <abs>/f/` - one survives, `stat("f/")` is ENOTDIR |
/// | `/` | silent - length 1, never stripped, and it is a directory |
///
/// A general path normalisation would collapse both separators and flip the
/// third row to `is not a dir`.
///
/// Deviation from upstream: oc accepts either platform separator where upstream
/// hardcodes `'/'`: a Windows operator writes `--link-dest=..\prev`, and
/// `is_separator` is what the rest of this module tree already uses to decide
/// the question.
fn strip_one_trailing_separator(path: &Path) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};

        let bytes = path.as_os_str().as_bytes();
        match bytes.split_last() {
            Some((last, rest))
                if !rest.is_empty() && std::path::is_separator(char::from(*last)) =>
            {
                PathBuf::from(OsString::from_vec(rest.to_vec()))
            }
            _ => path.to_path_buf(),
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};

        let units: Vec<u16> = path.as_os_str().encode_wide().collect();
        match units.split_last() {
            Some((last, rest))
                if !rest.is_empty()
                    && char::from_u32(u32::from(*last)).is_some_and(std::path::is_separator) =>
            {
                PathBuf::from(OsString::from_wide(rest))
            }
            _ => path.to_path_buf(),
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        path.to_path_buf()
    }
}

/// Returns what follows a leading `../`, or `None` when the path does not
/// start with one.
///
/// upstream: `main.c:900` `strncmp(bdir, "../", 3) == 0` - a bare `..` has no
/// separator after it and is not stripped.
fn strip_leading_parent(path: &Path) -> Option<&Path> {
    let mut components = path.components();
    if components.next() != Some(Component::ParentDir) {
        return None;
    }
    let rest = components.as_path();
    (!rest.as_os_str().is_empty()).then_some(rest)
}

/// Options that influence how a [`LocalCopyPlan`](crate::local_copy::LocalCopyPlan) is executed.
#[derive(Clone, Debug)]
pub struct LocalCopyOptions {
    pub(super) delete: bool,
    pub(super) delete_timing: DeleteTiming,
    pub(super) delete_excluded: bool,
    pub(super) delete_missing_args: bool,
    /// Mirrors the client's `--itemize-changes`/`-i` state. When set, the
    /// engine suppresses the plain `info_log!(Del,1,"deleting %n")` line so
    /// only the `*deleting` itemize record reaches output, matching upstream
    /// `log.c:log_delete` where `stdout_format_has_o_or_i` makes the itemize
    /// format and the plain "deleting %n" line mutually exclusive.
    pub(super) itemize_active: bool,
    pub(super) max_deletions: Option<u64>,
    pub(super) min_file_size: Option<u64>,
    pub(super) max_file_size: Option<u64>,
    pub(super) block_size_override: Option<NonZeroU32>,
    pub(super) remove_source_files: bool,
    pub(super) preallocate: bool,
    pub(super) fsync: bool,
    pub(super) bandwidth_limit: Option<NonZeroU64>,
    pub(super) compress: bool,
    pub(super) compression_algorithm: CompressionAlgorithm,
    pub(super) compression_level_override: Option<CompressionLevel>,
    pub(super) compression_level: CompressionLevel,
    /// Worker count for `ZSTD_c_nbWorkers`, only meaningful for zstd.
    /// upstream: `token.c:752`, `options.c:93`.
    pub(super) compression_threads: Option<NonZeroU8>,
    pub(super) adaptive_compress: bool,
    pub(super) skip_compress: SkipCompressList,
    pub(super) open_noatime: bool,
    pub(super) whole_file: Option<bool>,
    pub(super) copy_links: bool,
    pub(super) preserve_symlinks: bool,
    pub(super) copy_dirlinks: bool,
    pub(super) copy_unsafe_links: bool,
    pub(super) files_from: bool,
    pub(super) keep_dirlinks: bool,
    pub(super) safe_links: bool,
    pub(super) munge_links: bool,
    pub(super) preserve_owner: bool,
    pub(super) preserve_group: bool,
    pub(super) preserve_executability: bool,
    pub(super) preserve_permissions: bool,
    pub(super) preserve_times: bool,
    pub(super) preserve_atimes: bool,
    pub(super) preserve_crtimes: bool,
    pub(super) omit_link_times: bool,
    pub(super) owner_override: Option<u32>,
    pub(super) group_override: Option<u32>,
    pub(super) copy_as: Option<CopyAsIds>,
    pub(super) omit_dir_times: bool,
    #[cfg(all(any(unix, windows), feature = "acl"))]
    pub(super) preserve_acls: bool,
    pub(super) filters: Option<FilterSet>,
    pub(super) filter_program: Option<FilterProgram>,
    /// The session's CVS ignore list, shared with the caller's own `C` rules.
    pub(super) cvs_session: filters::CvsSession,
    /// Optional filename charset converter resolved from the user's
    /// `--iconv=LOCAL,REMOTE` request.
    ///
    /// `None` means raw bytes are passed through verbatim, matching upstream
    /// rsync's behaviour when `--iconv` is absent or `--no-iconv` is set.
    /// `Some(converter)` means file-list emit, file-list ingest, and filter
    /// matching call sites should transcode names through this converter
    /// when those producers are wired.
    ///
    /// This field is the local-copy mirror of
    /// `transfer::config::ConnectionConfig::iconv`, populated for SSH and
    /// daemon transfers via `apply_common_server_flags`. The local-copy
    /// path bypasses that bridge, so the converter is plumbed here directly
    /// from `core::client::run::build_local_copy_options`.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c::iconv_for_local` (file-list path entry transcode)
    /// - `options.c::recv_iconv_settings` (parse `--iconv=LOCAL,REMOTE`)
    pub(super) iconv: Option<FilenameConverter>,
    pub(super) numeric_ids: bool,
    pub(super) sparse: bool,
    pub(super) sparse_detect_strategy: SparseDetectStrategy,
    pub(super) checksum: bool,
    pub(super) checksum_algorithm: SignatureAlgorithm,
    pub(super) transfer_checksum_algorithm: SignatureAlgorithm,
    pub(super) size_only: bool,
    pub(super) ignore_times: bool,
    pub(super) ignore_existing: bool,
    pub(super) existing_only: bool,
    pub(super) ignore_missing_args: bool,
    pub(super) update: bool,
    pub(super) modify_window: ModifyWindow,
    pub(super) partial: bool,
    pub(super) partial_dir: Option<PathBuf>,
    pub(super) temp_dir: Option<PathBuf>,
    pub(super) delay_updates: bool,
    pub(super) inplace: bool,
    pub(super) write_devices: bool,
    pub(super) append: bool,
    pub(super) append_verify: bool,
    pub(super) collect_events: bool,
    /// Mirrors the client's `--list-only` state. When set, the engine runs in
    /// dry-run mode but lists every enumerated entry - including symlinks,
    /// devices, FIFOs, and sockets - instead of skipping non-regular files.
    ///
    /// upstream: generator.c:1318 `list_file_entry()` lists every flist entry
    /// regardless of `--links`/`--devices`/`--specials` because list-only mode
    /// never runs the generator's non-regular skip path (main.c:721
    /// `get_local_name()` returns NULL for `list_only`).
    pub(super) list_only: bool,
    pub(super) preserve_hard_links: bool,
    pub(super) relative_paths: bool,
    pub(super) one_file_system: u8,
    pub(super) recursive: bool,
    pub(super) dirs: bool,
    pub(super) devices: bool,
    pub(super) copy_devices_as_files: bool,
    pub(super) specials: bool,
    /// `--drop-D`: refuse to CREATE devices and specials.
    pub(super) drop_devices: bool,
    pub(super) force_replacements: bool,
    pub(super) implied_dirs: bool,
    pub(super) mkpath: bool,
    /// Fuzzy basis matching level (0 = off, 1 = `--fuzzy`, 2 = `-yy`).
    ///
    /// When non-zero and the exact destination file is absent, the local-copy
    /// executor scans the destination directory for a similarly-named file to
    /// use as the delta basis. Mirrors upstream `fuzzy_basis` in `options.c`.
    pub(super) fuzzy_level: u8,
    pub(super) prune_empty_dirs: bool,
    pub(super) timeout: Option<Duration>,
    pub(super) contimeout: Option<Duration>,
    pub(super) stop_at: Option<SystemTime>,
    #[cfg(all(any(unix, windows), feature = "xattr"))]
    pub(super) preserve_xattrs: bool,
    #[cfg(all(unix, feature = "xattr"))]
    pub(super) preserve_nfsv4_acls: bool,
    pub(super) backup: bool,
    pub(super) backup_dir: Option<PathBuf>,
    pub(super) backup_suffix: OsString,
    pub(super) link_dests: Vec<LinkDestEntry>,
    pub(super) reference_directories: Vec<ReferenceDirectory>,
    pub(super) chmod: Option<ChmodModifiers>,
    pub(super) user_mapping: Option<UserMapping>,
    pub(super) group_mapping: Option<GroupMapping>,
    pub(super) batch_writer: Option<Arc<Mutex<BatchWriter>>>,
    /// When `Some(true)`, the receiving side attempts super-user activities
    /// (preserving ownership, devices, specials) even when not running as root.
    /// When `Some(false)`, explicitly disables super-user attempts.
    /// When `None`, defers to the default (check effective UID).
    pub(super) super_mode: Option<bool>,
    /// When enabled, stores/restores privileged metadata via xattrs instead
    /// of actually requiring root privileges.
    pub(super) fake_super: bool,
    /// When enabled, `--delete` proceeds even when I/O errors occurred during
    /// the transfer. Without this flag, deletions are suppressed when any I/O
    /// errors are recorded, preventing data loss when the sender could not read
    /// all files.
    pub(super) ignore_errors: bool,
    /// Optional file path for logging transfer activity.
    pub(super) log_file: Option<PathBuf>,
    /// Optional format string for per-item log entries.
    pub(super) log_file_format: Option<String>,
    /// Platform-abstracted file copy strategy used by whole-file copy paths.
    ///
    /// Defaults to [`DefaultPlatformCopy`], which selects the best available
    /// mechanism (FICLONE/copy_file_range on Linux, clonefile/fcopyfile on
    /// macOS, ReFS reflink/CopyFileExW on Windows) with portable fallback.
    /// Tests can inject a fake implementation to verify dispatch.
    pub(super) platform_copy: Arc<dyn PlatformCopy>,
    /// I/O-level zero-copy policy for the file *content* path, mirroring
    /// `--zero-copy` / `--no-zero-copy`.
    ///
    /// Distinct from [`Self::platform_copy`], which selects the filesystem
    /// clone/reflink tier. This one governs the kernel-side content movers
    /// (io_uring, `copy_file_range`) that run when no clone tier applied, and
    /// [`fast_io::ZeroCopyPolicy::Disabled`] routes the copy through the
    /// portable read/write loop instead.
    pub(super) zero_copy: fast_io::ZeroCopyPolicy,
}

impl LocalCopyOptions {
    /// Creates a new [`LocalCopyOptions`] value with defaults applied.
    #[must_use]
    pub fn new() -> Self {
        Self {
            delete: false,
            delete_timing: DeleteTiming::default(),
            delete_excluded: false,
            delete_missing_args: false,
            itemize_active: false,
            max_deletions: None,
            min_file_size: None,
            max_file_size: None,
            block_size_override: None,
            remove_source_files: false,
            preallocate: false,
            fsync: false,
            bandwidth_limit: None,
            compress: false,
            compression_algorithm: CompressionAlgorithm::default_algorithm(),
            compression_level_override: None,
            compression_level: CompressionLevel::Default,
            compression_threads: None,
            adaptive_compress: false,
            skip_compress: SkipCompressList::default(),
            open_noatime: false,
            whole_file: None,
            copy_links: false,
            preserve_symlinks: false,
            copy_dirlinks: false,
            copy_unsafe_links: false,
            files_from: false,
            keep_dirlinks: false,
            safe_links: false,
            munge_links: false,
            preserve_owner: false,
            preserve_group: false,
            preserve_executability: false,
            preserve_permissions: false,
            preserve_times: false,
            preserve_atimes: false,
            preserve_crtimes: false,
            owner_override: None,
            group_override: None,
            copy_as: None,
            omit_dir_times: false,
            omit_link_times: false,
            #[cfg(all(any(unix, windows), feature = "acl"))]
            preserve_acls: false,
            filters: None,
            filter_program: None,
            cvs_session: filters::CvsSession::default(),
            iconv: None,
            numeric_ids: false,
            sparse: false,
            sparse_detect_strategy: SparseDetectStrategy::Auto,
            checksum: false,
            checksum_algorithm: SignatureAlgorithm::Xxh3_128 { seed: 0 },
            transfer_checksum_algorithm: SignatureAlgorithm::Xxh3_128 { seed: 0 },
            size_only: false,
            ignore_times: false,
            ignore_existing: false,
            existing_only: false,
            ignore_missing_args: false,
            update: false,
            modify_window: ModifyWindow::ZERO,
            partial: false,
            partial_dir: None,
            temp_dir: None,
            delay_updates: false,
            inplace: false,
            write_devices: false,
            append: false,
            append_verify: false,
            collect_events: false,
            list_only: false,
            preserve_hard_links: false,
            relative_paths: false,
            one_file_system: 0,
            recursive: true,
            dirs: false,
            devices: false,
            copy_devices_as_files: false,
            specials: false,
            drop_devices: false,
            force_replacements: false,
            implied_dirs: true,
            mkpath: false,
            fuzzy_level: 0,
            prune_empty_dirs: false,
            timeout: None,
            contimeout: None,
            stop_at: None,
            #[cfg(all(any(unix, windows), feature = "xattr"))]
            preserve_xattrs: false,
            #[cfg(all(unix, feature = "xattr"))]
            preserve_nfsv4_acls: false,
            backup: false,
            backup_dir: None,
            backup_suffix: OsString::from("~"),
            link_dests: Vec::new(),
            reference_directories: Vec::new(),
            chmod: None,
            user_mapping: None,
            group_mapping: None,
            batch_writer: None,
            super_mode: None,
            fake_super: false,
            ignore_errors: false,
            log_file: None,
            log_file_format: None,
            platform_copy: Arc::new(DefaultPlatformCopy::new()),
            zero_copy: fast_io::ZeroCopyPolicy::Auto,
        }
    }
}

impl Default for LocalCopyOptions {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct LinkDestEntry {
    pub(super) path: PathBuf,
    pub(super) is_relative: bool,
}

#[cfg(test)]
mod tests {
    mod anchored_at {
        use super::super::*;

        fn anchor(basis: &str, curr_dir: &str, pending: bool) -> PathBuf {
            ReferenceDirectory::new(ReferenceDirectoryKind::Link, basis)
                .anchored_at(Path::new(curr_dir), pending)
                .path
        }

        #[test]
        fn a_relative_basis_joins_the_destination_not_the_cwd() {
            assert_eq!(
                anchor("../alt", "/x/dst", false),
                PathBuf::from("/x/dst/../alt")
            );
            assert_eq!(anchor("sub", "/x/dst", false), PathBuf::from("/x/dst/sub"));
        }

        #[test]
        fn an_absolute_basis_is_kept_apart_from_one_trailing_separator() {
            assert_eq!(
                anchor("/srv/alt/", "/x/dst", false),
                PathBuf::from("/srv/alt")
            );
            assert_eq!(
                anchor("/srv/alt//", "/x/dst", false),
                PathBuf::from("/srv/alt/")
            );
            assert_eq!(anchor("/", "/x/dst", true), PathBuf::from("/"));
        }

        /// upstream: main.c:900-906 - only `dry_run > 1` drops a leading
        /// `../`, and only one of them, so the result names the same
        /// directory a live run would reach through the created destination.
        #[test]
        fn a_pending_destination_drops_exactly_one_leading_parent() {
            assert_eq!(anchor("../alt", "/x/dst", true), PathBuf::from("/x/alt"));
            assert_eq!(
                anchor("../../alt", "/x/dst", true),
                PathBuf::from("/x/../alt")
            );
            assert_eq!(anchor("sub", "/x/dst", true), PathBuf::from("/x/dst/sub"));
            assert_eq!(anchor("..", "/x/dst", true), PathBuf::from("/x/dst/.."));
        }

        #[test]
        fn the_requested_spelling_survives_the_rewrite() {
            let anchored = ReferenceDirectory::new(ReferenceDirectoryKind::Copy, "../alt/")
                .anchored_at(Path::new("/x/dst"), false);
            assert_eq!(anchored.requested(), Path::new("../alt/"));
            assert_eq!(anchored.kind(), ReferenceDirectoryKind::Copy);
        }
    }

    use super::*;

    #[test]
    fn delete_timing_default_is_during() {
        assert_eq!(DeleteTiming::default(), DeleteTiming::During);
    }

    #[test]
    fn delete_timing_eq() {
        assert_eq!(DeleteTiming::Before, DeleteTiming::Before);
        assert_eq!(DeleteTiming::During, DeleteTiming::During);
        assert_eq!(DeleteTiming::Delay, DeleteTiming::Delay);
        assert_eq!(DeleteTiming::After, DeleteTiming::After);
        assert_ne!(DeleteTiming::Before, DeleteTiming::After);
    }

    #[test]
    fn reference_directory_kind_eq() {
        assert_eq!(
            ReferenceDirectoryKind::Compare,
            ReferenceDirectoryKind::Compare
        );
        assert_eq!(ReferenceDirectoryKind::Copy, ReferenceDirectoryKind::Copy);
        assert_eq!(ReferenceDirectoryKind::Link, ReferenceDirectoryKind::Link);
        assert_ne!(
            ReferenceDirectoryKind::Compare,
            ReferenceDirectoryKind::Link
        );
    }

    #[test]
    fn reference_directory_new_creates_entry() {
        let dir = ReferenceDirectory::new(ReferenceDirectoryKind::Compare, "/tmp/ref");
        assert_eq!(dir.kind(), ReferenceDirectoryKind::Compare);
        assert_eq!(dir.path().to_str().unwrap(), "/tmp/ref");
    }

    #[test]
    fn reference_directory_new_with_path_buf() {
        let path = PathBuf::from("/tmp/ref");
        let dir = ReferenceDirectory::new(ReferenceDirectoryKind::Copy, path);
        assert_eq!(dir.kind(), ReferenceDirectoryKind::Copy);
    }

    #[test]
    fn reference_directory_clone() {
        let dir = ReferenceDirectory::new(ReferenceDirectoryKind::Link, "/tmp/link");
        let cloned = dir.clone();
        assert_eq!(dir, cloned);
    }

    #[test]
    fn local_copy_options_new_has_defaults() {
        let opts = LocalCopyOptions::new();
        assert!(!opts.delete);
        assert_eq!(opts.delete_timing, DeleteTiming::During);
        assert!(!opts.delete_excluded);
        assert!(opts.max_deletions.is_none());
        assert!(!opts.preallocate);
        assert!(!opts.fsync);
        assert!(opts.whole_file.is_none());
        assert!(opts.recursive);
    }

    #[test]
    fn local_copy_options_default_same_as_new() {
        let new_opts = LocalCopyOptions::new();
        let default_opts = LocalCopyOptions::default();
        assert_eq!(new_opts.delete, default_opts.delete);
        assert_eq!(new_opts.recursive, default_opts.recursive);
    }

    #[test]
    fn local_copy_options_clone() {
        let opts = LocalCopyOptions::new();
        let cloned = opts.clone();
        assert_eq!(opts.delete, cloned.delete);
        assert_eq!(opts.recursive, cloned.recursive);
    }
}
