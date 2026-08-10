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
