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
) -> Option<(PathBuf, String)> {
    let file_entry = config
        .file_list
        .as_ref()
        .and_then(|fl| fl.get(begin.file_entry_index));

    if begin.is_device_target {
        None
    } else {
        apply_metadata_acls_and_xattrs(
            target_path,
            file_entry,
            config.metadata_opts.as_ref(),
            config.acl_cache.as_deref(),
            config.acl_id_map.as_deref(),
            begin.xattr_list.as_ref(),
            config.xattr_filter.as_deref(),
        )
    }
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
    file_entry: Option<&protocol::flist::FileEntry>,
    metadata_opts: Option<&metadata::MetadataOptions>,
    acl_cache: Option<&AclCache>,
    acl_id_map: Option<&AclIdMapper>,
    xattr_list: Option<&protocol::xattr::XattrList>,
    xattr_filter: Option<&filters::FilterSet>,
) -> Option<(PathBuf, String)> {
    let (opts, entry) = match (metadata_opts, file_entry) {
        (Some(o), Some(e)) => (o, e),
        _ => return None,
    };

    // Skip the stat inside apply_metadata_from_file_entry: the file was
    // just renamed into place from a temp file, so its metadata will not
    // match the desired entry. Pass None to apply unconditionally.
    if let Err(e) = metadata::apply_metadata_with_cached_stat(file_path, entry, opts, None) {
        return Some((file_path.to_path_buf(), e.to_string()));
    }

    // upstream: set_file_attrs() calls set_acl() after perms/times/ownership
    if let Some(cache) = acl_cache {
        if let Some(access_ndx) = entry.acl_ndx() {
            let follow = !entry.is_symlink();
            if let Err(e) = metadata::apply_acls_from_cache(
                file_path,
                cache,
                access_ndx,
                entry.def_acl_ndx(),
                follow,
                Some(entry.mode()),
                acl_id_map,
            ) {
                return Some((file_path.to_path_buf(), e.to_string()));
            }
        }
    }

    // upstream: xattrs.c:set_xattr() - apply xattrs after metadata and ACLs
    if let Some(xattr_list) = xattr_list {
        let filter = xattr_filter.map(|set| move |name: &str| set.xattr_name_allowed(name));
        let filter_ref = filter.as_ref().map(|f| f as &dyn Fn(&str) -> bool);
        if let Err(e) = metadata::apply_xattrs_from_list(file_path, xattr_list, true, filter_ref) {
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
