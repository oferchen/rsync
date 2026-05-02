//! Read-only accessor methods for metadata preservation settings on
//! [`LocalCopyOptions`](super::super::types::LocalCopyOptions).
//!
//! Each accessor borrows `&self` and returns the current value of its
//! corresponding metadata field. Platform-conditional accessors (ACLs,
//! xattrs) are gated behind the same `cfg` flags as their setter
//! counterparts.

use ::metadata::{ChmodModifiers, CopyAsIds, GroupMapping, UserMapping};

use super::super::types::LocalCopyOptions;

impl LocalCopyOptions {
    /// Reports whether ownership preservation has been requested.
    #[must_use]
    pub const fn preserve_owner(&self) -> bool {
        self.preserve_owner
    }

    /// Returns the configured ownership override, if any.
    pub const fn owner_override(&self) -> Option<u32> {
        self.owner_override
    }

    /// Reports whether group preservation has been requested.
    #[must_use]
    pub const fn preserve_group(&self) -> bool {
        self.preserve_group
    }

    /// Returns the configured group override, if any.
    pub const fn group_override(&self) -> Option<u32> {
        self.group_override
    }

    /// Returns the resolved `--copy-as` identifiers, if any.
    ///
    /// When present, the receiver should switch effective UID/GID before
    /// file I/O operations.
    pub const fn copy_as_ids(&self) -> Option<&CopyAsIds> {
        self.copy_as.as_ref()
    }

    /// Returns the configured chmod modifiers, if any.
    pub const fn chmod(&self) -> Option<&ChmodModifiers> {
        self.chmod.as_ref()
    }

    /// Returns the configured user mapping, if any.
    pub const fn user_mapping(&self) -> Option<&UserMapping> {
        self.user_mapping.as_ref()
    }

    /// Returns the configured group mapping, if any.
    pub const fn group_mapping(&self) -> Option<&GroupMapping> {
        self.group_mapping.as_ref()
    }

    /// Reports whether permissions should be preserved.
    #[must_use]
    pub const fn preserve_permissions(&self) -> bool {
        self.preserve_permissions
    }

    /// Reports whether executability should be preserved.
    #[must_use]
    pub const fn preserve_executability(&self) -> bool {
        self.preserve_executability
    }

    /// Reports whether timestamps should be preserved.
    #[must_use]
    pub const fn preserve_times(&self) -> bool {
        self.preserve_times
    }

    /// Reports whether access times should be preserved.
    #[must_use]
    pub const fn preserve_atimes(&self) -> bool {
        self.preserve_atimes
    }

    /// Reports whether creation times should be preserved.
    #[must_use]
    pub const fn preserve_crtimes(&self) -> bool {
        self.preserve_crtimes
    }

    /// Reports whether directory modification times should be skipped during metadata preservation.
    #[must_use]
    pub const fn omit_dir_times_enabled(&self) -> bool {
        self.omit_dir_times
    }

    /// Returns whether symbolic link timestamps should be skipped.
    #[must_use]
    pub const fn omit_link_times_enabled(&self) -> bool {
        self.omit_link_times
    }

    #[cfg(all(any(unix, windows), feature = "acl"))]
    /// Returns whether POSIX ACLs should be preserved.
    #[must_use]
    pub const fn preserve_acls(&self) -> bool {
        self.preserve_acls
    }

    #[cfg(all(any(unix, windows), feature = "acl"))]
    /// Reports whether ACL preservation is enabled.
    #[must_use]
    pub const fn acls_enabled(&self) -> bool {
        self.preserve_acls
    }

    /// Reports whether numeric UID/GID preservation has been requested.
    #[must_use]
    pub const fn numeric_ids_enabled(&self) -> bool {
        self.numeric_ids
    }

    /// Reports the configured `--super` mode.
    pub const fn super_mode_setting(&self) -> Option<bool> {
        self.super_mode
    }

    /// Returns whether super-user activities should be attempted.
    ///
    /// When `--super` is explicitly set, that value is returned directly.
    /// Otherwise the decision falls back to checking whether the effective
    /// user is root (UID 0 on Unix). When `--fake-super` is active, the
    /// result is forced to `false` so callers route privileged operations
    /// (chown, mknod, mkfifo) through the fake-super xattr placeholder
    /// path, mirroring upstream's `am_root < 0` sentinel.
    // upstream: options.c:89 - am_root tri-state: 0 normal, 1 root, 2 --super,
    //                          -1 --fake-super (negative is "fake")
    // upstream: clientserver.c:1102-1105 - daemon `fake super = yes` forces am_root=-1
    // upstream: syscall.c do_mknod() - am_root<0 sentinel substitutes 0600 placeholder
    #[must_use]
    pub fn am_root(&self) -> bool {
        effective_am_root(self.super_mode, self.fake_super)
    }

    /// Reports whether `--fake-super` mode is enabled.
    #[must_use]
    pub const fn fake_super_enabled(&self) -> bool {
        self.fake_super
    }
}

/// Resolves the effective `am_root` boolean for privilege-gated operations.
///
/// Mirrors upstream rsync's `am_root` tri-state: when `fake_super` is set,
/// the result is `false` regardless of `super_mode` (matching upstream's
/// `am_root = -1` sentinel that demotes privileged paths to the
/// fake-super xattr placeholder branch). Otherwise the explicit `--super`
/// flag wins, falling back to the effective UID check.
// upstream: options.c:89 - am_root tri-state, -1 is "fake-super"
// upstream: syscall.c do_mknod() - am_root<0 sentinel substitutes 0600 placeholder
#[must_use]
pub(super) fn effective_am_root(super_mode: Option<bool>, fake_super: bool) -> bool {
    if fake_super {
        return false;
    }
    match super_mode {
        Some(value) => value,
        None => is_effective_root(),
    }
}

/// Returns whether the current process is running as the effective root user.
#[cfg(unix)]
pub(super) fn is_effective_root() -> bool {
    rustix::process::geteuid().is_root()
}

/// On non-Unix platforms, there is no concept of a root user.
#[cfg(not(unix))]
pub(super) fn is_effective_root() -> bool {
    false
}

#[cfg(all(unix, feature = "xattr"))]
impl LocalCopyOptions {
    /// Reports whether extended attribute preservation has been requested.
    #[must_use]
    pub const fn preserve_xattrs(&self) -> bool {
        self.preserve_xattrs
    }

    /// Reports whether NFSv4 ACL preservation has been requested.
    #[must_use]
    pub const fn preserve_nfsv4_acls(&self) -> bool {
        self.preserve_nfsv4_acls
    }
}
