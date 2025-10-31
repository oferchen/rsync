use rsync_meta::ChmodModifiers;

use super::types::LocalCopyOptions;

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

    /// Applies an explicit group override to transferred entries.
    #[must_use]
    #[doc(alias = "--chown")]
    pub const fn with_group_override(mut self, group: Option<u32>) -> Self {
        self.group_override = group;
        self
    }

    /// Configures chmod modifiers that should be applied after metadata preservation.
    #[must_use]
    pub fn with_chmod(mut self, modifiers: Option<ChmodModifiers>) -> Self {
        self.chmod = modifiers;
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

    #[cfg(feature = "acl")]
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

    /// Reports whether ownership preservation has been requested.
    #[must_use]
    pub const fn preserve_owner(&self) -> bool {
        self.preserve_owner
    }

    /// Returns the configured ownership override, if any.
    #[must_use]
    pub const fn owner_override(&self) -> Option<u32> {
        self.owner_override
    }

    /// Reports whether group preservation has been requested.
    #[must_use]
    pub const fn preserve_group(&self) -> bool {
        self.preserve_group
    }

    /// Returns the configured group override, if any.
    #[must_use]
    pub const fn group_override(&self) -> Option<u32> {
        self.group_override
    }

    /// Returns the configured chmod modifiers, if any.
    #[must_use]
    pub fn chmod(&self) -> Option<&ChmodModifiers> {
        self.chmod.as_ref()
    }

    /// Reports whether permissions should be preserved.
    #[must_use]
    pub const fn preserve_permissions(&self) -> bool {
        self.preserve_permissions
    }

    /// Reports whether timestamps should be preserved.
    #[must_use]
    pub const fn preserve_times(&self) -> bool {
        self.preserve_times
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

    #[cfg(feature = "acl")]
    /// Returns whether POSIX ACLs should be preserved.
    #[must_use]
    pub const fn preserve_acls(&self) -> bool {
        self.preserve_acls
    }

    #[cfg(feature = "acl")]
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
}

#[cfg(feature = "xattr")]
impl LocalCopyOptions {
    /// Requests that extended attributes be preserved when copying entries.
    #[must_use]
    #[doc(alias = "--xattrs")]
    #[doc(alias = "-X")]
    pub const fn xattrs(mut self, preserve: bool) -> Self {
        self.preserve_xattrs = preserve;
        self
    }

    /// Reports whether extended attribute preservation has been requested.
    #[must_use]
    pub const fn preserve_xattrs(&self) -> bool {
        self.preserve_xattrs
    }
}
