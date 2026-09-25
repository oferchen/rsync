//! Metadata application orchestration.
//!
//! Re-exports the public API for applying ownership, permissions, and
//! timestamps to files, directories, and symbolic links. Internal logic
//! is split across focused submodules following the single-responsibility
//! principle.

mod ownership;
mod permissions;
mod platform_warn;
mod timestamps;

pub use ownership::group_is_settable;
#[cfg(unix)]
pub use permissions::init_orig_umask;

#[cfg(test)]
mod tests;

use crate::error::MetadataError;
use crate::modify_window::ModifyWindow;
use crate::options::{AttrsFlags, MetadataOptions};
use std::fs;
#[cfg(unix)]
use std::os::fd::BorrowedFd;
use std::path::Path;

/// A borrowed handle to a destination's parent directory, resolved once and
/// shared across the ownership/timestamp/permission appliers so the hardened
/// `secure_open_dir` parent walk runs once per file instead of once per
/// attribute.
///
/// `None` means "no shared parent available" - the applier keeps its existing
/// per-attribute behaviour (its own walk, or the `--keep-dirlinks` / AT_FDCWD
/// path). On non-Unix targets file descriptors do not exist, so the alias
/// degenerates to a shared unit reference the stubs ignore.
#[cfg(unix)]
pub(crate) type ParentDirFd<'a> = Option<BorrowedFd<'a>>;
/// Non-Unix placeholder; see the Unix definition above.
#[cfg(not(unix))]
pub(crate) type ParentDirFd<'a> = Option<&'a ()>;

/// How a path-based applier resolves the destination's parent directory.
// Only the Unix appliers resolve a parent dirfd; elsewhere the value is
// threaded through and ignored.
#[cfg_attr(not(unix), allow(dead_code))]
#[derive(Clone, Copy, Debug)]
pub(crate) enum ParentWalk<'a> {
    /// `--keep-dirlinks`: resolve through `AT_FDCWD`, following dest-side
    /// symlinked directories like upstream `generator.c:1356`'s `link_stat`.
    Follow,
    /// Anchor the op on a confined parent dirfd (see [`confined_parent`]).
    ///
    /// `Some(root)` is the operator-named destination root. The path up to and
    /// including it is the operator's, so a symlink the operator put there is
    /// followed; only the part below the root is confined. `None` confines the
    /// whole path.
    Confined(Option<&'a crate::DestinationRoot>),
}

impl<'a> ParentWalk<'a> {
    /// Whether the walk follows symlinked parents through the ambient namespace.
    #[cfg(unix)]
    pub(crate) const fn follows(self) -> bool {
        matches!(self, Self::Follow)
    }

    /// The operator-named destination root, when known and confining.
    #[cfg(unix)]
    pub(crate) const fn root(self) -> Option<&'a crate::DestinationRoot> {
        match self {
            Self::Follow => None,
            Self::Confined(root) => root,
        }
    }
}

/// Opens `destination`'s parent directory for an anchored `*at` metadata op.
///
/// Returns `Ok(None)` when `destination` has no parent component, so the
/// caller issues the op on the bare name.
///
/// Upstream enters the destination operand once with a plain `change_dir()`
/// (`main.c` `get_local_name()`) and resolves each entry's parent relative to
/// that cwd through `secure_relative_open(NULL, dirpath, ...)`
/// (`syscall.c:1245` `do_lchown_at()`, likewise `do_chmod_at()` and the
/// utimes wrapper). So the confinement covers only the names below the root;
/// the operator's own path is never re-walked. oc keeps absolute paths, and
/// walking the fused path with `RESOLVE_NO_SYMLINKS` refuses a symlink the
/// operator put in the destination path (`base/link/inner/` with
/// `link -> real`), aborting every directory, symlink and special-file
/// metadata apply with `ENOTDIR`.
///
/// - Below `root`: [`fast_io::open_dir_beneath_nofollow`] on the root's pinned
///   descriptor (see [`crate::DestinationRoot`]) - the remainder refuses every
///   symlink.
/// - The root entry itself: its parent is entirely operator path, opened
///   through [`fast_io::operator_open_dir`].
/// - No root, or a destination outside it: the strict
///   [`fast_io::secure_open_dir`] walk over the whole parent.
#[cfg(unix)]
pub(crate) fn confined_parent(
    destination: &Path,
    root: Option<&crate::DestinationRoot>,
) -> std::io::Result<Option<std::os::fd::OwnedFd>> {
    let Some(parent) = destination.parent().filter(|p| !p.as_os_str().is_empty()) else {
        return Ok(None);
    };
    if let Some(root) = root {
        if let Ok(tail) = parent.strip_prefix(root.path()) {
            return fast_io::open_dir_beneath_nofollow(root.anchor()?, tail).map(Some);
        }
        if destination
            .strip_prefix(root.path())
            .is_ok_and(|tail| tail.as_os_str().is_empty())
        {
            return fast_io::operator_open_dir(parent).map(Some);
        }
    }
    fast_io::secure_open_dir(parent).map(Some)
}

/// Runs a metadata op anchored on `destination`'s parent dirfd.
///
/// `shared` is a parent the caller already resolved (the receiver applying
/// several attributes to one file); otherwise the parent is resolved here per
/// `root` (see [`confined_parent`]). `at` receives the parent dirfd and the
/// leaf name. `bare` handles a destination with no parent component or leaf.
#[cfg(unix)]
pub(crate) fn at_confined_parent(
    destination: &Path,
    shared: ParentDirFd<'_>,
    root: Option<&crate::DestinationRoot>,
    at: impl FnOnce(BorrowedFd<'_>, &std::ffi::OsStr) -> std::io::Result<()>,
    bare: impl FnOnce() -> std::io::Result<()>,
) -> std::io::Result<()> {
    use std::os::fd::AsFd;

    let owned;
    let parent = match shared {
        Some(parent) => Some(parent),
        None => {
            owned = confined_parent(destination, root)?;
            owned.as_ref().map(AsFd::as_fd)
        }
    };
    match (parent, destination.file_name()) {
        (Some(parent), Some(leaf)) => at(parent, leaf),
        _ => bare(),
    }
}

/// Applies metadata from `metadata` to the destination directory.
///
/// Preserves permission bits (best-effort on non-Unix targets) and
/// nanosecond timestamps. Delegates to [`apply_directory_metadata_with_options`]
/// with default options (all preservation flags enabled).
/// upstream: rsync.c:set_file_attrs() - directory metadata application
pub fn apply_directory_metadata(
    destination: &Path,
    metadata: &fs::Metadata,
) -> Result<(), MetadataError> {
    apply_directory_metadata_with_options(destination, metadata, MetadataOptions::default(), None)
}

/// Applies metadata from `metadata` to the destination directory using explicit options.
///
/// Applies ownership, timestamps, and permissions in the same order as
/// upstream rsync's `set_file_attrs()`: chown, then utimensat, with chmod
/// last.
///
/// `pre_transfer_meta` is the directory's stat from BEFORE this transfer
/// materialised it - `dest_mode()`'s `stat_mode`/`exists` inputs
/// (generator.c:1856 judges `exists` by the pre-mkdir `statret`). `None`
/// means the directory is new to this transfer, so a `--chmod` without
/// `--perms` takes the fresh-destination arm instead of rewriting bits an
/// existing directory would keep.
/// upstream: rsync.c:set_file_attrs() - order: chown (rsync.c:663) →
/// utimensat (rsync.c:769-791) → chmod (rsync.c:806-822), so a mode that
/// strips write permission can never block the times that follow it.
pub fn apply_directory_metadata_with_options(
    destination: &Path,
    metadata: &fs::Metadata,
    options: MetadataOptions,
    pre_transfer_meta: Option<&fs::Metadata>,
) -> Result<(), MetadataError> {
    ownership::set_owner_like(metadata, destination, true, &options, None)?;
    if options.times() {
        timestamps::set_timestamp_like(metadata, destination, true, None, Some(&options))?;
    }
    // upstream: rsync.c:589 - directories skip atime (`ATTRS_SKIP_ATIME`)
    // regardless of `--atimes`; only files get atime preservation.
    //
    // crtime is NOT subject to that rule. upstream: rsync.c:726-730 - the
    // `S_ISDIR` test that forces `ATTRS_SKIP_ATIME` belongs to atime alone;
    // the only directory that skips `ATTRS_SKIP_CRTIME` is the root folder of
    // an HFS+ volume. So a directory's creation time is preserved exactly as a
    // file's is, and omitting it here left every transferred directory
    // carrying a creation time derived from its own mtime.
    if options.crtimes() && !is_volume_root(destination) {
        timestamps::apply_crtime_from_source_metadata(destination, metadata)?;
    }
    permissions::apply_permissions_with_chmod(destination, metadata, &options, pre_transfer_meta)?;
    Ok(())
}

/// Reports whether `destination` is the root folder of an HFS+ volume, which
/// upstream refuses to stamp with a creation time.
///
/// upstream: rsync.c:728-730 - `if (sxp->st.st_ino == 2 && S_ISDIR(sxp->st.st_mode))`.
/// `sxp` is the *destination's* stat, so this consults the destination rather
/// than the source metadata the caller already holds. The extra stat is only
/// issued under `--crtimes`, and a stat failure answers "not a volume root" so
/// an unreadable destination degrades to attempting the set - the same
/// direction upstream takes when it cannot prove the special case.
#[cfg(unix)]
fn is_volume_root(destination: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;

    /// Inode number of an HFS+ volume root.
    const HFS_VOLUME_ROOT_INO: u64 = 2;

    fs::symlink_metadata(destination).is_ok_and(|meta| meta.ino() == HFS_VOLUME_ROOT_INO)
}

/// Non-Unix targets have no HFS+ volume-root inode convention, so no directory
/// is exempt from the creation-time set.
#[cfg(not(unix))]
fn is_volume_root(_destination: &Path) -> bool {
    false
}

/// Applies metadata from `metadata` to the destination file.
///
/// Preserves permission bits (best-effort on non-Unix targets) and
/// nanosecond timestamps. Delegates to [`apply_file_metadata_with_options`]
/// with default options (all preservation flags enabled).
/// upstream: rsync.c:set_file_attrs() - file metadata application
pub fn apply_file_metadata(
    destination: &Path,
    metadata: &fs::Metadata,
) -> Result<(), MetadataError> {
    apply_file_metadata_with_options(destination, metadata, &MetadataOptions::default())
}

/// Applies file metadata using explicit [`MetadataOptions`].
///
/// Applies ownership, timestamps, creation time, and permissions in the
/// same order as upstream rsync's `set_file_attrs()`.
/// upstream: rsync.c:set_file_attrs() - order: chown (rsync.c:663) →
/// utimensat (rsync.c:769-791) → crtime (rsync.c:751-767) → chmod
/// (rsync.c:806-822); the chmod comes last so a mode that strips write
/// permission can never block the times that precede it.
pub fn apply_file_metadata_with_options(
    destination: &Path,
    metadata: &fs::Metadata,
    options: &MetadataOptions,
) -> Result<(), MetadataError> {
    ownership::set_owner_like(metadata, destination, true, options, None)?;
    // upstream: rsync.c:587-612 - mtime and atime are handled independently
    if options.times() {
        timestamps::set_timestamp_like(metadata, destination, true, None, Some(options))?;
    } else if options.atimes() {
        // upstream: rsync.c:604-612 - atime applied when SKIP_MTIME but not SKIP_ATIME
        timestamps::apply_atime_only_from_metadata(
            metadata,
            destination,
            None,
            options.parent_walk(),
        )?;
    }
    if options.crtimes() {
        timestamps::apply_crtime_from_source_metadata(destination, metadata)?;
    }
    permissions::apply_permissions_with_chmod(destination, metadata, options, None)?;
    Ok(())
}

/// Computes the permission bits a directory ends up with BEFORE upstream's
/// during-transfer owner-`rwx` raise - the `--chmod` tweak composed with the
/// `dest_mode()` collapse when `!preserve_perms`.
///
/// `pre_transfer` is the directory's stat from BEFORE the transfer
/// materialised it (`None` for a fresh directory). The local-copy executor and
/// the network receiver both derive the during-transfer raise
/// (generator.c:1904-1912) and the `touch_up_dirs` restore (generator.c:2594)
/// from this one target value, so the two paths cannot drift.
///
/// See `permissions::directory_dest_mode` for the full upstream mapping.
#[cfg(unix)]
pub fn directory_dest_mode(
    destination: &Path,
    source_mode: u32,
    options: &MetadataOptions,
    pre_transfer: Option<&fs::Metadata>,
) -> u32 {
    permissions::directory_dest_mode(destination, source_mode, options, pre_transfer)
}

/// Pre-applies upstream's `dest_mode()` chmod for callers that have the
/// pre-transfer destination stat in hand.
///
/// See `permissions::apply_dest_mode_pre_transfer` for the full
/// upstream-reference documentation.
#[cfg(unix)]
pub fn apply_dest_mode_pre_transfer(
    destination: &Path,
    source_metadata: &fs::Metadata,
    options: &MetadataOptions,
    pre_transfer_meta: Option<&fs::Metadata>,
) -> Result<(), MetadataError> {
    permissions::apply_dest_mode_pre_transfer(
        destination,
        source_metadata,
        options,
        pre_transfer_meta,
    )
}

/// Reports whether a transfer-root directory self-locks under the configured
/// `--chmod` modifiers, returning the tweaked permission bits alongside the
/// verdict, or `None` when no `--chmod` is configured.
///
/// `am_root` is sampled through the same libc `geteuid` the chmod apply path
/// uses, so the self-lock decision and the on-disk fixup agree under
/// `fakeroot`. See [`crate::transfer_root_self_locks`] for the mechanism.
/// upstream: rsync.c:set_file_attrs() new_mode + generator.c:1904-1912 fixup.
#[cfg(unix)]
pub fn transfer_root_chmod_self_lock(
    destination: &Path,
    metadata: &fs::Metadata,
    options: &MetadataOptions,
    existing: Option<&fs::Metadata>,
) -> Result<Option<(u32, bool)>, MetadataError> {
    let Some(tweaked) =
        permissions::chmod_directory_target_mode(destination, metadata, options, existing)?
    else {
        return Ok(None);
    };
    let running_as_root = crate::identity::is_root();
    Ok(Some((
        tweaked,
        crate::transfer_root_self_locks(tweaked, running_as_root),
    )))
}

/// Applies file metadata using an open file descriptor for efficiency.
///
/// When an fd is available (e.g. after writing a file), this avoids redundant
/// path lookups by using `fchmod`/`fchown`/`futimens` instead of their
/// path-based equivalents. Falls back to path-based operations where fd-based
/// variants are unavailable (e.g. chmod modifiers that need a fresh stat).
#[cfg(unix)]
pub fn apply_file_metadata_with_fd(
    destination: &Path,
    metadata: &fs::Metadata,
    options: &MetadataOptions,
    fd: BorrowedFd<'_>,
) -> Result<(), MetadataError> {
    ownership::set_owner_like_with_fd(metadata, destination, options, fd, None)?;
    // upstream: rsync.c:587-612 - mtime and atime are handled independently
    if options.times() {
        timestamps::set_timestamp_with_fd(metadata, destination, fd, None, Some(options))?;
    } else if options.atimes() {
        timestamps::apply_atime_only_from_metadata_with_fd(metadata, destination, fd, None)?;
    }
    // crtime is always path-based (setattrlist on macOS) - no fd variant exists
    if options.crtimes() {
        timestamps::apply_crtime_from_source_metadata(destination, metadata)?;
    }
    // upstream: rsync.c:806-822 - the chmod runs after the times (rsync.c:769-791)
    permissions::apply_permissions_with_chmod_fd(destination, metadata, options, Some(fd), None)?;
    Ok(())
}

/// Applies only the file metadata fields that differ from `existing`.
///
/// Compares each metadata field (ownership, permissions, timestamps) against
/// the destination's current state and skips syscalls for values that already
/// match. This eliminates redundant `chown`/`chmod`/`utimensat` calls on the
/// no-change transfer path.
pub fn apply_file_metadata_if_changed(
    destination: &Path,
    metadata: &fs::Metadata,
    existing: &fs::Metadata,
    options: &MetadataOptions,
) -> Result<(), MetadataError> {
    let restat_after_chown =
        ownership::set_owner_like(metadata, destination, true, options, Some(existing))?;
    // upstream: rsync.c:564-567 - the chown may have cleared setuid/setgid bits,
    // so re-stat before the chmod compare so they get re-applied.
    let refreshed;
    let existing = if restat_after_chown {
        refreshed = fs::metadata(destination).map_err(|error| {
            MetadataError::new("inspect destination permissions", destination, error)
        })?;
        &refreshed
    } else {
        existing
    };
    // upstream: rsync.c:587-612 - mtime and atime are handled independently
    if options.times() {
        timestamps::set_timestamp_like(metadata, destination, true, Some(existing), Some(options))?;
    } else if options.atimes() {
        timestamps::apply_atime_only_from_metadata(
            metadata,
            destination,
            Some(existing),
            options.parent_walk(),
        )?;
    }
    if options.crtimes() {
        timestamps::apply_crtime_from_source_metadata(destination, metadata)?;
    }
    // upstream: rsync.c:806-822 - the chmod runs after the times (rsync.c:769-791)
    permissions::apply_permissions_with_chmod(destination, metadata, options, Some(existing))?;
    Ok(())
}

/// fd-based variant of [`apply_file_metadata_if_changed`].
///
/// Combines fd-based syscalls with comparison guards - only issues
/// `fchown`/`fchmod`/`futimens` when the value actually differs from
/// `existing`.
#[cfg(unix)]
pub fn apply_file_metadata_with_fd_if_changed(
    destination: &Path,
    metadata: &fs::Metadata,
    existing: &fs::Metadata,
    options: &MetadataOptions,
    fd: BorrowedFd<'_>,
) -> Result<(), MetadataError> {
    let restat_after_chown =
        ownership::set_owner_like_with_fd(metadata, destination, options, fd, Some(existing))?;
    // upstream: rsync.c:564-567 - the chown may have cleared setuid/setgid bits,
    // so re-stat before the chmod compare so they get re-applied.
    let refreshed;
    let existing = if restat_after_chown {
        refreshed = fs::metadata(destination).map_err(|error| {
            MetadataError::new("inspect destination permissions", destination, error)
        })?;
        &refreshed
    } else {
        existing
    };
    // upstream: rsync.c:587-612 - mtime and atime are handled independently
    if options.times() {
        timestamps::set_timestamp_with_fd(
            metadata,
            destination,
            fd,
            Some(existing),
            Some(options),
        )?;
    } else if options.atimes() {
        timestamps::apply_atime_only_from_metadata_with_fd(
            metadata,
            destination,
            fd,
            Some(existing),
        )?;
    }
    // crtime is always path-based (setattrlist on macOS) - no fd variant exists
    if options.crtimes() {
        timestamps::apply_crtime_from_source_metadata(destination, metadata)?;
    }
    // upstream: rsync.c:806-822 - the chmod runs after the times (rsync.c:769-791)
    permissions::apply_permissions_with_chmod_fd(
        destination,
        metadata,
        options,
        Some(fd),
        Some(existing),
    )?;
    Ok(())
}

/// Returns `true` when the destination's Windows read-only attribute differs
/// from what the sender's mode requires, so the quick-check must treat the file
/// as changed and let the apply path re-stamp the bit.
///
/// Windows preserves only the read-only attribute; the apply path
/// ([`permissions::set_permissions_like`] and `apply_permissions_from_entry`)
/// derives it from the POSIX owner-write bit, so a mode with `0o200` clear is
/// read-only. This collapses upstream's full-mode `perms_differ()`
/// (generator.c:418) to a single read-only-bit compare, mirroring the Unix
/// arm's permission leg. Kept as a pure, platform-independent helper so it is
/// unit-testable on Linux even though it only gates the `cfg(not(unix))` path.
/// upstream: generator.c:418 perms_differ() - the mode compare behind
/// unchanged_attrs()'s `if perms_differ(...)` leg.
#[cfg(any(not(unix), test))]
pub(crate) fn windows_readonly_differs(entry_permissions: u32, current_readonly: bool) -> bool {
    let required_readonly = entry_permissions & 0o200 == 0;
    required_readonly != current_readonly
}

/// Fast check whether all metadata attributes already match the destination.
///
/// Mirrors upstream `generator.c:468 unchanged_attrs()` - a pure in-memory
/// comparison that avoids the function-call overhead of the full
/// [`apply_metadata_with_cached_stat`] path. Returns `true` when every
/// preserved attribute (permissions, ownership, timestamps) matches the
/// cached stat, so the caller can skip the metadata-application chain
/// entirely on the no-change quick-check path.
///
/// # Upstream Reference
///
/// - `generator.c:468-509` - `unchanged_attrs()` checks `perms_differ`,
///   `ownership_differs`, `any_time_differs`, `acls_differ`, `xattrs_differ`
/// - `generator.c:1822-1827` - quick-check match calls `set_file_attrs` only
///   when `unchanged_attrs` would fail (implicit - upstream always calls
///   `set_file_attrs` but its internal guards skip every syscall when nothing
///   differs)
///
/// `modify_window` carries the `--modify-window` tolerance so the mtime leg
/// matches upstream `mtime_differs()` -> `same_time()` (util1.c:1573) exactly:
/// the default zero window keeps whole-second equality, a positive window
/// tolerates that many seconds of drift, and a negative window compares
/// nanoseconds too. Passing the same `ModifyWindow` the quick-check used keeps
/// the "content matches" and "attributes match" verdicts consistent.
#[inline]
pub fn metadata_unchanged(
    entry: &protocol::flist::FileEntry,
    options: &MetadataOptions,
    cached_meta: &fs::Metadata,
    modify_window: ModifyWindow,
) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        // upstream: generator.c:494-495 - perms_differ(file, sxp)
        if options.permissions() && (cached_meta.mode() & 0o7777) != (entry.permissions() & 0o7777)
        {
            return false;
        }

        // upstream: generator.c:418-426 perms_differ() - without --perms,
        // --executability compares only executability presence. dest_mode()
        // (rsync.c:449-473) tweaks x-bits solely for regular files, so
        // non-regular entries never differ on this leg.
        if !options.permissions()
            && options.executability()
            && entry.file_type().is_regular()
            && (cached_meta.mode() & 0o111 != 0) != (entry.permissions() & 0o111 != 0)
        {
            return false;
        }

        // upstream: generator.c:496-497 - ownership_differs(file, sxp)
        if options.owner()
            && let Some(uid) = entry.uid()
            && cached_meta.uid() != uid
        {
            return false;
        }
        if options.group()
            && let Some(gid) = entry.gid()
            && cached_meta.gid() != gid
        {
            return false;
        }

        // upstream: generator.c:492-493 - any_time_differs(sxp, file, fname)
        // -> mtime_differs() -> same_time(), so the `--modify-window` tolerance
        // governs this leg. Pass the sub-second component from both sides; a
        // negative window compares it (util1.c:1577) while the default zero
        // window reduces to whole-second equality.
        if options.times()
            && !modify_window.same_time(
                cached_meta.mtime(),
                cached_meta.mtime_nsec() as u32,
                entry.mtime(),
                entry.mtime_nsec(),
            )
        {
            return false;
        }

        // upstream: rsync.c unchanged_attrs - atime comparison uses seconds only
        if options.atimes()
            && entry.atime() != 0
            && (cached_meta.atime() != entry.atime() || cached_meta.atime_nsec() != 0)
        {
            return false;
        }
    }

    #[cfg(not(unix))]
    {
        // upstream: generator.c:494-495 perms_differ(). Windows preserves only
        // the read-only attribute, so the full-mode compare collapses to a
        // single read-only-bit compare (`windows_readonly_differs`). The apply
        // path (permissions::set_permissions_like /
        // apply_permissions_from_entry) re-stamps this bit under --perms, so the
        // quick-check must gate on it or a file differing ONLY in the read-only
        // attribute is judged unchanged and the update is silently skipped.
        if options.permissions()
            && windows_readonly_differs(entry.permissions(), cached_meta.permissions().readonly())
        {
            return false;
        }

        if options.times() {
            let current_mtime = filetime::FileTime::from_last_modification_time(cached_meta);
            // upstream: util1.c:1573 same_time() - apply the `--modify-window`
            // tolerance rather than an exact FileTime compare.
            if !modify_window.same_time(
                current_mtime.unix_seconds(),
                current_mtime.nanoseconds(),
                entry.mtime(),
                entry.mtime_nsec(),
            ) {
                return false;
            }
        }

        if options.atimes() && entry.atime() != 0 {
            let current_atime = filetime::FileTime::from_last_access_time(cached_meta);
            let entry_atime = filetime::FileTime::from_unix_time(entry.atime(), 0);
            if current_atime != entry_atime {
                return false;
            }
        }
    }

    // upstream: flist.c:1221-1222 - the `--chmod` tweak rides the flist mode,
    // and dest_mode() (rsync.c:470-471) then keeps an EXISTING destination's
    // own permission bits when `!preserve_perms`. On this quick-check path
    // the destination exists by definition, so only the `--perms` compare
    // can report a chmod-driven difference.
    #[cfg(unix)]
    if let Some(chmod) = options.chmod() {
        use std::os::unix::fs::MetadataExt;
        if options.permissions() {
            let new_mode = chmod.apply(entry.permissions(), cached_meta.is_dir());
            if (cached_meta.mode() & 0o7777) != (new_mode & 0o7777) {
                return false;
            }
        }
    }
    #[cfg(not(unix))]
    if options.chmod().is_some() {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let Some(uid) = options.owner_override()
            && cached_meta.uid() != uid
        {
            return false;
        }
        if let Some(gid) = options.group_override()
            && cached_meta.gid() != gid
        {
            return false;
        }
    }
    #[cfg(not(unix))]
    if options.owner_override().is_some() || options.group_override().is_some() {
        return false;
    }

    true
}

/// Applies metadata from `metadata` to the destination symbolic link without
/// following the link target. Delegates to [`apply_symlink_metadata_with_options`]
/// with default options.
/// upstream: rsync.c:set_file_attrs() - symlink path uses AT_SYMLINK_NOFOLLOW
pub fn apply_symlink_metadata(
    destination: &Path,
    metadata: &fs::Metadata,
) -> Result<(), MetadataError> {
    apply_symlink_metadata_with_options(destination, metadata, &MetadataOptions::default())
}

/// Applies symbolic link metadata using explicit [`MetadataOptions`].
///
/// Applies ownership, permissions, and timestamps. The link's own mode is set
/// only on platforms where [`crate::CAN_CHMOD_SYMLINK`] holds (macOS/BSD);
/// elsewhere the chmod is a no-op, matching upstream where `CAN_CHMOD_SYMLINK`
/// is undefined and a symlink's `st_mode` is a fixed `0o777`.
///
/// `rsync.c:806-822` calls `do_chmod_at()` for every file type with no
/// `S_ISLNK` gate (the comment at `rsync.c:819`, "ret == 1 if symlink could
/// not be set", shows a failed symlink chmod is a soft outcome). All the
/// portability lives in `syscall.c:1705-1743 do_chmod()`, which tries
/// `lchmod()`, falls through to `setattrlist(FSOPT_NOFOLLOW)` for `S_ISLNK`,
/// and only then gives up.
///
/// The mode itself never carries a `--chmod` tweak for a link: upstream gates
/// all three `tweak_mode()` sites on `!S_ISLNK` (flist.c:1966-1967,
/// flist.c:1221-1222, rsync.c:647-648).
pub fn apply_symlink_metadata_with_options(
    destination: &Path,
    metadata: &fs::Metadata,
    options: &MetadataOptions,
) -> Result<(), MetadataError> {
    apply_symlink_metadata_with_options_and_pre_transfer(destination, metadata, options, None)
}

/// Applies symlink metadata using the destination's PRE-transfer `lstat`.
///
/// Identical to [`apply_symlink_metadata_with_options`] except that the caller
/// supplies the `lstat` the destination had before the link was written. Every
/// symlink apply runs after `symlink(2)`, so the link's own stat cannot
/// distinguish "was already here" from "we just made it, and destroyed what
/// was"; upstream reads `sx.st` at generator.c:1937-1940, BEFORE
/// `atomic_create` (generator.c:2002) deletes the obstacle, and feeds that to
/// `dest_mode()`. A destination that was replaced - a symlink to a different
/// target, or a non-symlink obstacle - therefore keeps contributing its OLD
/// permission bits to the `exists` arm (rsync.c:470-471).
///
/// `pre_transfer_meta` is `None` when the caller did not replace anything, in
/// which case the link's current stat is its own pre-transfer stat. It is
/// ignored entirely when `options.destination_is_new()` says the destination
/// was absent.
pub fn apply_symlink_metadata_with_options_and_pre_transfer(
    destination: &Path,
    metadata: &fs::Metadata,
    options: &MetadataOptions,
    pre_transfer_meta: Option<&fs::Metadata>,
) -> Result<(), MetadataError> {
    ownership::set_owner_like(metadata, destination, false, options, None)?;
    // upstream: rsync.c:769-791 - the times land before the chmod
    // (rsync.c:806-822), for symlinks exactly as for every other type.
    if options.times() {
        timestamps::set_timestamp_like(metadata, destination, false, None, Some(options))?;
    }
    permissions::apply_symlink_permissions_like(destination, metadata, options, pre_transfer_meta)?;
    Ok(())
}

/// Applies metadata from a protocol `FileEntry` to a destination symbolic link
/// without following the link target.
///
/// Mirrors [`apply_metadata_from_file_entry`] but uses `lstat` for the cached
/// stat and `lutimes` / `utimensat(AT_SYMLINK_NOFOLLOW)` for timestamps so the
/// link's own mtime is updated instead of the target's. The link's own mode is
/// set only where [`crate::CAN_CHMOD_SYMLINK`] holds (macOS/BSD); ownership
/// (when applicable) is applied with `AT_SYMLINK_NOFOLLOW`.
///
/// This is the receiver-side counterpart to [`apply_symlink_metadata`] that
/// works directly with `FileEntry` metadata from the wire protocol, so the
/// network receiver does not need to construct an [`fs::Metadata`] instance
/// before calling it.
///
/// # Upstream Reference
///
/// - `rsync.c:806-822` - upstream chmods every file type with no `S_ISLNK`
///   gate; oc mirrors this on platforms where [`crate::CAN_CHMOD_SYMLINK`]
///   holds. Symlink portability lives in `syscall.c:1705-1743 do_chmod()`
///   (`lchmod()`, then `setattrlist(FSOPT_NOFOLLOW)`). The mode reaching that
///   chmod is never `--chmod`-tweaked for a link (flist.c:1966-1967,
///   flist.c:1221-1222 and rsync.c:647-648 all gate on `!S_ISLNK`).
/// - `rsync.c:set_times()` - uses `lutimes` when the target is a symlink
/// - `generator.c:1604` - `set_file_attrs(fname, file, NULL, NULL, 0)` runs
///   after `atomic_create` -> `do_symlink` so the new symlink's mtime matches
///   the sender's `F_MOD_NSEC_or_0(file)`.
pub fn apply_symlink_metadata_from_entry(
    destination: &Path,
    entry: &protocol::flist::FileEntry,
    options: &MetadataOptions,
) -> Result<(), MetadataError> {
    apply_symlink_metadata_from_entry_with_pre_transfer(destination, entry, options, None)
}

/// Applies symlink metadata using the destination's PRE-transfer `lstat`.
///
/// Identical to [`apply_symlink_metadata_from_entry`] except that the caller
/// supplies the `lstat` the destination had before the link was written. Every
/// symlink apply runs after `symlinkat(2)`, so the link's own stat cannot
/// distinguish "was already here" from "we just made it, and destroyed what
/// was"; upstream reads `sx.st` at generator.c:1937-1940, BEFORE
/// `atomic_create` deletes the obstacle, and feeds that to `dest_mode()`. A
/// destination that was replaced - a symlink to a different target, or a
/// non-symlink obstacle - therefore keeps contributing its OLD permission bits
/// to the `exists` arm.
///
/// `pre_transfer_meta` is `None` when the caller did not replace anything, in
/// which case the link's current stat is its own pre-transfer stat. It is
/// ignored entirely when `options.destination_is_new()` says the destination
/// was absent.
pub fn apply_symlink_metadata_from_entry_with_pre_transfer(
    destination: &Path,
    entry: &protocol::flist::FileEntry,
    options: &MetadataOptions,
    pre_transfer_meta: Option<&fs::Metadata>,
) -> Result<(), MetadataError> {
    let cached_meta = fs::symlink_metadata(destination).ok();

    #[cfg(unix)]
    ownership::apply_symlink_ownership_from_entry(
        destination,
        entry,
        options,
        cached_meta.as_ref(),
    )?;
    #[cfg(not(unix))]
    {
        let _ = entry;
        let _ = options;
        let _ = cached_meta.as_ref();
    }

    // upstream: rsync.c:769-791 - the times land before the chmod
    // (rsync.c:806-822), for symlinks exactly as for every other type.
    if options.times() {
        timestamps::apply_symlink_timestamps_from_entry(
            destination,
            entry,
            options,
            cached_meta.as_ref(),
        )?;
    }

    permissions::apply_symlink_permissions_from_entry(
        destination,
        entry,
        options,
        cached_meta.as_ref(),
        pre_transfer_meta,
    )?;

    Ok(())
}

/// Applies metadata from a protocol `FileEntry` to the destination file.
///
/// This is the receiver-side counterpart to [`apply_file_metadata`] that works
/// directly with `FileEntry` metadata from the wire protocol, avoiding the need
/// to construct an [`fs::Metadata`] instance.
///
/// # Examples
///
/// ```no_run
/// use metadata::{apply_metadata_from_file_entry, MetadataOptions};
/// use protocol::flist::FileEntry;
/// use std::path::Path;
///
/// # fn example(file_entry: &FileEntry) -> Result<(), metadata::MetadataError> {
/// let dest_path = Path::new("/path/to/reconstructed/file.txt");
///
/// let options = MetadataOptions::new()
///     .preserve_permissions(true)
///     .preserve_times(true);
///
/// apply_metadata_from_file_entry(dest_path, file_entry, &options)?;
/// # Ok(())
/// # }
/// ```
pub fn apply_metadata_from_file_entry(
    destination: &Path,
    entry: &protocol::flist::FileEntry,
    options: &MetadataOptions,
) -> Result<(), MetadataError> {
    let cached_meta = fs::metadata(destination).ok();
    apply_metadata_with_cached_stat(destination, entry, options, cached_meta)
}

/// Applies metadata using a pre-cached `stat` result.
///
/// Same as [`apply_metadata_from_file_entry`] but avoids an extra `stat`
/// syscall when the caller already has the destination's metadata (e.g.
/// from a quick-check comparison).
pub fn apply_metadata_with_cached_stat(
    destination: &Path,
    entry: &protocol::flist::FileEntry,
    options: &MetadataOptions,
    cached_meta: Option<fs::Metadata>,
) -> Result<(), MetadataError> {
    apply_metadata_with_pre_transfer_stat(destination, entry, options, cached_meta, None)
}

/// Applies metadata using both a cached post-rename stat and a pre-transfer
/// stat.
///
/// Identical to [`apply_metadata_with_cached_stat`] except the additional
/// `pre_transfer_meta` argument lets the receiver mirror upstream
/// `rsync.c:dest_mode()`: when `-p`/`-E`/`--chmod` are all off, upstream
/// still chmods a freshly-renamed temp file back to the pre-transfer
/// destination's permission bits (`exists=true` branch) or to the
/// umask-masked source mode (`exists=false` branch). Without the pre-
/// transfer stat the receiver would feed the temp file's `0o600`/umask-
/// default mode into the heuristic.
pub fn apply_metadata_with_pre_transfer_stat(
    destination: &Path,
    entry: &protocol::flist::FileEntry,
    options: &MetadataOptions,
    cached_meta: Option<fs::Metadata>,
    pre_transfer_meta: Option<fs::Metadata>,
) -> Result<(), MetadataError> {
    apply_metadata_with_attrs_flags_and_pre_transfer(
        destination,
        entry,
        options,
        cached_meta,
        AttrsFlags::empty(),
        pre_transfer_meta,
    )
}

/// Applies metadata from a [`protocol::flist::FileEntry`] with explicit
/// [`AttrsFlags`] controlling which time attributes to skip.
///
/// This is the full-featured variant that mirrors upstream `set_file_attrs()`
/// in `rsync.c`. Callers pass [`AttrsFlags`] to selectively skip mtime, atime,
/// or crtime application.
///
/// # Upstream Reference
///
/// - `rsync.c:574-625` - `set_file_attrs()` uses `flags` parameter to govern
///   which timestamps are applied and whether the comparison is exact.
/// - `rsync.c:585` - `flags |= ATTRS_SKIP_MTIME | ATTRS_SKIP_ATIME | ATTRS_SKIP_CRTIME`
///   when `omit_dir_times` or `omit_link_times` is active.
/// - `generator.c:1827` - Passes `maybe_ATTRS_REPORT | maybe_ATTRS_ACCURATE_TIME`
///   on quick-check match paths.
pub fn apply_metadata_with_attrs_flags(
    destination: &Path,
    entry: &protocol::flist::FileEntry,
    options: &MetadataOptions,
    cached_meta: Option<fs::Metadata>,
    attrs_flags: AttrsFlags,
) -> Result<(), MetadataError> {
    apply_metadata_with_attrs_flags_and_pre_transfer(
        destination,
        entry,
        options,
        cached_meta,
        attrs_flags,
        None,
    )
}

/// Like [`apply_metadata_with_attrs_flags`] but accepts a pre-transfer
/// destination stat so the permission-apply path can reproduce upstream
/// `rsync.c:dest_mode()` for the receiver chmod loop.
///
/// `pre_transfer_meta` is the destination's metadata captured before any
/// temp-file rename. Pass `Some(meta)` when a destination file existed at
/// transfer start; pass `None` when the destination is brand new.
pub fn apply_metadata_with_attrs_flags_and_pre_transfer(
    destination: &Path,
    entry: &protocol::flist::FileEntry,
    options: &MetadataOptions,
    cached_meta: Option<fs::Metadata>,
    attrs_flags: AttrsFlags,
    pre_transfer_meta: Option<fs::Metadata>,
) -> Result<(), MetadataError> {
    // Resolve the destination's parent directory ONCE through the hardened
    // `confined_parent` walk and share the borrowed dirfd across the three
    // metadata appliers below. Each applier would otherwise re-walk the same
    // parent (`fchownat`/`utimensat`/`fchmodat` each opening it), tripling the
    // per-file openat2+close syscall count on the receiver's hot path. Sharing
    // one resolution is pure de-duplication: the walk is byte-identical, so the
    // CVE-2026-29518 symlink-race confinement is preserved exactly.
    //
    // The share is skipped (dirfd stays `None`, each applier keeps its existing
    // behaviour) when:
    // - `--keep-dirlinks` is in effect (`ParentWalk::Follow`), where the
    //   appliers deliberately follow a symlinked parent through AT_FDCWD
    //   instead of walking it;
    // - the destination has no multi-component parent to walk; or
    // - the walk itself fails - then each applier re-walks and reports the
    //   failure exactly as before (unchanged error attribution).
    #[cfg(unix)]
    let parent_dir_owned: Option<std::os::fd::OwnedFd> = match options.parent_walk() {
        ParentWalk::Follow => None,
        ParentWalk::Confined(root) => confined_parent(destination, root).ok().flatten(),
    };
    #[cfg(unix)]
    let parent_dirfd: ParentDirFd<'_> = {
        use std::os::fd::AsFd;
        parent_dir_owned.as_ref().map(|fd| fd.as_fd())
    };
    #[cfg(not(unix))]
    let parent_dirfd: ParentDirFd<'_> = None;

    let restat_after_chown = ownership::apply_ownership_from_entry(
        destination,
        entry,
        options,
        cached_meta.as_ref(),
        parent_dirfd,
    )?;

    // upstream: rsync.c:564-567 - the chown may have cleared setuid/setgid bits,
    // so refresh the cached stat before the chmod compare re-applies them.
    let cached_meta = if restat_after_chown {
        fs::metadata(destination).ok().or(cached_meta)
    } else {
        cached_meta
    };

    // upstream: rsync.c:632 `set_times()` runs BEFORE rsync.c:658 `do_chmod_at()`,
    // so timestamps are applied ahead of the permission change. This ordering is
    // observable (upstream sets the mtime/atime, then the mode) and it also keeps
    // a read-only target mode from blocking the utimes call that would otherwise
    // follow it.

    // upstream: rsync.c:597 - `if (!(flags & ATTRS_SKIP_MTIME) && !same_mtime(...))`
    if options.times() && !attrs_flags.skip_mtime() {
        timestamps::apply_timestamps_from_entry(
            destination,
            entry,
            options,
            cached_meta.as_ref(),
            parent_dirfd,
        )?;
    }

    // upstream: rsync.c:604 - atime applied independently when SKIP_MTIME is set
    // but SKIP_ATIME is not, since apply_timestamps_from_entry handles both together
    if options.atimes() && attrs_flags.skip_mtime() && !attrs_flags.skip_atime() {
        timestamps::apply_atime_only_from_entry(
            destination,
            entry,
            cached_meta.as_ref(),
            options.parent_walk(),
            parent_dirfd,
        )?;
    }

    // upstream: rsync.c:751 - `if (crtimes_ndx && !(flags & ATTRS_SKIP_CRTIME))`
    //
    // Deliberately no `crtime != 0` test. Zero is a legitimate incoming value,
    // not a stand-in for "absent": upstream's `get_create_time()` returned 0 for
    // a daemon running without chroot (syscall.c, 3.4.3-3.4.4), so a file list
    // sourced from such a daemon carries 0 and upstream stamps it. rsync 3.5.0
    // reversed that: it removed the `am_daemon && !am_chrooted` crtime guard
    // (syscall.c), keeping --crtimes functional on a no-chroot daemon and
    // accepting the parent-symlink race as a documented residual, so a 3.5.0
    // daemon reads the real crtime and returns 0 only on a getattrlist failure -
    // either way 0 remains a legitimate value that must be stamped. "Absent" is
    // already excluded by `options.crtimes()`, which mirrors `crtimes_ndx` -
    // when it is set, the decoder always produces a crtime
    // (`flist/read/metadata.rs`: `Some(mtime)` for XMIT_CRTIME_EQ_MTIME, else
    // the varlong it read), so an entry can never reach here without one.
    if options.crtimes() && !attrs_flags.skip_crtime() {
        timestamps::apply_crtime_from_entry(destination, entry)?;
    }

    // upstream: rsync.c:658 - `do_chmod_at()` is the last attribute upstream
    // applies, after times (and after ACLs, which oc applies in the caller).
    permissions::apply_permissions_from_entry(
        destination,
        entry,
        options,
        cached_meta.as_ref(),
        pre_transfer_meta.as_ref(),
        parent_dirfd,
    )?;

    Ok(())
}
