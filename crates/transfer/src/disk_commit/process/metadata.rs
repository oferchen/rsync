//! Post-commit metadata application for the disk commit thread.
//!
//! Applies permissions, ownership, timestamps, ACLs, and xattrs to the
//! committed file, mirroring upstream `set_file_attrs()` in receiver.c.
//! Metadata is applied to the temp file before rename to match upstream
//! `rsync.c:finish_transfer()` line 748: "Change permissions before putting
//! the file into place."

use std::path::{Path, PathBuf};

use metadata::AclIdMapper;
use protocol::acl::AclCache;

use crate::delta_apply::ChecksumVerifier;
use crate::pipeline::messages::{BeginMessage, ComputedChecksum};

use super::super::config::DiskCommitConfig;

/// Applies metadata, ACLs, and xattrs to the given path.
///
/// Called with the temp file path before rename (upstream
/// `rsync.c:finish_transfer()` line 748), or with the final destination
/// path for inplace writes and after cross-device copy fallback.
///
/// Skips metadata for device targets: changing perms/ownership on a device
/// node after writing data is not appropriate.
pub(super) fn apply_file_metadata(
    target_path: &Path,
    begin: &BeginMessage,
    config: &DiskCommitConfig,
    inplace_pre_transfer: Option<&std::fs::Metadata>,
) -> Option<(PathBuf, String)> {
    // The desired metadata rides on the begin message per-file (see
    // `BeginMessage::file_entry`), so the disk thread no longer indexes a shared
    // clone of the whole receiver flist.
    let file_entry = begin.file_entry.as_ref();

    if begin.is_device_target {
        None
    } else {
        // upstream: receiver.c:964 dest_mode() runs against the PRE-transfer
        // destination stat. When metadata is applied to a temp/staged file
        // (target_path != final path), the final destination still holds the
        // file it had before this transfer, so stat it to reproduce
        // dest_mode()'s `stat_mode`/`exists` inputs: `Some(meta)` -> keep the
        // prior perm bits; a missing final path (`None`) -> brand-new file,
        // apply the umask-masked source mode.
        //
        // Writing directly to the final path (inplace) destroys the
        // pre-transfer state by the time we get here, so the caller stats the
        // destination BEFORE `open_output_file` and hands it in. Passing `None`
        // here instead would take the brand-new-file branch and chmod an
        // existing destination to `source_mode & (~CHMOD_BITS | dflt_perms)` -
        // mode 000 whenever the entry carries no perm bits - where upstream
        // preserves the destination's own bits.
        //
        // upstream: rsync.c:449-472 dest_mode() is called (receiver.c:964) with
        // `statret == 0` for an inplace write, because the destination exists.
        let pre_transfer_meta = if target_path != begin.file_path {
            std::fs::symlink_metadata(&begin.file_path).ok()
        } else {
            inplace_pre_transfer.cloned()
        };
        apply_metadata_acls_and_xattrs(
            target_path,
            MetadataApplyInputs {
                file_entry,
                metadata_opts: config.metadata_opts.as_ref(),
                acl_cache: config.acl_cache.as_deref(),
                acl_id_map: config.acl_id_map.as_deref(),
                xattr_list: begin.xattr_list.as_ref(),
                xattr_filter: config.xattr_filter.as_deref(),
                pre_transfer_meta,
                // upstream: xattrs.c:944 rsync_xal_set(fname, ..., fnamecmp) -
                // an abbreviated entry the generator left unrequested resolves
                // against the basis (fnamecmp) actually used. `xattr_basis`
                // carries the --fuzzy / --link-dest / --compare-dest /
                // --partial-dir basis when it differs from the destination;
                // absent that (fnamecmp == fname), the pre-transfer destination
                // still holds the referenced value, so fall back to file_path.
                basis_path: begin.xattr_basis.as_deref().unwrap_or(&begin.file_path),
            },
        )
    }
}

/// Cohesive inputs for [`apply_metadata_acls_and_xattrs`], sourced from the
/// receiver's caches and the begin message. Grouped into one struct so the
/// apply entry point stays within a sensible argument count.
struct MetadataApplyInputs<'a> {
    /// Flist entry describing the desired metadata, if resolvable.
    file_entry: Option<&'a protocol::flist::FileEntry>,
    /// Permission/ownership/timestamp options.
    metadata_opts: Option<&'a metadata::MetadataOptions>,
    /// Cached ACLs to apply after permissions.
    acl_cache: Option<&'a AclCache>,
    /// Cross-host id remapper for named ACL entries.
    acl_id_map: Option<&'a AclIdMapper>,
    /// Received xattr list to apply last.
    xattr_list: Option<&'a protocol::xattr::XattrList>,
    /// `x`-modifier xattr name filter.
    xattr_filter: Option<&'a filters::FilterSet>,
    /// Pre-transfer destination stat feeding `dest_mode()`.
    pre_transfer_meta: Option<std::fs::Metadata>,
    /// fnamecmp basis file for abbreviated xattr resolution.
    basis_path: &'a Path,
}

/// Applies file metadata, ACLs, and xattrs from the receiver's caches.
///
/// Combines `apply_metadata_from_file_entry` with `apply_acls_from_cache` and
/// `apply_xattrs_from_list` into a single call that mirrors upstream
/// `set_file_attrs()` in receiver.c. ACLs are applied after permissions so that
/// any ACL mask is set on the final mode. Xattrs are applied last.
///
/// Returns `Some((path, error_message))` on failure, `None` on success or when
/// no metadata/entry is available.
fn apply_metadata_acls_and_xattrs(
    file_path: &Path,
    inputs: MetadataApplyInputs<'_>,
) -> Option<(PathBuf, String)> {
    let MetadataApplyInputs {
        file_entry,
        metadata_opts,
        acl_cache,
        acl_id_map,
        xattr_list,
        xattr_filter,
        pre_transfer_meta,
        basis_path,
    } = inputs;

    let (opts, entry) = match (metadata_opts, file_entry) {
        (Some(o), Some(e)) => (o, e),
        _ => return None,
    };

    // Skip the cached post-rename stat: the file was just committed from a
    // temp file, so its on-disk metadata will not match the desired entry.
    // Pass the PRE-transfer stat instead so `set_file_attrs()`'s dest_mode()
    // chmod keeps an existing file's prior perm bits and applies the
    // umask-masked source mode to a brand-new file.
    if let Err(e) = metadata::apply_metadata_with_pre_transfer_stat(
        file_path,
        entry,
        opts,
        None,
        pre_transfer_meta,
    ) {
        return Some((file_path.to_path_buf(), e.to_string()));
    }

    // upstream: set_file_attrs() calls set_acl() after perms/times/ownership
    if let Some(cache) = acl_cache {
        if let Some(access_ndx) = entry.acl_ndx() {
            let follow = !entry.is_symlink();
            // upstream: acls.c:930-971 - under `--fake-super`, set_rsync_acl()
            // stashes the ACL in an xattr instead of calling sys_acl_set_file(),
            // since an unprivileged account cannot reliably apply an arbitrary
            // ACL (particularly named user/group entries).
            let result = if opts.fake_super_enabled() {
                metadata::store_acls_via_fake_super(
                    file_path,
                    cache,
                    access_ndx,
                    entry.def_acl_ndx(),
                    follow,
                )
            } else {
                metadata::apply_acls_from_cache(
                    file_path,
                    cache,
                    access_ndx,
                    entry.def_acl_ndx(),
                    follow,
                    Some(entry.mode()),
                    acl_id_map,
                )
            };
            if let Err(e) = result {
                return Some((file_path.to_path_buf(), e.to_string()));
            }
        }
    }

    // upstream: xattrs.c:set_xattr() - apply xattrs after metadata and ACLs
    if let Some(xattr_list) = xattr_list {
        let filter = xattr_filter.map(|set| move |name: &str| set.xattr_name_allowed(name));
        let filter_ref = filter.as_ref().map(|f| f as &dyn Fn(&str) -> bool);
        // upstream: rsync_xal_set(fname, ..., fnamecmp) resolves an abbreviated
        // value against the basis file - the pre-transfer destination, which
        // still holds its old attributes while we stage the temp file.
        if let Err(e) = metadata::apply_xattrs_from_list(
            file_path,
            xattr_list,
            true,
            Some(basis_path),
            filter_ref,
        ) {
            return Some((file_path.to_path_buf(), e.to_string()));
        }
    }

    None
}

/// Finalizes a checksum verifier into a `ComputedChecksum`.
pub(super) fn finalize_checksum(verifier: Option<ChecksumVerifier>) -> Option<ComputedChecksum> {
    verifier.map(|v| {
        let mut buf = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
        let len = v.finalize_into(&mut buf);
        ComputedChecksum { bytes: buf, len }
    })
}

#[cfg(test)]
mod tests {
    use super::apply_file_metadata;
    use crate::disk_commit::DiskCommitConfig;
    use crate::pipeline::messages::BeginMessage;

    fn begin_for(
        path: &std::path::Path,
        entry: Option<protocol::flist::FileEntry>,
    ) -> BeginMessage {
        BeginMessage {
            file_path: path.to_path_buf(),
            target_size: 0,
            file_entry_index: 0,
            checksum_verifier: None,
            is_device_target: false,
            is_inplace: false,
            append_offset: 0,
            xattr_list: None,
            xattr_basis: None,
            file_entry: entry,
        }
    }

    /// The disk thread must source a file's metadata from the entry carried on
    /// its begin message, not from a shared receiver flist (which this crate no
    /// longer clones for the disk thread). Proven by handing `apply_file_metadata`
    /// a config with an empty flist context and an entry only on the message:
    /// the permission and mtime it applies must be the message entry's, which
    /// only holds if the message channel is the source of truth.
    #[cfg(unix)]
    #[test]
    fn metadata_is_sourced_from_begin_message_entry() {
        use std::os::unix::fs::PermissionsExt;

        let dir = test_support::create_tempdir();
        let path = dir.path().join("from_message.dat");
        std::fs::write(&path, b"payload").unwrap();
        // Seed a mode that differs from the entry's so a pass-through (no apply)
        // or a wrong source would be visible.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

        let mut entry = protocol::flist::FileEntry::new_file(
            std::path::PathBuf::from("from_message.dat"),
            7,
            0o640,
        );
        entry.set_mtime(1_600_000_000, 0);

        let config = DiskCommitConfig {
            metadata_opts: Some(
                metadata::MetadataOptions::new()
                    .preserve_permissions(true)
                    .preserve_times(true),
            ),
            ..DiskCommitConfig::default()
        };

        // target_path == file_path: the final-destination apply path.
        let begin = begin_for(&path, Some(entry));
        let err = apply_file_metadata(&path, &begin, &config, None);
        assert!(err.is_none(), "metadata apply reported an error: {err:?}");

        let meta = std::fs::metadata(&path).unwrap();
        assert_eq!(
            meta.permissions().mode() & 0o777,
            0o640,
            "permissions must come from the begin-message entry"
        );
        let mtime = filetime::FileTime::from_last_modification_time(&meta).unix_seconds();
        assert_eq!(
            mtime, 1_600_000_000,
            "mtime must come from the begin-message entry"
        );
    }

    /// With no entry on the message (and no shared flist), the apply path is a
    /// no-op rather than a panic or an index into a list that no longer exists.
    /// This pins the `None` contract that replaced the old
    /// `file_list.get(index)` miss.
    #[test]
    fn absent_entry_applies_no_metadata() {
        let dir = test_support::create_tempdir();
        let path = dir.path().join("no_entry.dat");
        std::fs::write(&path, b"payload").unwrap();

        let config = DiskCommitConfig {
            metadata_opts: Some(metadata::MetadataOptions::new().preserve_permissions(true)),
            ..DiskCommitConfig::default()
        };
        let begin = begin_for(&path, None);
        assert!(apply_file_metadata(&path, &begin, &config, None).is_none());
    }

    /// ACL case: the access/default ACL indices live in the entry's `extras`
    /// box, which the receiver's `reclaim_heap_data` zeroes when it frees a
    /// completed flist segment. Carrying the entry on the begin message means
    /// the disk thread reads those indices from its own per-file copy, immune to
    /// that reclaim - so the ACL apply branch (`entry.acl_ndx()` /
    /// `entry.def_acl_ndx()` in `apply_metadata_acls_and_xattrs`) sees the right
    /// values. This pins the delivery of the ACL indices via the message; the OS
    /// ACL application itself is covered by the daemon ACL integration tests.
    #[test]
    fn acl_indices_ride_on_the_begin_message_entry() {
        let dir = test_support::create_tempdir();
        let path = dir.path().join("acl.dat");
        std::fs::write(&path, b"payload").unwrap();

        let mut entry =
            protocol::flist::FileEntry::new_file(std::path::PathBuf::from("acl.dat"), 7, 0o640);
        entry.set_acl_ndx(3);
        entry.set_def_acl_ndx(4);

        let begin = begin_for(&path, Some(entry));
        let carried = begin.file_entry.as_ref().expect("entry present on message");
        assert_eq!(
            carried.acl_ndx(),
            Some(3),
            "access ACL index must survive on the begin-message entry"
        );
        assert_eq!(
            carried.def_acl_ndx(),
            Some(4),
            "default ACL index must survive on the begin-message entry"
        );

        // With no ACL cache configured the apply path skips ACLs cleanly (the
        // `if let Some(cache)` guard), so carrying acl indices never regresses a
        // plain metadata apply.
        let config = DiskCommitConfig {
            metadata_opts: Some(metadata::MetadataOptions::new().preserve_permissions(true)),
            ..DiskCommitConfig::default()
        };
        assert!(apply_file_metadata(&path, &begin, &config, None).is_none());
    }
}
