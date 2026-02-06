use ::metadata::{ChmodModifiers, GroupMapping, UserMapping};

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

    #[cfg(all(unix, feature = "acl"))]
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
    pub const fn chmod(&self) -> Option<&ChmodModifiers> {
        self.chmod.as_ref()
    }

    /// Returns the configured user mapping, if any.
    #[must_use]
    pub const fn user_mapping(&self) -> Option<&UserMapping> {
        self.user_mapping.as_ref()
    }

    /// Returns the configured group mapping, if any.
    #[must_use]
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

    #[cfg(all(unix, feature = "acl"))]
    /// Returns whether POSIX ACLs should be preserved.
    #[must_use]
    pub const fn preserve_acls(&self) -> bool {
        self.preserve_acls
    }

    #[cfg(all(unix, feature = "acl"))]
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

    /// Reports the configured `--super` mode.
    #[must_use]
    pub const fn super_mode_setting(&self) -> Option<bool> {
        self.super_mode
    }

    /// Returns whether super-user activities should be attempted.
    ///
    /// When `--super` is explicitly set, that value is returned directly.
    /// Otherwise the decision falls back to checking whether the effective
    /// user is root (UID 0 on Unix).
    #[must_use]
    pub fn am_root(&self) -> bool {
        match self.super_mode {
            Some(value) => value,
            None => is_effective_root(),
        }
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

    /// Reports whether `--fake-super` mode is enabled.
    #[must_use]
    pub const fn fake_super_enabled(&self) -> bool {
        self.fake_super
    }
}

/// Returns whether the current process is running as the effective root user.
#[cfg(unix)]
fn is_effective_root() -> bool {
    rustix::process::geteuid().is_root()
}

/// On non-Unix platforms, there is no concept of a root user.
#[cfg(not(unix))]
fn is_effective_root() -> bool {
    false
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

    /// Reports whether extended attribute preservation has been requested.
    #[must_use]
    pub const fn preserve_xattrs(&self) -> bool {
        self.preserve_xattrs
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

    /// Reports whether NFSv4 ACL preservation has been requested.
    #[must_use]
    pub const fn preserve_nfsv4_acls(&self) -> bool {
        self.preserve_nfsv4_acls
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_preservation() {
        let options = LocalCopyOptions::new().owner(true);
        assert!(options.preserve_owner());
    }

    #[test]
    fn owner_override() {
        let options = LocalCopyOptions::new().with_owner_override(Some(1000));
        assert_eq!(options.owner_override(), Some(1000));
    }

    #[test]
    fn group_preservation() {
        let options = LocalCopyOptions::new().group(true);
        assert!(options.preserve_group());
    }

    #[test]
    fn group_override() {
        let options = LocalCopyOptions::new().with_group_override(Some(1000));
        assert_eq!(options.group_override(), Some(1000));
    }

    #[test]
    fn executability_preservation() {
        let options = LocalCopyOptions::new().executability(true);
        assert!(options.preserve_executability());
    }

    #[test]
    fn permissions_preservation() {
        let options = LocalCopyOptions::new().permissions(true);
        assert!(options.preserve_permissions());
    }

    #[test]
    fn times_preservation() {
        let options = LocalCopyOptions::new().times(true);
        assert!(options.preserve_times());
    }

    #[test]
    fn omit_dir_times() {
        let options = LocalCopyOptions::new().omit_dir_times(true);
        assert!(options.omit_dir_times_enabled());
    }

    #[test]
    fn omit_link_times() {
        let options = LocalCopyOptions::new().omit_link_times(true);
        assert!(options.omit_link_times_enabled());
    }

    #[test]
    fn numeric_ids() {
        let options = LocalCopyOptions::new().numeric_ids(true);
        assert!(options.numeric_ids_enabled());
    }

    #[test]
    fn chmod_none_by_default() {
        let options = LocalCopyOptions::new();
        assert!(options.chmod().is_none());
    }

    #[test]
    fn user_mapping_none_by_default() {
        let options = LocalCopyOptions::new();
        assert!(options.user_mapping().is_none());
    }

    #[test]
    fn group_mapping_none_by_default() {
        let options = LocalCopyOptions::new();
        assert!(options.group_mapping().is_none());
    }

    #[test]
    fn super_mode_none_by_default() {
        let options = LocalCopyOptions::new();
        assert_eq!(options.super_mode_setting(), None);
    }

    #[test]
    fn super_mode_set_true() {
        let options = LocalCopyOptions::new().super_mode(Some(true));
        assert_eq!(options.super_mode_setting(), Some(true));
        assert!(options.am_root());
    }

    #[test]
    fn super_mode_set_false() {
        let options = LocalCopyOptions::new().super_mode(Some(false));
        assert_eq!(options.super_mode_setting(), Some(false));
        assert!(!options.am_root());
    }

    #[test]
    fn super_mode_none_defers_to_euid() {
        let options = LocalCopyOptions::new();
        // am_root() should reflect whether we are actually root
        let expected = is_effective_root();
        assert_eq!(options.am_root(), expected);
    }

    #[test]
    fn fake_super_disabled_by_default() {
        let options = LocalCopyOptions::new();
        assert!(!options.fake_super_enabled());
    }

    #[test]
    fn fake_super_round_trip() {
        let options = LocalCopyOptions::new().fake_super(true);
        assert!(options.fake_super_enabled());

        let disabled = options.fake_super(false);
        assert!(!disabled.fake_super_enabled());
    }
}
