//! Builder-pattern setter methods for metadata preservation on
//! [`LocalCopyOptions`](super::super::types::LocalCopyOptions).
//!
//! Each setter consumes and returns `self`, enabling fluent configuration
//! chains. These methods control which metadata attributes are preserved
//! during local copy operations.

use ::metadata::{ChmodModifiers, CopyAsIds, GroupMapping, UserMapping};

use super::super::types::LocalCopyOptions;

impl LocalCopyOptions {
    /// Requests that ownership be preserved when applying metadata.
    #[must_use]
    #[doc(alias = "--owner")]
    pub const fn owner(mut self, preserve: bool) -> Self {
        self.preserve_owner = preserve;
        self
    }

    /// Applies an explicit ownership override to transferred entries.
    #[must_use]
    #[doc(alias = "--chown")]
    pub const fn with_owner_override(mut self, owner: Option<u32>) -> Self {
        self.owner_override = owner;
        self
    }

    /// Requests that the group be preserved when applying metadata.
    #[must_use]
    #[doc(alias = "--group")]
    pub const fn group(mut self, preserve: bool) -> Self {
        self.preserve_group = preserve;
        self
    }

    /// Requests that executability be preserved when applying metadata.
    #[must_use]
    #[doc(alias = "--executability")]
    #[doc(alias = "-E")]
    pub const fn executability(mut self, preserve: bool) -> Self {
        self.preserve_executability = preserve;
        self
    }

    /// Applies an explicit group override to transferred entries.
    #[must_use]
    #[doc(alias = "--chown")]
    pub const fn with_group_override(mut self, group: Option<u32>) -> Self {
        self.group_override = group;
        self
    }

    /// Sets the resolved `--copy-as` identifiers for privilege switching.
    ///
    /// When set, the receiver switches effective UID/GID before file I/O
    /// operations and restores them afterward via an RAII guard.
    ///
    /// # Upstream Reference
    ///
    /// - `rsync.c:do_as_root()` -- bracket around privileged file operations
    #[must_use]
    #[doc(alias = "--copy-as")]
    pub fn with_copy_as(mut self, ids: Option<CopyAsIds>) -> Self {
        self.copy_as = ids;
        self
    }

    /// Configures chmod modifiers that should be applied after metadata preservation.
    #[must_use]
    pub fn with_chmod(mut self, modifiers: Option<ChmodModifiers>) -> Self {
        self.chmod = modifiers;
        self
    }

    /// Applies a custom user mapping derived from `--usermap`.
    #[must_use]
    pub fn with_user_mapping(mut self, mapping: Option<UserMapping>) -> Self {
        self.user_mapping = mapping;
        self
    }

    /// Applies a custom group mapping derived from `--groupmap`.
    #[must_use]
    pub fn with_group_mapping(mut self, mapping: Option<GroupMapping>) -> Self {
        self.group_mapping = mapping;
        self
    }

    /// Requests that permissions be preserved when applying metadata.
    #[must_use]
    #[doc(alias = "--perms")]
    pub const fn permissions(mut self, preserve: bool) -> Self {
        self.preserve_permissions = preserve;
        self
    }

    /// Requests that timestamps be preserved when applying metadata.
    #[must_use]
    #[doc(alias = "--times")]
    pub const fn times(mut self, preserve: bool) -> Self {
        self.preserve_times = preserve;
        self
    }

    /// Requests that access times be preserved when applying metadata.
    ///
    /// When enabled, the source file's access time is preserved on the destination.
    /// This corresponds to the `-U` / `--atimes` flag in upstream rsync.
    #[must_use]
    #[doc(alias = "--atimes")]
    #[doc(alias = "-U")]
    pub const fn atimes(mut self, preserve: bool) -> Self {
        self.preserve_atimes = preserve;
        self
    }

    /// Requests that creation times be preserved when applying metadata.
    ///
    /// When enabled, the source file's creation time (birth time) is preserved
    /// on the destination. This corresponds to the `-N` / `--crtimes` flag in
    /// upstream rsync. Creation time setting is only supported on macOS; on
    /// other platforms the flag is accepted but has no effect.
    #[must_use]
    #[doc(alias = "--crtimes")]
    #[doc(alias = "-N")]
    pub const fn crtimes(mut self, preserve: bool) -> Self {
        self.preserve_crtimes = preserve;
        self
    }

    /// Skips preserving directory modification times even when [`Self::times`] is enabled.
    #[must_use]
    #[doc(alias = "--omit-dir-times")]
    pub const fn omit_dir_times(mut self, omit: bool) -> Self {
        self.omit_dir_times = omit;
        self
    }

    /// Controls whether symbolic link timestamps are preserved.
    #[must_use]
    #[doc(alias = "--omit-link-times")]
    pub const fn omit_link_times(mut self, omit: bool) -> Self {
        self.omit_link_times = omit;
        self
    }

    #[cfg(all(any(unix, windows), feature = "acl"))]
    /// Requests that POSIX ACLs be preserved when applying metadata.
    #[must_use]
    #[doc(alias = "--acls")]
    pub const fn acls(mut self, preserve: bool) -> Self {
        self.preserve_acls = preserve;
        self
    }

    /// Requests numeric UID/GID preservation.
    #[must_use]
    #[doc(alias = "--numeric-ids")]
    pub const fn numeric_ids(mut self, numeric: bool) -> Self {
        self.numeric_ids = numeric;
        self
    }

    /// Configures `--super` mode.
    ///
    /// When set to `Some(true)`, the receiving side attempts super-user
    /// activities (ownership preservation, device/special creation) even if
    /// the process is not running as root. `Some(false)` explicitly disables
    /// these attempts. `None` defers to the default behaviour, which checks
    /// the effective UID at runtime.
    #[must_use]
    #[doc(alias = "--super")]
    pub const fn super_mode(mut self, mode: Option<bool>) -> Self {
        self.super_mode = mode;
        self
    }

    /// Configures `--fake-super` mode.
    ///
    /// When enabled, privileged metadata (ownership, device numbers) is
    /// stored in extended attributes (`user.rsync.%stat`) instead of being
    /// applied directly. This allows backup and restore operations without
    /// root privileges.
    #[must_use]
    #[doc(alias = "--fake-super")]
    pub const fn fake_super(mut self, enabled: bool) -> Self {
        self.fake_super = enabled;
        self
    }
}

#[cfg(all(unix, feature = "xattr"))]
impl LocalCopyOptions {
    /// Requests that extended attributes be preserved when copying entries.
    #[must_use]
    #[doc(alias = "--xattrs")]
    #[doc(alias = "-X")]
    pub const fn xattrs(mut self, preserve: bool) -> Self {
        self.preserve_xattrs = preserve;
        self
    }

    /// Requests that NFSv4 ACLs be preserved when copying entries.
    ///
    /// NFSv4 ACLs are distinct from POSIX ACLs and use an ACE-based model
    /// with inheritance support. They are stored in the `system.nfs4_acl`
    /// extended attribute.
    #[must_use]
    pub const fn nfsv4_acls(mut self, preserve: bool) -> Self {
        self.preserve_nfsv4_acls = preserve;
        self
    }
}
