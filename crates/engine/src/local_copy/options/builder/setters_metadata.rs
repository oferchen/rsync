//! Setter methods for metadata preservation, extended attributes, and modifier options.

use ::metadata::{ChmodModifiers, CopyAsIds, GroupMapping, UserMapping};

use super::LocalCopyOptionsBuilder;

impl LocalCopyOptionsBuilder {
    /// Enables owner preservation.
    #[must_use]
    pub fn preserve_owner(mut self, enabled: bool) -> Self {
        self.preserve_owner = enabled;
        self
    }

    /// Alias for `preserve_owner` for rsync compatibility.
    #[must_use]
    pub fn owner(mut self, enabled: bool) -> Self {
        self.preserve_owner = enabled;
        self
    }

    /// Enables group preservation.
    #[must_use]
    pub fn preserve_group(mut self, enabled: bool) -> Self {
        self.preserve_group = enabled;
        self
    }

    /// Alias for `preserve_group` for rsync compatibility.
    #[must_use]
    pub fn group(mut self, enabled: bool) -> Self {
        self.preserve_group = enabled;
        self
    }

    /// Enables executability preservation.
    #[must_use]
    pub fn preserve_executability(mut self, enabled: bool) -> Self {
        self.preserve_executability = enabled;
        self
    }

    /// Alias for `preserve_executability` for rsync compatibility.
    #[must_use]
    pub fn executability(mut self, enabled: bool) -> Self {
        self.preserve_executability = enabled;
        self
    }

    /// Enables permission preservation.
    #[must_use]
    pub fn preserve_permissions(mut self, enabled: bool) -> Self {
        self.preserve_permissions = enabled;
        self
    }

    /// Alias for `preserve_permissions` for rsync compatibility.
    #[must_use]
    pub fn permissions(mut self, enabled: bool) -> Self {
        self.preserve_permissions = enabled;
        self
    }

    /// Alias for `preserve_permissions` for rsync compatibility.
    #[must_use]
    pub fn perms(mut self, enabled: bool) -> Self {
        self.preserve_permissions = enabled;
        self
    }

    /// Enables timestamp preservation.
    #[must_use]
    pub fn preserve_times(mut self, enabled: bool) -> Self {
        self.preserve_times = enabled;
        self
    }

    /// Alias for `preserve_times` for rsync compatibility.
    #[must_use]
    pub fn times(mut self, enabled: bool) -> Self {
        self.preserve_times = enabled;
        self
    }

    /// Enables access time preservation.
    ///
    /// When enabled, the source file's access time is preserved on the destination.
    /// This corresponds to the `-U` / `--atimes` flag in upstream rsync.
    #[must_use]
    pub fn preserve_atimes(mut self, enabled: bool) -> Self {
        self.preserve_atimes = enabled;
        self
    }

    /// Enables creation time preservation.
    #[must_use]
    #[doc(alias = "--crtimes")]
    #[doc(alias = "-N")]
    pub fn preserve_crtimes(mut self, enabled: bool) -> Self {
        self.preserve_crtimes = enabled;
        self
    }

    /// Enables omitting link times from preservation.
    #[must_use]
    pub fn omit_link_times(mut self, enabled: bool) -> Self {
        self.omit_link_times = enabled;
        self
    }

    /// Sets the owner override.
    #[must_use]
    pub fn owner_override(mut self, owner: Option<u32>) -> Self {
        self.owner_override = owner;
        self
    }

    /// Sets the group override.
    #[must_use]
    pub fn group_override(mut self, group: Option<u32>) -> Self {
        self.group_override = group;
        self
    }

    /// Sets the resolved `--copy-as` identifiers for privilege switching.
    ///
    /// When set, the receiver switches effective UID/GID before file I/O
    /// operations and restores them afterward.
    #[must_use]
    #[doc(alias = "--copy-as")]
    pub fn copy_as(mut self, ids: Option<CopyAsIds>) -> Self {
        self.copy_as = ids;
        self
    }

    /// Enables omitting directory times from preservation.
    #[must_use]
    pub fn omit_dir_times(mut self, enabled: bool) -> Self {
        self.omit_dir_times = enabled;
        self
    }

    /// Enables ACL preservation.
    #[cfg(all(any(unix, windows), feature = "acl"))]
    #[must_use]
    pub fn preserve_acls(mut self, enabled: bool) -> Self {
        self.preserve_acls = enabled;
        self
    }

    /// Alias for `preserve_acls` for rsync compatibility.
    #[cfg(all(any(unix, windows), feature = "acl"))]
    #[must_use]
    pub fn acls(mut self, enabled: bool) -> Self {
        self.preserve_acls = enabled;
        self
    }

    /// Enables extended attribute preservation.
    #[cfg(all(unix, feature = "xattr"))]
    #[must_use]
    pub fn preserve_xattrs(mut self, enabled: bool) -> Self {
        self.preserve_xattrs = enabled;
        self
    }

    /// Alias for `preserve_xattrs` for rsync compatibility.
    #[cfg(all(unix, feature = "xattr"))]
    #[must_use]
    pub fn xattrs(mut self, enabled: bool) -> Self {
        self.preserve_xattrs = enabled;
        self
    }

    /// Enables NFSv4 ACL preservation.
    #[cfg(all(unix, feature = "xattr"))]
    #[must_use]
    pub fn preserve_nfsv4_acls(mut self, enabled: bool) -> Self {
        self.preserve_nfsv4_acls = enabled;
        self
    }

    /// Alias for `preserve_nfsv4_acls` for rsync compatibility.
    #[cfg(all(unix, feature = "xattr"))]
    #[must_use]
    pub fn nfsv4_acls(mut self, enabled: bool) -> Self {
        self.preserve_nfsv4_acls = enabled;
        self
    }

    /// Enables numeric ID handling.
    #[must_use]
    pub fn numeric_ids(mut self, enabled: bool) -> Self {
        self.numeric_ids = enabled;
        self
    }

    /// Sets the chmod modifiers.
    #[must_use]
    pub fn chmod(mut self, modifiers: Option<ChmodModifiers>) -> Self {
        self.chmod = modifiers;
        self
    }

    /// Sets the user mapping.
    #[must_use]
    pub fn user_mapping(mut self, mapping: Option<UserMapping>) -> Self {
        self.user_mapping = mapping;
        self
    }

    /// Sets the group mapping.
    #[must_use]
    pub fn group_mapping(mut self, mapping: Option<GroupMapping>) -> Self {
        self.group_mapping = mapping;
        self
    }

    /// Configures `--super` mode.
    ///
    /// When set to `Some(true)`, the receiving side attempts super-user
    /// activities (ownership preservation, device/special creation) even
    /// if the process is not running as root.
    #[must_use]
    pub fn super_mode(mut self, mode: Option<bool>) -> Self {
        self.super_mode = mode;
        self
    }

    /// Configures `--fake-super` mode.
    ///
    /// When enabled, privileged metadata is stored in extended attributes
    /// instead of being applied directly.
    #[must_use]
    pub fn fake_super(mut self, enabled: bool) -> Self {
        self.fake_super = enabled;
        self
    }
}
