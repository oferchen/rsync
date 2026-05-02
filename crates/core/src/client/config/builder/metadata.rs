use super::*;

impl ClientConfigBuilder {
    /// Requests that ownership be preserved when applying metadata.
    #[must_use]
    #[doc(alias = "--owner")]
    pub const fn owner(mut self, preserve: bool) -> Self {
        self.preserve_owner = preserve;
        self
    }

    #[cfg(all(unix, feature = "xattr"))]
    /// Enables or disables extended attribute preservation for the transfer.
    #[must_use]
    #[doc(alias = "--xattrs")]
    #[doc(alias = "-X")]
    pub const fn xattrs(mut self, preserve: bool) -> Self {
        self.preserve_xattrs = preserve;
        self
    }

    #[cfg(not(all(unix, feature = "xattr")))]
    /// No-op on platforms without xattr support.
    #[must_use]
    pub const fn xattrs(self, _preserve: bool) -> Self {
        self
    }

    builder_setter! {
        /// Applies an explicit ownership override using numeric identifiers.
        #[doc(alias = "--chown")]
        owner_override: Option<u32>,
    }

    /// Requests that group metadata be preserved.
    #[must_use]
    #[doc(alias = "--group")]
    pub const fn group(mut self, preserve: bool) -> Self {
        self.preserve_group = preserve;
        self
    }

    builder_setter! {
        /// Applies an explicit group override using numeric identifiers.
        #[doc(alias = "--chown")]
        group_override: Option<u32>,
    }

    /// Sets the copy-as `USER[:GROUP]` specification.
    ///
    /// When set, rsync will attempt to set file ownership as if running as
    /// the specified user (and optionally group). This is useful when running
    /// rsync as root but wanting files owned by a different user.
    #[must_use]
    #[doc(alias = "--copy-as")]
    pub fn copy_as(mut self, spec: Option<OsString>) -> Self {
        self.copy_as = spec;
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

    builder_setter! {
        /// Enables or disables fake-super mode.
        ///
        /// When enabled, privileged attributes (ownership, special permissions,
        /// ACLs, etc.) are stored/restored using extended attributes instead of
        /// requiring real super-user privileges. This allows non-root users to
        /// backup files with full metadata preservation.
        #[doc(alias = "--fake-super")]
        fake_super: bool,
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

    builder_setter! {
        /// Requests that directory timestamps be skipped when preserving times.
        #[doc(alias = "--omit-dir-times")]
        omit_dir_times: bool,

        /// Controls whether symbolic link modification times should be preserved.
        #[doc(alias = "--omit-link-times")]
        omit_link_times: bool,
    }

    builder_setter! {
        /// Requests that numeric UID/GID values be preserved instead of names.
        #[doc(alias = "--numeric-ids")]
        numeric_ids: bool,

        /// Requests that destination files be preallocated before writing begins.
        #[doc(alias = "--preallocate")]
        preallocate: bool,
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

    #[cfg(all(any(unix, windows), feature = "acl"))]
    /// Enables or disables POSIX ACL preservation when applying metadata.
    #[must_use]
    #[doc(alias = "--acls")]
    #[doc(alias = "-A")]
    pub const fn acls(mut self, preserve: bool) -> Self {
        self.preserve_acls = preserve;
        self
    }

    #[cfg(not(all(any(unix, windows), feature = "acl")))]
    /// No-op on platforms without ACL support.
    #[must_use]
    pub const fn acls(self, _preserve: bool) -> Self {
        self
    }
}
