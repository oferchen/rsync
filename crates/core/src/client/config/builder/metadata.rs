use super::*;

impl ClientConfigBuilder {
    /// Requests that ownership be preserved when applying metadata.
    #[must_use]
    #[doc(alias = "--owner")]
    pub const fn owner(mut self, preserve: bool) -> Self {
        self.preserve_owner = preserve;
        self
    }

    #[cfg(feature = "xattr")]
    /// Enables or disables extended attribute preservation for the transfer.
    #[must_use]
    #[doc(alias = "--xattrs")]
    #[doc(alias = "-X")]
    pub const fn xattrs(mut self, preserve: bool) -> Self {
        self.preserve_xattrs = preserve;
        self
    }

    /// Applies an explicit ownership override using numeric identifiers.
    #[must_use]
    #[doc(alias = "--chown")]
    pub const fn owner_override(mut self, owner: Option<u32>) -> Self {
        self.owner_override = owner;
        self
    }

    /// Requests that group metadata be preserved.
    #[must_use]
    #[doc(alias = "--group")]
    pub const fn group(mut self, preserve: bool) -> Self {
        self.preserve_group = preserve;
        self
    }

    /// Applies an explicit group override using numeric identifiers.
    #[must_use]
    #[doc(alias = "--chown")]
    pub const fn group_override(mut self, group: Option<u32>) -> Self {
        self.group_override = group;
        self
    }

    /// Applies chmod modifiers that should be evaluated after metadata preservation.
    #[must_use]
    #[doc(alias = "--chmod")]
    pub fn chmod(mut self, modifiers: Option<ChmodModifiers>) -> Self {
        self.chmod = modifiers;
        self
    }

    /// Applies a custom user mapping derived from `--usermap`.
    #[must_use]
    pub fn user_mapping(mut self, mapping: Option<UserMapping>) -> Self {
        self.user_mapping = mapping;
        self
    }

    /// Applies a custom group mapping derived from `--groupmap`.
    #[must_use]
    pub fn group_mapping(mut self, mapping: Option<GroupMapping>) -> Self {
        self.group_mapping = mapping;
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
    /// When enabled, the source file's access time (atime) is preserved on the
    /// destination. This corresponds to the `-U` / `--atimes` flag in upstream rsync.
    #[must_use]
    #[doc(alias = "--atimes")]
    #[doc(alias = "-U")]
    pub const fn atimes(mut self, preserve: bool) -> Self {
        self.preserve_atimes = preserve;
        self
    }

    /// Requests that creation times be preserved when applying metadata.
    ///
    /// When enabled, the source file's creation time (crtime/birthtime) is preserved
    /// on the destination. This is primarily useful on macOS and Windows systems.
    /// Corresponds to the `-N` / `--crtimes` flag in upstream rsync.
    #[must_use]
    #[doc(alias = "--crtimes")]
    #[doc(alias = "-N")]
    pub const fn crtimes(mut self, preserve: bool) -> Self {
        self.preserve_crtimes = preserve;
        self
    }

    /// Requests that directory timestamps be skipped when preserving times.
    #[must_use]
    #[doc(alias = "--omit-dir-times")]
    pub const fn omit_dir_times(mut self, omit: bool) -> Self {
        self.omit_dir_times = omit;
        self
    }

    /// Controls whether symbolic link modification times should be preserved.
    #[must_use]
    #[doc(alias = "--omit-link-times")]
    pub const fn omit_link_times(mut self, omit: bool) -> Self {
        self.omit_link_times = omit;
        self
    }

    /// Requests that numeric UID/GID values be preserved instead of names.
    #[must_use]
    #[doc(alias = "--numeric-ids")]
    pub const fn numeric_ids(mut self, numeric_ids: bool) -> Self {
        self.numeric_ids = numeric_ids;
        self
    }

    /// Requests that destination files be preallocated before writing begins.
    #[must_use]
    #[doc(alias = "--preallocate")]
    pub const fn preallocate(mut self, preallocate: bool) -> Self {
        self.preallocate = preallocate;
        self
    }

    /// Enables or disables preservation of hard links between files.
    #[must_use]
    #[doc(alias = "--hard-links")]
    pub const fn hard_links(mut self, preserve: bool) -> Self {
        self.preserve_hard_links = preserve;
        self
    }

    /// Enables or disables copying of device nodes during the transfer.
    #[must_use]
    #[doc(alias = "--devices")]
    pub const fn devices(mut self, preserve: bool) -> Self {
        self.preserve_devices = preserve;
        self
    }

    /// Enables or disables copying of special files during the transfer.
    #[must_use]
    #[doc(alias = "--specials")]
    pub const fn specials(mut self, preserve: bool) -> Self {
        self.preserve_specials = preserve;
        self
    }

    #[cfg(feature = "acl")]
    /// Enables or disables POSIX ACL preservation when applying metadata.
    #[must_use]
    #[doc(alias = "--acls")]
    #[doc(alias = "-A")]
    pub const fn acls(mut self, preserve: bool) -> Self {
        self.preserve_acls = preserve;
        self
    }
}
