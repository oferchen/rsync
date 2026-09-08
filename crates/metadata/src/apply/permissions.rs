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
use std::io;
#[cfg(unix)]
use std::os::fd::BorrowedFd;

/// Reproduces upstream's during-transfer owner-`rwx` fixup for directories.
///
/// Directories written by a non-root transfer are raised to owner-`rwx` while
/// their contents land, then restored to the strict tweaked mode only when the
/// owner would otherwise lack write. Files pass through unchanged. Reuses the
/// shared [`crate::directory_transfer_mode`] so the local-copy, receiver, and
/// daemon chmod paths all compute one identical result (DRY). `am_root` is
/// sampled through the same libc `geteuid` the ownership gate uses so
/// `fakeroot`'s faked identity is honoured.
/// upstream: generator.c:1904-1912 fixup + generator.c:2565-2611 touch_up_dirs.
#[cfg(unix)]
fn tweak_directory_transfer_mode(mode: u32, file_type: fs::FileType) -> u32 {
    if !file_type.is_dir() {
        return mode;
    }
    (mode & !0o7777) | crate::directory_transfer_mode(mode, nix::unistd::geteuid().is_root())
}

/// Applies an fd-based `fchmod` through the `nix` crate (libc `fchmod(2)`).
///
/// Like ownership's chown helper, the mode change must go through the libc
/// symbol rather than a rustix raw syscall so `fakeroot`'s LD_PRELOAD
/// interposition observes it. With a raw-syscall chmod, fakeroot never sees the
/// mode; once a (libc-routed) chown records the inode in fakeroot's database,
/// its stat wrapper reports a stale mode and silently drops preserved
/// permission bits. Routing chmod through libc keeps it consistent with chown,
/// matching upstream (which drives every attribute through libc symbols).
/// upstream: syscall.c:do_fchmod() calls the fchmod(2) libc symbol.
#[cfg(unix)]
fn fchmod_libc(
    fd: BorrowedFd<'_>,
    mode: u32,
    destination: &Path,
    action: &'static str,
) -> Result<(), MetadataError> {
    nix::sys::stat::fchmod(
        fd,
        nix::sys::stat::Mode::from_bits_truncate(mode as libc::mode_t),
    )
    .map_err(|errno| MetadataError::new(action, destination, io::Error::from(errno)))
}

/// Process-wide `orig_umask`, captured once. See [`init_orig_umask`].
#[cfg(unix)]
static ORIG_UMASK: std::sync::OnceLock<u32> = std::sync::OnceLock::new();

/// Captures the process umask into the process-wide cache.
///
/// Must be called from the program entry point, BEFORE the daemon installs its
/// seccomp filter. `umask(2)` is not on the worker allowlist
/// (`daemon::seccomp::worker_seccomp_allowlist`), and a non-allowlisted syscall
/// is failed with `EPERM` rather than killing the process, so a *lazy* first
/// read inside a sandboxed worker gets `-1` back. Caching that sentinel makes
/// `dflt_perms` (`ACCESSPERMS & ~orig_umask`) zero, which collapses
/// `dest_mode()`'s new-file result to mode `000` on every write path.
///
/// Capturing eagerly at startup - the same place and for the same reason as
/// upstream - keeps the sandbox allowlist minimal and removes the ordering
/// hazard entirely: by the time any filter is installed the value is already
/// resolved.
///
/// Idempotent: the first call wins, later calls are no-ops.
///
/// # Upstream Reference
///
/// - `main.c:1797` - `umask(orig_umask = umask(0));` runs in `main()` before
///   any privilege drop or sandbox setup.
#[cfg(unix)]
#[allow(unsafe_code)]
pub fn init_orig_umask() {
    ORIG_UMASK.get_or_init(|| {
        // SAFETY: umask is a standard POSIX call. We set it to 0 to read
        // the current value, then immediately restore it. This is a
        // well-known pattern (used by upstream rsync main.c, GNU coreutils,
        // etc.). The OnceLock ensures this pair of calls happens at most
        // once per process, eliminating any window for concurrent umask
        // modifications.
        let old = unsafe { libc::umask(0) };
        unsafe { libc::umask(old) };
        sanitize_umask(old as u32)
    });
}

/// Default umask assumed when the `umask(2)` query itself failed.
#[cfg(unix)]
const FALLBACK_UMASK: u32 = 0o022;

/// Rejects a `umask(2)` return value that cannot be a umask.
///
/// A umask is 9 significant bits, so anything outside `0o777` means the query
/// failed rather than answered - a seccomp filter that fails a non-allowlisted
/// syscall with `EPERM` hands back `-1`, which as a `u32` is `u32::MAX`. Caching
/// that would make `dflt_perms` (`ACCESSPERMS & ~orig_umask`) zero and chmod
/// every newly created destination to mode `000`, so fall back to the POSIX
/// default instead of propagating a sentinel into `dest_mode()`.
///
/// Defence in depth only: [`init_orig_umask`] runs before any sandbox is
/// installed, so a correctly wired binary never reaches the fallback.
#[cfg(unix)]
const fn sanitize_umask(raw: u32) -> u32 {
    if raw & !0o777 == 0 {
        raw
    } else {
        FALLBACK_UMASK
    }
}

/// Returns the process umask captured by [`init_orig_umask`].
///
/// Falls back to capturing on first use for callers that never ran the entry
/// point (unit tests, library embedders). Production binaries prime this from
/// `main` so the value is resolved before any sandbox is installed.
#[cfg(unix)]
fn cached_umask() -> u32 {
    if let Some(umask) = ORIG_UMASK.get() {
        return *umask;
    }
    init_orig_umask();
    *ORIG_UMASK.get().unwrap_or(&0o022)
}

/// Returns the default permission seed for a child created under `parent`.
///
/// Mirrors upstream `generator.c:1349-1351` which calls `default_perms_for_dir(dn)`
/// when `--perms` is off. The helper folds the parent directory's POSIX default
/// ACL `user_obj`/`group_obj`/`other_obj` entries into the seed; when there is
/// no default ACL (or the filesystem does not support POSIX default ACLs) it
/// returns the umask-derived `ACCESSPERMS & ~orig_umask`.
///
/// upstream: `acls.c:1083-1139` `default_perms_for_dir`
/// upstream: `generator.c:1349-1352` per-parent `dflt_perms` lookup
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
/// upstream: generator.c:1740-1742 `dflt_perms = default_perms_for_dir(dn)`
/// upstream: generator.c:2770 `dflt_perms = (ACCESSPERMS & ~orig_umask)`
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
        let mut new_mode = source_mode & (!0o7777 | dflt_perms);
        if let Some(existing) = existing {
            // upstream: rsync.c:512-516 - a freshly-created directory that
            // inherited S_ISGID from a setgid parent keeps that bit even when
            // !preserve_perms strips it out of the dest_mode() result:
            // `if (inherit && S_ISDIR(new_mode) && sxp->st.st_mode & S_ISGID)`.
            if existing.file_type().is_dir() && (existing.permissions().mode() & 0o2000) != 0 {
                new_mode |= 0o2000;
            }
            // Skip the chmod if the mode already matches
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
/// Returns without acting when `-p` is in effect: that path drives the
/// chmod through `metadata.permissions().mode()` directly. A `--chmod`
/// without `--perms` stays on this path: upstream tweaks the flist mode at
/// build time (flist.c:1741-1742) and `dest_mode()` then still collapses it
/// against the pre-transfer destination, so the tweak must feed the exists
/// split here rather than bypass it.
///
/// upstream: receiver.c:964 (`dest_mode()` invocation) + rsync.c:449-472
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
    if options.permissions() {
        return Ok(());
    }

    // upstream: receiver.c:1176-1191 - the basis file is opened with O_NOFOLLOW
    // and the fd is dropped again unless it is a regular file, so a symlink /
    // fifo / device obstacle leaves `exists = fd1 != -1` false and the incoming
    // file takes the new-destination rule (its lstat mode - 0o755 for a symlink
    // on some platforms - must never become the file's permissions).
    let pre_transfer_meta = pre_transfer_meta.filter(|existing| existing.file_type().is_file());
    let new_mode = chmod_tweaked_dest_mode(
        options.chmod(),
        destination,
        source_metadata.permissions().mode(),
        false,
        true,
        options,
        pre_transfer_meta,
    );

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
/// upstream: rsync.c:set_file_attrs() - chmod path for direct permission copy
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
        // Only the read-only bit survives on Windows; warn once when the user
        // requested full POSIX modes, --chmod, or -E.
        super::platform_warn::warn_permissions_unsupported(options);
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
/// upstream: rsync.c:set_file_attrs() - skips chmod when mode already matches
#[cfg(unix)]
pub(super) fn permissions_match(target_mode: u32, existing: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    (existing.permissions().mode() & 0o7777) == (target_mode & 0o7777)
}

/// Applies the fake-super permission deflection for a computed mode.
///
/// Mirrors upstream `xattrs.c:set_stat_xattr()` under `am_root < 0`: the file's
/// intended mode (`fmode`, the full `S_IFMT` + chmod-applied mode) is compared
/// against the *real* on-disk mode forced to a self-accessible value -
/// `(fmode & ACCESSPERMS) | (S_ISDIR ? 0700 : 0600)` - so the destination stays
/// readable/writable during and after the transfer. Special bits
/// (setuid/setgid/sticky) are dropped from the real mode; they survive only in
/// the xattr. The normal chmod-to-`fmode` is skipped (upstream `rsync.c:660`).
///
/// The `user.rsync.%stat` xattr is only written when the real
/// mode/uid/gid/rdev cannot faithfully represent the intended values - i.e.
/// when `real_mode != fmode` (special bits or perms were dropped) or the
/// destination's on-disk owner/group differ from the intended ones. When the
/// real attributes already match, upstream writes no shim and removes any stale
/// `%stat` (`xattrs.c:1225-1237`), so an unprivileged same-owner copy of a
/// plain 0755 dir / 0644 file leaves no `%stat` behind.
/// upstream: xattrs.c:1188-1237 set_stat_xattr() - mode = (fmode & ACCESSPERMS)
///           | (S_ISDIR ? 0700 : 0600); write-or-remove based on faithfulness.
#[cfg(unix)]
fn apply_fake_super_mode(
    destination: &Path,
    fmode: u32,
    is_dir: bool,
    options: &MetadataOptions,
    existing: Option<&fs::Metadata>,
) -> Result<(), MetadataError> {
    const ACCESSPERMS: u32 = 0o777;
    const S_IFMT: u32 = 0o170000;

    // Recover the S_IFMT type bits (fmode may arrive with or without them).
    let type_bits = if fmode & S_IFMT != 0 {
        fmode & S_IFMT
    } else if is_dir {
        0o040000
    } else {
        0o100000
    };

    // upstream: xattrs.c:1219-1220 - enable full owner access, dump special bits.
    let real_mode = type_bits | (fmode & ACCESSPERMS) | if is_dir { 0o700 } else { 0o600 };

    #[cfg(feature = "xattr")]
    {
        use crate::fake_super::{load_fake_super, remove_fake_super, store_fake_super};
        use std::os::unix::fs::MetadataExt;

        // The intended stored mode carries the S_IFMT type bits so a later
        // fake-super read can rebuild both the type and the full perms.
        let stored_mode = type_bits | (fmode & 0o7777);

        // The ownership step recorded the intended uid/gid/rdev; reload them.
        let recorded = load_fake_super(destination).ok().flatten();
        let (want_uid, want_gid, want_rdev) = recorded
            .as_ref()
            .map(|s| (s.uid, s.gid, s.rdev))
            .unwrap_or((0, 0, None));

        // upstream: xattrs.c:1225-1229 - the shim is redundant when the real
        // (mode & type)==stored mode and the on-disk owner/group already equal
        // the intended values (rdev is 0 for non-devices). Compare against the
        // destination's actual on-disk owner/group.
        let dest_meta = fs::symlink_metadata(destination).ok();
        let (real_uid, real_gid) = dest_meta
            .as_ref()
            .map(|m| (m.uid(), m.gid()))
            .unwrap_or((want_uid, want_gid));

        let faithful = (real_mode & (S_IFMT | 0o7777)) == stored_mode
            && real_uid == want_uid
            && real_gid == want_gid
            && want_rdev.is_none();

        if faithful {
            // upstream: xattrs.c:1227-1233 - drop any stale %stat and skip write.
            remove_fake_super(destination).map_err(|error| {
                MetadataError::new("remove fake-super metadata", destination, error)
            })?;
        } else if let Some(mut stat) = recorded {
            if stat.mode != stored_mode {
                stat.mode = stored_mode;
                store_fake_super(destination, &stat).map_err(|error| {
                    MetadataError::new("store fake-super metadata", destination, error)
                })?;
            }
        }
    }

    if let Some(existing) = existing
        && permissions_match(real_mode, existing)
    {
        return Ok(());
    }

    // upstream: syscall.c:do_chmod_at() applied to the deflected real mode.
    chmod_path_honoring_keep_dirlinks(
        destination,
        real_mode & 0o7777,
        options,
        "preserve permissions",
    )
}

/// Applies permissions with optional chmod modifiers (path-based).
///
/// When chmod modifiers are configured, applies them on top of the base mode.
/// Otherwise delegates to [`apply_permissions_without_chmod`] for direct
/// permission copy or executability-only preservation.
/// upstream: rsync.c:set_file_attrs() - chmod with optional modifier chain
pub(super) fn apply_permissions_with_chmod(
    destination: &Path,
    metadata: &fs::Metadata,
    options: &MetadataOptions,
    existing: Option<&fs::Metadata>,
) -> Result<(), MetadataError> {
    // upstream: rsync.c:577-578 set_file_attrs() - under fake-super (am_root<0)
    // the intended mode is deflected into the xattr and the real mode is forced
    // self-accessible; the normal chmod is skipped.
    #[cfg(unix)]
    if options.fake_super_enabled() {
        let fmode = intended_fake_super_mode(destination, metadata, options, existing)?;
        return apply_fake_super_mode(
            destination,
            fmode,
            metadata.file_type().is_dir(),
            options,
            existing,
        );
    }

    #[cfg(unix)]
    {
        if let Some(modifiers) = options.chmod() {
            use std::os::unix::fs::PermissionsExt;
            let mut mode = chmod_tweaked_dest_mode(
                Some(modifiers),
                destination,
                metadata.permissions().mode(),
                metadata.is_dir(),
                metadata.file_type().is_file(),
                options,
                existing,
            );
            mode = tweak_directory_transfer_mode(mode, metadata.file_type());

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
        if metadata.file_type().is_dir() {
            // upstream: generator.c:1856 - even when !preserve_perms the
            // generator rewrites `file->mode = dest_mode(...)` for a
            // directory, judging `exists` by the pre-mkdir `statret`, and
            // set_file_attrs() (rsync.c:658) chmods the directory to it. The
            // caller's `existing` carries that pre-transfer stat: a fresh
            // directory takes the source mode masked by dflt_perms
            // (rsync.c:481-485) instead of the mkdir umask default, while a
            // pre-existing one keeps its own bits (rsync.c:470-480). The
            // during-transfer owner-rwx raise (generator.c:1904-1912) and its
            // touch_up_dirs restore (generator.c:2594) belong to the caller
            // that owns transfer ordering, not to this apply.
            let mut new_mode = directory_dest_mode(destination, source_mode, options, existing);
            if let Ok(current_meta) = fs::metadata(destination) {
                let current_mode = current_meta.permissions().mode();
                // upstream: rsync.c:512-516 - a freshly-created directory that
                // inherited S_ISGID from a setgid parent keeps that bit even
                // though dest_mode() dropped it.
                if existing.is_none() && current_mode & 0o2000 != 0 {
                    new_mode |= 0o2000;
                }
                if (current_mode & 0o7777) != new_mode {
                    // upstream: syscall.c:do_chmod():800-802 - when neither
                    // --perms nor --executability is active, chmod failure is
                    // non-fatal. upstream returns 0 so set_file_attrs()
                    // continues.
                    let _ = chmod_path_honoring_keep_dirlinks(
                        destination,
                        new_mode,
                        options,
                        "apply dest_mode",
                    );
                }
            }
        } else if let Some(new_mode) = compute_dest_mode(
            source_mode,
            options.destination_is_new(),
            existing,
            destination.parent(),
        ) {
            // upstream: syscall.c:do_chmod():800-802 - when neither --perms
            // nor --executability is active, chmod failure is non-fatal.
            // upstream returns 0 so set_file_attrs() continues.
            let _ = chmod_path_honoring_keep_dirlinks(
                destination,
                new_mode,
                options,
                "apply dest_mode",
            );
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

    // upstream: rsync.c:577-578 set_file_attrs() - fake-super deflects the mode
    // into the xattr and forces a self-accessible real mode; the open fd (which
    // could be a regular-file placeholder) is not used for the chmod so the
    // deflection stays path-based, matching set_stat_xattr().
    if options.fake_super_enabled() {
        let _ = fd;
        let fmode = intended_fake_super_mode(destination, metadata, options, existing)?;
        return apply_fake_super_mode(
            destination,
            fmode,
            metadata.file_type().is_dir(),
            options,
            existing,
        );
    }

    if let Some(modifiers) = options.chmod() {
        let mut mode = chmod_tweaked_dest_mode(
            Some(modifiers),
            destination,
            metadata.permissions().mode(),
            metadata.is_dir(),
            metadata.file_type().is_file(),
            options,
            existing,
        );
        mode = tweak_directory_transfer_mode(mode, metadata.file_type());

        if let Some(existing) = existing {
            if permissions_match(mode, existing) {
                return Ok(());
            }
        }

        if let Some(fd) = fd {
            fchmod_libc(fd, mode, destination, "preserve permissions")?;
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
            fchmod_libc(fd, mode, destination, "preserve permissions")?;
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
        // upstream: syscall.c:do_chmod():800-802 - when neither --perms
        // nor --executability is active, chmod failure is non-fatal.
        // upstream returns 0 so set_file_attrs() continues.
        if let Some(fd) = fd {
            let _ = fchmod_libc(fd, new_mode, destination, "apply dest_mode");
        } else {
            let _ = chmod_path_honoring_keep_dirlinks(
                destination,
                new_mode,
                options,
                "apply dest_mode",
            );
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
/// `generator.c:1356`'s `link_stat(fname, &sx.st, keep_dirlinks && is_dir)`.
///
/// Both branches remain visible to `fakeroot`: `secure_chmod_at` performs the
/// mode change with the libc `fchmodat(2)` symbol (only the parent-directory
/// walk uses `openat2`/`RESOLVE_NO_SYMLINKS`, which fakeroot ignores because it
/// tracks modes per inode on the chmod call, not on directory opens), and
/// `std::fs::set_permissions` uses the libc `chmod(2)` symbol. Both are
/// interposed by fakeroot's LD_PRELOAD wrapper, so no raw-syscall path bypasses
/// the faked mode here.
///
/// upstream: rsync.c:set_file_attrs() / generator.c:1356 link_stat
#[cfg(unix)]
fn chmod_path_honoring_keep_dirlinks(
    destination: &Path,
    mode: u32,
    options: &MetadataOptions,
    action: &'static str,
) -> Result<(), MetadataError> {
    if options.resolves_symlinked_parent(destination) {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(destination, fs::Permissions::from_mode(mode))
            .map_err(|error| MetadataError::new(action, destination, error))?;
    } else {
        fast_io::secure_chmod_at(destination, mode, true)
            .map_err(|error| MetadataError::new(action, destination, error))?;
    }
    Ok(())
}

/// Maps a symlink apply into [`chmod_tweaked_dest_mode`]'s `pre_transfer`
/// argument, i.e. `dest_mode()`'s `exists` input.
///
/// Both symlink apply paths run AFTER the link is on disk, so their own lstat
/// cannot tell "was here before" from "we just made it". `destination_is_new`
/// carries that answer down from the executor, which knows it: upstream's
/// `exists` is `statret == 0 && stype != FT_DIR` (generator.c:1938), the
/// generator's lstat from BEFORE `do_symlink` ran.
///
/// When the link is not new, its current stat IS the pre-transfer stat - except
/// for a destination that was REPLACED, where the fresh link's stat says
/// nothing about what upstream measured. `explicit` carries the caller's
/// pre-replace lstat for that case; upstream's `statret`/`sx.st` pair at
/// generator.c:1937-1940 is taken before `atomic_create` deletes the obstacle,
/// so a replaced destination - symlink or not - still feeds `dest_mode()` the
/// OLD mode. Callers that never replace anything pass `None` and get the
/// current stat.
#[cfg(unix)]
fn symlink_pre_transfer_stat<'a>(
    options: &MetadataOptions,
    current: &'a fs::Metadata,
    explicit: Option<&'a fs::Metadata>,
) -> Option<&'a fs::Metadata> {
    if options.destination_is_new() {
        None
    } else {
        explicit.or(Some(current))
    }
}

/// The single owner of "which permission bits does a symlink end up with".
///
/// A link is not a special case in upstream: it walks the SAME
/// `tweak_mode()`-then-`dest_mode()` pipeline every other type walks, and the
/// only two symlink-specific facts are which gates it fails. So this is a thin
/// adapter over [`chmod_tweaked_dest_mode`] that supplies those two facts as
/// arguments rather than restating the pipeline:
///
/// * **`modifiers = None` - the `!S_ISLNK` gate.** `--chmod` reaches
///   `tweak_mode()` at exactly three places and every one of them excludes a
///   link, so a link's mode is never tweaked:
///   - `flist.c:1741-1742` `send_file_name()` -
///     `if (chmod_modes && !S_ISLNK(file->mode) && file->mode)`
///   - `flist.c:996-997` `recv_file_entry()` -
///     `if (chmod_modes && !S_ISLNK(mode) && mode)`
///   - `rsync.c:647-648` `set_file_attrs()` (daemon `outgoing chmod`) -
///     `if (daemon_chmod_modes && !S_ISLNK(new_mode))`
/// * **`source_is_regular = false` - the `S_ISREG` gate.** `dest_mode()`'s
///   `-E` tweak is `if (preserve_executability && S_ISREG(flist_mode))`
///   (rsync.c:472), so `-E` contributes NOTHING to a link's mode. (`-E` still
///   drives the `p` COLUMN through `perms_differ()`; that is the caller's
///   business, not this one's.)
///
/// The `dest_mode()` collapse itself is not symlink-specific either:
/// `generator.c:1937-1940` runs
/// `file->mode = dest_mode(file->mode, sx.st.st_mode, dflt_perms, exists)` for
/// every type, sitting ABOVE the `preserve_links && ftype == FT_SYMLINK` branch
/// at generator.c:1948, so a link takes the same two arms as a file - keep the
/// destination's own bits when it already existed (rsync.c:470-480), else mask
/// the sender's bits with `dflt_perms` and drop the special bits
/// (rsync.c:481-485).
///
/// `pre_transfer` is the destination's PRE-transfer stat, exactly as
/// [`chmod_tweaked_dest_mode`] means it: `None` says the link is new.
///
/// The result is the full `CHMOD_BITS` the link should end up with; when it
/// already matches, the caller's `current != target` test turns the whole thing
/// into the no-op `BITS_EQUAL(sxp->st.st_mode, new_mode, CHMOD_BITS)` produces
/// upstream (rsync.c:807).
#[cfg(unix)]
fn symlink_target_mode(
    destination: &Path,
    source_mode: u32,
    options: &MetadataOptions,
    pre_transfer: Option<&fs::Metadata>,
) -> u32 {
    chmod_tweaked_dest_mode(
        None, // !S_ISLNK: --chmod never reaches a link
        destination,
        source_mode,
        false, // a link is never S_ISDIR
        false, // !S_ISREG: dest_mode()'s -E tweak never fires for a link
        options,
        pre_transfer,
    ) & 0o7777
}

/// Chmods a symbolic link's own permission bits, matching the itemize `p`
/// decision the receiver reports from a `FileEntry`.
///
/// Only runs where [`crate::CAN_CHMOD_SYMLINK`] holds (macOS/BSD). The mode is
/// decided by [`symlink_target_mode`]. The chmod uses
/// `fchmodat(AT_SYMLINK_NOFOLLOW)` via [`fast_io::secure_chmod_at`]
/// (`follow_symlinks = false`) so the link itself, not its target, is modified.
///
/// upstream: rsync.c:806-822 (`set_file_attrs()` chmods every file type with
/// no `S_ISLNK` gate) + syscall.c `do_chmod_at()`. The symlink chmod is a SOFT
/// outcome (`rsync.c:819`, `ret == 1`), so any failure is swallowed and never
/// propagated - exactly as an unsupported symlink chmod is on any platform
/// that compiled the const to `true` but hit a filesystem that refuses it at
/// runtime.
pub(super) fn apply_symlink_permissions_from_entry(
    destination: &Path,
    entry: &protocol::flist::FileEntry,
    options: &MetadataOptions,
    cached_meta: Option<&fs::Metadata>,
) -> Result<(), MetadataError> {
    #[cfg(unix)]
    if crate::CAN_CHMOD_SYMLINK {
        use std::os::unix::fs::PermissionsExt;

        let owned_meta;
        let meta = match cached_meta {
            Some(meta) => meta,
            None => match fs::symlink_metadata(destination) {
                Ok(meta) => {
                    owned_meta = meta;
                    &owned_meta
                }
                Err(_) => return Ok(()),
            },
        };
        let current = meta.permissions().mode() & 0o7777;
        let target = symlink_target_mode(
            destination,
            entry.mode(),
            options,
            // The receiver path has no replace-an-obstacle caller yet; when it
            // grows one it must pass the obstacle's pre-replace lstat here.
            symlink_pre_transfer_stat(options, meta, None),
        );
        if current != target {
            let _ = fast_io::secure_chmod_at(destination, target, false);
        }
    }
    #[cfg(not(unix))]
    let _ = (destination, entry, options, cached_meta);
    Ok(())
}

/// Chmods a symbolic link's own permission bits from a source [`fs::Metadata`],
/// matching the local-copy change-set `p` decision.
///
/// The local-copy counterpart to [`apply_symlink_permissions_from_entry`];
/// both defer to [`symlink_target_mode`], so the local and receiver paths can
/// never drift. In particular `--chmod` is NOT layered on here, and without
/// `-p` the mode is upstream's `dest_mode()` result rather than the umask
/// default the `symlink(2)` call left behind - see [`symlink_target_mode`] for
/// both halves. Only runs where [`crate::CAN_CHMOD_SYMLINK`] holds; the chmod
/// is a SOFT outcome and any error is swallowed.
///
/// `pre_transfer_meta` is the destination's lstat from BEFORE the executor
/// replaced an obstacle with the link, or `None` when nothing was replaced -
/// see [`symlink_pre_transfer_stat`] for why the fresh link's own stat cannot
/// stand in for it.
///
/// upstream: rsync.c:806-822 + syscall.c `do_chmod_at()`.
pub(super) fn apply_symlink_permissions_like(
    destination: &Path,
    source_metadata: &fs::Metadata,
    options: &MetadataOptions,
    pre_transfer_meta: Option<&fs::Metadata>,
) -> Result<(), MetadataError> {
    #[cfg(unix)]
    if crate::CAN_CHMOD_SYMLINK {
        use std::os::unix::fs::PermissionsExt;

        let meta = match fs::symlink_metadata(destination) {
            Ok(meta) => meta,
            Err(_) => return Ok(()),
        };
        let current = meta.permissions().mode() & 0o7777;
        let source = source_metadata.permissions().mode();
        let target = symlink_target_mode(
            destination,
            source,
            options,
            symlink_pre_transfer_stat(options, &meta, pre_transfer_meta),
        );
        if current != target {
            let _ = fast_io::secure_chmod_at(destination, target, false);
        }
    }
    #[cfg(not(unix))]
    let _ = (destination, source_metadata, options, pre_transfer_meta);
    Ok(())
}

/// Computes the intended full mode (`S_IFMT` + perms) that upstream's
/// `set_file_attrs()` would chmod a non-fake-super destination to.
///
/// This is the `new_mode` fed to `set_stat_xattr()` under `am_root < 0`: the
/// `--chmod` tweak composed with the `dest_mode()` collapse (tweak first,
/// per [`chmod_tweaked_dest_mode`]). When `--perms` is active the (tweaked)
/// source mode passes through; without `--perms` or `--chmod` the plain
/// [`compute_dest_mode`] reduction supplies the mode, so the recorded xattr
/// reflects the same mode a privileged transfer would have applied on disk.
/// upstream: rsync.c:495-519 set_file_attrs() new_mode / dest_mode + tweak_mode
#[cfg(unix)]
fn intended_fake_super_mode(
    destination: &Path,
    metadata: &fs::Metadata,
    options: &MetadataOptions,
    existing: Option<&fs::Metadata>,
) -> Result<u32, MetadataError> {
    use std::os::unix::fs::PermissionsExt;

    let mode = if options.permissions() || options.chmod().is_some() {
        chmod_tweaked_dest_mode(
            options.chmod(),
            destination,
            metadata.permissions().mode(),
            metadata.is_dir(),
            metadata.file_type().is_file(),
            options,
            existing,
        )
    } else {
        let source_mode = metadata.permissions().mode();
        compute_dest_mode(
            source_mode,
            options.destination_is_new(),
            existing,
            destination.parent(),
        )
        .unwrap_or(source_mode)
    };
    Ok(mode)
}

/// Computes the permission bits (`0o7777`) a directory would receive under
/// `--chmod` - the tweak composed with the `dest_mode()` collapse when
/// `!preserve_perms` - BEFORE upstream's during-transfer owner-`rwx` fixup.
///
/// `existing` is the directory's PRE-transfer stat: an existing destination
/// directory keeps its own bits when `--perms` is off (rsync.c:470-471), so
/// only a fresh directory (or a `--perms` transfer) can self-lock on the
/// tweaked mode.
///
/// Returns `None` when no `--chmod` modifiers are configured. The local-copy
/// executor uses this to detect a transfer-root directory whose tweaked mode
/// strips owner execute and therefore self-locks (see
/// [`crate::transfer_root_self_locks`]).
/// upstream: rsync.c:set_file_attrs() new_mode, pre generator.c:1904-1912 fixup.
#[cfg(unix)]
pub(super) fn chmod_directory_target_mode(
    destination: &Path,
    metadata: &fs::Metadata,
    options: &MetadataOptions,
    existing: Option<&fs::Metadata>,
) -> Result<Option<u32>, MetadataError> {
    let Some(modifiers) = options.chmod() else {
        return Ok(None);
    };
    use std::os::unix::fs::PermissionsExt;
    let mode = chmod_tweaked_dest_mode(
        Some(modifiers),
        destination,
        metadata.permissions().mode(),
        metadata.is_dir(),
        metadata.file_type().is_file(),
        options,
        existing,
    );
    Ok(Some(mode & 0o7777))
}

/// The single owner of "which permission bits does a directory end up with"
/// BEFORE upstream's during-transfer owner-`rwx` raise.
///
/// A directory walks the same `tweak_mode()`-then-`dest_mode()` pipeline every
/// other type walks, so this is a thin adapter over
/// [`chmod_tweaked_dest_mode`] supplying the two directory facts: `--chmod`
/// DOES reach a directory (the `!S_ISLNK` gates at flist.c:1741-1742 and
/// flist.c:996-997 pass it through, with `is_dir = true` selecting the `D`
/// clauses), and `dest_mode()`'s `-E` tweak never fires
/// (`S_ISREG(flist_mode)`, rsync.c:472).
///
/// `pre_transfer` is the destination's PRE-transfer stat - upstream judges a
/// directory's `exists` by the pre-mkdir `statret` (generator.c:1856), so a
/// fresh directory takes `flist_mode & (~CHMOD_BITS | dflt_perms)`
/// (rsync.c:481-485) and a pre-existing one keeps its own bits
/// (rsync.c:470-480). With `--perms` the (tweaked) source bits pass through.
///
/// The result deliberately EXCLUDES the during-transfer raise
/// (generator.c:1904-1912) and its `touch_up_dirs` restore
/// (generator.c:2594): those belong to the transfer-ordering owner - the
/// local-copy executor and the network receiver each drive them from this one
/// target value.
#[cfg(unix)]
pub(super) fn directory_dest_mode(
    destination: &Path,
    source_mode: u32,
    options: &MetadataOptions,
    pre_transfer: Option<&fs::Metadata>,
) -> u32 {
    chmod_tweaked_dest_mode(
        options.chmod(),
        destination,
        source_mode,
        true,  // S_ISDIR: --chmod `D` clauses apply
        false, // !S_ISREG: dest_mode()'s -E tweak never fires for a dir
        options,
        pre_transfer,
    ) & 0o7777
}

/// Composes the `--chmod` tweak with upstream's `dest_mode()` collapse:
/// tweak FIRST, collapse SECOND.
///
/// Upstream applies `--chmod` (CLI or daemon `incoming chmod = ...`) to the
/// flist mode when the list is built (flist.c:1741-1742 sender,
/// flist.c:996-997 `recv_file_entry`). Only then, when `!preserve_perms`,
/// does `dest_mode()` (rsync.c:464-486) collapse the result: an existing
/// destination keeps its own permission bits - the tweak is discarded -
/// while a fresh one masks the tweaked mode by `dflt_perms` and drops the
/// special bits. Inverting the order would apply the tweak to the
/// destination's bits, which upstream never does. The destination tempfile
/// mode is never the baseline either: reading it back would feed the
/// `O_TMPFILE` 0o600 default into the chmod chain (testsuite `chmod-option`
/// daemon upload).
///
/// `pre_transfer` is the destination's PRE-transfer stat - `dest_mode()`'s
/// `stat_mode`/`exists` inputs. `None` means the destination is new.
#[cfg(unix)]
fn chmod_tweaked_dest_mode(
    modifiers: Option<&crate::ChmodModifiers>,
    destination: &Path,
    source_mode: u32,
    source_is_dir: bool,
    source_is_regular: bool,
    options: &MetadataOptions,
    pre_transfer: Option<&fs::Metadata>,
) -> u32 {
    use std::os::unix::fs::PermissionsExt;

    let tweaked = match modifiers {
        Some(modifiers) => modifiers.apply(source_mode, source_is_dir),
        None => source_mode,
    };
    if options.permissions() {
        // upstream: receiver.c:1181 / generator.c:1855 - `dest_mode()` only
        // runs when `!preserve_perms`; with `--perms` the tweaked mode is
        // applied as-is.
        return tweaked;
    }

    if let Some(existing) = pre_transfer {
        // upstream: rsync.c:470-482 - an existing destination keeps its own
        // permission bits with the type bits from the (tweaked) flist mode.
        let mut new_mode = (tweaked & !0o7777) | (existing.permissions().mode() & 0o7777);
        if options.executability() && source_is_regular {
            // upstream: rsync.c:472-481 dest_mode() - for existing regular
            // files only, copy the (tweaked) source's exec presence: if it
            // has no exec bits, clear them on dest; else if dest has no exec
            // bits, grant exec to everyone who can already read
            // (`new_mode & 0444 >> 2`). Upstream skips this for new files -
            // the umask-masked source mode already encodes the right answer.
            if tweaked & 0o111 == 0 {
                new_mode &= !0o111;
            } else if new_mode & 0o111 == 0 {
                new_mode |= (new_mode & 0o444) >> 2;
            }
        }
        new_mode
    } else {
        // upstream: rsync.c:483-485 - fresh destination: mask by
        // `dflt_perms = default_perms_for_dir(dn)`, which folds the parent's
        // POSIX default ACL when one is present (acls.c:1083) and otherwise
        // reduces to `ACCESSPERMS & ~orig_umask`; special bits drop out.
        tweaked & (!0o7777 | default_perms_seed(destination.parent()))
    }
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
/// upstream: rsync.c:set_file_attrs() - receiver-side permission application
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
            // upstream: receiver.c:964 - even when `!preserve_perms` and
            // `!preserve_executability`, the receiver mutates `file->mode` via
            // `dest_mode()` and `set_file_attrs()` chmods the post-rename
            // destination to it. For an existing destination this preserves
            // the prior mode (so a re-transfer never silently downgrades to
            // the temp file's `0o600`/umask-default permissions); for a new
            // destination it applies `source_mode & (~CHMOD_BITS | dflt_perms)`.
            // Only the regular-file branch ships - upstream restricts the
            // `S_ISREG(flist_mode)` chmod-on-rename loop to data files.
            //
            // `pre_transfer_meta.is_some()` marks an existing destination:
            // apply the exists=true `dest_mode()` (keep the prior perm bits).
            // `cached_meta.is_none()` marks a freshly-committed file: every
            // receiver commit path (pipelined disk-commit, streaming sync,
            // alt-dest materialise) passes `cached_meta = None` to apply
            // unconditionally, so a brand-new file - which correctly has NO
            // pre-transfer stat - still gets the exists=false `dest_mode()`
            // (`source_mode & (~CHMOD_BITS | dflt_perms)`). Without this, a
            // network-received new file kept the temp file's `0o600` creation
            // mode over ssh/daemon instead of the umask-masked source mode a
            // local copy and upstream both land.
            //
            // Skip only when a cached post-rename stat is present but no
            // pre-transfer stat is (public API / quick-check skip on an
            // untouched existing file): the file already exists, upstream
            // keeps its bits, and applying the new-file formula would wrongly
            // mask them.
            if entry.file_type().is_regular()
                && (pre_transfer_meta.is_some() || cached_meta.is_none())
            {
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
                    // upstream: syscall.c:do_chmod():800-802 - when neither
                    // --perms nor --executability is active, chmod failure is
                    // non-fatal. upstream returns 0 so set_file_attrs() continues.
                    let _ = chmod_path_honoring_keep_dirlinks(
                        destination,
                        new_mode,
                        options,
                        "apply dest_mode",
                    );
                }
            } else if entry.file_type().is_dir() {
                // upstream: generator.c:1856 - even when !preserve_perms
                // the generator runs `file->mode = dest_mode(...)` for
                // directories, and set_file_attrs() (rsync.c:658) chmods
                // the dir to it. A new dir therefore lands the source mode
                // masked by dflt_perms (so a source 0700 dir stays 0700 rather
                // than the mkdir umask default); an existing dir keeps its own
                // permission bits (pre_transfer_meta = Some -> the exists=true
                // branch). Without this, a network-received dir was created
                // `mkdirat(0o777)` and never re-chmod'd, landing 0o755.
                let mut new_mode = directory_dest_mode(
                    destination,
                    entry.permissions(),
                    options,
                    pre_transfer_meta,
                );
                let fresh_meta;
                let current_meta = if let Some(meta) = cached_meta {
                    meta
                } else {
                    fresh_meta = fs::metadata(destination).map_err(|error| {
                        MetadataError::new("inspect destination permissions", destination, error)
                    })?;
                    &fresh_meta
                };
                let current_mode = current_meta.permissions().mode();
                // upstream: rsync.c:512-516 - a freshly-created dir (no
                // pre-transfer stat) that inherited S_ISGID from a setgid
                // parent keeps that bit even though dest_mode() dropped it.
                if pre_transfer_meta.is_none() && (current_mode & 0o2000) != 0 {
                    new_mode |= 0o2000;
                }
                // upstream: generator.c:1904-1912 - a non-root transfer raises
                // a directory lacking full owner-rwx to `mode | S_IRWXU` at its
                // first visit so its contents can still be written; the gate is
                // `!am_root && (file->mode & S_IRWXU) != S_IRWXU &&
                // dir_tweaking` with NO --perms condition (dir_tweaking =
                // !(list_only || solo_file || dry_run), generator.c:2743).
                // touch_up_dirs (generator.c:2594) later restores the strict
                // mode only when `!(file->mode & S_IWUSR)`; the receiver
                // records that restore, so the raised mode is what lands here.
                if !options.fake_super_enabled()
                    && !nix::unistd::geteuid().is_root()
                    && (new_mode & 0o700) != 0o700
                {
                    new_mode |= 0o700;
                }
                if (current_mode & 0o7777) != (new_mode & 0o7777) {
                    // upstream: syscall.c:do_chmod():800-802 - when neither
                    // --perms nor --executability is active, chmod failure is
                    // non-fatal. upstream returns 0 so set_file_attrs() continues.
                    let _ = chmod_path_honoring_keep_dirlinks(
                        destination,
                        new_mode,
                        options,
                        "apply dest_mode",
                    );
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
                // dirfd opened with RESOLVE_NO_SYMLINKS so a
                // symlink swapped into any parent component cannot redirect
                // the chmod outside the receiver's confinement (testsuite
                // chdir-symlink-race). Under `--keep-dirlinks` the helper
                // follows symlinked parents to mirror upstream `generator.c:1356`.
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
            // upstream: flist.c:996-997 - `recv_file_entry` runs
            // `tweak_mode(mode, chmod_modes)` while the flist is built, and
            // `dest_mode()` (rsync.c:464-486) then collapses the TWEAKED
            // mode when `!preserve_perms`. The chmod baseline is therefore
            // the entry's mode, NEVER the destination's tempfile mode -
            // reading the destination would feed back the `O_TMPFILE` 0o600
            // default for fresh transfers and produce 0o600 under e.g.
            // `Fo-x` instead of the expected umask default (UTS-17.REOPEN:
            // testsuite/chmod-option daemon upload).
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

            // The exists split consumes the PRE-transfer destination stat
            // (upstream receiver.c:1181-1192 judges `exists` by the basis
            // fd). When the caller tracked none (quick-check skip, public
            // `apply_metadata_from_file_entry` API), no rename happened and
            // the cached current stat IS the pre-transfer stat.
            let new_mode = chmod_tweaked_dest_mode(
                Some(chmod),
                destination,
                entry.permissions(),
                entry.file_type().is_dir(),
                entry.file_type().is_regular(),
                options,
                pre_transfer_meta.or(cached_meta),
            );
            if (new_mode & 0o7777) != (current_mode & 0o7777) {
                // upstream: syscall.c:do_chmod_at() symlink-race-safe variant.
                // Helper follows symlinked parents under `--keep-dirlinks` to
                // mirror upstream `generator.c:1356`.
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
                // mirror upstream `generator.c:1356`.
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
        // Only the read-only bit survives on Windows; warn once when the user
        // requested full POSIX modes, --chmod, or -E.
        super::platform_warn::warn_permissions_unsupported(options);
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

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use crate::MetadataOptions;
    use tempfile::tempdir;

    /// upstream: syscall.c:do_chmod():800-802 - when neither --perms nor
    /// --executability is active, do_chmod returns 0 on failure.
    /// Verify that a chmod ENOENT is swallowed in the !perms path.
    #[test]
    fn chmod_failure_swallowed_without_perms_or_executability() {
        let dir = tempdir().expect("tempdir");
        let source = dir.path().join("src.txt");
        std::fs::write(&source, b"data").expect("write");

        let source_meta = std::fs::metadata(&source).expect("metadata");

        // Destination that does not exist - chmod will fail with ENOENT.
        let dest = dir.path().join("nonexistent.txt");

        let options = MetadataOptions::new()
            .preserve_permissions(false)
            .preserve_executability(false)
            .preserve_times(false)
            .with_destination_is_new(true);

        // With the fix, this should succeed because chmod failure is non-fatal
        // when neither -p nor -E is set.
        let result = apply_permissions_with_chmod(&dest, &source_meta, &options, None);
        assert!(result.is_ok(), "expected Ok, got: {result:?}");
    }

    /// When --perms IS active, chmod failure must propagate.
    #[test]
    fn chmod_failure_propagates_with_perms() {
        let dir = tempdir().expect("tempdir");
        let source = dir.path().join("src.txt");
        std::fs::write(&source, b"data").expect("write");

        let source_meta = std::fs::metadata(&source).expect("metadata");

        let dest = dir.path().join("nonexistent.txt");

        let options = MetadataOptions::new()
            .preserve_permissions(true)
            .preserve_executability(false)
            .preserve_times(false)
            .with_destination_is_new(true);

        let result = apply_permissions_with_chmod(&dest, &source_meta, &options, None);
        assert!(result.is_err(), "expected Err with -p active, got Ok");
    }

    /// A real umask must survive verbatim: the sanitiser exists to reject a
    /// failed query, not to second-guess the process's actual mask.
    #[cfg(unix)]
    #[test]
    fn sanitize_umask_passes_every_real_umask_through() {
        for raw in 0..=0o777u32 {
            assert_eq!(
                super::sanitize_umask(raw),
                raw,
                "{raw:o} is a valid 9-bit umask and must not be rewritten",
            );
        }
    }

    /// `umask(2)` failing returns `-1`, which as a `u32` is `u32::MAX`. Caching
    /// it would make `dflt_perms` = `0o777 & !u32::MAX` = 0 and chmod every new
    /// destination to mode 000, so it must be rejected. This is the value a
    /// seccomp filter answering `EPERM` actually produced on the daemon
    /// receiver.
    #[cfg(unix)]
    #[test]
    fn sanitize_umask_rejects_a_failed_query() {
        assert_eq!(super::sanitize_umask(u32::MAX), super::FALLBACK_UMASK);
        assert_ne!(
            0o777 & !super::sanitize_umask(u32::MAX),
            0,
            "a rejected sentinel must not yield dflt_perms == 0",
        );
        // Any value with bits above the 9 umask bits is equally impossible.
        assert_eq!(super::sanitize_umask(0o1000), super::FALLBACK_UMASK);
    }

    /// `symlink_target_mode()` table pins.
    ///
    /// The absolute mode a NEW link ends up with depends on the process umask,
    /// and that dependence is proven against the real 3.5.0 binary at the CLI
    /// level (source 0o777 under umask 022 / 077 / 000 gives 755 / 700 / 777 in
    /// both). What is pinned here is the structure upstream fixes regardless of
    /// umask: which `dest_mode()` arm a link takes, that `--chmod` never
    /// reaches it, and that `-E` never reaches it.
    fn symlink_opts(perms: bool, exec: bool, is_new: bool) -> MetadataOptions {
        MetadataOptions::new()
            .preserve_permissions(perms)
            .preserve_executability(exec)
            .preserve_times(false)
            .with_destination_is_new(is_new)
    }

    /// upstream: `dest_mode()` only runs under `!preserve_perms`
    /// (generator.c:1937), so with `-p` the link takes the sender's bits
    /// verbatim - no umask reduction, and no `--chmod`.
    #[test]
    fn symlink_target_mode_under_preserve_perms_is_the_source_bits_verbatim() {
        let dir = tempdir().expect("tempdir");
        let link = dir.path().join("l");
        let chmod = crate::ChmodModifiers::parse("go-rwx").expect("parse chmod");

        for source in [0o777u32, 0o700, 0o644, 0o711] {
            let plain = symlink_opts(true, false, true);
            assert_eq!(
                symlink_target_mode(&link, 0o120000 | source, &plain, None),
                source,
                "-p must hand back the source bits {source:o} untouched"
            );
            let with_chmod = symlink_opts(true, false, true).with_chmod(Some(chmod.clone()));
            assert_eq!(
                symlink_target_mode(&link, 0o120000 | source, &with_chmod, None),
                source,
                "--chmod must not reach a link (upstream gates tweak_mode on \
                 !S_ISLNK at flist.c:1741-1742); go-rwx would have given {:o}",
                source & 0o700
            );
        }
    }

    /// upstream: rsync.c:470-480 - `dest_mode()`'s `exists` arm returns
    /// `(flist_mode & ~CHMOD_BITS) | (stat_mode & CHMOD_BITS)`, so a link that
    /// was already there keeps its own bits and `BITS_EQUAL` at rsync.c:807
    /// makes the chmod a no-op. `-E` cannot change that: its tweak is nested
    /// under `S_ISREG(flist_mode)` (rsync.c:472).
    ///
    /// Both `-E` directions are exercised, because each fires only one of
    /// upstream's two branches and a fixture that trips neither would pass
    /// with the `S_ISREG` gate removed:
    ///   * dest has NO exec, source HAS exec -> `new_mode |= (new_mode & 0444) >> 2`
    ///     would grant 0o644 -> 0o755;
    ///   * dest HAS exec, source has NO exec -> `new_mode &= ~0111` would strip
    ///     0o755 -> 0o644.
    ///
    /// A link must show neither.
    #[test]
    fn symlink_target_mode_existing_link_keeps_its_own_bits() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().expect("tempdir");
        let chmod = crate::ChmodModifiers::parse("a=rw").expect("parse chmod");

        // (dest bits, source mode, what the S_ISREG-gated -E tweak would do)
        let cases = [(0o644u32, 0o120777u32, 0o755u32), (0o755, 0o120600, 0o644)];

        for (dest_bits, source_mode, would_be) in cases {
            let link = dir.path().join(format!("l{dest_bits:o}"));
            std::fs::write(&link, b"x").expect("write");
            std::fs::set_permissions(&link, std::fs::Permissions::from_mode(dest_bits))
                .expect("seed dest bits");
            let on_disk = std::fs::metadata(&link).expect("meta");

            for (exec, label) in [(false, "no -p, no -E"), (true, "-E, no -p")] {
                let opts = symlink_opts(false, exec, false);
                assert_eq!(
                    symlink_target_mode(&link, source_mode, &opts, Some(&on_disk)),
                    dest_bits,
                    "{label}: an existing link keeps its own {dest_bits:o}; the \
                     S_ISREG-gated -E tweak would have made it {would_be:o}"
                );
                let opts = opts.with_chmod(Some(chmod.clone()));
                assert_eq!(
                    symlink_target_mode(&link, source_mode, &opts, Some(&on_disk)),
                    dest_bits,
                    "{label} + --chmod=a=rw: still {dest_bits:o}, never 0o666"
                );
            }
        }
    }

    /// upstream: rsync.c:481-485 - the new arm is
    /// `flist_mode & (~CHMOD_BITS | dflt_perms)`. Two umask-independent
    /// consequences are pinned here: the mask can only remove bits the source
    /// had, and the special bits always drop out ("turn off special
    /// permissions", rsync.c:482-483).
    #[test]
    fn symlink_target_mode_new_link_masks_the_source_and_drops_special_bits() {
        let dir = tempdir().expect("tempdir");
        let link = dir.path().join("l");
        let dflt = default_perms_seed(link.parent());
        let opts = symlink_opts(false, false, true);

        for source in [0o777u32, 0o700, 0o644, 0o711, 0o755] {
            let got = symlink_target_mode(&link, 0o120000 | source, &opts, None);
            assert_eq!(got, source & dflt, "new link takes source & dflt_perms");
            assert_eq!(got & !source, 0, "the mask can only remove bits");
        }

        let got = symlink_target_mode(&link, 0o120000 | 0o4755, &opts, None);
        assert_eq!(got & 0o7000, 0, "suid/sgid/sticky must not survive");
    }

    /// `-E` alone must not synthesise a mode for a link: upstream's `-E` tweak
    /// lives inside `dest_mode()` under `S_ISREG(flist_mode)` (rsync.c:472), so
    /// a link under `-E` takes the plain arm, never an exec blend of the
    /// destination's bits with the source's.
    #[test]
    fn symlink_target_mode_executability_never_blends_a_link() {
        let dir = tempdir().expect("tempdir");
        let link = dir.path().join("l");
        let dflt = default_perms_seed(link.parent());

        let opts = symlink_opts(false, true, true);
        assert_eq!(
            symlink_target_mode(&link, 0o120700, &opts, None),
            0o700 & dflt,
            "-E on a new link is the plain dest_mode() arm; an exec blend from \
             a 0o755 link would have produced 0o744"
        );
    }

    /// upstream: generator.c:1937-1940 reads `sx.st.st_mode` BEFORE
    /// `atomic_create` (generator.c:2002) deletes an obstacle, so a link that
    /// replaced one must feed `dest_mode()` the OBSTACLE's old bits, not the
    /// umask default the fresh `symlink(2)` left behind. `explicit` carries
    /// that pre-replace lstat; absent it, the current stat stands in; and a
    /// NEW destination takes no pre-transfer stat at all.
    #[test]
    fn symlink_pre_transfer_stat_prefers_the_explicit_obstacle_stat() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().expect("tempdir");
        let current_path = dir.path().join("current");
        let obstacle_path = dir.path().join("obstacle");
        std::fs::write(&current_path, b"x").expect("write current");
        std::fs::write(&obstacle_path, b"x").expect("write obstacle");
        std::fs::set_permissions(&current_path, std::fs::Permissions::from_mode(0o755))
            .expect("seed current");
        std::fs::set_permissions(&obstacle_path, std::fs::Permissions::from_mode(0o600))
            .expect("seed obstacle");
        let current = std::fs::metadata(&current_path).expect("current meta");
        let obstacle = std::fs::metadata(&obstacle_path).expect("obstacle meta");

        let existing = symlink_opts(false, false, false);
        let got = symlink_pre_transfer_stat(&existing, &current, Some(&obstacle))
            .expect("existing destination has a pre-transfer stat");
        assert_eq!(
            got.permissions().mode() & 0o7777,
            0o600,
            "a replaced destination must hand dest_mode() the obstacle's bits"
        );
        let got = symlink_pre_transfer_stat(&existing, &current, None)
            .expect("existing destination has a pre-transfer stat");
        assert_eq!(
            got.permissions().mode() & 0o7777,
            0o755,
            "with nothing replaced, the current stat is its own pre-transfer stat"
        );
        let new = symlink_opts(false, false, true);
        assert!(
            symlink_pre_transfer_stat(&new, &current, Some(&obstacle)).is_none(),
            "destination_is_new wins: a brand-new destination has no \
             pre-transfer stat, whatever the caller passes"
        );
    }

    /// `directory_dest_mode()` table pins, mirroring the symlink pins above:
    /// which `dest_mode()` arm a directory takes, that `--chmod`'s `D` clauses
    /// DO reach it (contrast with a link), and that the result excludes the
    /// during-transfer owner-rwx raise.
    ///
    /// upstream: generator.c:1856 (`exists` judged by the pre-mkdir statret) +
    /// rsync.c:464-486 dest_mode().
    #[test]
    fn directory_dest_mode_new_dir_masks_the_source_and_drops_special_bits() {
        let dir = tempdir().expect("tempdir");
        let dest = dir.path().join("d");
        let dflt = default_perms_seed(dest.parent());
        let opts = MetadataOptions::new()
            .preserve_permissions(false)
            .preserve_times(false);

        for source in [0o777u32, 0o750, 0o644, 0o500] {
            let got = directory_dest_mode(&dest, 0o040000 | source, &opts, None);
            assert_eq!(got, source & dflt, "new dir takes source & dflt_perms");
            assert_eq!(got & !source, 0, "the mask can only remove bits");
        }

        // upstream: rsync.c:482-483 - "turn off special permissions".
        let got = directory_dest_mode(&dest, 0o042755, &opts, None);
        assert_eq!(got & 0o7000, 0, "setgid/setuid/sticky must not survive");
    }

    /// upstream: rsync.c:470-480 - the exists arm keeps the destination
    /// directory's own permission bits; the raise/restore dance is the
    /// caller's business and must not leak into this value.
    #[test]
    fn directory_dest_mode_existing_dir_keeps_its_own_bits() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().expect("tempdir");
        let dest = dir.path().join("d");
        std::fs::create_dir(&dest).expect("create dir");
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o555))
            .expect("seed dest bits");
        let on_disk = std::fs::metadata(&dest).expect("meta");
        let opts = MetadataOptions::new()
            .preserve_permissions(false)
            .preserve_times(false);

        assert_eq!(
            directory_dest_mode(&dest, 0o040755, &opts, Some(&on_disk)),
            0o555,
            "an existing dir keeps its own bits, not the sender's 0o755"
        );
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755)).expect("unlock");
    }

    /// upstream: flist.c:1741-1742 gates `tweak_mode()` on `!S_ISLNK`, so a
    /// directory IS tweaked (with `is_dir` selecting the `D` clauses), and with
    /// `--perms` the tweaked mode passes through with no collapse
    /// (generator.c:1856 runs only under `!preserve_perms`).
    #[test]
    fn directory_dest_mode_chmod_reaches_a_directory() {
        let dir = tempdir().expect("tempdir");
        let dest = dir.path().join("d");
        let chmod = crate::ChmodModifiers::parse("go-rwx").expect("parse chmod");

        let perms = MetadataOptions::new()
            .preserve_permissions(true)
            .preserve_times(false)
            .with_chmod(Some(chmod.clone()));
        assert_eq!(
            directory_dest_mode(&dest, 0o040755, &perms, None),
            0o700,
            "--chmod=go-rwx must reach a directory under -p"
        );

        let no_perms = MetadataOptions::new()
            .preserve_permissions(false)
            .preserve_times(false)
            .with_chmod(Some(chmod));
        let dflt = default_perms_seed(dest.parent());
        assert_eq!(
            directory_dest_mode(&dest, 0o040755, &no_perms, None),
            0o700 & dflt,
            "tweak FIRST, dest_mode() collapse SECOND (flist.c:1741-1742)"
        );
    }
}
