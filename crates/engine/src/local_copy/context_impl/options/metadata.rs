impl<'a> CopyContext<'a> {
    /// Builds a [`MetadataOptions`] snapshot from the current copy options.
    pub(super) fn metadata_options(&self) -> MetadataOptions {
        MetadataOptions::new()
            .preserve_owner(self.options.preserve_owner())
            .preserve_group(self.options.preserve_group())
            .preserve_executability(self.options.preserve_executability())
            .preserve_permissions(self.options.preserve_permissions())
            .preserve_times(self.options.preserve_times())
            .preserve_atimes(self.options.preserve_atimes())
            .preserve_crtimes(self.options.preserve_crtimes())
            .numeric_ids(self.options.numeric_ids_enabled())
            .fake_super(self.options.fake_super_enabled())
            .with_owner_override(self.options.owner_override())
            .with_group_override(self.options.group_override())
            .with_chmod(self.options.chmod().cloned())
            .with_user_mapping(self.options.user_mapping().cloned())
            .with_group_mapping(self.options.group_mapping().cloned())
            .with_keep_dirlinks(self.options.keep_dirlinks_enabled())
    }

    /// Reports whether ACL preservation is enabled.
    #[cfg(all(any(unix, windows), feature = "acl"))]
    pub(super) const fn acls_enabled(&self) -> bool {
        self.options.acls_enabled()
    }

    #[cfg(all(any(unix, windows), feature = "xattr"))]
    pub(super) const fn xattrs_enabled(&self) -> bool {
        self.options.preserve_xattrs()
    }

    /// Returns whether `--numeric-ids` is enabled.
    #[cfg(unix)]
    pub(super) const fn numeric_ids_enabled(&self) -> bool {
        self.options.numeric_ids_enabled()
    }
}
