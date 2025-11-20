use super::*;

impl ClientConfig {
    /// Reports whether ownership preservation was requested.
    #[must_use]
    #[doc(alias = "--owner")]
    pub const fn preserve_owner(&self) -> bool {
        self.preserve_owner
    }

    /// Returns the configured ownership override, if any.
    #[must_use]
    pub const fn owner_override(&self) -> Option<u32> {
        self.owner_override
    }

    /// Reports whether group preservation was requested.
    #[must_use]
    #[doc(alias = "--group")]
    pub const fn preserve_group(&self) -> bool {
        self.preserve_group
    }

    /// Returns the configured group override, if any.
    #[must_use]
    pub const fn group_override(&self) -> Option<u32> {
        self.group_override
    }

    /// Reports whether executability should be preserved.
    #[must_use]
    #[doc(alias = "--executability")]
    #[doc(alias = "-E")]
    pub const fn preserve_executability(&self) -> bool {
        self.preserve_executability
    }

    /// Returns the configured chmod modifiers, if any.
    #[must_use]
    #[doc(alias = "--chmod")]
    pub fn chmod(&self) -> Option<&ChmodModifiers> {
        self.chmod.as_ref()
    }

    /// Returns the configured user mapping, if any.
    #[must_use]
    #[doc(alias = "--usermap")]
    pub fn user_mapping(&self) -> Option<&UserMapping> {
        self.user_mapping.as_ref()
    }

    /// Returns the configured group mapping, if any.
    #[must_use]
    #[doc(alias = "--groupmap")]
    pub fn group_mapping(&self) -> Option<&GroupMapping> {
        self.group_mapping.as_ref()
    }

    /// Reports whether permissions should be preserved.
    #[must_use]
    #[doc(alias = "--perms")]
    pub const fn preserve_permissions(&self) -> bool {
        self.preserve_permissions
    }

    /// Reports whether timestamps should be preserved.
    #[must_use]
    #[doc(alias = "--times")]
    pub const fn preserve_times(&self) -> bool {
        self.preserve_times
    }

    /// Reports whether directory timestamps should be skipped when preserving times.
    #[must_use]
    #[doc(alias = "--omit-dir-times")]
    pub const fn omit_dir_times(&self) -> bool {
        self.omit_dir_times
    }

    /// Indicates whether symbolic link modification times should be skipped.
    #[must_use]
    #[doc(alias = "--omit-link-times")]
    pub const fn omit_link_times(&self) -> bool {
        self.omit_link_times
    }

    /// Reports whether POSIX ACLs should be preserved.
    #[cfg(feature = "acl")]
    #[must_use]
    #[doc(alias = "--acls")]
    #[doc(alias = "-A")]
    pub const fn preserve_acls(&self) -> bool {
        self.preserve_acls
    }

    /// Reports whether extended attributes should be preserved.
    #[cfg(feature = "xattr")]
    #[must_use]
    #[doc(alias = "--xattrs")]
    #[doc(alias = "-X")]
    pub const fn preserve_xattrs(&self) -> bool {
        self.preserve_xattrs
    }

    /// Returns whether hard links should be preserved when copying files.
    #[must_use]
    #[doc(alias = "--hard-links")]
    pub const fn preserve_hard_links(&self) -> bool {
        self.preserve_hard_links
    }

    /// Reports whether numeric UID/GID values should be preserved.
    #[must_use]
    #[doc(alias = "--numeric-ids")]
    pub const fn numeric_ids(&self) -> bool {
        self.numeric_ids
    }

    /// Reports whether destination files should be preallocated before writing.
    #[doc(alias = "--preallocate")]
    pub const fn preallocate(&self) -> bool {
        self.preallocate
    }

    /// Reports whether device nodes should be preserved during the transfer.
    #[must_use]
    #[doc(alias = "--devices")]
    pub const fn preserve_devices(&self) -> bool {
        self.preserve_devices
    }

    /// Reports whether special files such as FIFOs should be preserved.
    #[must_use]
    #[doc(alias = "--specials")]
    pub const fn preserve_specials(&self) -> bool {
        self.preserve_specials
    }
}
