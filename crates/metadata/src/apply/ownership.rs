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
use crate::id_lookup::{map_gid, map_uid};
#[cfg(unix)]
use crate::ownership;
#[cfg(unix)]
use protocol::idlist::{trace_set_gid, trace_set_uid};
#[cfg(unix)]
use rustix::fs::{self as unix_fs, AtFlags, CWD};
#[cfg(unix)]
use rustix::process::{RawGid, RawUid};
#[cfg(unix)]
use std::io;
#[cfg(unix)]
use std::os::fd::BorrowedFd;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

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

/// Returns `true` when the current process may set a file's group to `gid`
/// without privilege: it is the effective gid or one of the supplementary
/// groups. Mirrors upstream `is_in_group()` (uidlist.c:195-239), the test that
/// gates `FLAG_SKIP_GROUP` for a non-root sender.
#[cfg(unix)]
#[allow(unsafe_code)]
fn process_in_group(gid: unix_fs::Gid) -> bool {
    if rustix::process::getegid() == gid {
        return true;
    }
    let target = gid.as_raw();
    // SAFETY: the first call passes a NULL buffer with size 0 to learn the
    // supplementary-group count; the second fills an exactly-sized buffer.
    // Both are standard POSIX `getgroups` invocations with no aliasing.
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

/// Gates an owner UID resolved from the *preserve* path (`-o`/`-a`, no explicit
/// `--chown`/`--usermap`). Mirrors upstream `change_uid = am_root && ...`
/// (rsync.c:526): a non-root process never sets a file's owner uid, so the
/// chown is skipped rather than attempted and failed. Before this gate oc-rsync
/// attempted it and surfaced the resulting `EPERM` as a fatal exit-code-23
/// error, e.g. under `-aR` when an implied parent directory is owned by another
/// user. Explicit overrides keep their fail-loud behaviour and are not routed here.
#[cfg(unix)]
pub(super) fn gate_preserved_owner(owner: Option<unix_fs::Uid>) -> Option<unix_fs::Uid> {
    owner.filter(|_| rustix::process::geteuid().is_root())
}

/// Gates a group GID resolved from the *preserve* path (`-g`/`-a`, no explicit
/// `--chown`/`--groupmap`). Mirrors upstream's `FLAG_SKIP_GROUP` gate
/// (uidlist.c:284): a non-root process may only set a group it belongs to, so a
/// non-member group is skipped rather than attempted and failed. Explicit
/// overrides keep their fail-loud behaviour and are not routed here.
#[cfg(unix)]
pub(super) fn gate_preserved_group(group: Option<unix_fs::Gid>) -> Option<unix_fs::Gid> {
    group.filter(|gid| rustix::process::geteuid().is_root() || process_in_group(*gid))
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
        Some(ownership::uid_from_raw(uid as RawUid))
    } else if options.owner() {
        let mut raw_uid = metadata.uid() as RawUid;
        if let Some(mapping) = options.user_mapping()
            && let Some(mapped) = mapping
                .map_uid(raw_uid)
                .map_err(|error| MetadataError::new("apply user mapping", destination, error))?
        {
            raw_uid = mapped;
        }
        gate_preserved_owner(map_uid(raw_uid, options.numeric_ids_enabled()))
    } else {
        None
    };
    let group = if let Some(gid) = options.group_override() {
        Some(ownership::gid_from_raw(gid as RawGid))
    } else if options.group() {
        let mut raw_gid = metadata.gid() as RawGid;
        if let Some(mapping) = options.group_mapping()
            && let Some(mapped) = mapping
                .map_gid(raw_gid)
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
// upstream: rsync.c:set_file_attrs() - chownat with conditional AT_SYMLINK_NOFOLLOW
pub(super) fn set_owner_like(
    metadata: &fs::Metadata,
    destination: &Path,
    follow_symlinks: bool,
    options: &MetadataOptions,
    existing: Option<&fs::Metadata>,
) -> Result<(), MetadataError> {
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
            return store_fake_super_from_local_metadata(destination, metadata);
        }

        let (owner, group) = resolve_ownership(metadata, options, destination)?;

        if owner.is_none() && group.is_none() {
            return Ok(());
        }

        if let Some(existing) = existing {
            if ownership_matches(&owner, &group, existing) {
                return Ok(());
            }
        }

        let flags = if follow_symlinks {
            AtFlags::empty()
        } else {
            AtFlags::SYMLINK_NOFOLLOW
        };

        // upstream: rsync.c:535-546 - DEBUG_GTE(OWN, 1) fires before do_lchown.
        trace_chown_change(destination, owner, group, existing);

        unix_fs::chownat(CWD, destination, owner, group, flags).map_err(|error| {
            MetadataError::new("preserve ownership", destination, io::Error::from(error))
        })?
    }

    #[cfg(not(unix))]
    {
        let _ = destination;
        let _ = metadata;
        let _ = follow_symlinks;
        let _ = options;
        let _ = existing;
    }

    Ok(())
}

/// fd-based variant of [`set_owner_like`] that uses `fchown` instead of `chownat`.
///
/// Under `--fake-super` the open file descriptor is unused; ownership is
/// captured into the `user.rsync.%stat` xattr instead of issuing `fchown`.
#[cfg(unix)]
pub(super) fn set_owner_like_with_fd(
    metadata: &fs::Metadata,
    destination: &Path,
    options: &MetadataOptions,
    fd: BorrowedFd<'_>,
    existing: Option<&fs::Metadata>,
) -> Result<(), MetadataError> {
    // upstream: xattrs.c:set_stat_xattr() under am_root < 0 - skip fchown.
    if options.fake_super_enabled()
        && (options.owner()
            || options.group()
            || options.owner_override().is_some()
            || options.group_override().is_some())
    {
        let _ = fd;
        return store_fake_super_from_local_metadata(destination, metadata);
    }

    let (owner, group) = resolve_ownership(metadata, options, destination)?;

    if owner.is_none() && group.is_none() {
        return Ok(());
    }

    if let Some(existing) = existing {
        if ownership_matches(&owner, &group, existing) {
            return Ok(());
        }
    }

    // upstream: rsync.c:535-546 - DEBUG_GTE(OWN, 1) fires before do_lchown.
    trace_chown_change(destination, owner, group, existing);

    unix_fs::fchown(fd, owner, group).map_err(|error| {
        MetadataError::new("preserve ownership", destination, io::Error::from(error))
    })?;

    Ok(())
}

/// Applies ownership from a protocol `FileEntry` on Unix.
///
/// Resolves UID/GID from the entry using overrides, mappings, and numeric-id
/// rules. Delegates to fake-super xattr storage when `--fake-super` is active.
/// Skips the chown syscall when the resolved values already match `cached_meta`.
// upstream: rsync.c:set_file_attrs() - chown path for receiver-side file entries
#[cfg(unix)]
pub(super) fn apply_ownership_from_entry(
    destination: &Path,
    entry: &protocol::flist::FileEntry,
    options: &MetadataOptions,
    cached_meta: Option<&fs::Metadata>,
) -> Result<(), MetadataError> {
    use rustix::fs::{AtFlags, CWD, chownat};
    use rustix::process::{RawGid, RawUid};

    if !options.owner()
        && !options.group()
        && options.owner_override().is_none()
        && options.group_override().is_none()
    {
        return Ok(());
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
        return apply_ownership_via_fake_super(destination, entry, raw_uid, raw_gid);
    }

    let owner = if let Some(uid_override) = options.owner_override() {
        Some(ownership::uid_from_raw(uid_override as RawUid))
    } else if options.owner() {
        gate_preserved_owner(entry.uid().and_then(|uid| {
            let mut mapped_uid = uid as RawUid;
            if let Some(mapping) = options.user_mapping()
                && let Ok(Some(mapped)) = mapping.map_uid(mapped_uid)
            {
                mapped_uid = mapped;
            }
            map_uid(mapped_uid, options.numeric_ids_enabled())
        }))
    } else {
        None
    };

    let group = if let Some(gid_override) = options.group_override() {
        Some(ownership::gid_from_raw(gid_override as RawGid))
    } else if options.group() {
        gate_preserved_group(entry.gid().and_then(|gid| {
            let mut mapped_gid = gid as RawGid;
            if let Some(mapping) = options.group_mapping()
                && let Ok(Some(mapped)) = mapping.map_gid(mapped_gid)
            {
                mapped_gid = mapped;
            }
            map_gid(mapped_gid, options.numeric_ids_enabled())
        }))
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

            chownat(CWD, destination, owner, group, AtFlags::empty()).map_err(|error| {
                MetadataError::new("preserve ownership", destination, io::Error::from(error))
            })?;
        }
    }

    Ok(())
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
    use rustix::fs::chownat;

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
        Some(ownership::uid_from_raw(uid_override as RawUid))
    } else if options.owner() {
        gate_preserved_owner(entry.uid().and_then(|uid| {
            let mut mapped_uid = uid as RawUid;
            if let Some(mapping) = options.user_mapping()
                && let Ok(Some(mapped)) = mapping.map_uid(mapped_uid)
            {
                mapped_uid = mapped;
            }
            map_uid(mapped_uid, options.numeric_ids_enabled())
        }))
    } else {
        None
    };

    let group = if let Some(gid_override) = options.group_override() {
        Some(ownership::gid_from_raw(gid_override as RawGid))
    } else if options.group() {
        gate_preserved_group(entry.gid().and_then(|gid| {
            let mut mapped_gid = gid as RawGid;
            if let Some(mapping) = options.group_mapping()
                && let Ok(Some(mapped)) = mapping.map_gid(mapped_gid)
            {
                mapped_gid = mapped;
            }
            map_gid(mapped_gid, options.numeric_ids_enabled())
        }))
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

        chownat(CWD, destination, owner, group, AtFlags::SYMLINK_NOFOLLOW).map_err(|error| {
            MetadataError::new("preserve ownership", destination, io::Error::from(error))
        })?;
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
) -> Result<(), MetadataError> {
    Ok(())
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
}
