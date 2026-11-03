// The sender half's single read of an entry's ACL and xattrs, and the slot
// that carries them to the receiver half's apply.
impl CopyContext<'_> {
    /// Reads `source`'s ACL and xattrs under `-A`/`-X` as the sender half
    /// lists the entry, or reports the failed read and returns `None` so the
    /// caller drops the entry.
    ///
    /// `metadata` is the entry as listed (followed under `-L`); `name` is the
    /// name upstream's `get_acl` message prints.
    ///
    /// upstream: flist.c:2050-2087 send_file_name() - get_acl() then
    /// get_xattr(), each failure `io_error |= IOERR_GENERAL; return NULL`.
    pub(crate) fn read_source_attrs(
        &mut self,
        source: &Path,
        metadata: &fs::Metadata,
        name: &Path,
    ) -> Option<SourceAttrs> {
        #[allow(unused_mut)] // REASON: filled only with -A/-X support compiled in
        let mut attrs = SourceAttrs::default();
        #[cfg(all(target_os = "linux", feature = "acl"))]
        if self.acls_enabled() && !metadata.file_type().is_symlink() {
            use std::os::unix::fs::PermissionsExt;
            match ::metadata::LocalAcls::read(
                source,
                metadata.permissions().mode(),
                metadata.is_dir(),
                self.options.fake_super_enabled(),
            ) {
                Ok(acls) => attrs.acls = Some(acls),
                Err(error) => {
                    eprintln!(
                        "rsync: [sender] get_acl: sys_acl_get_file({}, {}): {}",
                        name.display(),
                        error.acl_type(),
                        crate::local_copy::upstream_io_error(&error.error)
                    );
                    self.record_flist_io_error();
                    return None;
                }
            }
        }
        #[cfg(all(any(unix, windows), feature = "xattr"))]
        if self.xattrs_enabled() {
            match self.read_source_xattrs(source, metadata) {
                Ok(list) => attrs.xattrs = Some(list),
                Err(error) => {
                    let line = error
                        .get_xattr_text(&logging::full_fname(source))
                        .map_or_else(
                            || error.to_string(),
                            |(call, cause)| {
                                format!("{call}: {}", crate::local_copy::upstream_io_error(cause))
                            },
                        );
                    eprintln!("rsync: [sender] {line}");
                    self.record_flist_io_error();
                    return None;
                }
            }
        }
        let _ = (source, metadata, name);
        Some(attrs)
    }

    /// The sender's xattr list for `source`, shaped by the sender side of any
    /// `x` filter rule.
    ///
    /// upstream: xattrs.c:rsync_xal_get() with `am_sender` set.
    #[cfg(all(any(unix, windows), feature = "xattr"))]
    fn read_source_xattrs(
        &self,
        source: &Path,
        metadata: &fs::Metadata,
    ) -> Result<protocol::xattr::XattrList, ::metadata::MetadataError> {
        let rules = self
            .filter_program
            .as_ref()
            .filter(|program| program.has_xattr_rules());
        let sender_filter = rules.map(|program| {
            move |name: &str| program.allows_xattr(name, ::filters::XattrSide::Sender)
        });
        ::metadata::read_xattrs_for_wire(
            source,
            &::metadata::XattrSendOptions {
                role: ::metadata::XattrRole::Sender,
                follow_symlinks: !metadata.file_type().is_symlink(),
                // upstream: xattrs.c:249 - the sender reads every namespace but
                // system.*, whoever it runs as.
                am_root: false,
                // The local-copy options carry `-X` as a bool. upstream: xattrs.c:274.
                preserve_xattrs: 1,
                fake_super: self.options.fake_super_enabled(),
                filter: sender_filter.as_ref().map(|f| f as &dyn Fn(&str) -> bool),
                checksum_seed: 0,
            },
        )
    }

    /// Hands the receiver half the values the sender half read for the entry
    /// it is about to copy.
    pub(crate) fn set_entry_attrs(&mut self, attrs: SourceAttrs) {
        self.entry_attrs = attrs;
    }

    /// Takes the current entry's values, leaving none behind.
    pub(crate) fn take_entry_attrs(&mut self) -> SourceAttrs {
        std::mem::take(&mut self.entry_attrs)
    }

    /// The current entry's values.
    #[cfg(any(
        all(any(unix, windows), feature = "xattr"),
        all(any(unix, windows), feature = "acl")
    ))]
    pub(crate) const fn entry_attrs(&self) -> &SourceAttrs {
        &self.entry_attrs
    }
}
