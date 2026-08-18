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

#[cfg(all(any(unix, windows), feature = "acl"))]
use ::metadata::{sync_acls, sync_acls_via_fake_super};

#[cfg(all(unix, feature = "xattr"))]
use ::metadata::sync_xattrs;

#[cfg(all(unix, feature = "xattr"))]
use ::metadata::nfsv4_acl::sync_nfsv4_acls;

/// Synchronizes extended attributes from source to destination if requested.
///
/// If a filter program with xattr rules is provided, only attributes
/// that pass the filter are synchronized. Unsupported-filesystem errors
/// are silently ignored.
///
/// # Errors
///
/// Returns [`LocalCopyError`] if xattr synchronization fails.
#[cfg(all(unix, feature = "xattr"))]
pub(crate) fn sync_xattrs_if_requested(
    preserve_xattrs: bool,
    mode: LocalCopyExecution,
    source: &Path,
    destination: &Path,
    follow_symlinks: bool,
    filter_program: Option<&FilterProgram>,
) -> Result<(), LocalCopyError> {
    if preserve_xattrs && !mode.is_dry_run() {
        if let Some(program) = filter_program {
            if program.has_xattr_rules() {
                // upstream: copy_xattrs (xattrs.c:358) is this function's
                // analogue, and every live caller is generator-side -
                // gen_entry_copy_xattrs (generator.c:1599), the backup copy
                // (generator.c:2430) and copy_file (util1.c:513). Its
                // `user_only = am_sender ? 0 : am_root <= 0` therefore always
                // evaluates with `am_sender == 0`, so the filter is consulted
                // with receiver semantics even though the names are read from
                // the SOURCE.
                let filter = |name: &str| program.allows_xattr(name, XattrSide::Receiver);
                sync_xattrs(source, destination, follow_symlinks, Some(&filter))
                    .map_err(map_metadata_error)?;
            } else {
                sync_xattrs(source, destination, follow_symlinks, None)
                    .map_err(map_metadata_error)?;
            }
        } else {
            sync_xattrs(source, destination, follow_symlinks, None).map_err(map_metadata_error)?;
        }
    }
    Ok(())
}

/// Whether the destination's extended attributes differ from the source's, for
/// the itemize `x` column.
///
/// Mirrors upstream `generator.c:566-572`, which calls `xattr_diff()` for every
/// itemized entry under `preserve_xattrs`. In a local copy upstream still forks
/// a sender and a generator, so the two sides collect their lists with
/// different `rsync_xal_get()` globals: the sender with `am_sender = 1`
/// (`user_only = 0`, plus the `rsync.%FOO` strip below `-XX`), the generator
/// with `am_sender = 0` (`user_only = !am_root`, no strip). Reading each side
/// with its own [`XattrRole`] reproduces that split; the comparison itself is
/// [`::metadata::dest_xattrs_differ`], shared with the network receiver.
///
/// Call this before [`sync_xattrs_if_requested`] writes the source attributes
/// onto the destination, or the comparison sees its own output and can never
/// report a difference.
///
/// A source that cannot be read reports no difference, matching the destination
/// side's failure handling: upstream's `get_xattr()` failure likewise leaves an
/// empty list rather than inventing a change.
#[cfg(all(unix, feature = "xattr"))]
pub(crate) fn xattrs_differ_from_source(
    source: &Path,
    destination: &Path,
    follow_symlinks: bool,
    fake_super: bool,
    filter_program: Option<&FilterProgram>,
) -> bool {
    // The two sides read their lists with different `am_sender`, so they must
    // also consult the xattr chain with different sides: upstream elides a
    // side-flagged rule before its pattern is consulted (exclude.c:1010), so a
    // `H`/`S` rule shapes only the sender's list and a `P`/`R` rule only the
    // generator's. Sharing one closure across both reads would compare
    // asymmetrically filtered lists and manufacture a spurious `x` column.
    let rules = filter_program.filter(|program| program.has_xattr_rules());
    let sender_filter =
        rules.map(|program| move |name: &str| program.allows_xattr(name, XattrSide::Sender));
    let receiver_filter =
        rules.map(|program| move |name: &str| program.allows_xattr(name, XattrSide::Receiver));
    let sender_opts = ::metadata::XattrSendOptions {
        role: ::metadata::XattrRole::Sender,
        follow_symlinks,
        am_root: ::metadata::am_root(),
        // The local-copy options carry `-X` as a bool, so the `rsync.%FOO`
        // strip always applies on the sender side. upstream: xattrs.c:260-267.
        preserve_xattrs: 1,
        fake_super,
        filter: sender_filter.as_ref().map(|f| f as &dyn Fn(&str) -> bool),
        // Both lists are read from local disk with full values, so the
        // abbreviation digest is never consulted. upstream: xattrs.c:584-594.
        checksum_seed: 0,
    };
    let Ok(sender) = ::metadata::read_xattrs_for_wire(source, &sender_opts) else {
        return false;
    };
    ::metadata::dest_xattrs_differ(
        &sender,
        destination,
        &::metadata::XattrSendOptions {
            role: ::metadata::XattrRole::Generator,
            filter: receiver_filter.as_ref().map(|f| f as &dyn Fn(&str) -> bool),
            ..sender_opts
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
pub(crate) const fn xattrs_differ_from_source(
    _source: &std::path::Path,
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
) -> Result<(), LocalCopyError> {
    if preserve_acls && !mode.is_dry_run() {
        if fake_super {
            sync_acls_via_fake_super(source, destination, follow_symlinks)
                .map_err(map_metadata_error)?;
        } else {
            sync_acls(source, destination, follow_symlinks).map_err(map_metadata_error)?;
        }
    }
    Ok(())
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

/// Stores the effective fake-super `user.rsync.%stat` xattr on the destination.
///
/// Under `--fake-super` the source may be a placeholder whose real
/// mode/uid/gid/rdev live in its own `user.rsync.%stat` xattr (written by an
/// earlier fake-super receive). The metadata-apply step only saw the
/// placeholder's raw `fs::Metadata`, so it stored the wrong ownership. Rewrite
/// the destination xattr from [`::metadata::effective_source_stat`], which
/// prefers the source's recorded stat and falls back to its `fs::Metadata`.
///
/// No-op unless `--fake-super` is active together with ownership preservation,
/// matching `metadata::apply::ownership::set_owner_like`'s fake-super gate.
// upstream: xattrs.c:set_stat_xattr() driven by x_lstat()/get_stat_xattr()
#[cfg(all(unix, feature = "xattr"))]
pub(crate) fn store_effective_fake_super_if_requested(
    options: &::metadata::MetadataOptions,
    source: &Path,
    destination: &Path,
    metadata: &std::fs::Metadata,
) -> Result<(), LocalCopyError> {
    let ownership_requested = options.owner()
        || options.group()
        || options.owner_override().is_some()
        || options.group_override().is_some();
    if !options.fake_super_enabled() || !ownership_requested {
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
    if let Ok(Some(existing)) = ::metadata::load_fake_super(destination) {
        if options.chmod().is_some() {
            stat.mode = existing.mode;
        }
        if existing == stat {
            return Ok(());
        }
    }

    let _ = metadata;
    ::metadata::store_fake_super(destination, &stat).map_err(|error| {
        LocalCopyError::io(
            "store fake-super metadata",
            destination.to_path_buf(),
            error,
        )
    })
}

/// Converts a [`MetadataError`] into a [`LocalCopyError`].
pub(crate) fn map_metadata_error(error: MetadataError) -> LocalCopyError {
    let (context, path, source) = error.into_parts();
    LocalCopyError::io(context, path, source)
}
