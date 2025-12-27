use super::*;

impl ClientConfigBuilder {
    /// Adds a `--compare-dest` reference directory consulted during execution.
    #[must_use]
    #[doc(alias = "--compare-dest")]
    pub fn compare_destination<P: Into<PathBuf>>(mut self, path: P) -> Self {
        self.reference_directories.push(ReferenceDirectory::new(
            ReferenceDirectoryKind::Compare,
            path,
        ));
        self
    }

    /// Adds a `--copy-dest` reference directory consulted during execution.
    #[must_use]
    #[doc(alias = "--copy-dest")]
    pub fn copy_destination<P: Into<PathBuf>>(mut self, path: P) -> Self {
        self.reference_directories
            .push(ReferenceDirectory::new(ReferenceDirectoryKind::Copy, path));
        self
    }

    /// Adds a `--link-dest` reference directory consulted during execution.
    #[must_use]
    #[doc(alias = "--link-dest")]
    pub fn link_destination<P: Into<PathBuf>>(mut self, path: P) -> Self {
        self.reference_directories
            .push(ReferenceDirectory::new(ReferenceDirectoryKind::Link, path));
        self
    }

    /// Extends the link-destination list used when creating hard links for unchanged files.
    #[must_use]
    #[doc(alias = "--link-dest")]
    pub fn extend_link_dests<I, P>(mut self, paths: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        self.link_dest_paths.extend(
            paths
                .into_iter()
                .map(Into::into)
                .filter(|path| !path.as_os_str().is_empty()),
        );
        self
    }

    /// Enables or disables creation of backups before overwriting or deleting entries.
    #[must_use]
    #[doc(alias = "--backup")]
    #[doc(alias = "-b")]
    pub const fn backup(mut self, backup: bool) -> Self {
        self.backup = backup;
        self
    }

    /// Configures the optional directory that should receive backup entries.
    #[must_use]
    #[doc(alias = "--backup-dir")]
    pub fn backup_directory<P: Into<PathBuf>>(mut self, directory: Option<P>) -> Self {
        self.backup_dir = directory.map(Into::into);
        if self.backup_dir.is_some() {
            self.backup = true;
        }
        self
    }

    /// Overrides the suffix appended to backup file names.
    #[must_use]
    #[doc(alias = "--suffix")]
    pub fn backup_suffix<S: Into<OsString>>(mut self, suffix: Option<S>) -> Self {
        self.backup_suffix = suffix.map(Into::into);
        if self.backup_suffix.is_some() {
            self.backup = true;
        }
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
    fn compare_destination_adds_directory() {
        let config = builder()
            .compare_destination("/tmp/compare")
            .build();
        assert_eq!(config.reference_directories().len(), 1);
        assert_eq!(
            config.reference_directories()[0].kind(),
            ReferenceDirectoryKind::Compare
        );
    }

    #[test]
    fn copy_destination_adds_directory() {
        let config = builder()
            .copy_destination("/tmp/copy")
            .build();
        assert_eq!(config.reference_directories().len(), 1);
        assert_eq!(
            config.reference_directories()[0].kind(),
            ReferenceDirectoryKind::Copy
        );
    }

    #[test]
    fn link_destination_adds_directory() {
        let config = builder()
            .link_destination("/tmp/link")
            .build();
        assert_eq!(config.reference_directories().len(), 1);
        assert_eq!(
            config.reference_directories()[0].kind(),
            ReferenceDirectoryKind::Link
        );
    }

    #[test]
    fn multiple_reference_directories_accumulate() {
        let config = builder()
            .compare_destination("/tmp/compare")
            .copy_destination("/tmp/copy")
            .link_destination("/tmp/link")
            .build();
        assert_eq!(config.reference_directories().len(), 3);
    }

    #[test]
    fn backup_sets_flag() {
        let config = builder().backup(true).build();
        assert!(config.backup());
    }

    #[test]
    fn backup_false_clears_flag() {
        let config = builder()
            .backup(true)
            .backup(false)
            .build();
        assert!(!config.backup());
    }

    #[test]
    fn backup_directory_sets_path() {
        let config = builder()
            .backup_directory(Some("/tmp/backup"))
            .build();
        assert!(config.backup_directory().is_some());
        assert_eq!(
            config.backup_directory().unwrap().to_str().unwrap(),
            "/tmp/backup"
        );
    }

    #[test]
    fn backup_directory_enables_backup() {
        let config = builder()
            .backup_directory(Some("/tmp/backup"))
            .build();
        assert!(config.backup());
    }

    #[test]
    fn backup_directory_none_clears_path() {
        let config = builder()
            .backup_directory(Some("/tmp/backup"))
            .backup_directory(None::<&str>)
            .build();
        assert!(config.backup_directory().is_none());
    }

    #[test]
    fn backup_suffix_sets_value() {
        let config = builder()
            .backup_suffix(Some("~"))
            .build();
        assert!(config.backup_suffix().is_some());
        assert_eq!(config.backup_suffix().unwrap().to_str().unwrap(), "~");
    }

    #[test]
    fn backup_suffix_enables_backup() {
        let config = builder()
            .backup_suffix(Some(".bak"))
            .build();
        assert!(config.backup());
    }

    #[test]
    fn backup_suffix_none_clears_value() {
        let config = builder()
            .backup_suffix(Some("~"))
            .backup_suffix(None::<&str>)
            .build();
        assert!(config.backup_suffix().is_none());
    }

    #[test]
    fn default_reference_directories_is_empty() {
        let config = builder().build();
        assert!(config.reference_directories().is_empty());
    }

    #[test]
    fn default_backup_is_false() {
        let config = builder().build();
        assert!(!config.backup());
    }

    #[test]
    fn default_backup_directory_is_none() {
        let config = builder().build();
        assert!(config.backup_directory().is_none());
    }

    #[test]
    fn default_backup_suffix_is_none() {
        let config = builder().build();
        assert!(config.backup_suffix().is_none());
    }
}
