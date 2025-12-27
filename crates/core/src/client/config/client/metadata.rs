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

    /// Returns the configured copy-as `USER[:GROUP]` specification, if any.
    ///
    /// When set, rsync will attempt to set file ownership as if running as
    /// the specified user (and optionally group). This is useful when running
    /// rsync as root but wanting files owned by a different user.
    #[must_use]
    #[doc(alias = "--copy-as")]
    pub fn copy_as(&self) -> Option<&OsStr> {
        self.copy_as.as_deref()
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

    /// Reports whether fake-super mode is enabled.
    ///
    /// When enabled, privileged attributes (ownership, special permissions,
    /// ACLs, etc.) are stored/restored using extended attributes instead of
    /// requiring real super-user privileges. This allows non-root users to
    /// backup files with full metadata preservation.
    #[must_use]
    #[doc(alias = "--fake-super")]
    pub const fn fake_super(&self) -> bool {
        self.fake_super
    }

    /// Reports whether timestamps should be preserved.
    #[must_use]
    #[doc(alias = "--times")]
    pub const fn preserve_times(&self) -> bool {
        self.preserve_times
    }

    /// Reports whether access times should be preserved.
    ///
    /// When enabled, the source file's access time (atime) is preserved on the
    /// destination. This corresponds to the `-U` / `--atimes` flag in upstream rsync.
    #[must_use]
    #[doc(alias = "--atimes")]
    #[doc(alias = "-U")]
    pub const fn preserve_atimes(&self) -> bool {
        self.preserve_atimes
    }

    /// Reports whether creation times should be preserved.
    ///
    /// When enabled, the source file's creation time (crtime/birthtime) is preserved
    /// on the destination. This is primarily useful on macOS and Windows systems.
    /// Corresponds to the `-N` / `--crtimes` flag in upstream rsync.
    #[must_use]
    #[doc(alias = "--crtimes")]
    #[doc(alias = "-N")]
    pub const fn preserve_crtimes(&self) -> bool {
        self.preserve_crtimes
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

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> ClientConfig {
        ClientConfig::default()
    }

    // Tests for preserve_owner
    #[test]
    fn preserve_owner_default_is_false() {
        let config = default_config();
        assert!(!config.preserve_owner());
    }

    // Tests for owner_override
    #[test]
    fn owner_override_default_is_none() {
        let config = default_config();
        assert!(config.owner_override().is_none());
    }

    // Tests for preserve_group
    #[test]
    fn preserve_group_default_is_false() {
        let config = default_config();
        assert!(!config.preserve_group());
    }

    // Tests for group_override
    #[test]
    fn group_override_default_is_none() {
        let config = default_config();
        assert!(config.group_override().is_none());
    }

    // Tests for copy_as
    #[test]
    fn copy_as_default_is_none() {
        let config = default_config();
        assert!(config.copy_as().is_none());
    }

    // Tests for preserve_executability
    #[test]
    fn preserve_executability_default_is_false() {
        let config = default_config();
        assert!(!config.preserve_executability());
    }

    // Tests for chmod
    #[test]
    fn chmod_default_is_none() {
        let config = default_config();
        assert!(config.chmod().is_none());
    }

    // Tests for user_mapping
    #[test]
    fn user_mapping_default_is_none() {
        let config = default_config();
        assert!(config.user_mapping().is_none());
    }

    // Tests for group_mapping
    #[test]
    fn group_mapping_default_is_none() {
        let config = default_config();
        assert!(config.group_mapping().is_none());
    }

    // Tests for preserve_permissions
    #[test]
    fn preserve_permissions_default_is_false() {
        let config = default_config();
        assert!(!config.preserve_permissions());
    }

    // Tests for fake_super
    #[test]
    fn fake_super_default_is_false() {
        let config = default_config();
        assert!(!config.fake_super());
    }

    // Tests for preserve_times
    #[test]
    fn preserve_times_default_is_false() {
        let config = default_config();
        assert!(!config.preserve_times());
    }

    // Tests for preserve_atimes
    #[test]
    fn preserve_atimes_default_is_false() {
        let config = default_config();
        assert!(!config.preserve_atimes());
    }

    // Tests for preserve_crtimes
    #[test]
    fn preserve_crtimes_default_is_false() {
        let config = default_config();
        assert!(!config.preserve_crtimes());
    }

    // Tests for omit_dir_times
    #[test]
    fn omit_dir_times_default_is_false() {
        let config = default_config();
        assert!(!config.omit_dir_times());
    }

    // Tests for omit_link_times
    #[test]
    fn omit_link_times_default_is_false() {
        let config = default_config();
        assert!(!config.omit_link_times());
    }

    // Tests for preserve_hard_links
    #[test]
    fn preserve_hard_links_default_is_false() {
        let config = default_config();
        assert!(!config.preserve_hard_links());
    }

    // Tests for numeric_ids
    #[test]
    fn numeric_ids_default_is_false() {
        let config = default_config();
        assert!(!config.numeric_ids());
    }

    // Tests for preallocate
    #[test]
    fn preallocate_default_is_false() {
        let config = default_config();
        assert!(!config.preallocate());
    }

    // Tests for preserve_devices
    #[test]
    fn preserve_devices_default_is_false() {
        let config = default_config();
        assert!(!config.preserve_devices());
    }

    // Tests for preserve_specials
    #[test]
    fn preserve_specials_default_is_false() {
        let config = default_config();
        assert!(!config.preserve_specials());
    }
}
