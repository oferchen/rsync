//! Permission preservation and chmod operations.
//!
//! Handles permission bits (full mode on Unix, read-only flag on Windows),
//! chmod modifier application, executability-only preservation, and both
//! path-based and fd-based permission syscalls.

use crate::error::MetadataError;
use crate::options::MetadataOptions;
use std::fs;
use std::path::Path;

#[cfg(unix)]
use rustix::fs as unix_fs;
#[cfg(unix)]
use std::io;
#[cfg(unix)]
use std::os::fd::BorrowedFd;

/// Returns the process umask, cached for thread safety.
///
/// upstream: `main.c` stores `orig_umask` once at startup. We query it
/// the first time a permission application needs the umask and cache the
/// result so the double set-and-restore syscall happens at most once per
/// process.
#[cfg(unix)]
#[allow(unsafe_code)]
fn cached_umask() -> u32 {
    use std::sync::OnceLock;
    static UMASK: OnceLock<u32> = OnceLock::new();
    *UMASK.get_or_init(|| {
        // SAFETY: umask is a standard POSIX call. We set it to 0 to read
        // the current value, then immediately restore it. This is a
        // well-known pattern (used by upstream rsync main.c, GNU coreutils,
        // etc.). The OnceLock ensures this pair of calls happens at most
        // once per process, eliminating any window for concurrent umask
        // modifications.
        let old = unsafe { libc::umask(0) };
        unsafe { libc::umask(old) };
        old as u32
    })
}

/// Returns the default permission seed for a child created under `parent`.
///
/// Mirrors upstream `generator.c:1337-1339` which calls `default_perms_for_dir(dn)`
/// when `--perms` is off. The helper folds the parent directory's POSIX default
/// ACL `user_obj`/`group_obj`/`other_obj` entries into the seed; when there is
/// no default ACL (or the filesystem does not support POSIX default ACLs) it
/// returns the umask-derived `ACCESSPERMS & ~orig_umask`.
///
/// upstream: `acls.c:1083-1139` `default_perms_for_dir`
/// upstream: `generator.c:1337-1340` per-parent `dflt_perms` lookup
#[cfg(unix)]
fn default_perms_seed(parent: Option<&Path>) -> u32 {
    let umask = cached_umask();
    #[cfg(all(
        feature = "acl",
        any(target_os = "linux", target_os = "macos", target_os = "freebsd")
    ))]
    {
        if let Some(parent) = parent {
            return crate::default_perms_for_dir(parent, umask);
        }
    }
    #[cfg(not(all(
        feature = "acl",
        any(target_os = "linux", target_os = "macos", target_os = "freebsd")
    )))]
    {
        let _ = parent;
    }
    0o777 & !umask
}

/// Computes the destination file mode matching upstream `rsync.c:dest_mode()`.
///
/// When `-p` (preserve permissions) is not active, upstream rsync still applies
/// the source mode masked by the umask-derived default permissions. This ensures
/// that execute bits from the source are preserved (masked by umask) instead of
/// being lost to `open()`'s default `0o666 & ~umask`.
///
/// For new files: `source_mode & (~0o7777 | dflt_perms)`
/// For existing files: keeps existing permissions (returns `None`)
///
/// The `dest_parent` argument carries the destination's parent directory so the
/// new-file seed can inherit the parent's POSIX default ACL via
/// [`default_perms_seed`] when the `acl` feature is enabled. Falls back to the
/// umask-derived seed when the parent is unknown or no default ACL is present.
///
/// upstream: rsync.c:449-472 `dest_mode()`
/// upstream: generator.c:1337-1339 `dflt_perms = default_perms_for_dir(dn)`
/// upstream: generator.c:2280 `dflt_perms = (ACCESSPERMS & ~orig_umask)`
#[cfg(unix)]
fn compute_dest_mode(
    source_mode: u32,
    is_new: bool,
    existing: Option<&fs::Metadata>,
    dest_parent: Option<&Path>,
) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;

    if is_new {
        // upstream: dest_mode() for new files:
        // new_mode = flist_mode & (~CHMOD_BITS | dflt_perms)
        let dflt_perms = default_perms_seed(dest_parent);
        let new_mode = source_mode & (!0o7777 | dflt_perms);
        // Skip the chmod if the mode already matches
        if let Some(existing) = existing {
            if (existing.permissions().mode() & 0o7777) == (new_mode & 0o7777) {
                return None;
            }
        }
        Some(new_mode)
    } else if let Some(existing) = existing {
        // upstream: dest_mode() for existing files returns
        // (flist_mode & ~CHMOD_BITS) | (stat_mode & CHMOD_BITS)
        // which keeps existing permissions. No chmod needed.
        let stat_mode = existing.permissions().mode();
        let new_mode = (source_mode & !0o7777) | (stat_mode & 0o7777);
        if (new_mode & 0o7777) != (stat_mode & 0o7777) {
            Some(new_mode)
        } else {
            None
        }
    } else {
        None
    }
}

/// Pre-applies the upstream `rsync.c:dest_mode()` chmod for the source-
/// `Metadata` apply path used by the local-copy executor and the receiver
/// data fast path.
///
/// Mirrors upstream's `file->mode = dest_mode(...)` rewrite that runs
/// BEFORE the temp file is opened; the freshly-renamed temp file then gets
/// chmod'd to that mode by `set_file_attrs()`. Without this pre-chmod the
/// destination would silently inherit the temp file's `0o600`/umask-default
/// permissions instead of upstream's `dest_mode()` result.
///
/// Returns without acting when `-p`/`--chmod` are in effect: those paths
/// already drive the chmod through `metadata.permissions().mode()` or the
/// chmod modifier chain.
///
/// upstream: rsync.c:954-965 (`dest_mode()` invocation) + rsync.c:457-465
/// (`dest_mode()` body)
#[cfg(unix)]
pub fn apply_dest_mode_pre_transfer(
    destination: &Path,
    source_metadata: &fs::Metadata,
    options: &MetadataOptions,
    pre_transfer_meta: Option<&fs::Metadata>,
) -> Result<(), MetadataError> {
    use std::os::unix::fs::PermissionsExt;

    if !source_metadata.file_type().is_file() {
        return Ok(());
    }
    if options.permissions() || options.chmod().is_some() {
        return Ok(());
    }

    let source_mode = source_metadata.permissions().mode();
    let base_mode = if let Some(existing) = pre_transfer_meta {
        // Existing destination: keep its prior permission bits.
        let stat_mode = existing.permissions().mode();
        (source_mode & !0o7777) | (stat_mode & 0o7777)
    } else {
        // New destination: source mode masked by default permissions.
        let dflt_perms = 0o777 & !cached_umask();
        source_mode & (!0o7777 | dflt_perms)
    };

    let mut target_perms = base_mode & 0o7777;
    if options.executability() {
        // upstream: rsync.c:457-465 - layer `-E` executability on top of the
        // dest_mode() base.
        if source_mode & 0o111 == 0 {
            target_perms &= !0o111;
        } else if target_perms & 0o111 == 0 {
            target_perms |= (target_perms & 0o444) >> 2;
        }
    }
    let new_mode = (base_mode & !0o7777) | target_perms;

    // Compare against the file's CURRENT (post-rename) mode. If the temp
    // file already happens to match the target we skip the chmod syscall.
    let current_mode = fs::metadata(destination)
        .map_err(|error| MetadataError::new("inspect destination permissions", destination, error))?
        .permissions()
        .mode();
    if (current_mode & 0o7777) != (new_mode & 0o7777) {
        chmod_path_honoring_keep_dirlinks(destination, new_mode, options, "apply dest_mode")?;
    }
    Ok(())
}

/// Computes the upstream `dest_mode()` result for the receiver entry path.
///
/// Returns the mode bits the destination would have AFTER upstream rewrites
/// `file->mode = dest_mode(...)`. The `-E` layer (if active) goes on top of
/// this base mode. Used both by the no-flag chmod fallback (which mirrors
/// upstream's unconditional `set_file_attrs()` chmod) and by the `-E`
/// without `-p` path.
///
/// upstream: rsync.c:449-472 `dest_mode()`
#[cfg(unix)]
fn dest_mode_for_existing_or_new(
    entry: &protocol::flist::FileEntry,
    pre_transfer_meta: Option<&fs::Metadata>,
) -> u32 {
    use std::os::unix::fs::PermissionsExt;

    let source_mode = entry.permissions();
    if let Some(existing) = pre_transfer_meta {
        // Existing file: `(flist_mode & ~CHMOD_BITS) | (stat_mode & CHMOD_BITS)`
        // - keep the destination's prior permission bits.
        let stat_mode = existing.permissions().mode();
        (source_mode & !0o7777) | (stat_mode & 0o7777)
    } else {
        // New file: `flist_mode & (~CHMOD_BITS | dflt_perms)` so exec bits
        // survive the umask wash while special bits (suid/sgid/sticky) drop
        // out.
        let dflt_perms = 0o777 & !cached_umask();
        source_mode & (!0o7777 | dflt_perms)
    }
}

/// Sets permissions on `destination` to match `metadata` (full mode on Unix,
/// read-only flag on Windows).
///
/// On Unix, copies the full mode bits (including suid/sgid/sticky). On
/// Windows, only the read-only flag is mirrored. The `options` carrier lets
/// the Unix path honor `--keep-dirlinks` via [`chmod_path_honoring_keep_dirlinks`]
/// instead of the dirfd sandbox that rejects symlinked parents.
// upstream: rsync.c:set_file_attrs() - chmod path for direct permission copy
pub(super) fn set_permissions_like(
    metadata: &fs::Metadata,
    destination: &Path,
    options: &MetadataOptions,
) -> Result<(), MetadataError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mode = metadata.permissions().mode();
        // upstream: syscall.c:do_chmod_at() - symlink-race-safe variant
        // anchored on the parent dirfd. Mirrors the receiver chmod-apply
        // path through `apply_permissions_from_entry` so chmod-symlink-race
        // cannot redirect this syscall outside the receiver confinement.
        // Under `--keep-dirlinks` the user has opted into following dest-side
        // symlinks-to-dirs, so the sandbox refusal is wrong - fall through to
        // `chmod_path_honoring_keep_dirlinks` which uses `std::fs::set_permissions`.
        chmod_path_honoring_keep_dirlinks(destination, mode, options, "preserve permissions")?;
    }

    #[cfg(not(unix))]
    {
        let _ = options;
        let readonly = metadata.permissions().readonly();
        let mut destination_permissions = fs::metadata(destination)
            .map_err(|error| {
                MetadataError::new("inspect destination permissions", destination, error)
            })?
            .permissions();
        destination_permissions.set_readonly(readonly);
        fs::set_permissions(destination, destination_permissions)
            .map_err(|error| MetadataError::new("preserve permissions", destination, error))?
    }

    Ok(())
}

/// Returns `true` when `target_mode` already matches the permission bits on
/// `existing`, comparing only the lower 12 bits (suid/sgid/sticky + rwx).
// upstream: rsync.c:set_file_attrs() - skips chmod when mode already matches
#[cfg(unix)]
pub(super) fn permissions_match(target_mode: u32, existing: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    (existing.permissions().mode() & 0o7777) == (target_mode & 0o7777)
}

/// Applies permissions with optional chmod modifiers (path-based).
///
/// When chmod modifiers are configured, applies them on top of the base mode.
/// Otherwise delegates to [`apply_permissions_without_chmod`] for direct
/// permission copy or executability-only preservation.
// upstream: rsync.c:set_file_attrs() - chmod with optional modifier chain
pub(super) fn apply_permissions_with_chmod(
    destination: &Path,
    metadata: &fs::Metadata,
    options: &MetadataOptions,
    existing: Option<&fs::Metadata>,
) -> Result<(), MetadataError> {
    #[cfg(unix)]
    {
        if let Some(modifiers) = options.chmod() {
            let mut mode = base_mode_for_permissions(destination, metadata, options, existing)?;
            mode = modifiers.apply(mode, metadata.file_type());

            if let Some(existing) = existing {
                if permissions_match(mode, existing) {
                    return Ok(());
                }
            }

            // upstream: syscall.c:do_chmod_at() - symlink-race-safe variant
            // anchored on the parent dirfd.
            chmod_path_honoring_keep_dirlinks(destination, mode, options, "preserve permissions")?;
            return Ok(());
        }
    }

    if options.permissions() || options.executability() {
        apply_permissions_without_chmod(destination, metadata, options, existing)?;
        return Ok(());
    }

    // upstream: rsync.c:dest_mode() - when no explicit permission option is
    // active, still apply source-mode-based permissions masked by umask.
    // Without this, newly created files get `0o666 & ~umask` from open()
    // instead of `source_mode & (~CHMOD_BITS | dflt_perms)`.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let source_mode = metadata.permissions().mode();
        if let Some(new_mode) = compute_dest_mode(
            source_mode,
            options.destination_is_new(),
            existing,
            destination.parent(),
        ) {
            // upstream: syscall.c:do_chmod_at() - symlink-race-safe variant.
            chmod_path_honoring_keep_dirlinks(destination, new_mode, options, "apply dest_mode")?;
        }
    }

    Ok(())
}

/// fd-based variant of permission application.
///
/// Uses `fchmod` when an fd is available and we can determine the mode without
/// reading the current destination permissions. Falls back to path-based
/// operations for chmod modifiers that require a fresh stat, or when no fd
/// is provided.
#[cfg(unix)]
pub(super) fn apply_permissions_with_chmod_fd(
    destination: &Path,
    metadata: &fs::Metadata,
    options: &MetadataOptions,
    fd: Option<BorrowedFd<'_>>,
    existing: Option<&fs::Metadata>,
) -> Result<(), MetadataError> {
    use std::os::unix::fs::PermissionsExt;

    if let Some(modifiers) = options.chmod() {
        let mut mode = base_mode_for_permissions(destination, metadata, options, existing)?;
        mode = modifiers.apply(mode, metadata.file_type());

        if let Some(existing) = existing {
            if permissions_match(mode, existing) {
                return Ok(());
            }
        }

        if let Some(fd) = fd {
            unix_fs::fchmod(
                fd,
                unix_fs::Mode::from_raw_mode(mode as rustix::fs::RawMode),
            )
            .map_err(|error| {
                MetadataError::new("preserve permissions", destination, io::Error::from(error))
            })?;
        } else {
            // upstream: syscall.c:do_chmod_at() - symlink-race-safe variant.
            chmod_path_honoring_keep_dirlinks(destination, mode, options, "preserve permissions")?;
        }
        return Ok(());
    }

    if options.permissions() {
        let mode = metadata.permissions().mode();

        if let Some(existing) = existing {
            if permissions_match(mode, existing) {
                return Ok(());
            }
        }

        if let Some(fd) = fd {
            unix_fs::fchmod(
                fd,
                unix_fs::Mode::from_raw_mode(mode as rustix::fs::RawMode),
            )
            .map_err(|error| {
                MetadataError::new("preserve permissions", destination, io::Error::from(error))
            })?;
        } else {
            set_permissions_like(metadata, destination, options)?;
        }
        return Ok(());
    }

    if options.executability() && metadata.is_file() {
        apply_permissions_without_chmod(destination, metadata, options, existing)?;
        return Ok(());
    }

    // upstream: rsync.c:dest_mode() - when no explicit permission option is
    // active, still apply source-mode-based permissions masked by umask.
    let source_mode = metadata.permissions().mode();
    if let Some(new_mode) = compute_dest_mode(
        source_mode,
        options.destination_is_new(),
        existing,
        destination.parent(),
    ) {
        if let Some(fd) = fd {
            unix_fs::fchmod(
                fd,
                unix_fs::Mode::from_raw_mode(new_mode as rustix::fs::RawMode),
            )
            .map_err(|error| {
                MetadataError::new("apply dest_mode", destination, io::Error::from(error))
            })?;
        } else {
            // upstream: syscall.c:do_chmod_at() - symlink-race-safe variant.
            chmod_path_honoring_keep_dirlinks(destination, new_mode, options, "apply dest_mode")?;
        }
    }

    Ok(())
}

/// Issues a path-based chmod that honors `--keep-dirlinks`.
///
/// When `--keep-dirlinks` is inactive, dispatches to `fast_io::secure_chmod_at`,
/// which anchors on the parent dirfd opened through `secure_open_dir` and
/// rejects symlinked parents (`ELOOP`/`ENOTDIR`) to defeat chmod-symlink-race
/// attacks against the receiver confinement.
///
/// When `--keep-dirlinks` is active, the user has explicitly opted into
/// following dest-side symlinks-to-dirs, so the sandbox refusal is wrong: the
/// parent in our test path is a symlink to a real directory and the chmod must
/// land on the canonical file. Falls back to `std::fs::set_permissions`, which
/// resolves symlinks through the OS path walk like upstream
/// `generator.c:1344`'s `link_stat(fname, &sx.st, keep_dirlinks && is_dir)`.
///
/// upstream: rsync.c:set_file_attrs() / generator.c:1344 link_stat
#[cfg(unix)]
fn chmod_path_honoring_keep_dirlinks(
    destination: &Path,
    mode: u32,
    options: &MetadataOptions,
    action: &'static str,
) -> Result<(), MetadataError> {
    if options.keep_dirlinks() {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(destination, fs::Permissions::from_mode(mode))
            .map_err(|error| MetadataError::new(action, destination, error))?;
    } else {
        fast_io::secure_chmod_at(destination, mode, true)
            .map_err(|error| MetadataError::new(action, destination, error))?;
    }
    Ok(())
}

/// Determines the base mode before chmod modifiers are applied.
///
/// When `--perms` is active, returns the source mode directly. Otherwise
/// mirrors upstream `rsync.c:447-472 dest_mode()`: the chmod tweak (CLI
/// `--chmod` or daemon `incoming chmod = ...`) runs on top of the
/// source mode collapsed via `dest_mode()`, not on top of whatever the
/// destination tempfile happens to carry. Without this, a freshly-renamed
/// `O_TMPFILE` 0o600 leaks through as the chmod baseline and a daemon
/// upload with `--no-perms` lands a file at 0o600 instead of the
/// umask-default the testsuite `chmod-option` test pins.
#[cfg(unix)]
fn base_mode_for_permissions(
    destination: &Path,
    metadata: &fs::Metadata,
    options: &MetadataOptions,
    existing: Option<&fs::Metadata>,
) -> Result<u32, MetadataError> {
    use std::os::unix::fs::PermissionsExt;

    if options.permissions() {
        return Ok(metadata.permissions().mode());
    }

    // upstream: rsync.c:455-470 - existing files keep destination's perm
    // bits with the type bits from the source mode; new files mask the
    // source mode by `dflt_perms = default_perms_for_dir(dn)`, which folds
    // the parent's POSIX default ACL when one is present (acls.c:1083) and
    // otherwise reduces to `ACCESSPERMS & ~orig_umask`. The destination
    // tempfile mode is never the baseline.
    let source_mode = metadata.permissions().mode();
    let mut destination_permissions = if let Some(existing) = existing {
        (source_mode & !0o7777) | (existing.permissions().mode() & 0o7777)
    } else {
        let dflt_perms = default_perms_seed(destination.parent());
        source_mode & (!0o7777 | dflt_perms)
    };

    if options.executability() && metadata.is_file() && existing.is_some() {
        // upstream: rsync.c:457-465 dest_mode() - for existing files only,
        // copy source's exec presence: if source has no exec bits, clear
        // them on dest; else if dest has no exec bits, grant exec to
        // everyone who can already read (`new_mode & 0444 >> 2`). When dest
        // already has some exec bits they are preserved verbatim. Upstream
        // skips this branch for new files - the umask-masked source mode
        // already encodes the right answer there.
        if source_mode & 0o111 == 0 {
            destination_permissions &= !0o111;
        } else if destination_permissions & 0o111 == 0 {
            destination_permissions |= (destination_permissions & 0o444) >> 2;
        }
    }

    // `destination` is unused on the new-file path now that the base mode
    // is derived from the source rather than from a destination stat.
    // Keep the parameter for API parity with the fd-based sibling.
    let _ = destination;
    Ok(destination_permissions)
}

/// Applies permissions without chmod modifiers (direct copy or executability only).
fn apply_permissions_without_chmod(
    destination: &Path,
    metadata: &fs::Metadata,
    options: &MetadataOptions,
    existing: Option<&fs::Metadata>,
) -> Result<(), MetadataError> {
    let _ = &existing; // used only on unix
    if options.permissions() {
        #[cfg(unix)]
        if let Some(existing) = existing {
            use std::os::unix::fs::PermissionsExt;
            if permissions_match(metadata.permissions().mode(), existing) {
                return Ok(());
            }
        }
        set_permissions_like(metadata, destination, options)?;
        return Ok(());
    }

    #[cfg(unix)]
    {
        if options.executability() && metadata.is_file() {
            use std::os::unix::fs::PermissionsExt;

            let mut destination_permissions = if let Some(existing) = existing {
                existing.permissions().mode()
            } else {
                fs::metadata(destination)
                    .map_err(|error| {
                        MetadataError::new("inspect destination permissions", destination, error)
                    })?
                    .permissions()
                    .mode()
            };

            // upstream: rsync.c:457-465 dest_mode() - if source has no exec
            // bits, clear them on dest; else if dest has no exec bits, grant
            // exec to everyone who can already read (`new_mode & 0444 >> 2`).
            // When dest already has some exec bits they are preserved
            // verbatim.
            if metadata.permissions().mode() & 0o111 == 0 {
                destination_permissions &= !0o111;
            } else if destination_permissions & 0o111 == 0 {
                destination_permissions |= (destination_permissions & 0o444) >> 2;
            }

            if let Some(existing) = existing {
                if permissions_match(destination_permissions, existing) {
                    return Ok(());
                }
            }

            // upstream: syscall.c:do_chmod_at() - symlink-race-safe variant.
            chmod_path_honoring_keep_dirlinks(
                destination,
                destination_permissions,
                options,
                "preserve permissions",
            )?;
        }
    }

    Ok(())
}

/// Applies permissions from a protocol `FileEntry`.
///
/// Handles the receiver-side chmod path: applies the entry's permission bits
/// directly, then layers any `--chmod` modifiers on top. Skips the syscall
/// when the resulting mode already matches `cached_meta`.
///
/// `pre_transfer_meta` is the destination's metadata captured BEFORE the
/// transfer started (before any temp-file rename). It mirrors upstream
/// `rsync.c:dest_mode()`'s `stat_mode` argument: the receiver runs
/// `dest_mode()` against the pre-transfer destination so the dest's prior
/// permission bits (or umask-masked source bits for new files) propagate
/// onto the freshly-renamed temp file. `Some(meta)` means "the file existed
/// pre-transfer at this mode"; `None` means "no pre-transfer destination
/// state available" - either the file is new or the caller cannot supply
/// it.
// upstream: rsync.c:set_file_attrs() - receiver-side permission application
pub(super) fn apply_permissions_from_entry(
    destination: &Path,
    entry: &protocol::flist::FileEntry,
    options: &MetadataOptions,
    cached_meta: Option<&fs::Metadata>,
    pre_transfer_meta: Option<&fs::Metadata>,
) -> Result<(), MetadataError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        if !options.permissions() && !options.executability() && options.chmod().is_none() {
            // upstream: rsync.c:954-965 - even when `!preserve_perms` and
            // `!preserve_executability`, the receiver mutates `file->mode` via
            // `dest_mode()` and `set_file_attrs()` chmods the post-rename
            // destination to it. For an existing destination this preserves
            // the prior mode (so a re-transfer never silently downgrades to
            // the temp file's `0o600`/umask-default permissions); for a new
            // destination it applies `source_mode & (~CHMOD_BITS | dflt_perms)`.
            // Only the regular-file branch ships - upstream restricts the
            // `S_ISREG(flist_mode)` chmod-on-rename loop to data files. Skip
            // when the caller has supplied neither a pre-transfer stat nor a
            // cached post-rename stat: we cannot distinguish a fresh-from-
            // rename temp file from an untouched destination, and the source-
            // `Metadata` apply path already handles both cases via
            // `apply_dest_mode_pre_transfer`.
            if entry.file_type().is_regular() && pre_transfer_meta.is_some() {
                let new_mode = dest_mode_for_existing_or_new(entry, pre_transfer_meta);
                let fresh_meta;
                let current_meta = if let Some(meta) = cached_meta {
                    meta
                } else {
                    fresh_meta = fs::metadata(destination).map_err(|error| {
                        MetadataError::new("inspect destination permissions", destination, error)
                    })?;
                    &fresh_meta
                };
                if (current_meta.permissions().mode() & 0o7777) != (new_mode & 0o7777) {
                    chmod_path_honoring_keep_dirlinks(
                        destination,
                        new_mode,
                        options,
                        "apply dest_mode",
                    )?;
                }
            }
            return Ok(());
        }

        // Track whether the -p path actually changed permissions so the
        // --chmod branch below knows if cached_meta is still valid.
        let mut perms_changed = false;

        if options.permissions() {
            let mode = entry.permissions();
            // upstream: rsync.c:set_file_attrs() - skips chmod when mode already matches
            let needs_chmod = match cached_meta {
                Some(meta) => (meta.permissions().mode() & 0o7777) != (mode & 0o7777),
                None => true,
            };

            if needs_chmod {
                // upstream: syscall.c:do_chmod_at() - chmod the leaf through a
                // dirfd opened with RESOLVE_BENEATH/RESOLVE_NO_SYMLINKS so a
                // symlink swapped into any parent component cannot redirect
                // the chmod outside the receiver's confinement (testsuite
                // chdir-symlink-race). Under `--keep-dirlinks` the helper
                // follows symlinked parents to mirror upstream `generator.c:1344`.
                chmod_path_honoring_keep_dirlinks(
                    destination,
                    mode,
                    options,
                    "preserve permissions",
                )?;
                perms_changed = true;
            }
        }

        if let Some(chmod) = options.chmod() {
            // upstream: rsync.c:495+518 - `new_mode = file->mode` then
            // `new_mode = tweak_mode(new_mode, daemon_chmod_modes)`. The
            // chmod baseline is the source file's mode (already collapsed
            // through `dest_mode()` in the generator at generator.c:1455 +
            // :1535 when `!preserve_perms`), NEVER the destination's
            // tempfile mode. Reading the destination would feed back the
            // `O_TMPFILE` 0o600 default for fresh transfers and produce
            // 0o600 under e.g. `Fo-x` instead of the expected umask
            // default (UTS-17.REOPEN: testsuite/chmod-option daemon
            // upload).
            let fresh_meta;
            let current_meta = if options.permissions() && perms_changed {
                fresh_meta = fs::metadata(destination)
                    .map_err(|error| MetadataError::new("read permissions", destination, error))?;
                &fresh_meta
            } else if let Some(meta) = cached_meta {
                meta
            } else {
                fresh_meta = fs::metadata(destination)
                    .map_err(|error| MetadataError::new("read permissions", destination, error))?;
                &fresh_meta
            };
            let current_mode = current_meta.permissions().mode();

            let base_mode = if options.permissions() {
                // -p: the immediately preceding branch chmod'd to the
                // source mode, so current_mode IS the source mode.
                current_mode
            } else {
                // --no-perms: mirror upstream `dest_mode()`. For a fresh
                // transfer (`cached_meta.is_none()`), use the new-file
                // branch (`flist_mode & (~CHMOD_BITS | dflt_perms)`) where
                // `dflt_perms` honours the parent's POSIX default ACL via
                // `default_perms_for_dir` (acls.c:1083); for a quick-check
                // skip on an existing dest, use the existing-file branch
                // (keep destination's perm bits).
                let source_mode = entry.permissions();
                if cached_meta.is_none() {
                    let dflt_perms = default_perms_seed(destination.parent());
                    source_mode & (!0o7777 | dflt_perms)
                } else {
                    (source_mode & !0o7777) | (current_mode & 0o7777)
                }
            };

            let new_mode = chmod.apply(base_mode, current_meta.file_type());
            if new_mode != current_mode {
                // upstream: syscall.c:do_chmod_at() symlink-race-safe variant.
                // Helper follows symlinked parents under `--keep-dirlinks` to
                // mirror upstream `generator.c:1344`.
                chmod_path_honoring_keep_dirlinks(destination, new_mode, options, "apply chmod")?;
            }
        } else if options.executability()
            && !options.permissions()
            && entry.file_type().is_regular()
        {
            // upstream: rsync.c:457-465 dest_mode() - `-E` without `-p` and
            // without `--chmod` transfers only the executability bits from
            // source to destination, layered on top of the pre-transfer
            // destination mode (or the source-mode-masked-by-dflt-perms when
            // the file is fresh). Using the post-rename temp file's
            // `0o600`/umask-default bits would silently drop bits like the
            // world-read bit upstream preserves. When the caller hasn't
            // separately tracked the pre-transfer stat (e.g. quick-check
            // path, public `apply_metadata_from_file_entry` API), fall back
            // to `cached_meta` because no rename happened and the current
            // stat IS the pre-transfer stat.
            let base_meta = pre_transfer_meta.or(cached_meta);
            let new_mode = dest_mode_for_existing_or_new(entry, base_meta);
            let mut destination_permissions = new_mode & 0o7777;

            if entry.permissions() & 0o111 == 0 {
                destination_permissions &= !0o111;
            } else if destination_permissions & 0o111 == 0 {
                destination_permissions |= (destination_permissions & 0o444) >> 2;
            }

            let fresh_meta;
            let current_meta = if let Some(meta) = cached_meta {
                meta
            } else {
                fresh_meta = fs::metadata(destination).map_err(|error| {
                    MetadataError::new("inspect destination permissions", destination, error)
                })?;
                &fresh_meta
            };
            if (current_meta.permissions().mode() & 0o7777) != destination_permissions {
                // upstream: syscall.c:do_chmod_at() symlink-race-safe variant.
                // Helper follows symlinked parents under `--keep-dirlinks` to
                // mirror upstream `generator.c:1344`.
                chmod_path_honoring_keep_dirlinks(
                    destination,
                    destination_permissions,
                    options,
                    "preserve permissions",
                )?;
            }
        }
    }

    #[cfg(not(unix))]
    {
        let _ = pre_transfer_meta;
        if options.permissions() {
            let readonly = entry.permissions() & 0o200 == 0;
            let dest_perms_meta = if let Some(meta) = cached_meta {
                meta.permissions()
            } else {
                fs::metadata(destination)
                    .map_err(|error| {
                        MetadataError::new("read destination permissions", destination, error)
                    })?
                    .permissions()
            };
            let mut dest_perms = dest_perms_meta;
            if dest_perms.readonly() != readonly {
                dest_perms.set_readonly(readonly);
                fs::set_permissions(destination, dest_perms).map_err(|error| {
                    MetadataError::new("preserve permissions", destination, error)
                })?;
            }
        }
    }

    Ok(())
}
