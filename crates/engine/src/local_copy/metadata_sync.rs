//! Helpers for synchronizing extended attributes and ACLs.

use super::LocalCopyError;
use ::metadata::MetadataError;

#[cfg(any(
    all(unix, any(feature = "acl", feature = "xattr")),
    all(windows, feature = "acl")
))]
use std::path::Path;

#[cfg(any(
    all(unix, any(feature = "acl", feature = "xattr")),
    all(windows, feature = "acl")
))]
use super::LocalCopyExecution;

#[cfg(all(any(unix, windows), feature = "xattr"))]
use super::FilterProgram;

#[cfg(all(unix, feature = "xattr"))]
use ::filters::XattrSide;

#[cfg(all(unix, feature = "xattr"))]
use ::metadata::XattrSyncFilters;

#[cfg(all(any(unix, windows), feature = "acl"))]
use ::metadata::{sync_acls_confined, sync_acls_via_fake_super};

#[cfg(all(unix, feature = "xattr"))]
use ::metadata::nfsv4_acl::sync_nfsv4_acls;

/// Applies the xattrs the sender half read for this entry, if requested.
///
/// `source_xattrs` is the list read once when the entry was listed; the
/// source is not read again. If a filter program with xattr rules is
/// provided, its sender side already shaped the list and its receiver side
/// governs which destination names are pruned.
///
/// # Errors
///
/// Returns [`LocalCopyError`] if writing the destination's xattrs fails.
#[cfg(all(unix, feature = "xattr"))]
pub(crate) fn sync_xattrs_if_requested(
    preserve_xattrs: bool,
    mode: LocalCopyExecution,
    source_xattrs: Option<&protocol::xattr::XattrList>,
    destination: &Path,
    follow_symlinks: bool,
    filter_program: Option<&FilterProgram>,
    confine_root: Option<&Path>,
) -> Result<(), LocalCopyError> {
    let Some(list) = source_xattrs.filter(|_| preserve_xattrs && !mode.is_dry_run()) else {
        return Ok(());
    };
    // The two halves of this call are upstream's two sides - see
    // [`XattrSyncFilters`]. The sender's `rsync_xal_get()` already applied
    // the sender side when the list was read; deleting the extraneous
    // destination names is the generator's prune in `rsync_xal_set()`, and a
    // side-flagged rule participates in exactly one of them (exclude.c:1010).
    let rules = filter_program.filter(|program| program.has_xattr_rules());
    let sender =
        rules.map(|program| move |name: &str| program.allows_xattr(name, XattrSide::Sender));
    let receiver =
        rules.map(|program| move |name: &str| program.allows_xattr(name, XattrSide::Receiver));
    let filters = XattrSyncFilters {
        source: sender.as_ref().map(|f| f as &dyn Fn(&str) -> bool),
        destination: receiver.as_ref().map(|f| f as &dyn Fn(&str) -> bool),
    };
    ::metadata::sync_xattrs_from_list(list, destination, follow_symlinks, filters, confine_root)
        .map_err(map_metadata_error)
}

/// Whether the destination's extended attributes differ from the sender's
/// list, for the itemize `x` column.
///
/// Mirrors upstream `generator.c:571-577`, which calls `xattr_diff()` for every
/// itemized entry under `preserve_xattrs`. In a local copy upstream still forks
/// a sender and a generator, so the two sides collect their lists with
/// different `rsync_xal_get()` globals: the sender with `am_sender = 1`
/// (`user_only = 0`, plus the `rsync.%FOO` strip below `-XX`), the generator
/// with `am_sender = 0` (`user_only = !am_root`, no strip). `source_xattrs` is
/// the sender's list, read once when the entry was listed; the destination is
/// read here with the generator's role. The comparison itself is
/// [`::metadata::dest_xattrs_differ`], shared with the network receiver.
///
/// Call this before [`sync_xattrs_if_requested`] writes the source attributes
/// onto the destination, or the comparison sees its own output and can never
/// report a difference. No list means no difference.
#[cfg(all(unix, feature = "xattr"))]
pub(crate) fn xattrs_differ(
    source_xattrs: Option<&protocol::xattr::XattrList>,
    destination: &Path,
    follow_symlinks: bool,
    fake_super: bool,
    filter_program: Option<&FilterProgram>,
) -> bool {
    let Some(sender) = source_xattrs else {
        return false;
    };
    // A `P`/`R` rule shapes only the generator's list (exclude.c:1010), so the
    // destination is read through the receiver side of the xattr chain.
    let receiver_filter = filter_program
        .filter(|program| program.has_xattr_rules())
        .map(|program| move |name: &str| program.allows_xattr(name, XattrSide::Receiver));
    ::metadata::dest_xattrs_differ(
        sender,
        destination,
        &::metadata::XattrSendOptions {
            role: ::metadata::XattrRole::Generator,
            follow_symlinks,
            am_root: ::metadata::am_root(),
            preserve_xattrs: 1,
            fake_super,
            filter: receiver_filter.as_ref().map(|f| f as &dyn Fn(&str) -> bool),
            // Both lists hold full values, so the abbreviation digest is never
            // consulted. upstream: xattrs.c:612-622.
            checksum_seed: 0,
        },
    )
}

/// Reports no difference on Windows, whose `-X` maps onto NTFS Alternate Data
/// Streams rather than POSIX extended attributes.
///
/// The `x` column describes upstream's POSIX extended attributes; without a
/// comparison there is nothing to report, and claiming a change on every file
/// is worse than staying silent.
///
/// The gate is the callers' (`all(any(unix, windows), feature = "xattr")`) minus
/// the Unix half the real implementation above covers, so no build carries a
/// definition it never calls - `-D warnings` makes an unused `pub(crate)` item
/// fatal, which is exactly how this landed red on the Windows daemon job.
#[cfg(all(windows, feature = "xattr"))]
pub(crate) const fn xattrs_differ(
    _source_xattrs: Option<&protocol::xattr::XattrList>,
    _destination: &std::path::Path,
    _follow_symlinks: bool,
    _fake_super: bool,
    _filter_program: Option<&FilterProgram>,
) -> bool {
    false
}

/// Synchronizes POSIX/extended ACLs from source to destination if requested.
///
/// No-op when `preserve_acls` is false or in dry-run mode. Under
/// `--fake-super`, the ACL is stashed in the destination's `%aacl`/`%dacl`
/// xattr instead of being applied with a real `setfacl` - an unprivileged
/// fake-super receiver cannot reliably apply an arbitrary ACL (particularly
/// named user/group entries), matching how ownership is stashed in `%stat`
/// ([`store_effective_fake_super_if_requested`]). Unsupported-filesystem
/// errors are silently ignored.
///
/// # Errors
///
/// Returns [`LocalCopyError`] if ACL synchronization fails.
///
/// # Upstream Reference
///
/// Mirrors the `am_root < 0` branches of `get_rsync_acl()`/`set_rsync_acl()`
/// in `acls.c` lines 472-509 and 933-971.
#[cfg(all(any(unix, windows), feature = "acl"))]
pub(crate) fn sync_acls_if_requested(
    preserve_acls: bool,
    fake_super: bool,
    mode: LocalCopyExecution,
    source: &Path,
    destination: &Path,
    follow_symlinks: bool,
    confine_root: Option<&Path>,
) -> Result<(), LocalCopyError> {
    if preserve_acls && !mode.is_dry_run() {
        if fake_super {
            sync_acls_via_fake_super(source, destination, follow_symlinks)
                .map_err(map_metadata_error)?;
        } else {
            // upstream: rsync.c:555-599,800 - the receiver half of a local copy
            // applies the ACL through the confined leaf pin.
            sync_acls_confined(source, destination, follow_symlinks, confine_root)
                .map_err(map_metadata_error)?;
        }
    }
    Ok(())
}

/// Applies the ACLs the sender half read for this entry, if requested.
///
/// On Linux `attrs` carries the access and default ACL read once when the
/// entry was listed, and the source is not read again. Elsewhere the ACL is
/// still read here, from `source`.
///
/// # Errors
///
/// Returns [`LocalCopyError`] if applying the ACL fails.
///
/// upstream: acls.c:set_acl() applies the ACL the file list carried.
#[cfg(all(any(unix, windows), feature = "acl"))]
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_source_acls_if_requested(
    preserve_acls: bool,
    fake_super: bool,
    mode: LocalCopyExecution,
    source: &Path,
    attrs: &super::SourceAttrs,
    destination: &Path,
    follow_symlinks: bool,
    confine_root: Option<&Path>,
) -> Result<(), LocalCopyError> {
    #[cfg(target_os = "linux")]
    {
        let _ = (source, follow_symlinks);
        match attrs.acls.as_ref() {
            Some(acls) if preserve_acls && !mode.is_dry_run() => acls
                .apply(destination, fake_super, confine_root)
                .map_err(map_metadata_error),
            _ => Ok(()),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = attrs;
        sync_acls_if_requested(
            preserve_acls,
            fake_super,
            mode,
            source,
            destination,
            follow_symlinks,
            confine_root,
        )
    }
}

/// Synchronize NFSv4 ACLs from source to destination if preservation is requested.
///
/// NFSv4 ACLs are stored in the `system.nfs4_acl` extended attribute and use
/// a different permission model than POSIX ACLs (ACE-based with inheritance).
/// This function copies the NFSv4 ACL from source to destination when:
/// - `preserve_nfsv4_acls` is true
/// - The operation is not a dry run
/// - The source has an NFSv4 ACL
#[cfg(all(unix, feature = "xattr"))]
pub(crate) fn sync_nfsv4_acls_if_requested(
    preserve_nfsv4_acls: bool,
    mode: LocalCopyExecution,
    source: &Path,
    destination: &Path,
    follow_symlinks: bool,
) -> Result<(), LocalCopyError> {
    if preserve_nfsv4_acls && !mode.is_dry_run() {
        sync_nfsv4_acls(source, destination, follow_symlinks).map_err(map_metadata_error)?;
    }
    Ok(())
}

/// Applies the source's recorded fake-super stat to the destination.
///
/// Under `--fake-super` the source may be a placeholder whose real
/// mode/uid/gid/rdev live in its own `user.rsync.%stat` xattr (written by an
/// earlier fake-super receive). Upstream reads the source through `x_lstat()`,
/// so that recorded stat is the file's stat, but the metadata-apply step only
/// saw the placeholder's raw `fs::Metadata`. Re-run upstream's
/// `set_stat_xattr()` against the recorded stat: the real mode follows the
/// recorded permissions, and the destination `%stat` is written or removed
/// depending on whether the real mode, owner and group can represent it.
///
/// No-op unless `--fake-super` is active. Without `--owner` or `--group` the
/// recorded id is 0, as for any other fake-super receive.
/// upstream: xattrs.c:set_stat_xattr() driven by x_lstat()/get_stat_xattr()
#[cfg(all(unix, feature = "xattr"))]
pub(crate) fn store_effective_fake_super_if_requested(
    options: &::metadata::MetadataOptions,
    source: &Path,
    destination: &Path,
    metadata: &std::fs::Metadata,
) -> Result<(), LocalCopyError> {
    if !options.fake_super_enabled() {
        return Ok(());
    }

    // Only forward a placeholder's recorded stat. When the source carries its
    // own `user.rsync.%stat` (an earlier fake-super receive), it - not the
    // placeholder's raw perms - is the source of truth for uid/gid/mode/rdev.
    // For a real-file source, the ownership + permission apply steps already
    // wrote or removed the destination `%stat` following upstream's
    // set_stat_xattr write-or-remove rule, so re-storing here would resurrect a
    // shim upstream deliberately dropped for a faithful same-owner copy.
    // upstream: xattrs.c:get_stat_xattr() consumed via x_lstat().
    let Ok(Some(mut stat)) = ::metadata::load_fake_super(source) else {
        return Ok(());
    };

    // A `--chmod` tweak makes the destination's deflected mode (written by the
    // permission step) authoritative; keep it rather than the placeholder's.
    if options.chmod().is_some()
        && let Ok(Some(existing)) = ::metadata::load_fake_super(destination)
    {
        stat.mode = existing.mode;
    }
    (stat.uid, stat.gid) = ::metadata::fake_super_ids(options, Some(stat.uid), Some(stat.gid));
    let _ = metadata;
    // upstream: xattrs.c:1304-1356 set_stat_xattr() with the x_lstat() stat.
    ::metadata::apply_fake_super_stat(destination, &stat, options, None).map_err(map_metadata_error)
}

/// Reads a `--fake-super` source's `%stat` record, failing the copy when it
/// does not parse.
///
/// Upstream reads the `%stat` of every entry while it builds the file list and
/// exits `RERR_FILEIO` on a value it cannot parse. Any other read failure
/// reads as no record.
// upstream: flist.c make_file() -> x_lstat() -> xattrs.c:1253-1258
// get_stat_xattr() exit_cleanup(RERR_FILEIO).
#[cfg(all(unix, feature = "xattr"))]
pub(crate) fn ensure_fake_super_source_stat(
    fake_super: bool,
    source: &Path,
) -> Result<Option<::metadata::FakeSuperStat>, LocalCopyError> {
    if !fake_super {
        return Ok(None);
    }
    match ::metadata::load_fake_super(source) {
        Ok(recorded) => Ok(recorded),
        Err(error) if ::metadata::is_corrupt_fake_super(&error) => Err(LocalCopyError::file_io(
            crate::local_copy::LocalCopySide::Sender,
            error.to_string(),
        )),
        Err(_) => Ok(None),
    }
}

/// Reads a `--fake-super` source the way the sender's `x_lstat()` does, so a
/// placeholder reports the device, FIFO or socket, mode and ownership its
/// `%stat` records. Without `--fake-super` the source reads as it is.
///
/// A record that does not parse ends the copy with `RERR_FILEIO`, raised by
/// the sender.
// upstream: flist.c make_file() -> x_lstat() -> xattrs.c:1216-1265
// get_stat_xattr().
pub(crate) fn fake_super_source_stat(
    source: &std::path::Path,
    metadata: &std::fs::Metadata,
    fake_super: bool,
) -> Result<fast_io::EntryMetadata, LocalCopyError> {
    let stat = fast_io::EntryMetadata::from(metadata.clone());
    if !fake_super {
        return Ok(stat);
    }
    ::metadata::fold_fake_super_stat(source, stat).map_err(|error| {
        if ::metadata::is_corrupt_fake_super(&error) {
            LocalCopyError::file_io(crate::local_copy::LocalCopySide::Sender, error.to_string())
        } else {
            LocalCopyError::io("inspect source", source.to_path_buf(), error)
        }
    })
}

/// Reads a `--fake-super` destination through its `%stat` record, so the
/// copy compares and itemizes against the attributes the file stands for.
///
/// A record that does not parse ends the copy with `RERR_FILEIO`, raised by
/// the generator as upstream's is.
// upstream: generator.c:1745 gen_entry_stat() -> link_stat() ->
// xattrs.c:1372 x_lstat() -> xattrs.c:1253-1258 get_stat_xattr().
pub(crate) fn fake_super_destination_stat(
    destination: &std::path::Path,
    stat: fast_io::EntryMetadata,
) -> Result<fast_io::EntryMetadata, LocalCopyError> {
    ::metadata::fold_fake_super_stat(destination, stat).map_err(|error| {
        if ::metadata::is_corrupt_fake_super(&error) {
            LocalCopyError::file_io(
                crate::local_copy::LocalCopySide::Generator,
                error.to_string(),
            )
        } else {
            LocalCopyError::io(
                "inspect existing destination",
                destination.to_path_buf(),
                error,
            )
        }
    })
}
/// Converts a [`MetadataError`] into a [`LocalCopyError`].
pub(crate) fn map_metadata_error(error: MetadataError) -> LocalCopyError {
    let (context, path, source) = error.into_parts();
    LocalCopyError::io(context, path, source)
}
