//! Ownership resolution and chown operations.
//!
//! Handles UID/GID resolution with overrides, mappings, and numeric-id rules,
//! plus path-based and fd-based chown application. Includes fake-super support
//! for non-root ownership preservation via extended attributes.

use crate::error::MetadataError;
use crate::options::MetadataOptions;
use std::fs;
use std::path::Path;

#[cfg(unix)]
use crate::id_lookup::{lookup_group_by_name, lookup_user_by_name, map_gid, map_uid};
#[cfg(unix)]
use crate::ownership;
#[cfg(unix)]
use protocol::idlist::{trace_set_gid, trace_set_uid};
#[cfg(unix)]
use rustix::fs::{self as unix_fs};
#[cfg(unix)]
use rustix::process::{RawGid, RawUid};
#[cfg(unix)]
use std::io;
#[cfg(unix)]
use std::os::fd::BorrowedFd;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

/// Applies a path-based `chown`/`lchown`, anchoring on the parent dirfd to
/// defeat ancestor-symlink-swap TOCTOU attacks.
///
/// When `--keep-dirlinks` is inactive, dispatches to
/// [`fast_io::secure_chown_at`], which walks the parent through
/// `secure_open_dir` (`openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS)` on
/// Linux 5.6+, `open(O_NOFOLLOW | O_DIRECTORY)` elsewhere) and anchors
/// `fchownat` on that dirfd. `AT_SYMLINK_NOFOLLOW` alone only guards the leaf;
/// a symlink swapped into a receiver-created ancestor directory would
/// otherwise be followed, redirecting the chown outside the module. Mirrors
/// the chmod-symlink-race cutover in
/// [`super::permissions::set_permissions_like`].
///
/// When `--keep-dirlinks` is active the user has opted into following
/// dest-side symlinks-to-dirs, so the sandbox refusal is wrong: fall back to
/// the path-based `nix` chown (through `AT_FDCWD`) which resolves symlinked
/// parents like upstream `generator.c:1356`'s `link_stat`.
///
/// Both branches perform the ownership change through the libc
/// `chown`/`lchown`/`fchownat` symbol rather than a raw syscall. This is
/// mandatory for `fakeroot` compatibility: fakeroot interposes the libc
/// symbols via `LD_PRELOAD` and fakes the change for a non-root process; a
/// raw syscall bypasses libc, so fakeroot never sees the call and the kernel
/// returns `EPERM`, dropping every file to `0:0`. Only the parent-directory
/// walk uses `openat2`, which fakeroot ignores because it tracks ownership per
/// inode on the chown call, not on directory opens.
// upstream: syscall.c:do_lchown()/do_chown() call the lchown(2)/chown(2) libc
// symbols; rsync 3.4.3+ resolves them under the module dirfd (CVE-2026-29518).
#[cfg(unix)]
fn chown_path(
    path: &Path,
    owner: Option<unix_fs::Uid>,
    group: Option<unix_fs::Gid>,
    follow_symlinks: bool,
    keep_dirlinks: bool,
) -> Result<(), MetadataError> {
    if keep_dirlinks {
        let flag = if follow_symlinks {
            nix::fcntl::AtFlags::empty()
        } else {
            nix::fcntl::AtFlags::AT_SYMLINK_NOFOLLOW
        };
        return nix::unistd::fchownat(
            nix::fcntl::AT_FDCWD,
            path,
            owner.map(|uid| nix::unistd::Uid::from_raw(uid.as_raw())),
            group.map(|gid| nix::unistd::Gid::from_raw(gid.as_raw())),
            flag,
        )
        .map_err(|errno| MetadataError::new("preserve ownership", path, io::Error::from(errno)));
    }

    // `u32::MAX` is `fchownat`'s `(uid_t)-1` / `(gid_t)-1` "leave unchanged"
    // sentinel, matching `owner`/`group` of `None`.
    let uid = owner.map(|uid| uid.as_raw()).unwrap_or(u32::MAX);
    let gid = group.map(|gid| gid.as_raw()).unwrap_or(u32::MAX);
    fast_io::secure_chown_at(path, uid, gid, follow_symlinks)
        .map_err(|error| MetadataError::new("preserve ownership", path, error))
}

/// fd-based counterpart to [`chown_path`] using the libc `fchown(2)` symbol via
/// `nix`. See [`chown_path`] for why the call must go through libc rather than a
/// raw syscall (fakeroot `LD_PRELOAD` interposition).
// upstream: syscall.c:do_fchown() calls the fchown(2) libc symbol.
#[cfg(unix)]
fn chown_fd(
    fd: BorrowedFd<'_>,
    path: &Path,
    owner: Option<unix_fs::Uid>,
    group: Option<unix_fs::Gid>,
) -> Result<(), MetadataError> {
    nix::unistd::fchown(
        fd,
        owner.map(|uid| nix::unistd::Uid::from_raw(uid.as_raw())),
        group.map(|gid| nix::unistd::Gid::from_raw(gid.as_raw())),
    )
    .map_err(|errno| MetadataError::new("preserve ownership", path, io::Error::from(errno)))
}

/// Emits the upstream level-1 `set uid of`/`set gid of` traces for a chown.
///
/// upstream: `rsync.c:535-545` - `DEBUG_GTE(OWN, 1)` block emitted from
/// `set_file_attrs` before `do_lchown`. The trace fires only for the fields
/// that actually change against `existing`; when `existing` is unknown we
/// emit using `from = 0` to match upstream's behaviour for fresh inodes
/// where `sxp->st` is the just-allocated stat block.
#[cfg(unix)]
fn trace_chown_change(
    destination: &Path,
    owner: Option<unix_fs::Uid>,
    group: Option<unix_fs::Gid>,
    existing: Option<&fs::Metadata>,
) {
    let path = destination.display().to_string();
    if let Some(new_uid) = owner {
        let new_raw: u32 = new_uid.as_raw();
        let from = existing.map(|m| m.uid()).unwrap_or(new_raw);
        if from != new_raw {
            trace_set_uid(&path, from, new_raw);
        }
    }
    if let Some(new_gid) = group {
        let new_raw: u32 = new_gid.as_raw();
        let from = existing.map(|m| m.gid()).unwrap_or(new_raw);
        if from != new_raw {
            trace_set_gid(&path, from, new_raw);
        }
    }
}

/// `(uid_t)-1` / `(gid_t)-1` widened to `u32`. `chown(2)` reads this value as
/// "leave unchanged", so it can never be set as an actual owner.
#[cfg(unix)]
const IMPOSSIBLE_ID: u32 = u32::MAX;

/// Builds upstream's `set_file_attrs()` "impossible to set" warning body.
///
/// Kept pure so the exact wording can be pinned in a unit test. The path is
/// quoted like upstream `full_fname`, and `kind` is `"uid"` or `"gid"`.
// upstream: rsync.c:558-561 - "uid 4294967295 (-1) is impossible to set on %s".
#[cfg(unix)]
fn impossible_id_message(kind: &str, destination: &Path) -> String {
    format!(
        "{kind} 4294967295 (-1) is impossible to set on \"{}\"",
        destination.display()
    )
}

/// Emits the "impossible to set" warning to stderr, mirroring the crate's other
/// stderr warning helpers (`special::warn_skip_special`,
/// `xattr_stub::warn_xattr_unsupported`) which print the message verbatim.
#[cfg(unix)]
fn warn_impossible_id(kind: &str, destination: &Path) {
    eprintln!("{}", impossible_id_message(kind, destination));
}

/// Returns `true` when a resolved id of `(uid_t)-1` cannot be applied because
/// the destination is not already owned by `-1`.
// upstream: rsync.c:558-560 - `uid == (uid_t)-1 && sxp->st.st_uid != (uid_t)-1`.
#[cfg(unix)]
fn id_is_impossible(resolved: Option<u32>, pre_chown_id: Option<u32>) -> bool {
    matches!(resolved, Some(IMPOSSIBLE_ID)) && pre_chown_id.unwrap_or(0) != IMPOSSIBLE_ID
}

/// Returns `true` when the pre-chown mode carried setuid/setgid, so the caller
/// must re-stat: `chown` clears those bits on many systems and the later mode
/// compare must observe the post-chown state to restore them.
// upstream: rsync.c:564-567 - `if (sxp->st.st_mode & (S_ISUID | S_ISGID)) link_stat(...)`.
#[cfg(unix)]
fn suid_sgid_needs_restat(pre_chown_mode: Option<u32>) -> bool {
    pre_chown_mode
        .map(|mode| mode & (0o4000 | 0o2000) != 0)
        .unwrap_or(false)
}

/// Performs upstream's post-`do_lchown` bookkeeping from `set_file_attrs()`.
///
/// After the chown succeeds upstream does two things (rsync.c:558-568): warns
/// when a resolved uid/gid is `(uid_t)-1`, and re-stats the destination when it
/// carried setuid/setgid bits. `pre_chown` is the destination's stat captured
/// before the chown. Returns `true` when the caller should refresh its cached
/// destination stat before the permission comparison so the dropped
/// setuid/setgid bits get re-applied.
// upstream: rsync.c:558-568 set_file_attrs() - impossible-id warning + suid/sgid re-stat.
#[cfg(unix)]
fn post_chown_bookkeeping(
    destination: &Path,
    owner: Option<unix_fs::Uid>,
    group: Option<unix_fs::Gid>,
    pre_chown: Option<&fs::Metadata>,
) -> bool {
    if id_is_impossible(
        owner.map(|uid| uid.as_raw()),
        pre_chown.map(|meta| meta.uid()),
    ) {
        warn_impossible_id("uid", destination);
    }
    if id_is_impossible(
        group.map(|gid| gid.as_raw()),
        pre_chown.map(|meta| meta.gid()),
    ) {
        warn_impossible_id("gid", destination);
    }
    suid_sgid_needs_restat(pre_chown.map(|meta| meta.mode()))
}

/// Returns `true` when the current process may set a file's group to `gid`
/// without privilege: it is the effective gid or one of the supplementary
/// groups. Mirrors upstream `is_in_group()` (uidlist.c:195-239), the test that
/// gates `FLAG_SKIP_GROUP` for a non-root sender.
///
/// The effective-gid check uses `nix` (libc `getegid`) so the identity matches
/// upstream's libc-based checks; this fallback is never reached under fakeroot,
/// where `gate_preserved_group` short-circuits on the faked-root euid.
#[cfg(unix)]
#[allow(unsafe_code)]
pub(super) fn process_in_group(gid: unix_fs::Gid) -> bool {
    if nix::unistd::getegid() == nix::unistd::Gid::from_raw(gid.as_raw()) {
        return true;
    }
    let target = gid.as_raw();
    // SAFETY: the first call passes a NULL buffer with size 0 to learn the
    // supplementary-group count; the second fills an exactly-sized buffer.
    // Both are standard POSIX `getgroups` invocations with no aliasing.
    // (`nix::unistd::getgroups` is unavailable on Apple targets, so the libc
    // symbol is used directly for cross-platform support.)
    let count = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
    if count <= 0 {
        return false;
    }
    let mut groups = vec![0 as libc::gid_t; count as usize];
    // SAFETY: `groups` is sized to `count`, matching the value just returned.
    let filled = unsafe { libc::getgroups(count, groups.as_mut_ptr()) };
    if filled < 0 {
        return false;
    }
    groups[..filled as usize].contains(&target)
}

/// Gates a resolved owner UID against process privilege, whether it came from
/// the *preserve* path (`-o`/`-a`) or an explicit `--chown`/`--usermap`
/// override. Mirrors upstream `change_uid = am_root && ...` (rsync.c:526):
/// `preserve_uid` is set identically by `-o`, `--chown`, and `--usermap`
/// (options.c:1793,1811,1833), and upstream's gate makes no distinction
/// between them - a non-root process never sets a file's owner uid, so the
/// chown is skipped rather than attempted and failed. Before this gate
/// oc-rsync attempted it and surfaced the resulting `EPERM` as a fatal
/// exit-code-23 error, e.g. under `-aR` when an implied parent directory is
/// owned by another user, or under a non-root `--chown=user:group`.
#[cfg(unix)]
pub(super) fn gate_preserved_owner(owner: Option<unix_fs::Uid>) -> Option<unix_fs::Uid> {
    // nix (libc `geteuid`) so the euid reflects fakeroot's faked identity,
    // making `am_root` true under fakeroot exactly like upstream. rustix's raw
    // syscall would report the real non-root euid and gate the chown away.
    owner.filter(|_| nix::unistd::geteuid().is_root())
}

/// Gates a resolved group GID against process privilege, whether it came from
/// the *preserve* path (`-g`/`-a`) or an explicit `--chown`/`--groupmap`
/// override. Mirrors upstream's `FLAG_SKIP_GROUP` gate (uidlist.c:284), which
/// is set on the mapped id regardless of whether it was reached via
/// `preserve_gid`, `--chown`, or `--groupmap` (options.c:1809,1832,1848): a
/// non-root process may only set a group it belongs to, so a non-member group
/// is skipped rather than attempted and failed.
#[cfg(unix)]
pub(super) fn gate_preserved_group(group: Option<unix_fs::Gid>) -> Option<unix_fs::Gid> {
    // See `gate_preserved_owner`: nix (libc `geteuid`) so fakeroot's faked root
    // euid is honoured, matching upstream's `am_root` gate.
    group.filter(|gid| nix::unistd::geteuid().is_root() || process_in_group(*gid))
}

/// Resolves the preserved owner of `entry` to a local uid.
///
/// `--usermap` is consulted first on the raw sender id (its rules match numeric
/// ids and names alike, mirroring upstream `recv_add_id`'s uidmap scan). When no
/// rule matches and `--numeric-ids` is off, the sender-transmitted user name
/// (INC_RECURSE `XMIT_USER_NAME_FOLLOWS`) is resolved against the receiver's user
/// database so ownership follows the *name* across hosts with differing id
/// namespaces, rather than re-deriving the name from the receiver's
/// `getpwuid(raw)` - which is wrong when the raw sender id is absent or bound to
/// a different name locally. Without an inline name (local copy) the raw id is
/// round-tripped through the receiver's database exactly as before.
// upstream: flist.c:914 recv_user_name / uidlist.c:307 match_uid
#[cfg(unix)]
fn resolve_owner_uid(
    entry: &protocol::flist::FileEntry,
    options: &MetadataOptions,
) -> Option<unix_fs::Uid> {
    entry.uid().and_then(|uid| {
        let numeric = options.numeric_ids_enabled();
        let raw = uid as RawUid;
        let name = entry.user_name();
        // upstream: uidlist.c:255-280 recv_add_id - a `--usermap` rule is scanned
        // FIRST, keyed on the sender-transmitted name (name/wildcard rules) or
        // the raw sender id (numeric rules), before any receiver-local name
        // resolution. The name-list path (non-INC_RECURSE) resolves entries in
        // `remap_flist_ownership_from_id_lists`; this per-entry path serves the
        // INC_RECURSE inline-name case, so the map is consulted only when the
        // sender transmitted an inline name here.
        if let Some(name) = name
            && let Some(mapping) = options.user_mapping()
            && let Ok(Some(target)) = mapping.map_uid_named(raw, Some(name.as_bytes()), numeric)
        {
            return Some(ownership::uid_from_raw(target));
        }
        // upstream: uidlist.c:273-280 - no map rule matched; fall back to
        // user_to_uid(name), i.e. resolve the sender name against the receiver's
        // user database so ownership follows the name across differing id
        // namespaces. Skipped under --numeric-ids (the sender omits names).
        if !numeric && let Some(name) = name {
            let local = lookup_user_by_name(name.as_bytes())
                .ok()
                .flatten()
                .unwrap_or(raw);
            return Some(ownership::uid_from_raw(local));
        }
        // No transmitted name: receiver-local resolution of the raw id.
        map_uid(raw, numeric)
    })
}

/// Resolves the preserved group of `entry` to a local gid.
///
/// The group counterpart of [`resolve_owner_uid`]: `--groupmap` first, then the
/// sender-transmitted group name (INC_RECURSE `XMIT_GROUP_NAME_FOLLOWS`) resolved
/// against the receiver's group database.
// upstream: flist.c:926 recv_group_name / uidlist.c:317 match_gid
#[cfg(unix)]
fn resolve_group_gid(
    entry: &protocol::flist::FileEntry,
    options: &MetadataOptions,
) -> Option<unix_fs::Gid> {
    entry.gid().and_then(|gid| {
        let numeric = options.numeric_ids_enabled();
        let raw = gid as RawGid;
        let name = entry.group_name();
        // upstream: uidlist.c:255-280 recv_add_id - see `resolve_owner_uid`; the
        // group `--groupmap` scan is keyed on the sender-transmitted group name
        // (or the raw sender gid for numeric rules) before receiver-local
        // resolution. Consulted per-entry only for the INC_RECURSE inline-name
        // case; the id-list path resolves in
        // `remap_flist_ownership_from_id_lists`.
        if let Some(name) = name
            && let Some(mapping) = options.group_mapping()
            && let Ok(Some(target)) = mapping.map_gid_named(raw, Some(name.as_bytes()), numeric)
        {
            return Some(ownership::gid_from_raw(target));
        }
        if !numeric && let Some(name) = name {
            let local = lookup_group_by_name(name.as_bytes())
                .ok()
                .flatten()
                .unwrap_or(raw);
            return Some(ownership::gid_from_raw(local));
        }
        map_gid(raw, numeric)
    })
}

/// Resolves the target UID and GID after applying overrides, mappings, and
/// numeric-id rules. Returns `(None, None)` when no ownership change is
/// requested.
///
/// Resolution priority: override > mapping > source metadata, matching
/// upstream's chown logic.
// upstream: rsync.c:set_file_attrs() - UID/GID resolution before chown
#[cfg(unix)]
pub(super) fn resolve_ownership(
    metadata: &fs::Metadata,
    options: &MetadataOptions,
    destination: &Path,
) -> Result<(Option<unix_fs::Uid>, Option<unix_fs::Gid>), MetadataError> {
    if options.owner_override().is_none()
        && options.group_override().is_none()
        && !options.owner()
        && !options.group()
    {
        return Ok((None, None));
    }

    let owner = if let Some(uid) = options.owner_override() {
        gate_preserved_owner(Some(ownership::uid_from_raw(uid as RawUid)))
    } else if options.owner() {
        let mut raw_uid = metadata.uid() as RawUid;
        if let Some(mapping) = options.user_mapping()
            && let Some(mapped) = mapping
                .map_uid(raw_uid, options.numeric_ids_enabled())
                .map_err(|error| MetadataError::new("apply user mapping", destination, error))?
        {
            raw_uid = mapped;
        }
        gate_preserved_owner(map_uid(raw_uid, options.numeric_ids_enabled()))
    } else {
        None
    };
    let group = if let Some(gid) = options.group_override() {
        gate_preserved_group(Some(ownership::gid_from_raw(gid as RawGid)))
    } else if options.group() {
        let mut raw_gid = metadata.gid() as RawGid;
        if let Some(mapping) = options.group_mapping()
            && let Some(mapped) = mapping
                .map_gid(raw_gid, options.numeric_ids_enabled())
                .map_err(|error| MetadataError::new("apply group mapping", destination, error))?
        {
            raw_gid = mapped;
        }
        gate_preserved_group(map_gid(raw_gid, options.numeric_ids_enabled()))
    } else {
        None
    };

    Ok((owner, group))
}

/// Returns `true` when the resolved ownership already matches `existing`.
///
/// Compares only the fields that are being changed - `None` values are
/// treated as "no change requested" and always match.
// upstream: rsync.c:set_file_attrs() - skips chown when uid/gid already match
#[cfg(unix)]
pub(super) fn ownership_matches(
    owner: &Option<unix_fs::Uid>,
    group: &Option<unix_fs::Gid>,
    existing: &fs::Metadata,
) -> bool {
    let uid_ok = match owner {
        Some(uid) => uid.as_raw() == existing.uid(),
        None => true,
    };
    let gid_ok = match group {
        Some(gid) => gid.as_raw() == existing.gid(),
        None => true,
    };
    uid_ok && gid_ok
}

/// Applies ownership (chown) to a path, optionally following symlinks.
///
/// Uses `chownat` with `AT_SYMLINK_NOFOLLOW` when `follow_symlinks` is false.
/// Skips the syscall entirely when both resolved UID and GID are `None`,
/// or when the resolved values already match `existing`.
///
/// Under `--fake-super` on Unix targets that follow symlinks (regular files
/// and directories), ownership/mode/rdev are encoded into the
/// `user.rsync.%stat` xattr instead of being applied via `chown`. This mirrors
/// upstream rsync's `set_file_attrs()` behaviour when `am_root < 0`.
///
/// Returns `true` when the destination carried setuid/setgid bits that the
/// chown may have cleared, so the caller must re-stat before the permission
/// comparison (upstream rsync.c:564-567). Returns `false` when no chown ran.
// upstream: rsync.c:set_file_attrs() - chownat with conditional AT_SYMLINK_NOFOLLOW
pub(super) fn set_owner_like(
    metadata: &fs::Metadata,
    destination: &Path,
    follow_symlinks: bool,
    options: &MetadataOptions,
    existing: Option<&fs::Metadata>,
) -> Result<bool, MetadataError> {
    #[cfg(unix)]
    {
        // upstream: xattrs.c:set_stat_xattr() encodes mode/uid/gid/rdev under
        // fake-super. Symlinks are excluded because lsetxattr on a symlink is
        // not portable; symlink fake-super follows upstream's "skip" path.
        // Mirrors `apply_ownership_from_entry`'s gate so the local-copy and
        // network-receiver paths agree on when the xattr is written.
        if options.fake_super_enabled()
            && follow_symlinks
            && (options.owner()
                || options.group()
                || options.owner_override().is_some()
                || options.group_override().is_some())
        {
            store_fake_super_from_local_metadata(destination, metadata)?;
            return Ok(false);
        }

        let (owner, group) = resolve_ownership(metadata, options, destination)?;

        if owner.is_none() && group.is_none() {
            return Ok(false);
        }

        if let Some(existing) = existing {
            if ownership_matches(&owner, &group, existing) {
                return Ok(false);
            }
        }

        // upstream: rsync.c:535-546 - DEBUG_GTE(OWN, 1) fires before do_lchown.
        trace_chown_change(destination, owner, group, existing);

        chown_path(
            destination,
            owner,
            group,
            follow_symlinks,
            options.keep_dirlinks(),
        )?;

        // upstream: rsync.c:558-568 - impossible-id warning + suid/sgid re-stat.
        Ok(post_chown_bookkeeping(destination, owner, group, existing))
    }

    #[cfg(not(unix))]
    {
        let _ = destination;
        let _ = metadata;
        let _ = follow_symlinks;
        let _ = options;
        let _ = existing;
        Ok(false)
    }
}

/// fd-based variant of [`set_owner_like`] that uses `fchown` instead of `chownat`.
///
/// Under `--fake-super` the open file descriptor is unused; ownership is
/// captured into the `user.rsync.%stat` xattr instead of issuing `fchown`.
///
/// Returns `true` when a re-stat is required after the chown (see
/// [`set_owner_like`]).
#[cfg(unix)]
pub(super) fn set_owner_like_with_fd(
    metadata: &fs::Metadata,
    destination: &Path,
    options: &MetadataOptions,
    fd: BorrowedFd<'_>,
    existing: Option<&fs::Metadata>,
) -> Result<bool, MetadataError> {
    // upstream: xattrs.c:set_stat_xattr() under am_root < 0 - skip fchown.
    if options.fake_super_enabled()
        && (options.owner()
            || options.group()
            || options.owner_override().is_some()
            || options.group_override().is_some())
    {
        let _ = fd;
        store_fake_super_from_local_metadata(destination, metadata)?;
        return Ok(false);
    }

    let (owner, group) = resolve_ownership(metadata, options, destination)?;

    if owner.is_none() && group.is_none() {
        return Ok(false);
    }

    if let Some(existing) = existing {
        if ownership_matches(&owner, &group, existing) {
            return Ok(false);
        }
    }

    // upstream: rsync.c:535-546 - DEBUG_GTE(OWN, 1) fires before do_lchown.
    trace_chown_change(destination, owner, group, existing);

    chown_fd(fd, destination, owner, group)?;

    // upstream: rsync.c:558-568 - impossible-id warning + suid/sgid re-stat.
    Ok(post_chown_bookkeeping(destination, owner, group, existing))
}

/// Applies ownership from a protocol `FileEntry` on Unix.
///
/// Resolves UID/GID from the entry using overrides, mappings, and numeric-id
/// rules. Delegates to fake-super xattr storage when `--fake-super` is active.
/// Skips the chown syscall when the resolved values already match `cached_meta`.
///
/// Returns `true` when the destination carried setuid/setgid bits that the
/// chown may have cleared, so the caller must re-stat before applying
/// permissions (upstream rsync.c:564-567). Returns `false` when no chown ran.
// upstream: rsync.c:set_file_attrs() - chown path for receiver-side file entries
#[cfg(unix)]
pub(super) fn apply_ownership_from_entry(
    destination: &Path,
    entry: &protocol::flist::FileEntry,
    options: &MetadataOptions,
    cached_meta: Option<&fs::Metadata>,
) -> Result<bool, MetadataError> {
    use rustix::process::{RawGid, RawUid};

    if !options.owner()
        && !options.group()
        && options.owner_override().is_none()
        && options.group_override().is_none()
    {
        return Ok(false);
    }

    let raw_uid = if let Some(uid_override) = options.owner_override() {
        Some(uid_override)
    } else if options.owner() {
        entry.uid()
    } else {
        None
    };

    let raw_gid = if let Some(gid_override) = options.group_override() {
        Some(gid_override)
    } else if options.group() {
        entry.gid()
    } else {
        None
    };

    // upstream: rsync.c:set_file_attrs() - fake-super stores ownership in xattr
    if options.fake_super_enabled() {
        apply_ownership_via_fake_super(destination, entry, raw_uid, raw_gid)?;
        return Ok(false);
    }

    let owner = if let Some(uid_override) = options.owner_override() {
        gate_preserved_owner(Some(ownership::uid_from_raw(uid_override as RawUid)))
    } else if options.owner() {
        gate_preserved_owner(resolve_owner_uid(entry, options))
    } else {
        None
    };

    let group = if let Some(gid_override) = options.group_override() {
        gate_preserved_group(Some(ownership::gid_from_raw(gid_override as RawGid)))
    } else if options.group() {
        gate_preserved_group(resolve_group_gid(entry, options))
    } else {
        None
    };

    if owner.is_some() || group.is_some() {
        // upstream: rsync.c:set_file_attrs() - skips chown when uid/gid already match
        let needs_chown = match cached_meta {
            Some(meta) => {
                let current_uid = meta.uid();
                let current_gid = meta.gid();
                let desired_uid = owner.map(|o| o.as_raw()).unwrap_or(current_uid);
                let desired_gid = group.map(|g| g.as_raw()).unwrap_or(current_gid);
                current_uid != desired_uid || current_gid != desired_gid
            }
            None => true,
        };

        if needs_chown {
            // upstream: rsync.c:535-546 - DEBUG_GTE(OWN, 1) fires before do_lchown.
            trace_chown_change(destination, owner, group, cached_meta);

            chown_path(destination, owner, group, true, options.keep_dirlinks())?;

            // upstream: rsync.c:558-568 - impossible-id warning + suid/sgid re-stat.
            return Ok(post_chown_bookkeeping(
                destination,
                owner,
                group,
                cached_meta,
            ));
        }
    }

    Ok(false)
}

/// Applies ownership from a protocol `FileEntry` to a symbolic link on Unix
/// without following the link target.
///
/// Mirrors [`apply_ownership_from_entry`] but uses `AT_SYMLINK_NOFOLLOW` so the
/// `chownat` syscall targets the link itself, matching upstream's
/// `do_lchown(fname, uid, gid)` path in `set_file_attrs()`. Fake-super xattr
/// storage is skipped because `lsetxattr` on symlinks is not portable; upstream
/// rsync also takes the skip path for symlinks under `am_root < 0`.
// upstream: rsync.c:set_file_attrs() - do_lchown for symlinks
#[cfg(unix)]
pub(super) fn apply_symlink_ownership_from_entry(
    destination: &Path,
    entry: &protocol::flist::FileEntry,
    options: &MetadataOptions,
    cached_meta: Option<&fs::Metadata>,
) -> Result<(), MetadataError> {
    if !options.owner()
        && !options.group()
        && options.owner_override().is_none()
        && options.group_override().is_none()
    {
        return Ok(());
    }

    // upstream: rsync.c:set_file_attrs() - fake-super skips lsetxattr on symlinks
    if options.fake_super_enabled() {
        return Ok(());
    }

    let owner = if let Some(uid_override) = options.owner_override() {
        gate_preserved_owner(Some(ownership::uid_from_raw(uid_override as RawUid)))
    } else if options.owner() {
        gate_preserved_owner(resolve_owner_uid(entry, options))
    } else {
        None
    };

    let group = if let Some(gid_override) = options.group_override() {
        gate_preserved_group(Some(ownership::gid_from_raw(gid_override as RawGid)))
    } else if options.group() {
        gate_preserved_group(resolve_group_gid(entry, options))
    } else {
        None
    };

    if owner.is_none() && group.is_none() {
        return Ok(());
    }

    // upstream: rsync.c:set_file_attrs() - skips chown when uid/gid already match
    let needs_chown = match cached_meta {
        Some(meta) => {
            let current_uid = meta.uid();
            let current_gid = meta.gid();
            let desired_uid = owner.map(|o| o.as_raw()).unwrap_or(current_uid);
            let desired_gid = group.map(|g| g.as_raw()).unwrap_or(current_gid);
            current_uid != desired_uid || current_gid != desired_gid
        }
        None => true,
    };

    if needs_chown {
        trace_chown_change(destination, owner, group, cached_meta);

        chown_path(destination, owner, group, false, options.keep_dirlinks())?;

        // upstream: rsync.c:558-561 - impossible-id warning also fires for
        // symlink chowns. The suid/sgid re-stat is irrelevant here because
        // symlinks are never chmod'd, so the returned signal is discarded.
        let _ = post_chown_bookkeeping(destination, owner, group, cached_meta);
    }

    Ok(())
}

/// Stores ownership metadata via fake-super xattr instead of applying directly.
///
/// Used when `--fake-super` is enabled, allowing non-root users to
/// preserve ownership information in extended attributes. Encodes
/// mode, uid, gid, and rdev into a `user.rsync.%stat` xattr.
// upstream: rsync.c:set_file_attrs() with am_root==0 and fake_super active
#[cfg(unix)]
fn apply_ownership_via_fake_super(
    destination: &Path,
    entry: &protocol::flist::FileEntry,
    uid: Option<u32>,
    gid: Option<u32>,
) -> Result<(), MetadataError> {
    use crate::fake_super::{FakeSuperStat, load_fake_super, store_fake_super};

    // upstream: xattrs.c:set_stat_xattr() encodes the full mode (S_IFMT + perms)
    // so a later read can rebuild the file type, not just the permission bits.
    let mode = entry.mode();
    let uid = uid.unwrap_or(0);
    let gid = gid.unwrap_or(0);

    let rdev = if entry.file_type().is_device() {
        match (entry.rdev_major(), entry.rdev_minor()) {
            (Some(major), Some(minor)) => Some((major, minor)),
            _ => None,
        }
    } else {
        None
    };

    let stat = FakeSuperStat {
        mode,
        uid,
        gid,
        rdev,
    };

    // upstream: xattrs.c:read_stat_xattr() consults the existing xattr so an
    // unchanged stat skips the rewrite. Mirrors `set_file_attrs()`'s "no-op
    // when current state already matches" fast path.
    if let Ok(Some(existing)) = load_fake_super(destination)
        && existing == stat
    {
        return Ok(());
    }

    store_fake_super(destination, &stat)
        .map_err(|error| MetadataError::new("store fake-super metadata", destination, error))
}

/// Stores fake-super metadata derived from a local `fs::Metadata` snapshot.
///
/// This is the local-copy counterpart to [`apply_ownership_via_fake_super`]:
/// the caller already has the source's `fs::Metadata`, so there is no wire
/// `FileEntry` to consult. Mode (with the full `S_IFMT` bits), uid, gid, and
/// device rdev are captured via [`FakeSuperStat::from_metadata`] and stored
/// in the `user.rsync.%stat` xattr.
///
/// Mirrors `apply_ownership_via_fake_super`'s "no-op when the existing xattr
/// already matches" fast path. On platforms or builds without xattr support
/// this is a graceful no-op.
// upstream: xattrs.c:set_stat_xattr() / rsync.c:set_file_attrs() with am_root<0
#[cfg(unix)]
fn store_fake_super_from_local_metadata(
    destination: &Path,
    metadata: &fs::Metadata,
) -> Result<(), MetadataError> {
    #[cfg(feature = "xattr")]
    {
        use crate::fake_super::{FakeSuperStat, load_fake_super, store_fake_super};

        let stat = FakeSuperStat::from_metadata(metadata);

        if let Ok(Some(existing)) = load_fake_super(destination)
            && existing == stat
        {
            return Ok(());
        }

        store_fake_super(destination, &stat)
            .map_err(|error| MetadataError::new("store fake-super metadata", destination, error))
    }
    #[cfg(not(feature = "xattr"))]
    {
        let _ = (destination, metadata);
        Ok(())
    }
}

/// No-op stub for non-Unix platforms where ownership (`chown`) is not supported.
///
/// Returns `Ok(())` unconditionally since Windows and other non-Unix targets
/// do not support POSIX ownership semantics.
#[cfg(not(unix))]
pub(super) fn apply_ownership_from_entry(
    _destination: &Path,
    _entry: &protocol::flist::FileEntry,
    _options: &MetadataOptions,
    _cached_meta: Option<&fs::Metadata>,
) -> Result<bool, MetadataError> {
    Ok(false)
}

#[cfg(all(test, unix))]
mod own_debug_tests {
    //! `--debug=OWN` level-1 emission tests for the chown helpers.
    //!
    //! These exercise the trace path without performing a real `chownat`
    //! (which would require root). The helper resolves the uid/gid pair
    //! and decides whether each side changed against `existing`; the
    //! pinning is on the upstream wire shapes from `rsync.c:535-545`.

    use super::*;
    use logging::{DebugFlag, DiagnosticEvent, VerbosityConfig, drain_events, init};
    use std::fs;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn init_at(level: u8) {
        let mut cfg = VerbosityConfig::default();
        cfg.debug.own = level;
        init(cfg);
        let _ = drain_events();
    }

    fn own_messages() -> Vec<String> {
        drain_events()
            .into_iter()
            .filter_map(|event| match event {
                DiagnosticEvent::Debug {
                    flag: DebugFlag::Own,
                    message,
                    ..
                } => Some(message),
                _ => None,
            })
            .collect()
    }

    fn fake_existing(path: &PathBuf) -> fs::Metadata {
        fs::write(path, b"").expect("write probe");
        fs::metadata(path).expect("probe metadata")
    }

    #[test]
    fn level1_set_uid_emits_with_destination_path() {
        // upstream: rsync.c:537-540 - "set uid of %s from %u to %u".
        init_at(1);

        let tmp = tempdir().expect("tempdir");
        let path = tmp.path().join("probe");
        let meta = fake_existing(&path);
        let current_uid = meta.uid() as u64;
        let new_uid = current_uid as u32 ^ 1; // any value distinct from current
        let owner = Some(ownership::uid_from_raw(new_uid as RawUid));

        trace_chown_change(&path, owner, None, Some(&meta));

        let expected = format!(
            "set uid of {} from {} to {}",
            path.display(),
            current_uid,
            new_uid
        );
        let m = own_messages();
        assert!(
            m.iter().any(|s| s == &expected),
            "missing {expected:?}, got {m:?}"
        );
    }

    #[test]
    fn level1_set_gid_emits_with_destination_path() {
        // upstream: rsync.c:541-545 - "set gid of %s from %u to %u".
        init_at(1);

        let tmp = tempdir().expect("tempdir");
        let path = tmp.path().join("probe");
        let meta = fake_existing(&path);
        let current_gid = meta.gid() as u64;
        let new_gid = current_gid as u32 ^ 1;
        let group = Some(ownership::gid_from_raw(new_gid as RawGid));

        trace_chown_change(&path, None, group, Some(&meta));

        let expected = format!(
            "set gid of {} from {} to {}",
            path.display(),
            current_gid,
            new_gid
        );
        let m = own_messages();
        assert!(
            m.iter().any(|s| s == &expected),
            "missing {expected:?}, got {m:?}"
        );
    }

    #[test]
    fn level1_no_emission_when_uid_unchanged() {
        // upstream: rsync.c:536-540 - `if (change_uid)` gates the uid trace.
        init_at(1);

        let tmp = tempdir().expect("tempdir");
        let path = tmp.path().join("probe");
        let meta = fake_existing(&path);
        let same_uid = meta.uid() as u32;
        let owner = Some(ownership::uid_from_raw(same_uid as RawUid));

        trace_chown_change(&path, owner, None, Some(&meta));
        assert!(
            own_messages().is_empty(),
            "uid unchanged must not emit the level-1 trace"
        );
    }

    #[test]
    fn level0_suppresses_set_uid_set_gid() {
        // upstream: DEBUG_GTE(OWN, 1) is false when --debug=OWN is disabled.
        init_at(0);

        let tmp = tempdir().expect("tempdir");
        let path = tmp.path().join("probe");
        let meta = fake_existing(&path);
        let owner = Some(ownership::uid_from_raw(((meta.uid() as u32) ^ 1) as RawUid));
        let group = Some(ownership::gid_from_raw(((meta.gid() as u32) ^ 1) as RawGid));

        trace_chown_change(&path, owner, group, Some(&meta));
        assert!(
            own_messages().is_empty(),
            "level-0 must suppress all OWN traces"
        );
    }

    #[test]
    #[cfg(unix)]
    fn resolves_inline_owner_name_to_local_id() {
        // upstream: flist.c:914 recv_user_name - the receiver resolves the
        // SENDER-transmitted user name to a LOCAL id so ownership follows the
        // NAME across hosts with differing id namespaces. A raw sender id that
        // does not exist locally must not leak through as the file owner.
        use protocol::flist::FileEntry;

        let mut entry = FileEntry::new_file("f".into(), 0, 0o644);
        entry.set_uid(4_000_123); // no such uid on the receiver
        entry.set_user_name("root".to_string());

        let opts = MetadataOptions::new()
            .preserve_owner(true)
            .numeric_ids(false);
        assert_eq!(
            resolve_owner_uid(&entry, &opts).map(|u| u.as_raw()),
            Some(0),
            "sent name 'root' must resolve to local uid 0, not the raw sender id"
        );

        // --numeric-ids keeps the raw sender id (no name resolution).
        let opts_num = MetadataOptions::new()
            .preserve_owner(true)
            .numeric_ids(true);
        assert_eq!(
            resolve_owner_uid(&entry, &opts_num).map(|u| u.as_raw()),
            Some(4_000_123),
            "--numeric-ids must keep the raw sender id"
        );
    }

    #[test]
    #[cfg(unix)]
    fn usermap_matches_sender_inline_name_not_raw_id() {
        // upstream: uidlist.c:255-268 recv_add_id - a `--usermap` NAME rule is
        // keyed on the sender-transmitted (inline, INC_RECURSE) name, not a name
        // re-derived from the raw sender id on the receiver. Sender uid 1500 does
        // not exist locally; the inline name "deploy" drives the rule. Target 0
        // is numeric so the assertion needs no /etc/passwd entry.
        use protocol::flist::FileEntry;

        let mut entry = FileEntry::new_file("f".into(), 0, 0o644);
        entry.set_uid(1500);
        entry.set_user_name("deploy".to_string());

        let opts = MetadataOptions::new()
            .preserve_owner(true)
            .numeric_ids(false)
            .with_user_mapping(Some(crate::UserMapping::parse("deploy:0").unwrap()));
        assert_eq!(
            resolve_owner_uid(&entry, &opts).map(|u| u.as_raw()),
            Some(0),
            "usermap must match the sender name 'deploy', not the local uid 1500"
        );
    }

    #[test]
    #[cfg(unix)]
    fn usermap_numeric_rule_keys_on_raw_sender_id() {
        // upstream: uidlist.c:262-267 - a numeric `--usermap` rule matches the
        // raw sender id regardless of the transmitted name.
        use protocol::flist::FileEntry;

        let mut entry = FileEntry::new_file("f".into(), 0, 0o644);
        entry.set_uid(1500);
        entry.set_user_name("deploy".to_string());

        let opts = MetadataOptions::new()
            .preserve_owner(true)
            .numeric_ids(false)
            .with_user_mapping(Some(crate::UserMapping::parse("1500:5000").unwrap()));
        assert_eq!(
            resolve_owner_uid(&entry, &opts).map(|u| u.as_raw()),
            Some(5000),
            "numeric usermap must map the raw sender id 1500 to 5000"
        );
    }

    #[test]
    #[cfg(unix)]
    fn usermap_wildcard_matches_sender_inline_name() {
        // upstream: uidlist.c:256-258 - NFLAGS_WILD_NAME_MATCH wildmatch on the
        // transmitted name.
        use protocol::flist::FileEntry;

        let mut entry = FileEntry::new_file("f".into(), 0, 0o644);
        entry.set_uid(1500);
        entry.set_user_name("deploy".to_string());

        let opts = MetadataOptions::new()
            .preserve_owner(true)
            .numeric_ids(false)
            .with_user_mapping(Some(crate::UserMapping::parse("dep*:0").unwrap()));
        assert_eq!(
            resolve_owner_uid(&entry, &opts).map(|u| u.as_raw()),
            Some(0),
            "wildcard usermap must match the sender name 'deploy'"
        );
    }

    #[test]
    #[cfg(unix)]
    fn groupmap_matches_sender_inline_name_symmetric() {
        // upstream: uidlist.c:317-337 match_gid - group counterpart keyed on the
        // sender-transmitted group name.
        use protocol::flist::FileEntry;

        let mut entry = FileEntry::new_file("f".into(), 0, 0o644);
        entry.set_gid(2500);
        entry.set_group_name("build".to_string());

        let opts = MetadataOptions::new()
            .preserve_group(true)
            .numeric_ids(false)
            .with_group_mapping(Some(crate::GroupMapping::parse("build:0").unwrap()));
        assert_eq!(
            resolve_group_gid(&entry, &opts).map(|g| g.as_raw()),
            Some(0),
            "groupmap must match the sender name 'build', not the local gid 2500"
        );
    }
}

#[cfg(all(test, unix))]
mod post_chown_tests {
    //! Decision-path pins for the post-`do_lchown` bookkeeping upstream runs in
    //! `set_file_attrs()` (rsync.c:558-568). These exercise the pure predicates
    //! without needing root so the setuid re-stat and impossible-id warning
    //! logic is covered on CI as well as under a privileged run.

    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    #[test]
    fn setuid_or_setgid_mode_forces_restat() {
        // upstream: rsync.c:564-567 - a destination carrying setuid/setgid must
        // be re-stat'd after chown because the chown clears those bits; the mode
        // apply then restores them. Without the re-stat a `-p` transfer of a
        // setuid binary whose owner changes would silently drop the setuid bit,
        // because the chmod compare would still see the stale (pre-chown) mode
        // and skip the syscall.
        assert!(suid_sgid_needs_restat(Some(0o4755)), "setuid must re-stat");
        assert!(suid_sgid_needs_restat(Some(0o2755)), "setgid must re-stat");
        assert!(
            suid_sgid_needs_restat(Some(0o6755)),
            "setuid+setgid must re-stat"
        );
    }

    #[test]
    fn plain_mode_skips_restat() {
        // upstream: rsync.c:564 - the re-stat is gated on `S_ISUID | S_ISGID`
        // only; ordinary and sticky-only modes take the cheap path.
        assert!(
            !suid_sgid_needs_restat(Some(0o0755)),
            "plain mode: no re-stat"
        );
        assert!(
            !suid_sgid_needs_restat(Some(0o1755)),
            "sticky-only: no re-stat"
        );
        assert!(!suid_sgid_needs_restat(None), "absent stat: no re-stat");
    }

    #[test]
    fn resolved_minus_one_is_impossible_unless_dest_already_minus_one() {
        // upstream: rsync.c:558-560 - chown treats (uid_t)-1 as "no change", so
        // an owner that resolves to -1 can never be applied and upstream warns,
        // but only when the destination is not already owned by -1.
        assert!(id_is_impossible(Some(u32::MAX), Some(1000)));
        assert!(
            id_is_impossible(Some(u32::MAX), None),
            "a freshly created dest is never owned by -1"
        );
        assert!(
            !id_is_impossible(Some(u32::MAX), Some(u32::MAX)),
            "dest already -1: upstream's `st_uid != -1` guard is false"
        );
        assert!(
            !id_is_impossible(Some(1000), Some(1000)),
            "a real id can be set"
        );
        assert!(!id_is_impossible(None, Some(1000)), "no change requested");
    }

    #[test]
    fn warning_wording_matches_upstream_verbatim() {
        // upstream: rsync.c:558-561 - "uid 4294967295 (-1) is impossible to set
        // on %s\n" with the path quoted by full_fname.
        assert_eq!(
            impossible_id_message("uid", Path::new("/tmp/x")),
            "uid 4294967295 (-1) is impossible to set on \"/tmp/x\""
        );
        assert_eq!(
            impossible_id_message("gid", Path::new("/tmp/x")),
            "gid 4294967295 (-1) is impossible to set on \"/tmp/x\""
        );
    }

    #[test]
    fn bookkeeping_reports_restat_from_a_real_setuid_stat() {
        // End-to-end over a real stat: a setuid file signals the re-stat, a
        // plain file does not. Exercised without root because the owning user
        // may set the setuid bit on a file it owns.
        let dir = tempdir().expect("tempdir");

        let plain = dir.path().join("plain");
        fs::write(&plain, b"x").expect("write plain");
        let plain_meta = fs::metadata(&plain).expect("stat plain");
        assert!(
            !post_chown_bookkeeping(&plain, None, None, Some(&plain_meta)),
            "a plain file must not force a re-stat"
        );

        let suid = dir.path().join("suid");
        fs::write(&suid, b"x").expect("write suid");
        fs::set_permissions(&suid, fs::Permissions::from_mode(0o4755)).expect("chmod suid");
        let suid_meta = fs::metadata(&suid).expect("stat suid");
        if suid_meta.mode() & 0o4000 == 0 {
            // The filesystem refused to retain the setuid bit for this user;
            // the pure predicate is still covered by the tests above.
            return;
        }
        assert!(
            post_chown_bookkeeping(&suid, None, None, Some(&suid_meta)),
            "a setuid file must force a re-stat so the chown-dropped bit is restored"
        );
    }
}
