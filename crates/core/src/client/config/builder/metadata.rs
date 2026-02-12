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

    #[cfg(all(unix, feature = "acl"))]
    /// Enables or disables POSIX ACL preservation when applying metadata.
    #[must_use]
    #[doc(alias = "--acls")]
    #[doc(alias = "-A")]
    pub const fn acls(mut self, preserve: bool) -> Self {
        self.preserve_acls = preserve;
        self
    }

    #[cfg(not(all(unix, feature = "acl")))]
    /// No-op on platforms without ACL support.
    #[must_use]
    pub const fn acls(self, _preserve: bool) -> Self {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn builder() -> ClientConfigBuilder {
        ClientConfigBuilder::default()
    }

    #[test]
    fn owner_sets_preserve() {
        let config = builder().owner(true).build();
        assert!(config.preserve_owner());
    }

    #[test]
    fn owner_false_clears_preserve() {
        let config = builder().owner(true).owner(false).build();
        assert!(!config.preserve_owner());
    }

    #[test]
    fn owner_override_sets_value() {
        let config = builder().owner_override(Some(1000)).build();
        assert_eq!(config.owner_override(), Some(1000));
    }

    #[test]
    fn owner_override_none_clears_value() {
        let config = builder()
            .owner_override(Some(1000))
            .owner_override(None)
            .build();
        assert!(config.owner_override().is_none());
    }

    #[test]
    fn group_sets_preserve() {
        let config = builder().group(true).build();
        assert!(config.preserve_group());
    }

    #[test]
    fn group_false_clears_preserve() {
        let config = builder().group(true).group(false).build();
        assert!(!config.preserve_group());
    }

    #[test]
    fn group_override_sets_value() {
        let config = builder().group_override(Some(1000)).build();
        assert_eq!(config.group_override(), Some(1000));
    }

    #[test]
    fn group_override_none_clears_value() {
        let config = builder()
            .group_override(Some(1000))
            .group_override(None)
            .build();
        assert!(config.group_override().is_none());
    }

    #[test]
    fn copy_as_sets_value() {
        let config = builder()
            .copy_as(Some(OsString::from("user:group")))
            .build();
        assert!(config.copy_as().is_some());
    }

    #[test]
    fn copy_as_none_clears_value() {
        let config = builder()
            .copy_as(Some(OsString::from("user:group")))
            .copy_as(None)
            .build();
        assert!(config.copy_as().is_none());
    }

    #[test]
    fn executability_sets_flag() {
        let config = builder().executability(true).build();
        assert!(config.preserve_executability());
    }

    #[test]
    fn executability_false_clears_flag() {
        let config = builder().executability(true).executability(false).build();
        assert!(!config.preserve_executability());
    }

    #[test]
    fn permissions_sets_flag() {
        let config = builder().permissions(true).build();
        assert!(config.preserve_permissions());
    }

    #[test]
    fn permissions_false_clears_flag() {
        let config = builder().permissions(true).permissions(false).build();
        assert!(!config.preserve_permissions());
    }

    #[test]
    fn fake_super_sets_flag() {
        let config = builder().fake_super(true).build();
        assert!(config.fake_super());
    }

    #[test]
    fn fake_super_false_clears_flag() {
        let config = builder().fake_super(true).fake_super(false).build();
        assert!(!config.fake_super());
    }

    #[test]
    fn times_sets_flag() {
        let config = builder().times(true).build();
        assert!(config.preserve_times());
    }

    #[test]
    fn times_false_clears_flag() {
        let config = builder().times(true).times(false).build();
        assert!(!config.preserve_times());
    }

    #[test]
    fn atimes_sets_flag() {
        let config = builder().atimes(true).build();
        assert!(config.preserve_atimes());
    }

    #[test]
    fn atimes_false_clears_flag() {
        let config = builder().atimes(true).atimes(false).build();
        assert!(!config.preserve_atimes());
    }

    #[test]
    fn crtimes_sets_flag() {
        let config = builder().crtimes(true).build();
        assert!(config.preserve_crtimes());
    }

    #[test]
    fn crtimes_false_clears_flag() {
        let config = builder().crtimes(true).crtimes(false).build();
        assert!(!config.preserve_crtimes());
    }

    #[test]
    fn omit_dir_times_sets_flag() {
        let config = builder().omit_dir_times(true).build();
        assert!(config.omit_dir_times());
    }

    #[test]
    fn omit_dir_times_false_clears_flag() {
        let config = builder().omit_dir_times(true).omit_dir_times(false).build();
        assert!(!config.omit_dir_times());
    }

    #[test]
    fn omit_link_times_sets_flag() {
        let config = builder().omit_link_times(true).build();
        assert!(config.omit_link_times());
    }

    #[test]
    fn omit_link_times_false_clears_flag() {
        let config = builder()
            .omit_link_times(true)
            .omit_link_times(false)
            .build();
        assert!(!config.omit_link_times());
    }

    #[test]
    fn numeric_ids_sets_flag() {
        let config = builder().numeric_ids(true).build();
        assert!(config.numeric_ids());
    }

    #[test]
    fn numeric_ids_false_clears_flag() {
        let config = builder().numeric_ids(true).numeric_ids(false).build();
        assert!(!config.numeric_ids());
    }

    #[test]
    fn preallocate_sets_flag() {
        let config = builder().preallocate(true).build();
        assert!(config.preallocate());
    }

    #[test]
    fn preallocate_false_clears_flag() {
        let config = builder().preallocate(true).preallocate(false).build();
        assert!(!config.preallocate());
    }

    #[test]
    fn hard_links_sets_flag() {
        let config = builder().hard_links(true).build();
        assert!(config.preserve_hard_links());
    }

    #[test]
    fn hard_links_false_clears_flag() {
        let config = builder().hard_links(true).hard_links(false).build();
        assert!(!config.preserve_hard_links());
    }

    #[test]
    fn devices_sets_flag() {
        let config = builder().devices(true).build();
        assert!(config.preserve_devices());
    }

    #[test]
    fn devices_false_clears_flag() {
        let config = builder().devices(true).devices(false).build();
        assert!(!config.preserve_devices());
    }

    #[test]
    fn specials_sets_flag() {
        let config = builder().specials(true).build();
        assert!(config.preserve_specials());
    }

    #[test]
    fn specials_false_clears_flag() {
        let config = builder().specials(true).specials(false).build();
        assert!(!config.preserve_specials());
    }

    #[test]
    fn default_preserve_owner_is_false() {
        let config = builder().build();
        assert!(!config.preserve_owner());
    }

    #[test]
    fn default_preserve_group_is_false() {
        let config = builder().build();
        assert!(!config.preserve_group());
    }

    #[test]
    fn default_preserve_times_is_false() {
        let config = builder().build();
        assert!(!config.preserve_times());
    }

    #[test]
    fn default_preserve_permissions_is_false() {
        let config = builder().build();
        assert!(!config.preserve_permissions());
    }
}
