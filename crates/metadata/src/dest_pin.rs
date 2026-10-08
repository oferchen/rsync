//! Pins a destination leaf beneath the receiver's destination root before an
//! xattr or ACL write.
//!
//! `lsetxattr` and `acl_set_file` decline to follow the *leaf* symlink but the
//! kernel still follows symlinks in PARENT components, re-walking them on every
//! call. A parent flipped to a symlink mid-apply therefore redirects an
//! attacker-chosen attribute outside the destination tree. Upstream closes that
//! by driving the writes off a held `O_NOFOLLOW` fd, and by refusing outright
//! when it cannot get one - never by falling back to the path.
//!
//! # Upstream Reference
//!
//! - `rsync-3.5.1/rsync.c:582-597` - the secure re-pin of the leaf, and
//!   `xattr_refuse` when the re-pin fails on a slashed path
//! - `rsync-3.5.1/rsync.c:713-716,800` - xattr and ACL writes skipped on refusal

use std::path::Path;

/// Where a destination metadata write lands.
pub(crate) enum DestPin {
    /// The path-based l-variant. Upstream's `fd < 0` arm: what a non-hardened
    /// receiver uses, and what oc uses when no confinement root is supplied.
    Path,
    /// A confined `O_NOFOLLOW` fd pinning the leaf. A parent flipped after the
    /// pin cannot redirect the write.
    #[cfg(unix)]
    Pinned(std::fs::File),
    /// The pin was required and failed on a slashed path. Every write is
    /// skipped: falling back to `Path` here would reinstate exactly the
    /// redirect the pin exists to refuse.
    #[cfg(unix)]
    Refused,
}

/// Pins `path` beneath `confine_root`.
///
/// `None` keeps upstream's path-based arm for a receiver that is not hardened.
#[cfg(unix)]
pub(crate) fn pin_destination(path: &Path, confine_root: Option<&Path>) -> DestPin {
    let Some(root) = confine_root else {
        return DestPin::Path;
    };
    let Ok(relative) = path.strip_prefix(root) else {
        // Not beneath the root the caller named: the caller, not this pin, is
        // the layer that knows what that means. Keep the path-based behaviour
        // rather than inventing a refusal here.
        return DestPin::Path;
    };
    // upstream: rsync.c:589-594 - a directory leaf needs O_DIRECTORY.
    let kind = if path.is_dir() {
        fast_io::DestLeafKind::Directory
    } else {
        fast_io::DestLeafKind::NonDirectory
    };
    match fast_io::pin_dest_leaf_confined(root, relative, kind) {
        Ok(file) => DestPin::Pinned(file),
        // upstream: rsync.c:597 - `if (held_fd < 0 && strchr(fname, '/'))
        // xattr_refuse = 1;`. A single-component name has no parent to flip,
        // so the path-based call is still safe there.
        Err(_) if relative.parent().is_some_and(|p| !p.as_os_str().is_empty()) => {
            DestPin::Refused
        }
        Err(_) => DestPin::Path,
    }
}

/// The confined pin is Unix-only; Windows destination attributes go through
/// the ADS/DACL layer, which does not have this shape.
#[cfg(not(unix))]
pub(crate) fn pin_destination(_path: &Path, _confine_root: Option<&Path>) -> DestPin {
    DestPin::Path
}
